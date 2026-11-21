use std::collections::BTreeMap;
use std::time::Duration;

use shagger_shared::net::game_state::Vec3;
use tokio::time::Instant;

use super::{PlayerInput, Route, WashoiNavigation, horizontal_distance};
use crate::games::washoi::{Difficulty, goals::OwnVitals};

const STALL_TIMEOUT: Duration = Duration::from_millis(800);
const RETRY_DELAY: Duration = Duration::from_secs(5);
const MIN_PROGRESS: f64 = 8.0;
const DISPLACEMENT: f64 = 128.0;
const MAX_BLOCKS: usize = 16;
const LOCAL_PICKUP_RADIUS: f64 = 24.0;
// Washoi takes a beneficial item on the first tick a player overlaps it. An
// item that stays after this wait is one the bot can't take, such as full
// ammo or a new weapon that needs USE to swap.
const PICKUP_DWELL: Duration = Duration::from_millis(400);
const DECLINED_PICKUP: Duration = Duration::from_secs(15);

#[derive(Clone, Copy)]
pub(in crate::games::washoi) struct RouteObservation<'a> {
    pub(in crate::games::washoi) now: Instant,
    pub(in crate::games::washoi) vitals: OwnVitals,
    pub(in crate::games::washoi) difficulty: Difficulty,
    pub(in crate::games::washoi) pickup_active: &'a [bool],
    pub(in crate::games::washoi) allow_takeoff: bool,
    pub(in crate::games::washoi) landed_id: Option<&'a str>,
}

#[derive(Clone, Copy, PartialEq)]
struct Leg {
    from: [f64; 3],
    to: [f64; 3],
}

struct Progress {
    next: usize,
    distance: f64,
    updated: Instant,
}

struct ActiveRoute {
    index: usize,
    next: usize,
    progress: Option<Progress>,
    arrived: Option<Instant>,
}

#[derive(Default)]
pub(in crate::games::washoi) struct RouteFollower {
    active: Option<ActiveRoute>,
    last_position: Option<Vec3>,
    blocked: Vec<(Leg, Instant)>,
    pickup_unavailable: BTreeMap<usize, Instant>,
}

impl RouteFollower {
    /// Drop the active route, such as on death or a learned traversal, and
    /// keep what the bot learned about blocked legs and missing pickups.
    /// Both memories expire on their own.
    pub(in crate::games::washoi) fn detach(&mut self) {
        self.active = None;
        self.last_position = None;
    }

    pub(in crate::games::washoi) fn takeoff<'a>(
        &self,
        navigation: &'a WashoiNavigation,
    ) -> Option<&'a str> {
        let active = self.active.as_ref()?;
        let route = &navigation.routes[active.index];
        (active.next >= route.waypoints.len() - 1)
            .then_some(route.learned_route.as_deref())
            .flatten()
    }

    pub(in crate::games::washoi) fn steer(
        &mut self,
        navigation: &WashoiNavigation,
        position: Vec3,
        observation: RouteObservation<'_>,
    ) -> PlayerInput {
        let RouteObservation {
            now,
            vitals,
            difficulty,
            pickup_active,
            allow_takeoff,
            landed_id,
        } = observation;
        self.blocked.retain(|(_, until)| now < *until);
        self.pickup_unavailable.retain(|_, until| now < *until);
        if self
            .last_position
            .replace(position)
            .is_some_and(|previous| {
                (position.x - previous.x)
                    .hypot(position.z - previous.z)
                    .hypot(position.y - previous.y)
                    > DISPLACEMENT
            })
        {
            self.active = None;
        }
        if self.active.is_none() {
            let unblocked = |route: &Route| {
                let available = route.pickup.is_none_or(|pickup| {
                    pickup.benefits(vitals) && !self.pickup_unavailable.contains_key(&pickup.id)
                });
                available
                    && (0..route.waypoints.len())
                        .all(|next| !self.blocked.iter().any(|(leg, _)| *leg == route.leg(next)))
            };
            let continued = landed_id.and_then(|id| {
                navigation.route_at(position, |route| {
                    route.resumed_from.as_deref() == Some(id) && unblocked(route)
                })
            });
            let Some(index) = continued.or_else(|| {
                navigation.select_route(
                    position,
                    |route| {
                        route.resumed_from.is_none()
                            && (allow_takeoff || route.learned_route.is_none())
                            && unblocked(route)
                    },
                    |route| {
                        route.pickup.map_or(0.0, |pickup| {
                            let travel = route
                                .waypoints
                                .iter()
                                .fold((route.entry, 0.0), |(from, total), &to| {
                                    let distance = (to[0] - from[0]).hypot(to[2] - from[2]);
                                    (to, total + distance)
                                })
                                .1;
                            pickup.value(vitals, difficulty) / (1.0 + travel / 512.0)
                        })
                    },
                )
            }) else {
                return PlayerInput::default();
            };
            self.active = Some(ActiveRoute {
                index,
                next: 0,
                progress: None,
                arrived: None,
            });
        }

        let active = self.active.as_mut().expect("route selected");
        let input = navigation.steer(active.index, &mut active.next, position);
        let route = &navigation.routes[active.index];
        let Some(&goal) = route.waypoints.get(active.next) else {
            // Learn availability only at the end of a route within pickup
            // range. The server's full-map mask is broader than bot knowledge.
            if let Some(pickup) = route.pickup
                && let Some(&target) = route.waypoints.last()
                && horizontal_distance(position, target) <= LOCAL_PICKUP_RADIUS
                && (target[1] - position.y).abs() <= LOCAL_PICKUP_RADIUS
            {
                let recheck = if pickup_active.get(pickup.id) == Some(&false) {
                    Some(difficulty.pickup_recheck())
                } else {
                    let arrived = *active.arrived.get_or_insert(now);
                    (now - arrived >= PICKUP_DWELL).then_some(DECLINED_PICKUP)
                };
                if let Some(recheck) = recheck {
                    self.pickup_unavailable.insert(pickup.id, now + recheck);
                    self.active = None;
                }
            }
            return input;
        };
        let distance = horizontal_distance(position, goal);
        let progress = active.progress.get_or_insert(Progress {
            next: active.next,
            distance,
            updated: now,
        });
        if active.next != progress.next || distance + MIN_PROGRESS <= progress.distance {
            *progress = Progress {
                next: active.next,
                distance,
                updated: now,
            };
        }
        if now - progress.updated >= STALL_TIMEOUT {
            if self.blocked.len() == MAX_BLOCKS {
                self.blocked.remove(0);
            }
            self.blocked
                .push((route.leg(active.next), now + RETRY_DELAY));
            self.active = None;
            // Leave a neutral update between abandoning one segment and
            // driving toward a new anchor.
            return PlayerInput::default();
        }
        input
    }
}

impl Route {
    fn leg(&self, next: usize) -> Leg {
        let from = if next == 0 {
            self.entry
        } else {
            self.waypoints[next - 1]
        };
        Leg {
            from,
            to: self.waypoints[next],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observe(now: Instant, allow_takeoff: bool, landed_id: Option<&str>) -> RouteObservation<'_> {
        RouteObservation {
            now,
            vitals: OwnVitals::default(),
            difficulty: Difficulty::default(),
            pickup_active: &[],
            allow_takeoff,
            landed_id,
        }
    }

    #[test]
    fn low_health_prefers_recovery_and_only_notices_missing_pickups_locally() {
        let nav: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","routes":[
            {"entry":[0,0,0],"waypoints":[[40,0,0]],"pickup":{"id":0,"kind":"health"}},
            {"entry":[0,0,0],"waypoints":[[0,0,40]],"pickup":{"id":1,"kind":"armor"}},
            {"entry":[40,0,0],"waypoints":[[40,0,40]],"pickup":{"id":1,"kind":"armor"}}
        ]}"#,
        )
        .unwrap();
        let now = Instant::now();
        let origin = Vec3::default();
        let health = Vec3::new(40.0, 0.0, 0.0);
        let mut follower = RouteFollower::default();
        let observation = RouteObservation {
            vitals: OwnVitals {
                health: Some(10),
                armor: Some(50),
            },
            pickup_active: &[false, true],
            ..observe(now, false, None)
        };
        assert_eq!(
            follower.steer(&nav, origin, observation).direction,
            [1.0, 0.0, 0.0]
        );
        assert_eq!(
            follower.steer(&nav, health, observation).direction,
            [0.0; 3]
        );
        assert_eq!(
            follower.steer(&nav, health, observation).direction,
            [0.0, 0.0, 1.0]
        );
        // A death or learned traversal detaches the route. The missing
        // health stays remembered until its recheck time.
        follower.detach();
        assert_eq!(
            follower.steer(&nav, origin, observation).direction,
            [0.0, 0.0, 1.0]
        );

        let mut healthy = RouteFollower::default();
        let observation = RouteObservation {
            vitals: OwnVitals {
                health: Some(100),
                armor: Some(0),
            },
            ..observation
        };
        assert_eq!(
            healthy.steer(&nav, origin, observation).direction,
            [0.0, 0.0, 1.0]
        );
    }

    #[test]
    fn leaves_a_pickup_that_stays_after_arrival_and_skips_full_health() {
        let nav: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","routes":[
            {"entry":[0,0,0],"waypoints":[[40,0,0]],"pickup":{"id":0,"kind":"health","item":"large"}},
            {"entry":[0,0,0],"waypoints":[[0,0,40]],"pickup":{"id":1,"kind":"weapon"}},
            {"entry":[0,0,40],"waypoints":[[40,0,40]],"pickup":{"id":0,"kind":"health","item":"large"}},
            {"entry":[0,0,40],"waypoints":[[0,0,80]]}
        ]}"#,
        )
        .unwrap();
        let now = Instant::now();
        let observation = |now| RouteObservation {
            vitals: OwnVitals {
                health: Some(100),
                armor: Some(0),
            },
            pickup_active: &[true, true],
            ..observe(now, false, None)
        };
        let mut follower = RouteFollower::default();
        // The health scores higher but full health can't take it.
        assert_eq!(
            follower
                .steer(&nav, Vec3::default(), observation(now))
                .direction,
            [0.0, 0.0, 1.0]
        );
        let weapon = Vec3::new(0.0, 0.0, 40.0);
        assert_eq!(
            follower.steer(&nav, weapon, observation(now)).direction,
            [0.0; 3]
        );
        let waited = now + PICKUP_DWELL;
        assert_eq!(
            follower.steer(&nav, weapon, observation(waited)).direction,
            [0.0; 3]
        );
        assert_eq!(
            follower.steer(&nav, weapon, observation(waited)).direction,
            [0.0, 0.0, 1.0]
        );
    }

    #[test]
    fn graph_continuation_requires_the_matching_landing() {
        let graph: WashoiNavigation = serde_json::from_str(
            r#"{
            "map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,0,0],[0,0,40],[60,0,0],[200,0,200],[160,0,200]],
                "edges":[[0,1],[0,2],[3,4]],"entries":[0],"goals":[1,2,4],
                "learned_goals":[[2,"jump"]],"learned_landings":[[3,"jump"]]
            }
        }"#,
        )
        .unwrap();
        let now = Instant::now();
        let mut follower = RouteFollower::default();
        let start = Vec3::new(0.0, 0.0, 0.0);
        assert_eq!(
            follower
                .steer(&graph, start, observe(now, true, None))
                .direction,
            [1.0, 0.0, 0.0]
        );
        assert_eq!(follower.takeoff(&graph), Some("jump"));
        follower = RouteFollower::default();
        assert_eq!(
            follower
                .steer(&graph, start, observe(now, false, None))
                .direction,
            [0.0, 0.0, 1.0]
        );
        let landing = Vec3::new(200.0, 0.0, 200.0);
        assert_eq!(
            follower
                .steer(&graph, landing, observe(now, false, Some("other")))
                .direction,
            [0.0; 3]
        );
        assert_eq!(
            follower
                .steer(&graph, landing, observe(now, false, Some("jump")))
                .direction,
            [-1.0, 0.0, 0.0]
        );
    }

    #[test]
    fn switches_after_a_stall_and_reattaches_after_displacement() {
        let nav = serde_json::from_str(
            r#"{"map":"test","routes":[
            {"entry":[0,0,0],"waypoints":[[60,0,0]]},
            {"entry":[0,0,0],"waypoints":[[0,0,60]]},
            {"entry":[200,0,0],"waypoints":[[140,0,0]]}
        ]}"#,
        )
        .unwrap();
        let mut follower = RouteFollower::default();
        let start = Instant::now();
        let anchor = Vec3::new(0.0, 0.0, 0.0);
        assert_eq!(
            follower
                .steer(&nav, anchor, observe(start, true, None))
                .direction,
            [1.0, 0.0, 0.0]
        );
        let stalled = start + STALL_TIMEOUT;
        assert_eq!(
            follower
                .steer(&nav, anchor, observe(stalled, true, None))
                .direction,
            [0.0; 3]
        );
        assert_eq!(
            follower
                .steer(&nav, anchor, observe(stalled, true, None))
                .direction,
            [0.0, 0.0, 1.0]
        );
        let displaced = Vec3::new(200.0, 0.0, 0.0);
        assert_eq!(
            follower
                .steer(&nav, displaced, observe(stalled, true, None))
                .direction,
            [-1.0, 0.0, 0.0]
        );
    }
}

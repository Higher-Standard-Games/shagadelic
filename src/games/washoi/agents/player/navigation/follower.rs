use std::collections::BTreeMap;
use std::time::Duration;

use crate::games::washoi::SimTime;
use shagger_shared::net::game_state::{Vec3, Velocity};

use crate::games::washoi::agents::player::navigation::traversal::{Outcome, Traversal};
use crate::games::washoi::agents::player::navigation::{
    MAX_HEIGHT_DELTA, PlayerInput, Route, WashoiNavigation, horizontal_distance,
};
use crate::games::washoi::{Difficulty, Sight, agents::player::strategy::goals::OwnVitals};

const STALL_TIMEOUT: Duration = Duration::from_millis(800);
const RETRY_DELAY: Duration = Duration::from_secs(5);
const MIN_PROGRESS: f64 = 8.0;
const DISPLACEMENT: f64 = 128.0;
const MAX_BLOCKS: usize = 16;
const LOCAL_PICKUP_RADIUS: f64 = 24.0;
// Close enough on the final leg to see an item vanish, as in TypeScript
// `BotPerception.ts`, where an item that vanishes beside the bot is its own
// take and known to the tick.
const PICKUP_SIGHT_RADIUS: f64 = 96.0;
// Washoi takes a beneficial item on the first tick a player overlaps it. An
// item that stays after this wait is one the bot can't take, such as full
// ammo or a new weapon that needs USE to swap. A bot at a plain goal also
// waits this long before choosing its next one.
const ARRIVAL_DWELL: Duration = Duration::from_millis(400);
const DECLINED_PICKUP: Duration = Duration::from_secs(15);
// Planning visits every goal, so a bot with nowhere to go retries slowly.
const REPLAN_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy)]
pub(in crate::games::washoi) struct RouteObservation<'a> {
    pub(in crate::games::washoi) now: SimTime,
    pub(in crate::games::washoi) vitals: OwnVitals,
    pub(in crate::games::washoi) velocity: Option<Velocity>,
    pub(in crate::games::washoi) difficulty: Difficulty,
    pub(in crate::games::washoi) pickup_active: &'a [bool],
    pub(in crate::games::washoi) allow_takeoff: bool,
    pub(in crate::games::washoi) landed_id: Option<&'a str>,
    /// A uniform draw in [-1, 1] that blurs a remembered take time.
    pub(in crate::games::washoi) timing_noise: f64,
    /// The map's static collision, for walks back onto the graph.
    pub(in crate::games::washoi) sight: Option<&'a Sight>,
    /// Fight movement crosses the walk, so slow progress is no stall.
    pub(in crate::games::washoi) fighting: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct Leg {
    from: [f64; 3],
    to: [f64; 3],
}

struct Progress {
    next: usize,
    distance: f64,
    updated: SimTime,
}

enum RouteSlot {
    Shared(usize),
    Planned(Box<Route>),
}

impl RouteSlot {
    fn get<'a>(&'a self, navigation: &'a WashoiNavigation) -> &'a Route {
        match self {
            Self::Shared(index) => &navigation.routes[*index],
            Self::Planned(route) => route,
        }
    }
}

struct ActiveRoute {
    route: RouteSlot,
    next: usize,
    progress: Option<Progress>,
    arrived: Option<SimTime>,
    /// The bot saw the route's item in place from close by.
    seen_up: bool,
    traversal: Option<Traversal>,
}

#[derive(Default)]
pub(in crate::games::washoi) struct RouteFollower {
    active: Option<ActiveRoute>,
    last_position: Option<Vec3>,
    blocked: Vec<(Leg, SimTime)>,
    pickup_unavailable: BTreeMap<usize, SimTime>,
    replan_at: Option<SimTime>,
    /// The last waypoint the bot reached. Momentum can carry a bot past the
    /// end of a route and out of every node's attach radius.
    anchor: Option<[f64; 3]>,
    exact: bool,
}

impl RouteFollower {
    /// Drop the active route, such as on death or a learned traversal, and
    /// keep what the bot learned about blocked legs and missing pickups.
    /// Both memories expire on their own.
    pub(in crate::games::washoi) fn detach(&mut self) {
        self.active = None;
        self.last_position = None;
        self.replan_at = None;
        self.anchor = None;
        self.exact = false;
    }

    pub(in crate::games::washoi) fn traversing(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|route| route.traversal.is_some())
    }

    pub(in crate::games::washoi) fn exact(&self) -> bool {
        self.exact
    }

    pub(in crate::games::washoi) fn takeoff<'a>(
        &'a self,
        navigation: &'a WashoiNavigation,
    ) -> Option<&'a str> {
        let active = self.active.as_ref()?;
        let route = active.route.get(navigation);
        if self.exact || route.maneuver_at(active.next).is_some() {
            return None;
        }
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
            velocity,
            difficulty,
            pickup_active,
            allow_takeoff,
            landed_id,
            timing_noise,
            sight,
            fighting,
        } = observation;
        self.exact = false;
        let traversing = self.traversing();
        self.blocked.retain(|(_, until)| now < *until);
        self.pickup_unavailable.retain(|_, until| now < *until);
        let previous = self.last_position.replace(position);
        let observed_exit = previous.is_some_and(|before| {
            self.active.as_ref().is_some_and(|active| {
                active
                    .route
                    .get(navigation)
                    .maneuver_at(active.next + 1)
                    .is_some_and(|action| action.observed_teleport(before, position))
            })
        });
        if !traversing
            && !observed_exit
            && previous.is_some_and(|previous| {
                (position.x - previous.x)
                    .hypot(position.z - previous.z)
                    .hypot(position.y - previous.y)
                    > DISPLACEMENT
            })
        {
            self.active = None;
            self.anchor = None;
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
            let score = |route: &Route| {
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
            };
            let shared = continued.or_else(|| {
                navigation.select_route(
                    position,
                    |route| {
                        route.resumed_from.is_none()
                            && (allow_takeoff || route.learned_route.is_none())
                            && unblocked(route)
                    },
                    score,
                )
            });
            let route = match shared {
                Some(index) => RouteSlot::Shared(index),
                None => {
                    if self.replan_at.is_some_and(|at| now < at) {
                        return PlayerInput::default();
                    }
                    let planned = navigation.plan(position, self.anchor, sight, unblocked, score);
                    let Some(route) = planned else {
                        self.replan_at = Some(now + REPLAN_DELAY);
                        return PlayerInput::default();
                    };
                    RouteSlot::Planned(Box::new(route))
                }
            };
            self.replan_at = None;
            self.active = Some(ActiveRoute {
                route,
                next: 0,
                progress: None,
                arrived: None,
                seen_up: false,
                traversal: None,
            });
        }

        let ActiveRoute {
            route,
            next,
            progress,
            arrived,
            seen_up,
            traversal,
        } = self.active.as_mut().expect("route selected");
        let route = route.get(navigation);
        let reached = *next;
        if observed_exit
            || route.maneuver_at(*next + 1).is_some_and(|action| {
                if traversal.is_some() {
                    action.enter_before_waypoint(position) && action.pad_launch(position, velocity)
                } else {
                    action.enter_before_waypoint(position) || action.pad_launch(position, velocity)
                }
            })
        {
            *next += 1;
            *traversal = None;
        }
        let input = WashoiNavigation::steer(route, next, position, velocity);
        if *next > reached {
            self.anchor = Some(route.waypoints[*next - 1]);
        }
        if let Some(maneuver) = route.maneuver_at(*next) {
            self.exact = true;
            let state = traversal.get_or_insert_with(|| Traversal::new(maneuver, now));
            return match state.update(position, velocity, now) {
                Outcome::Running(input) => input,
                Outcome::Arrived(yaw) => {
                    self.anchor = Some(route.waypoints[*next]);
                    *next += 1;
                    *traversal = None;
                    *progress = None;
                    PlayerInput {
                        yaw: yaw.unwrap_or(input.yaw),
                        ..PlayerInput::default()
                    }
                }
                Outcome::Failed => {
                    if self.blocked.len() == MAX_BLOCKS {
                        self.blocked.remove(0);
                    }
                    self.blocked.push((route.leg(*next), now + RETRY_DELAY));
                    self.active = None;
                    PlayerInput::default()
                }
            };
        }
        if let Some(pickup) = route.pickup
            && let Some(&target) = route.waypoints.last()
            && *next + 1 >= route.waypoints.len()
            && horizontal_distance(position, target) <= PICKUP_SIGHT_RADIUS
            && (target[1] - position.y).abs() <= MAX_HEIGHT_DELTA
            && pickup_active.get(pickup.id) == Some(&true)
        {
            *seen_up = true;
        }
        let Some(&goal) = route.waypoints.get(*next) else {
            let Some(&target) = route.waypoints.last() else {
                return input;
            };
            if horizontal_distance(position, target) > LOCAL_PICKUP_RADIUS
                || (target[1] - position.y).abs() > LOCAL_PICKUP_RADIUS
            {
                return input;
            }
            // Learn availability only at the end of a route within pickup
            // range. The server's full-map mask is broader than bot knowledge.
            let taken = route
                .pickup
                .filter(|pickup| pickup_active.get(pickup.id) == Some(&false));
            let waited = now - *arrived.get_or_insert(now) >= ARRIVAL_DWELL;
            if let Some(pickup) = taken {
                // Seen taken, here by the bot itself: back after its respawn
                // time. Found gone: taken some time in the last respawn
                // period, so expect it halfway, give or take the difficulty's
                // timing error.
                let respawn = pickup.respawn();
                let away = if *seen_up {
                    respawn
                } else {
                    let error = difficulty.item_timing_error().as_secs_f64();
                    let guess = respawn.as_secs_f64() / 2.0 + timing_noise.clamp(-1.0, 1.0) * error;
                    Duration::from_secs_f64(guess.max(0.0))
                };
                self.pickup_unavailable.insert(pickup.id, now + away);
            } else if waited && let Some(pickup) = route.pickup {
                self.pickup_unavailable
                    .insert(pickup.id, now + DECLINED_PICKUP);
            }
            if taken.is_some() || waited {
                self.active = None;
            }
            return input;
        };
        let distance = horizontal_distance(position, goal);
        let progress = progress.get_or_insert(Progress {
            next: *next,
            distance,
            updated: now,
        });
        if fighting || *next != progress.next || distance + MIN_PROGRESS <= progress.distance {
            *progress = Progress {
                next: *next,
                distance,
                updated: now,
            };
        }
        if now - progress.updated >= STALL_TIMEOUT {
            if self.blocked.len() == MAX_BLOCKS {
                self.blocked.remove(0);
            }
            self.blocked.push((route.leg(*next), now + RETRY_DELAY));
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

    #[test]
    fn verifies_jump_pad_and_teleport_then_continues_the_route() {
        let graph: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,24,0],[80,24,0],[80,100,160],[1000,24,500],[1064,24,500]],
                "edges":[[3,4],[4,0]], "entries":[0],"goals":[4],
                "maneuvers":[
                    [0,1,{"kind":"jump","takeoff":[0,24,0],"landing":[80,24,0],
                          "yaw":1.5707963267948966,"speed":0,"radius":48,"ticks":150}],
                    [1,2,{"kind":"jumppad","min":[70,0,-8],"max":[90,16,8],"landing":[80,100,160]}],
                    [2,3,{"kind":"teleporter","min":[72,76,152],"max":[88,132,168],
                          "exit":[1000,24,500],"yaw":1.5}]
                ]
            }}"#,
        )
        .unwrap();
        let mut follower = RouteFollower::default();
        let start = SimTime::ZERO;
        let mut steer = |ms, p: [f64; 3], grounded, vertical| {
            let now = start + Duration::from_millis(ms);
            let mut observation = observe(now, false, None);
            observation.velocity = Some(Velocity {
                grounded,
                linear: Vec3::new(0.0, vertical, 0.0),
                ..Velocity::default()
            });
            let input = follower.steer(&graph, Vec3::new(p[0], p[1], p[2]), observation);
            (input, follower.exact())
        };
        let (settle, exact) = steer(0, [0.0, 44.0, 0.0], false, -100.0);
        assert!(exact);
        assert_eq!(settle.buttons, 0);
        let (jump, exact) = steer(104, [0.0, 24.0, 0.0], true, 0.0);
        assert!(exact);
        assert_eq!(jump.buttons & (1 << 4), 1 << 4);
        let (airborne, exact) = steer(120, [12.0, 48.0, 0.0], false, 260.0);
        assert!(exact);
        assert_eq!(airborne.buttons & (1 << 4), 0);
        steer(600, [80.0, 24.0, 0.0], true, 0.0);
        assert!(steer(616, [80.0, 24.0, 0.0], true, 0.0).1);
        assert!(steer(632, [80.0, 50.0, 0.0], false, 500.0).1);
        assert!(steer(1000, [80.0, 100.0, 160.0], true, 0.0).1);
        assert!(steer(1016, [80.0, 100.0, 160.0], true, 0.0).1);
        let (exit, exact) = steer(1032, [1000.0, 24.0, 500.0], true, 0.0);
        assert!(exact);
        assert_eq!(exit.yaw, 1.5);
        let (walk, exact) = steer(1048, [1000.0, 24.0, 500.0], true, 0.0);
        assert!(!exact);
        assert_eq!(walk.direction, [1.0, 0.0, 0.0]);
    }

    fn observe(now: SimTime, allow_takeoff: bool, landed_id: Option<&str>) -> RouteObservation<'_> {
        RouteObservation {
            now,
            vitals: OwnVitals::default(),
            velocity: None,
            difficulty: Difficulty::default(),
            pickup_active: &[],
            allow_takeoff,
            landed_id,
            timing_noise: 0.0,
            sight: None,
            fighting: false,
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
        let now = SimTime::ZERO;
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
        // Found gone, so expected back after half its 20 s respawn, here
        // with the most a medium bot can be off: 7.83 - 8.33 * 0.4 s.
        let mut late = RouteFollower::default();
        let noisy = RouteObservation {
            timing_noise: 1.0,
            ..observation
        };
        late.steer(&nav, origin, noisy);
        late.steer(&nav, health, noisy);
        assert_eq!(
            late.pickup_unavailable[&0],
            now + Duration::from_secs_f64(10.0 + (7.83 - 8.33 * 0.4))
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
        let now = SimTime::ZERO;
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
        let waited = now + ARRIVAL_DWELL;
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
                "edges":[[0,1],[1,0],[0,2],[2,0],[3,4],[4,3]],"entries":[0],"goals":[1,2,4],
                "learned_goals":[[2,"jump"]],"learned_landings":[[3,"jump"]]
            }
        }"#,
        )
        .unwrap();
        let now = SimTime::ZERO;
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
        let slot = |follower: &RouteFollower| {
            matches!(
                follower.active.as_ref().map(|active| &active.route),
                Some(RouteSlot::Shared(_))
            )
        };
        // Another recording's landing doesn't take the baked continuation,
        // but the bot still plans a walk from the nearest graph node.
        assert_eq!(
            follower
                .steer(&graph, landing, observe(now, false, Some("other")))
                .direction,
            [-1.0, 0.0, 0.0]
        );
        assert!(!slot(&follower));
        follower.detach();
        assert_eq!(
            follower
                .steer(&graph, landing, observe(now, false, Some("jump")))
                .direction,
            [-1.0, 0.0, 0.0]
        );
        assert!(slot(&follower));
    }

    #[test]
    fn replans_from_any_graph_node_and_moves_on_from_a_plain_goal() {
        let graph: WashoiNavigation = serde_json::from_str(
            r#"{
            "map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,0,0],[200,0,0],[200,0,200],[0,0,200]],
                "edges":[[0,1],[1,2],[2,3],[3,0]],"entries":[0],"goals":[2,3]
            }
        }"#,
        )
        .unwrap();
        let now = SimTime::ZERO;
        let mut follower = RouteFollower::default();
        // Node 1 isn't an entry, so only the planner can start here. The
        // nearer goal wins when neither holds a pickup.
        let side = Vec3::new(200.0, 0.0, 0.0);
        assert_eq!(
            follower
                .steer(&graph, side, observe(now, false, None))
                .direction,
            [0.0, 0.0, 1.0]
        );
        let goal = Vec3::new(200.0, 0.0, 200.0);
        let at_goal = |now| observe(now, false, None);
        let halfway = Vec3::new(200.0, 0.0, 100.0);
        assert_eq!(
            follower.steer(&graph, halfway, at_goal(now)).direction,
            [0.0, 0.0, 1.0]
        );
        assert_eq!(
            follower.steer(&graph, goal, at_goal(now)).direction,
            [0.0; 3]
        );
        let waited = now + ARRIVAL_DWELL;
        assert_eq!(
            follower.steer(&graph, goal, at_goal(waited)).direction,
            [0.0; 3]
        );
        assert_eq!(
            follower.steer(&graph, goal, at_goal(waited)).direction,
            [-1.0, 0.0, 0.0]
        );

        // Away from every node, the bot waits before planning again.
        let mut lost = RouteFollower::default();
        let nowhere = Vec3::new(100.0, 0.0, 100.0);
        assert_eq!(
            lost.steer(&graph, nowhere, at_goal(now)).direction,
            [0.0; 3]
        );
        assert!(lost.replan_at.is_some_and(|at| at > now));
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
        let start = SimTime::ZERO;
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

use std::time::Duration;

use shagger_shared::net::game_state::Vec3;
use tokio::time::Instant;

use super::{PlayerInput, Route, WashoiNavigation, horizontal_distance};

const STALL_TIMEOUT: Duration = Duration::from_millis(800);
const RETRY_DELAY: Duration = Duration::from_secs(5);
const MIN_PROGRESS: f64 = 8.0;
const DISPLACEMENT: f64 = 128.0;
const MAX_BLOCKS: usize = 16;

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
}

#[derive(Default)]
pub(in crate::games::washoi) struct RouteFollower {
    active: Option<ActiveRoute>,
    last_position: Option<Vec3>,
    blocked: Vec<(Leg, Instant)>,
}

impl RouteFollower {
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
        now: Instant,
    ) -> PlayerInput {
        self.blocked.retain(|(_, until)| now < *until);
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
            let Some(index) = navigation.route_at(position, |route| {
                (0..route.waypoints.len())
                    .all(|next| !self.blocked.iter().any(|(leg, _)| *leg == route.leg(next)))
            }) else {
                return PlayerInput::default();
            };
            self.active = Some(ActiveRoute {
                index,
                next: 0,
                progress: None,
            });
        }

        let active = self.active.as_mut().expect("route selected");
        let input = navigation.steer(active.index, &mut active.next, position);
        let route = &navigation.routes[active.index];
        let Some(&goal) = route.waypoints.get(active.next) else {
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
            follower.steer(&nav, anchor, start).direction,
            [1.0, 0.0, 0.0]
        );
        let stalled = start + STALL_TIMEOUT;
        assert_eq!(follower.steer(&nav, anchor, stalled).direction, [0.0; 3]);
        assert_eq!(
            follower.steer(&nav, anchor, stalled).direction,
            [0.0, 0.0, 1.0]
        );
        let displaced = Vec3::new(200.0, 0.0, 0.0);
        assert_eq!(
            follower.steer(&nav, displaced, stalled).direction,
            [-1.0, 0.0, 0.0]
        );
    }
}

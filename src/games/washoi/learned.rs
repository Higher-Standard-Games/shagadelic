//! Washoi's version 1 demo route format. See TypeScript LearnedRoutes.ts and
//! sequences/LearnedRouteSequence.ts for the wire order and takeoff gates.

use std::{
    io::{self, Read},
    path::Path,
    time::Duration,
};

use serde::Deserialize;
use shagger_shared::net::game_state::{Entity, Vec3};
use tokio::time::Instant;

use super::{behavior::INPUT_PERIOD, protocol::PlayerInput};

const MAX_FILE: u64 = 8 * 1024 * 1024;
const MAX_TICKS: usize = 4096;
const MAX_BURST: usize = 4;
const MAX_BACKLOG: usize = 25;
const LANDING_GRACE: Duration = Duration::from_millis(320);

#[derive(Deserialize)]
#[serde(try_from = "RouteFile")]
pub struct LearnedRoutes {
    map: String,
    routes: Vec<LearnedRoute>,
}

#[derive(Deserialize)]
struct RouteFile {
    version: u32,
    map: String,
    routes: Vec<LearnedRoute>,
}

#[derive(Deserialize)]
struct LearnedRoute {
    id: String,
    start: Point,
    landing: Point,
    ticks: usize,
    script: Vec<CommandRun>,
}

#[derive(Deserialize)]
struct Point {
    x: f64,
    y: f64,
    z: f64,
}

#[derive(Deserialize)]
struct CommandRun(usize, f64, f64, f64, f64, u16);

impl TryFrom<RouteFile> for LearnedRoutes {
    type Error = &'static str;

    fn try_from(data: RouteFile) -> Result<Self, Self::Error> {
        if data.version != 1 || data.map.trim().is_empty() {
            return Err("unsupported learned route file");
        }
        let mut ids = std::collections::BTreeSet::new();
        for route in &data.routes {
            if route.id.is_empty()
                || !ids.insert(route.id.as_str())
                || !route.start.finite()
                || !route.landing.finite()
                || !(1..=MAX_TICKS).contains(&route.ticks)
            {
                return Err("invalid learned route metadata");
            }
            let ticks = route.script.iter().try_fold(0usize, |total, run| {
                let CommandRun(count, forward, right, yaw, pitch, _) = *run;
                if count == 0
                    || ![forward, right, yaw, pitch].into_iter().all(f64::is_finite)
                    || forward.abs() > 1.0
                    || right.abs() > 1.0
                {
                    return None;
                }
                total.checked_add(count)
            });
            if ticks != Some(route.ticks) {
                return Err("learned route script does not match its declared duration");
            }
        }
        Ok(Self {
            map: data.map,
            routes: data.routes,
        })
    }
}

impl LearnedRoutes {
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_FILE + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "learned route file exceeds size limit",
            ));
        }
        serde_json::from_slice(&bytes).map_err(io::Error::from)
    }

    pub fn map(&self) -> &str {
        &self.map
    }
}

#[derive(Default)]
pub(super) enum Traversal {
    #[default]
    Waiting,
    Playing {
        route: usize,
        started: Instant,
        sent: usize,
        row: usize,
        offset: usize,
    },
    Landing {
        route: usize,
        until: Instant,
    },
    Finished {
        landed: bool,
    },
}

impl Traversal {
    pub(super) fn observe(&mut self, library: &LearnedRoutes, entity: &Entity, now: Instant) {
        let Some(position) = entity.position else {
            return;
        };
        let Some(velocity) = entity.velocity else {
            return;
        };
        match *self {
            Self::Waiting
                if velocity.grounded
                    && !velocity.is_crouching
                    && !velocity.is_sliding
                    && velocity
                        .linear
                        .x
                        .hypot(velocity.linear.z)
                        .hypot(velocity.linear.y)
                        <= 20.0 =>
            {
                if let Some(route) = library
                    .routes
                    .iter()
                    .position(|route| route.start.near(position, 6.0, 8.0))
                {
                    *self = Self::Playing {
                        route,
                        started: now,
                        sent: 0,
                        row: 0,
                        offset: 0,
                    };
                }
            }
            Self::Landing { route, until } => {
                if velocity.grounded && library.routes[route].landing.near(position, 64.0, 48.0) {
                    *self = Self::Finished { landed: true };
                } else if now >= until {
                    *self = Self::Finished { landed: false };
                }
            }
            _ => {}
        }
    }

    pub(super) fn drive(
        &mut self,
        library: &LearnedRoutes,
        now: Instant,
    ) -> Option<Vec<PlayerInput>> {
        let Self::Playing {
            route,
            started,
            sent,
            row,
            offset,
        } = self
        else {
            return None;
        };
        let learned = &library.routes[*route];
        let owed = 1 + (now - *started).as_millis() / INPUT_PERIOD.as_millis();
        if owed.saturating_sub(*sent as u128) > MAX_BACKLOG as u128 {
            *self = Self::Finished { landed: false };
            return Some(Vec::new());
        }
        let due = (owed.min(learned.ticks as u128) as usize).saturating_sub(*sent);
        let mut inputs = Vec::with_capacity(due.min(MAX_BURST));
        for _ in 0..due.min(MAX_BURST) {
            let CommandRun(count, forward, right, yaw, pitch, buttons) = learned.script[*row];
            inputs.push(PlayerInput {
                direction: [right, 0.0, forward],
                yaw,
                pitch,
                buttons,
                weapon_slot: 0,
            });
            *offset += 1;
            *sent += 1;
            if *offset == count {
                *row += 1;
                *offset = 0;
            }
        }
        if *sent == learned.ticks {
            *self = Self::Landing {
                route: *route,
                until: now + LANDING_GRACE,
            };
        }
        Some(inputs)
    }

    pub(super) fn abort_if_active(&mut self) {
        if matches!(self, Self::Playing { .. } | Self::Landing { .. }) {
            *self = Self::Finished { landed: false };
        }
    }

    pub(super) fn active(&self) -> bool {
        matches!(self, Self::Playing { .. } | Self::Landing { .. })
    }

    pub(super) fn landing_result(&self) -> Option<bool> {
        if let Self::Finished { landed } = self {
            Some(*landed)
        } else {
            None
        }
    }
}

impl Point {
    fn finite(&self) -> bool {
        [self.x, self.y, self.z].into_iter().all(f64::is_finite)
    }

    fn near(&self, position: Vec3, radius: f64, height: f64) -> bool {
        (self.x - position.x).hypot(self.z - position.z) <= radius
            && (self.y - position.y).abs() <= height
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shagger_shared::net::game_state::Velocity;

    #[test]
    fn preserves_script_order_over_late_wakeups_and_confirms_landing() {
        let route = r#"{"version":1,"map":"test","routes":[{
            "id":"demo@1","start":{"x":0,"y":0,"z":0},"landing":{"x":100,"y":0,"z":0},
            "ticks":3,"script":[[2,1,0,0,0,1],[1,0,1,1.5,0.25,9]]
        }]}"#;
        let library: LearnedRoutes = serde_json::from_str(route).unwrap();
        assert!(
            serde_json::from_str::<LearnedRoutes>(&route.replace("\"ticks\":3", "\"ticks\":4"))
                .is_err()
        );
        let mut traversal = Traversal::default();
        let mut entity = Entity {
            position: Some(Vec3::default()),
            velocity: Some(Velocity {
                grounded: true,
                ..Velocity::default()
            }),
            ..Entity::default()
        };
        let start = Instant::now();
        traversal.observe(&library, &entity, start);
        let first = traversal.drive(&library, start).unwrap();
        let rest = traversal.drive(&library, start + INPUT_PERIOD * 2).unwrap();
        assert_eq!(first[0].direction, [0.0, 0.0, 1.0]);
        assert_eq!(rest[0].direction, first[0].direction);
        assert_eq!(rest[1].direction, [1.0, 0.0, 0.0]);
        assert_eq!(rest[1].buttons, 9);
        assert!(
            traversal
                .drive(&library, start + INPUT_PERIOD * 3)
                .is_none()
        );
        assert_eq!(traversal.landing_result(), None);
        entity.position = Some(Vec3::new(100.0, 0.0, 0.0));
        traversal.observe(&library, &entity, start + INPUT_PERIOD * 3);
        assert_eq!(traversal.landing_result(), Some(true));

        let mut stalled = Traversal::default();
        entity.position = Some(Vec3::default());
        stalled.observe(&library, &entity, start);
        assert!(
            stalled
                .drive(&library, start + INPUT_PERIOD * 26)
                .unwrap()
                .is_empty()
        );
        assert_eq!(stalled.landing_result(), Some(false));
    }
}

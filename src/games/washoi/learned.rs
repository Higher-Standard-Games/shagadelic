//! Washoi's version 1 demo route format. See TypeScript LearnedRoutes.ts and
//! sequences/LearnedRouteSequence.ts for the wire order and takeoff gates.

use std::{
    io::{self, Read},
    path::Path,
    time::Duration,
};

use crate::games::washoi::SimTime;
use serde::Deserialize;
use shagger_shared::net::game_state::{Entity, Vec3};

use super::{behavior::INPUT_PERIOD, protocol::PlayerInput};

const MAX_FILE: u64 = 8 * 1024 * 1024;
const MAX_TICKS: usize = 4096;
const MAX_BURST: usize = 4;
const MAX_BACKLOG: usize = 25;
const LANDING_GRACE: Duration = Duration::from_millis(320);
const APPROACH_TIMEOUT: Duration = Duration::from_secs(6);
const START_RADIUS: f64 = 6.0;
const START_HEIGHT: f64 = 8.0;
const APPROACH_RADIUS: f64 = 96.0;
const APPROACH_HEIGHT: f64 = 24.0;
const STILL_SPEED: f64 = 20.0;
// SHAG's InputButtons in src/engine/network/components/InputComponent.ts.
const FORWARD: u16 = 1 << 0;
const CROUCH: u16 = 1 << 5;

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
    yaw: f64,
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
                || !route.yaw.is_finite()
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

    pub(super) fn approach_target(&self, id: &str, position: Vec3) -> Option<usize> {
        self.routes.iter().position(|route| {
            route.id == id && route.start.near(position, APPROACH_RADIUS, APPROACH_HEIGHT)
        })
    }

    pub(super) fn matches_takeoff(&self, id: &str, position: Vec3) -> bool {
        self.routes
            .iter()
            .any(|route| route.id == id && route.start.near(position, START_RADIUS, START_HEIGHT))
    }

    pub(super) fn matches_landing(&self, id: &str, position: Vec3) -> bool {
        self.routes
            .iter()
            .any(|route| route.id == id && route.landing.near(position, START_RADIUS, START_HEIGHT))
    }
}

#[derive(Default)]
pub(super) enum Traversal {
    #[default]
    Waiting,
    Approaching {
        route: usize,
        until: SimTime,
        settling: bool,
    },
    Playing {
        route: usize,
        started: SimTime,
        sent: usize,
        row: usize,
        offset: usize,
    },
    Landing {
        route: usize,
        until: SimTime,
    },
    Landed {
        route: usize,
    },
    Failed,
}

impl Traversal {
    pub(super) fn begin_approach(
        &mut self,
        library: &LearnedRoutes,
        id: &str,
        position: Vec3,
        now: SimTime,
    ) -> bool {
        if matches!(self, Self::Waiting)
            && let Some(route) = library.approach_target(id, position)
        {
            *self = Self::Approaching {
                route,
                until: now + APPROACH_TIMEOUT,
                settling: false,
            };
            return true;
        }
        false
    }

    pub(super) fn approach(
        &mut self,
        library: &LearnedRoutes,
        entity: &Entity,
        now: SimTime,
    ) -> Option<PlayerInput> {
        let Self::Approaching {
            route,
            until,
            settling,
        } = self
        else {
            return None;
        };
        let index = *route;
        let destination = &library.routes[index];
        let mut input = PlayerInput {
            yaw: destination.yaw,
            ..PlayerInput::default()
        };
        if now >= *until {
            *self = Self::Failed;
            return Some(input);
        }
        let (Some(position), Some(velocity)) = (entity.position, entity.velocity) else {
            return Some(input);
        };
        let x = destination.start.x - position.x;
        let z = destination.start.z - position.z;
        let distance = x.hypot(z);
        let height = (destination.start.y - position.y).abs();
        if !destination
            .start
            .near(position, APPROACH_RADIUS, APPROACH_HEIGHT)
        {
            *self = Self::Failed;
            return Some(input);
        }
        if !velocity.grounded || velocity.is_sliding {
            return Some(input);
        }
        let speed = velocity_xz(velocity.linear);
        if distance <= START_RADIUS && height <= START_HEIGHT {
            if ready_to_play(velocity) {
                self.start(index, now);
            } else {
                *settling = true;
            }
            return Some(input);
        }
        if *settling && speed > STILL_SPEED {
            return Some(input);
        }
        *settling = false;
        if distance <= START_RADIUS {
            return Some(input);
        }
        // Washoi's movement normalizes the wish direction. Alternate real
        // crouched pushes with coasting to keep the approach speed bounded.
        input.buttons = CROUCH;
        if speed <= 80.0_f64.min((1200.0 * distance).sqrt()) {
            input.buttons |= FORWARD;
            input.direction = [x / distance, 0.0, z / distance];
        }
        Some(input)
    }

    pub(super) fn observe(
        &mut self,
        library: &LearnedRoutes,
        entity: &Entity,
        now: SimTime,
        auto_start: bool,
    ) {
        let Some(position) = entity.position else {
            return;
        };
        let Some(velocity) = entity.velocity else {
            return;
        };
        match *self {
            Self::Waiting if auto_start && ready_to_play(velocity) => {
                if let Some(route) = library
                    .routes
                    .iter()
                    .position(|route| route.start.near(position, START_RADIUS, START_HEIGHT))
                {
                    self.start(route, now);
                }
            }
            Self::Landing { route, until } => {
                if velocity.grounded && library.routes[route].landing.near(position, 64.0, 48.0) {
                    *self = Self::Landed { route };
                } else if now >= until {
                    *self = Self::Failed;
                }
            }
            _ => {}
        }
    }

    pub(super) fn drive(
        &mut self,
        library: &LearnedRoutes,
        now: SimTime,
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
            *self = Self::Failed;
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
        if matches!(
            self,
            Self::Approaching { .. } | Self::Playing { .. } | Self::Landing { .. }
        ) {
            *self = Self::Failed;
        }
    }

    fn start(&mut self, route: usize, now: SimTime) {
        *self = Self::Playing {
            route,
            started: now,
            sent: 0,
            row: 0,
            offset: 0,
        };
    }

    pub(super) fn active(&self) -> bool {
        matches!(self, Self::Playing { .. } | Self::Landing { .. })
    }

    pub(super) fn landing_result(&self) -> Option<bool> {
        match self {
            Self::Landed { .. } => Some(true),
            Self::Failed => Some(false),
            _ => None,
        }
    }

    pub(super) fn landed_id<'a>(&self, library: &'a LearnedRoutes) -> Option<&'a str> {
        match self {
            Self::Landed { route } => Some(library.routes[*route].id.as_str()),
            _ => None,
        }
    }
}

fn velocity_xz(linear: Vec3) -> f64 {
    linear.x.hypot(linear.z)
}

fn ready_to_play(velocity: shagger_shared::net::game_state::Velocity) -> bool {
    velocity.grounded
        && !velocity.is_crouching
        && !velocity.is_sliding
        && velocity_xz(velocity.linear).hypot(velocity.linear.y) <= STILL_SPEED
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
            "yaw":0,"ticks":3,"script":[[2,1,0,0,0,1],[1,0,1,1.5,0.25,9]]
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
        let start = SimTime::ZERO;
        traversal.observe(&library, &entity, start, true);
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
        traversal.observe(&library, &entity, start + INPUT_PERIOD * 3, true);
        assert_eq!(traversal.landing_result(), Some(true));

        let mut stalled = Traversal::default();
        entity.position = Some(Vec3::default());
        stalled.observe(&library, &entity, start, true);
        assert!(
            stalled
                .drive(&library, start + INPUT_PERIOD * 26)
                .unwrap()
                .is_empty()
        );
        assert_eq!(stalled.landing_result(), Some(false));

        let mut approaching = Traversal::default();
        let entry = Vec3::new(48.0, 0.0, 0.0);
        entity.position = Some(entry);
        approaching.begin_approach(&library, "demo@1", entry, start);
        let creep = approaching.approach(&library, &entity, start).unwrap();
        assert_eq!(creep.direction, [-1.0, 0.0, 0.0]);
        assert_eq!(creep.buttons, FORWARD | CROUCH);
        entity.position = Some(Vec3::default());
        entity.velocity.as_mut().unwrap().is_crouching = true;
        let settle = approaching.approach(&library, &entity, start).unwrap();
        assert_eq!(settle.buttons, 0);
        assert!(approaching.drive(&library, start).is_none());
        entity.velocity.as_mut().unwrap().is_crouching = false;
        approaching.approach(&library, &entity, start);
        assert_eq!(approaching.drive(&library, start).unwrap().len(), 1);
    }
}

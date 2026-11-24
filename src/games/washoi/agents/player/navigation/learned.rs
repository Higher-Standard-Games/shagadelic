//! Washoi's version 1 demo route format. See TypeScript LearnedRoutes.ts and
//! sequences/LearnedRouteSequence.ts for the wire order and takeoff gates.

use std::{
    io::{self, Read},
    path::Path,
    time::Duration,
};

use crate::games::washoi::{Body, SimTime};
use serde::Deserialize;
use shagger_shared::{
    bsp::trigger_table::TriggerVolume, collision::collision_world::CollisionWorld,
    net::game_state::Vec3,
};

use super::flying_start::{self, ENTRY_BACK, State};

use crate::games::washoi::{hosting::net::protocol::PlayerInput, observation::INPUT_PERIOD};

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
/// Once on the mark, the bot stands still before judging it unless pushed
/// this far off (`SETTLE_RADIUS` in SHAG's
/// `src/games/fps/bot/sequences/LearnedRouteSequence.ts`). Its other-floor
/// check at 32 needs no port: APPROACH_HEIGHT fails the approach first.
const SETTLE_RADIUS: f64 = 24.0;
// SHAG's InputButtons in src/engine/network/components/InputComponent.ts.
const FORWARD: u16 = 1 << 0;
const CROUCH: u16 = 1 << 5;
const SPRINT: u16 = 1 << 6;
/// Ticks the server is ahead of the snapshot a join is decided on: a
/// snapshot every 2 ticks, and the decision (`JOIN_LEAD_TICKS`).
const JOIN_LEAD_TICKS: usize = 3;
/// The lead is a guess: the script has to land arriving a tick or two
/// sooner or later (`JOIN_LEADS_CHECKED`).
const JOIN_LEADS_CHECKED: [usize; 4] = [2, 3, 4, 5];
/// The join must land from this far to either side of the predicted state
/// too, units. Not in TypeScript: snapshots quantize the bot's position and
/// velocity, and the run-in's heading is never quite the route's. Live on
/// washoi_dm2, a recording whose landing lies 30 units from a teleporter
/// joined and missed in five of seven runs; this declines such joins, and
/// the bot stops on the mark instead.
const JOIN_MARGIN: f64 = 6.0;
/// Within this of the run-in's entry point, turn and run in.
const ENTRY_RADIUS: f64 = 40.0;
/// A run-in this far past the run-up without a join has missed it.
const RUN_IN_OVERSHOOT: f64 = 48.0;

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

    /// The length of the recording `id`, in ticks.
    pub(in crate::games::washoi) fn ticks(&self, id: &str) -> Option<usize> {
        self.routes
            .iter()
            .find(|route| route.id == id)
            .map(|route| route.ticks)
    }

    pub(in crate::games::washoi) fn approach_target(
        &self,
        id: &str,
        position: Vec3,
    ) -> Option<usize> {
        self.routes.iter().position(|route| {
            route.id == id && route.start.near(position, APPROACH_RADIUS, APPROACH_HEIGHT)
        })
    }

    pub(in crate::games::washoi) fn matches_takeoff(&self, id: &str, position: Vec3) -> bool {
        self.routes
            .iter()
            .any(|route| route.id == id && route.start.near(position, START_RADIUS, START_HEIGHT))
    }

    pub(in crate::games::washoi) fn matches_landing(&self, id: &str, position: Vec3) -> bool {
        self.routes
            .iter()
            .any(|route| route.id == id && route.landing.near(position, START_RADIUS, START_HEIGHT))
    }
}

/// A run-in toward a learned route, after `LearnedRouteSequence._flyingStart`.
pub(in crate::games::washoi) struct RunIn {
    commands: Vec<PlayerInput>,
    run_up: Vec<State>,
    /// Still making for the entry point behind the mark.
    to_entry: bool,
}

/// The map's collision and triggers, for a bot's own rollouts.
pub(in crate::games::washoi) struct Rollouts<'a> {
    pub(in crate::games::washoi) world: &'a mut dyn CollisionWorld,
    pub(in crate::games::washoi) triggers: &'a [TriggerVolume],
}

impl LearnedRoute {
    /// The script, one command a tick.
    fn commands(&self) -> Vec<PlayerInput> {
        self.script
            .iter()
            .flat_map(|&CommandRun(count, forward, right, yaw, pitch, buttons)| {
                std::iter::repeat_n(
                    PlayerInput {
                        direction: [right, 0.0, forward],
                        yaw,
                        pitch,
                        buttons,
                        weapon_slot: 0,
                    },
                    count,
                )
            })
            .collect()
    }

    fn start_vec(&self) -> Vec3 {
        Vec3::new(self.start.x, self.start.y, self.start.z)
    }

    fn landing_vec(&self) -> Vec3 {
        Vec3::new(self.landing.x, self.landing.y, self.landing.z)
    }
}

/// Sprint straight at `target`, looking where the run goes.
fn run_toward(position: Vec3, target: Vec3) -> PlayerInput {
    let (x, z) = (target.x - position.x, target.z - position.z);
    let length = x.hypot(z).max(1e-9);
    PlayerInput {
        direction: [x / length, 0.0, z / length],
        yaw: x.atan2(z),
        buttons: FORWARD | SPRINT,
        ..PlayerInput::default()
    }
}

#[derive(Default)]
pub(in crate::games::washoi) enum Traversal {
    #[default]
    Waiting,
    Approaching {
        route: usize,
        until: SimTime,
        settling: bool,
        /// A run-in that may join the script on the run (`flying_start`).
        fly: Option<Box<RunIn>>,
        /// A run-in was tried: the bot may be far from the mark.
        ran_in: bool,
    },
    Playing {
        route: usize,
        started: SimTime,
        /// The script tick played first: 0 from the mark, later when joined
        /// on the run.
        first: usize,
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
    /// Make for a learned route's mark. With the map's collision, and
    /// walkable ground from here to an entry point behind the mark and on
    /// to the mark, the bot runs in and may join the script on the run.
    pub(in crate::games::washoi) fn begin_approach(
        &mut self,
        library: &LearnedRoutes,
        id: &str,
        position: Vec3,
        now: SimTime,
        rollouts: Option<Rollouts<'_>>,
        walkable: &dyn Fn(Vec3, Vec3) -> bool,
    ) -> bool {
        if matches!(self, Self::Waiting)
            && let Some(route) = library.approach_target(id, position)
        {
            let learned = &library.routes[route];
            let fly = rollouts.and_then(|rollouts| {
                let commands = learned.commands();
                let start = learned.start_vec();
                let run_up = flying_start::run_up_states(
                    start,
                    &commands,
                    rollouts.world,
                    rollouts.triggers,
                );
                let entry = flying_start::entry_point(start, learned.yaw);
                (!run_up.is_empty() && walkable(entry, start) && walkable(position, entry)).then(
                    || {
                        Box::new(RunIn {
                            commands,
                            run_up,
                            to_entry: true,
                        })
                    },
                )
            });
            *self = Self::Approaching {
                route,
                until: now + APPROACH_TIMEOUT,
                settling: false,
                ran_in: fly.is_some(),
                fly,
            };
            return true;
        }
        false
    }

    pub(in crate::games::washoi) fn approach(
        &mut self,
        library: &LearnedRoutes,
        entity: &Body,
        now: SimTime,
        rollouts: Option<Rollouts<'_>>,
    ) -> Option<PlayerInput> {
        if let Some(input) = self.run_in(library, entity, now, rollouts) {
            return Some(input);
        }
        let Self::Approaching {
            route,
            until,
            settling,
            ran_in,
            ..
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
        // A run-in that missed leaves the bot past the mark: it walks back.
        if !*ran_in
            && !destination
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
        // As `LearnedRouteSequence.tick` settles: a bot that came onto the
        // mark stands up and stops anywhere within SETTLE_RADIUS before
        // judging it again, as judging it while still moving sent the bot
        // back and forth across it.
        let still = speed <= STILL_SPEED && !velocity.is_crouching;
        if *settling && distance <= SETTLE_RADIUS && !still {
            return Some(input);
        }
        *settling = false;
        if distance <= START_RADIUS && height <= START_HEIGHT {
            if ready_to_play(velocity) {
                self.start(index, now);
            } else {
                *settling = true;
            }
            return Some(input);
        }
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

    /// `_flyingStart`: run in toward the mark, joining the script at a
    /// matching run-up state that a rollout says lands. None when there is
    /// no run-in or it has missed, and the bot should stop on the mark.
    fn run_in(
        &mut self,
        library: &LearnedRoutes,
        entity: &Body,
        now: SimTime,
        rollouts: Option<Rollouts<'_>>,
    ) -> Option<PlayerInput> {
        let Self::Approaching {
            route, until, fly, ..
        } = self
        else {
            return None;
        };
        let (index, until) = (*route, *until);
        let (Some(run_in), Some(rollouts)) = (fly.as_mut(), rollouts) else {
            return None;
        };
        let (Some(position), Some(velocity)) = (entity.position, entity.velocity) else {
            return None;
        };
        if now >= until {
            return None;
        }
        let learned = &library.routes[index];
        let start = learned.start_vec();
        let Rollouts { world, triggers } = rollouts;
        if velocity.grounded {
            // The snapshot is a few ticks old and the bot is still
            // sprinting: join where the server will be when the script
            // arrives, and only if it lands for a lead a tick or two off.
            let here = (position, velocity.linear);
            let first = run_in.commands[0];
            let ((ahead, moving), grounded) =
                flying_start::predict(here, &first, JOIN_LEAD_TICKS, world, triggers);
            let join = grounded
                .then(|| flying_start::find_join(&run_in.run_up, ahead, moving))
                .flatten()
                .filter(|&k| {
                    JOIN_LEADS_CHECKED.iter().all(|&lead| {
                        let ((at, moving), _) =
                            flying_start::predict(here, &first, lead, world, triggers);
                        // Across the run, to either side.
                        let speed = moving.x.hypot(moving.z).max(1e-9);
                        let side = (moving.z / speed, -moving.x / speed);
                        [-JOIN_MARGIN, 0.0, JOIN_MARGIN].iter().all(|&offset| {
                            let shifted =
                                Vec3::new(at.x + side.0 * offset, at.y, at.z + side.1 * offset);
                            flying_start::join_lands(
                                &run_in.commands,
                                k,
                                (shifted, moving),
                                learned.landing_vec(),
                                world,
                                triggers,
                            )
                        })
                    })
                });
            if let Some(k) = join {
                self.start_at(library, index, now, k + 1);
                return Some(PlayerInput {
                    yaw: learned.yaw,
                    ..PlayerInput::default()
                });
            }
        }
        let heading = (learned.yaw.sin(), learned.yaw.cos());
        let along = |point: Vec3| (point.x - start.x) * heading.0 + (point.z - start.z) * heading.1;
        let here = along(position);
        if run_in.to_entry {
            let entry = flying_start::entry_point(start, learned.yaw);
            let off = (position.x - entry.x).hypot(position.z - entry.z);
            if off > ENTRY_RADIUS && here >= -ENTRY_BACK / 2.0 {
                return Some(run_toward(position, entry));
            }
            run_in.to_entry = false;
        }
        let run_up_length = run_in.run_up.last().map_or(0.0, |&(last, _)| along(last));
        if here > run_up_length + RUN_IN_OVERSHOOT {
            // Past every run-up state without a join: stop on the mark.
            *fly = None;
            return None;
        }
        // Sprint along the line, steering onto it a little ahead.
        let ahead = (here + 96.0).max(96.0);
        let on_line = Vec3::new(
            start.x + heading.0 * ahead,
            start.y,
            start.z + heading.1 * ahead,
        );
        Some(run_toward(position, on_line))
    }

    pub(in crate::games::washoi) fn observe(
        &mut self,
        library: &LearnedRoutes,
        entity: &Body,
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

    pub(in crate::games::washoi) fn drive(
        &mut self,
        library: &LearnedRoutes,
        now: SimTime,
    ) -> Option<Vec<PlayerInput>> {
        let Self::Playing {
            route,
            started,
            first,
            sent,
            row,
            offset,
        } = self
        else {
            return None;
        };
        let learned = &library.routes[*route];
        let owed = *first as u128 + 1 + (now - *started).as_millis() / INPUT_PERIOD.as_millis();
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

    pub(in crate::games::washoi) fn abort_if_active(&mut self) {
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
            first: 0,
            sent: 0,
            row: 0,
            offset: 0,
        };
    }

    /// Play the script from tick `first`, joined on the run.
    fn start_at(&mut self, library: &LearnedRoutes, route: usize, now: SimTime, first: usize) {
        let (mut row, mut offset) = (0, first);
        for &CommandRun(count, ..) in &library.routes[route].script {
            if offset < count {
                break;
            }
            offset -= count;
            row += 1;
        }
        *self = Self::Playing {
            route,
            started: now,
            first,
            sent: first,
            row,
            offset,
        };
    }

    pub(in crate::games::washoi) fn active(&self) -> bool {
        matches!(self, Self::Playing { .. } | Self::Landing { .. })
    }

    pub(in crate::games::washoi) fn landing_result(&self) -> Option<bool> {
        match self {
            Self::Landed { .. } => Some(true),
            Self::Failed => Some(false),
            _ => None,
        }
    }

    pub(in crate::games::washoi) fn landed_id<'a>(
        &self,
        library: &'a LearnedRoutes,
    ) -> Option<&'a str> {
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
        let mut entity = Body {
            position: Some(Vec3::default()),
            velocity: Some(Velocity {
                grounded: true,
                ..Velocity::default()
            }),
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
        approaching.begin_approach(&library, "demo@1", entry, start, None, &|_, _| false);
        let creep = approaching
            .approach(&library, &entity, start, None)
            .unwrap();
        assert_eq!(creep.direction, [-1.0, 0.0, 0.0]);
        assert_eq!(creep.buttons, FORWARD | CROUCH);
        entity.position = Some(Vec3::default());
        entity.velocity.as_mut().unwrap().is_crouching = true;
        let settle = approaching
            .approach(&library, &entity, start, None)
            .unwrap();
        assert_eq!(settle.buttons, 0);
        assert!(approaching.drive(&library, start).is_none());
        entity.velocity.as_mut().unwrap().is_crouching = false;
        approaching.approach(&library, &entity, start, None);
        assert_eq!(approaching.drive(&library, start).unwrap().len(), 1);
    }

    /// `LearnedRouteSequence`'s settle: a bot that came onto the mark and
    /// overshot stops and stands up anywhere within 24 before it creeps
    /// back.
    #[test]
    fn settles_within_the_settle_radius_before_creeping_back() {
        let route = r#"{"version":1,"map":"test","routes":[{
            "id":"demo@1","start":{"x":0,"y":0,"z":0},"landing":{"x":100,"y":0,"z":0},
            "yaw":0,"ticks":1,"script":[[1,1,0,0,0,1]]
        }]}"#;
        let library: LearnedRoutes = serde_json::from_str(route).unwrap();
        let start = SimTime::ZERO;
        let body = |x: f64, y: f64, speed: f64, crouching: bool| Body {
            position: Some(Vec3::new(x, y, 0.0)),
            velocity: Some(Velocity {
                grounded: true,
                is_crouching: crouching,
                linear: Vec3::new(speed, 0.0, 0.0),
                ..Velocity::default()
            }),
        };
        let mut traversal = Traversal::default();
        assert!(traversal.begin_approach(
            &library,
            "demo@1",
            Vec3::new(4.0, 0.0, 0.0),
            start,
            None,
            &|_, _| false
        ));
        // On the mark at creep speed: settle.
        let on = traversal
            .approach(&library, &body(4.0, 0.0, 60.0, true), start, None)
            .unwrap();
        assert_eq!(on.buttons, 0);
        // Coasted 14 past it, slowing and still crouched: keep settling.
        let past = traversal
            .approach(&library, &body(14.0, 0.0, 30.0, true), start, None)
            .unwrap();
        assert_eq!(past.buttons, 0);
        // Still but crouched: stand up first.
        let crouched = traversal
            .approach(&library, &body(14.0, 0.0, 0.0, true), start, None)
            .unwrap();
        assert_eq!(crouched.buttons, 0);
        // Standing still off the mark: creep back.
        let back = traversal
            .approach(&library, &body(14.0, 0.0, 0.0, false), start, None)
            .unwrap();
        assert_eq!(back.buttons, FORWARD | CROUCH);
        assert_eq!(back.direction, [-1.0, 0.0, 0.0]);
    }

    #[test]
    fn runs_in_and_joins_a_learned_route_on_the_run() {
        use crate::games::washoi::agents::player::navigation::rollout::{
            Rollout,
            box_world::{Boxes, world},
        };
        let mut floor = world(Boxes {
            solids: vec![([-512.0, -16.0, -1024.0], [512.0, 0.0, 4096.0])],
            ..Boxes::default()
        });
        // A sprint north from a mark, a jump, and the run on after it.
        let script = [
            [60.0, 1.0, 0.0, 0.0, 0.0, 65.0],
            [1.0, 1.0, 0.0, 0.0, 0.0, 81.0],
            [150.0, 1.0, 0.0, 0.0, 0.0, 65.0],
        ];
        let start = Vec3::new(0.0, 24.0, 0.0);
        let commands: Vec<PlayerInput> = script
            .iter()
            .flat_map(|row| {
                std::iter::repeat_n(
                    PlayerInput {
                        direction: [row[2], 0.0, row[1]],
                        buttons: row[5] as u16,
                        ..PlayerInput::default()
                    },
                    row[0] as usize,
                )
            })
            .collect();
        let mut body = Rollout::full(start, Vec3::default(), true, &[]);
        for command in &commands {
            body.step(command, &mut floor);
        }
        let landing = body.position;
        let library: LearnedRoutes = serde_json::from_str(&format!(
            r#"{{"version":1,"map":"test","routes":[{{
                "id":"run@1","start":{{"x":0,"y":24,"z":0}},
                "landing":{{"x":{},"y":{},"z":{}}},"yaw":0,"ticks":211,
                "script":[[60,1,0,0,0,65],[1,1,0,0,0,81],[150,1,0,0,0,65]]
            }}]}}"#,
            landing.x, landing.y, landing.z
        ))
        .unwrap();
        let standing = |position: Vec3, linear: Vec3| Body {
            position: Some(position),
            velocity: Some(Velocity {
                linear,
                grounded: true,
                ..Velocity::default()
            }),
        };
        let now = SimTime::ZERO;
        fn rollouts(world: &mut dyn CollisionWorld) -> Option<Rollouts<'_>> {
            Some(Rollouts {
                world,
                triggers: &[],
            })
        }
        let mut traversal = Traversal::default();
        let near = Vec3::new(0.0, 24.0, 40.0);
        assert!(traversal.begin_approach(
            &library,
            "run@1",
            near,
            now,
            rollouts(&mut floor),
            &|_, _| true
        ));
        // Short of the run-in, it sprints back to the entry behind the mark.
        let back = traversal
            .approach(
                &library,
                &standing(near, Vec3::default()),
                now,
                rollouts(&mut floor),
            )
            .unwrap();
        assert_eq!(back.buttons, FORWARD | SPRINT);
        assert!(back.direction[2] < -0.99, "{:?}", back.direction);
        // Running in as the run-up runs at its state 40, it joins there,
        // three ticks on for the server's lead, and plays from that tick.
        let run_up = flying_start::run_up_states(start, &commands, &mut floor, &[]);
        let (at, moving) = run_up[40];
        traversal.approach(&library, &standing(at, moving), now, rollouts(&mut floor));
        let Traversal::Playing { first, .. } = traversal else {
            panic!("joined on the run");
        };
        assert!((41..=46).contains(&first), "{first}");
        let sent = traversal.drive(&library, now).unwrap();
        assert_eq!(sent, vec![commands[first]]);
        // Without walkable ground to the entry, no run-in: it stops on the mark.
        let mut stopping = Traversal::default();
        stopping.begin_approach(
            &library,
            "run@1",
            near,
            now,
            rollouts(&mut floor),
            &|_, _| false,
        );
        let creep = stopping
            .approach(
                &library,
                &standing(near, Vec3::default()),
                now,
                rollouts(&mut floor),
            )
            .unwrap();
        assert_ne!(creep.buttons & CROUCH, 0);
    }
}

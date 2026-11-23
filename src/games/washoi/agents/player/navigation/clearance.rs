//! Keeping the feet clear of walls, corners, and ledges, from TypeScript
//! `bot/BotVisionTracer.ts` (`steer`, `probe`, `score`, `lean`, and
//! `triggerFraction`), as `bot/movement/BotSteering.ts` `steer` applies it
//! on the ground.
//!
//! Every mover decides where it wants to go, and none of them sees the
//! room. This takes the heading a decision asks for and returns the nearest
//! one that fits through the map:
//!
//! 1. Sweep the standing hull along the wanted heading, as far as the bot
//!    covers in 0.45 s at its speed (never past the point it is making for),
//!    and look for floor under the end.
//! 2. Clear: keep the heading, leaned off a wall close at one side, so the
//!    bot runs the middle of a corridor rather than along its wall.
//! 3. Blocked: try headings either side, and take the one that runs clear
//!    furthest, nearest the wanted heading, on the side chosen last time
//!    while that side still works.
//!
//! Jump pads and teleporters count as walls unless the move means to use
//! one. A surface the sweep meets facing up (a ramp, a stair) is floor. A
//! heading that runs out over a ledge counts as blocked unless the move
//! means to drop.

use shagger_shared::{
    bsp::trigger_table::{TriggerType, TriggerVolume},
    collision::collision_world::{Aabb, CollisionWorld},
    net::game_state::Vec3,
    server::simulation::types::Vec3 as SimVec3,
};

use std::f64::consts::PI;

const DEG: f64 = PI / 180.0;
/// A move to a point this much lower than the bot means to drop off an
/// edge (`BotSteering.ts` `STEER_DROP_HEIGHT`).
pub(in crate::games::washoi) const STEER_DROP_HEIGHT: f64 = 40.0;
/// Room kept between the hull and a wall it runs past. A sweep that grazes
/// a corner counts as blocked.
const WALL_MARGIN: f64 = 8.0;
/// The standing hull around an origin 24 above the feet, clear of the
/// bottom 18 units a step climbs, and `WALL_MARGIN` wider each side.
const SWEEP_BOUNDS: Aabb = Aabb {
    min: SimVec3 {
        x: -15.0 - WALL_MARGIN,
        y: -6.0,
        z: -15.0 - WALL_MARGIN,
    },
    max: SimVec3 {
        x: 15.0 + WALL_MARGIN,
        y: 40.0,
        z: 15.0 + WALL_MARGIN,
    },
};
/// The hull alone, for a move along a line already built clear of walls.
const HULL_BOUNDS: Aabb = Aabb {
    min: SimVec3 {
        x: -15.0,
        y: -6.0,
        z: -15.0,
    },
    max: SimVec3 {
        x: 15.0,
        y: 40.0,
        z: 15.0,
    },
};
/// How far ahead the bot looks, in seconds of travel at its speed.
const LOOKAHEAD_SECONDS: f64 = 0.45;
const MIN_LOOKAHEAD: f64 = 96.0;
const MAX_LOOKAHEAD: f64 = 320.0;
/// Speed assumed for a bot starting off, units/s.
const MIN_SPEED: f64 = 220.0;
/// Headings tried either side of the wanted one, degrees.
const OFFSETS: [f64; 6] = [12.0, 24.0, 36.0, 50.0, 65.0, 80.0];
/// A hit whose normal points up this much is floor, not a wall.
const FLOOR_NORMAL_Y: f64 = 0.7;
/// Floor is looked for this far under the end of a sweep.
const FLOOR_DEPTH: f64 = 80.0;
const HULL_HALF: f64 = 16.0;
/// The fan of rays that leans the heading off walls: angle off the heading
/// (degrees), how hard a wall there pushes, and the gap from the hull
/// inside which it starts to, as a share of the fan's reach.
const FAN: [(f64, f64, f64); 3] = [(30.0, 1.0, 1.0), (60.0, 0.7, 0.6), (90.0, 0.45, 0.35)];
/// The fan reaches the bot's travel in this long past the hull, within bounds.
const FAN_SECONDS: f64 = 0.4;
const FAN_MIN_REACH: f64 = 56.0;
const FAN_MAX_REACH: f64 = 200.0;
/// Radians of turn for a wall touching the hull on the fan's strongest ray.
const FAN_GAIN: f64 = 0.35;
/// The most the lean turns the heading; less along a line that must not be
/// lost.
const MAX_LEAN: f64 = 30.0 * DEG;
const LINE_LEAN: f64 = 15.0 * DEG;
/// A side chosen to get round something is kept this long while it works.
const SIDE_COMMIT_TICKS: u32 = 50;
/// Changing to the other side costs this much of a score.
const SIDE_SWITCH_COST: f64 = 0.35;
/// A turn off the wanted heading has to score this much better.
const MIN_GAIN: f64 = 0.2;

/// A flight is followed this far ahead for walls, in seconds at its speed.
const AIR_LOOKAHEAD_SECONDS: f64 = 0.3;
/// Slower than this in the air, there is nothing to bend.
const AIR_MIN_SPEED: f64 = 150.0;

/// `wallAheadSide`: whether a flight from `position` at `velocity` runs
/// into a wall within 0.3 s, and if so the side to turn to (-1 left, +1
/// right, as yaw turns), the one whose rays at 30 and 60 degrees run
/// clearer. 0 when the way is clear or the bot is too slow to matter.
pub(in crate::games::washoi) fn wall_ahead_side(
    world: &mut dyn CollisionWorld,
    position: Vec3,
    velocity: Vec3,
) -> f64 {
    let speed = velocity.x.hypot(velocity.z);
    if speed < AIR_MIN_SPEED {
        return 0.0;
    }
    let yaw = velocity.x.atan2(velocity.z);
    let reach = speed * AIR_LOOKAHEAD_SECONDS;
    let mut ray = |angle: f64, bounds: Option<Aabb>| {
        world.trace(
            sim(position),
            SimVec3 {
                x: position.x + angle.sin() * reach,
                y: position.y,
                z: position.z + angle.cos() * reach,
            },
            bounds,
            None,
        )
    };
    let ahead = ray(yaw, Some(HULL_BOUNDS));
    if ahead.fraction >= 1.0
        || ahead
            .normal
            .is_some_and(|normal| normal.y.abs() >= FLOOR_NORMAL_Y)
    {
        return 0.0;
    }
    let mut room = |side: f64| {
        ray(yaw + side * PI / 6.0, None).fraction + ray(yaw + side * PI / 3.0, None).fraction
    };
    if room(1.0) >= room(-1.0) { 1.0 } else { -1.0 }
}

/// A pad whose throw lands less than this far to the side of the pad
/// throws straight up: flying to its destination steers back onto the pad.
const PAD_RIDE_MIN_REACH: f64 = 64.0;

fn over_pad(pad: &TriggerVolume, position: Vec3) -> bool {
    position.x >= pad.min.x - 32.0
        && position.x <= pad.max.x + 32.0
        && position.z >= pad.min.z - 32.0
        && position.z <= pad.max.z + 32.0
        && position.y >= pad.min.y - 32.0
        && position.y <= pad.max.y + 96.0
}

/// `padUnder`: the jump pad at or just below `position` (the bot leaves it
/// within a tick of the throw), by ID, and where it throws. A pad that
/// throws straight up is left out: TypeScript flies on to its destination,
/// and on `washoi_dm2`'s pad, whose destination is right above it, that
/// held bots bouncing on it for up to half a Duel.
pub(in crate::games::washoi) fn pad_under(
    triggers: &[TriggerVolume],
    position: Vec3,
) -> Option<(usize, [f64; 3])> {
    triggers
        .iter()
        .filter(|trigger| trigger.kind == TriggerType::Push)
        .filter(|pad| {
            let center = ((pad.min.x + pad.max.x) / 2.0, (pad.min.z + pad.max.z) / 2.0);
            (pad.dest.x - center.0).hypot(pad.dest.z - center.1) >= PAD_RIDE_MIN_REACH
        })
        .find(|pad| over_pad(pad, position))
        .map(|pad| (pad.id, [pad.dest.x, pad.dest.y, pad.dest.z]))
}

/// Whether `position` is on or just over jump pad `id`.
pub(in crate::games::washoi) fn on_pad(
    triggers: &[TriggerVolume],
    id: usize,
    position: Vec3,
) -> bool {
    triggers
        .iter()
        .any(|pad| pad.id == id && pad.kind == TriggerType::Push && over_pad(pad, position))
}

/// One heading to check, as `SteerRequest`.
#[derive(Clone, Copy, Debug)]
pub(in crate::games::washoi) struct Request {
    pub(in crate::games::washoi) tick: u32,
    /// Origin, 24 above the feet.
    pub(in crate::games::washoi) position: Vec3,
    pub(in crate::games::washoi) velocity: Vec3,
    pub(in crate::games::washoi) grounded: bool,
    /// World heading the decision wants, `atan2(x, z)`.
    pub(in crate::games::washoi) yaw: f64,
    /// Distance to the point the bot is making for, when it has one.
    pub(in crate::games::washoi) target_distance: Option<f64>,
    /// The move means to go down off an edge.
    pub(in crate::games::washoi) dropping: bool,
    /// The move means to use a jump pad or teleporter.
    pub(in crate::games::washoi) through_triggers: bool,
    /// Following a line built clear of the walls: a smaller lean, and only
    /// what the hull itself would hit turns the bot.
    pub(in crate::games::washoi) on_line: bool,
}

/// The heading to move on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::games::washoi) struct Steered {
    pub(in crate::games::washoi) yaw: f64,
    /// How much of the lookahead runs clear on that heading, 0 to 1.
    pub(in crate::games::washoi) clear: f64,
    /// Turned off the wanted heading to get round something, not just leaned.
    pub(in crate::games::washoi) turned: bool,
}

struct Probe {
    clear: f64,
    floor: bool,
}

fn sim(point: Vec3) -> SimVec3 {
    SimVec3 {
        x: point.x,
        y: point.y,
        z: point.z,
    }
}

/// `BotVisionTracer`'s steering memory: the side it last turned to, and the
/// lean held across decisions.
#[derive(Default)]
pub(in crate::games::washoi) struct Clearance {
    side: Option<(f64, u32)>,
    lean: f64,
}

/// The lean eases toward the fan's turn by this share each decision, and
/// one under this much is no lean. TypeScript applies the fan's turn
/// afresh every decision to the feet alone; a Washoi bot's view turns with
/// its feet, so a lean that changes each decision swings the view and the
/// movement keys with it, which cost 3 points of time at full speed.
const LEAN_EASE: f64 = 0.35;
const LEAN_DEADBAND: f64 = 4.0 * DEG;

impl Clearance {
    /// The heading to move on for `request`, or `None` to leave it alone
    /// (in the air).
    pub(in crate::games::washoi) fn steer(
        &mut self,
        world: &mut dyn CollisionWorld,
        triggers: &[TriggerVolume],
        request: &Request,
    ) -> Option<Steered> {
        if !request.grounded {
            return None;
        }
        let speed = request.velocity.x.hypot(request.velocity.z).max(MIN_SPEED);
        let mut reach = (speed * LOOKAHEAD_SECONDS).clamp(MIN_LOOKAHEAD, MAX_LOOKAHEAD);
        if let Some(distance) = request.target_distance {
            reach = reach.min(distance + HULL_HALF).max(32.0);
        }
        let bounds = if request.on_line {
            HULL_BOUNDS
        } else {
            SWEEP_BOUNDS
        };
        let triggers = if request.through_triggers {
            &[][..]
        } else {
            triggers
        };
        let wanted = probe(
            world,
            triggers,
            request.position,
            request.yaw,
            reach,
            bounds,
        );
        let mind_floor = !request.dropping;
        if wanted.clear >= 1.0 && (wanted.floor || !mind_floor) {
            let cap = if request.on_line { LINE_LEAN } else { MAX_LEAN };
            let push = lean(world, request.position, request.yaw, speed, cap);
            self.lean += (push - self.lean) * LEAN_EASE;
            let lean = if self.lean.abs() < LEAN_DEADBAND {
                0.0
            } else {
                self.lean
            };
            return Some(Steered {
                yaw: request.yaw + lean,
                clear: 1.0,
                turned: false,
            });
        }
        let committed = self
            .side
            .filter(|(_, until)| request.tick < *until)
            .map_or(0.0, |(sign, _)| sign);
        let stay = score(&wanted, 0.0, 0.0, committed, mind_floor);
        let (mut best_yaw, mut best_score, mut best_clear, mut best_sign) =
            (request.yaw, stay, wanted.clear, 0.0);
        for offset in OFFSETS.map(|degrees| degrees * DEG) {
            for sign in [-1.0, 1.0] {
                let yaw = request.yaw + sign * offset;
                let tried = probe(world, triggers, request.position, yaw, reach, bounds);
                let score = score(&tried, offset, sign, committed, mind_floor);
                if score > best_score {
                    (best_yaw, best_score, best_clear, best_sign) = (yaw, score, tried.clear, sign);
                }
            }
        }
        if best_sign == 0.0 || best_score < stay + MIN_GAIN {
            return Some(Steered {
                yaw: request.yaw,
                clear: wanted.clear,
                turned: false,
            });
        }
        self.side = Some((best_sign, request.tick + SIDE_COMMIT_TICKS));
        Some(Steered {
            yaw: best_yaw,
            clear: best_clear,
            turned: true,
        })
    }
}

fn score(probe: &Probe, offset: f64, sign: f64, committed: f64, mind_floor: bool) -> f64 {
    let ledge = if mind_floor && !probe.floor { 1.0 } else { 0.0 };
    let switching = if committed != 0.0 && sign != 0.0 && sign != committed {
        SIDE_SWITCH_COST
    } else {
        0.0
    };
    probe.clear * 2.0 + offset.cos() - ledge * 1.5 - switching
}

/// Sweep the hull `reach` along `yaw` from `from`, and look for floor at
/// the end.
fn probe(
    world: &mut dyn CollisionWorld,
    triggers: &[TriggerVolume],
    from: Vec3,
    yaw: f64,
    reach: f64,
    bounds: Aabb,
) -> Probe {
    let (dx, dz) = (yaw.sin(), yaw.cos());
    let end = Vec3::new(from.x + dx * reach, from.y, from.z + dz * reach);
    let mut sweep = world.trace(sim(from), sim(end), Some(bounds), None);
    // Within the margin of a wall already, the wider box starts inside it
    // and every heading would look blocked: the hull alone says which ways
    // are open.
    if sweep.fraction <= 0.0 && bounds != HULL_BOUNDS {
        sweep = world.trace(sim(from), sim(end), Some(HULL_BOUNDS), None);
    }
    // Running up a ramp meets it like a wall; a surface facing up is floor.
    let blocked = sweep.fraction < 1.0
        && !sweep
            .normal
            .is_some_and(|normal| normal.y >= FLOOR_NORMAL_Y);
    let clear =
        (if blocked { sweep.fraction } else { 1.0 }).min(trigger_fraction(triggers, from, end));
    let at = if clear >= 1.0 {
        end
    } else {
        Vec3::new(
            from.x + dx * reach * clear,
            from.y,
            from.z + dz * reach * clear,
        )
    };
    let down = world.trace(
        sim(at),
        SimVec3 {
            x: at.x,
            y: at.y - FLOOR_DEPTH,
            z: at.z,
        },
        None,
        None,
    );
    Probe {
        clear,
        floor: down.fraction < 1.0,
    }
}

/// How far along the run from `from` to `end` the hull first enters a jump
/// pad or teleporter, 0 to 1 (1: none). One the bot already stands in does
/// not count: the way out of it is the way it is going.
fn trigger_fraction(triggers: &[TriggerVolume], from: Vec3, end: Vec3) -> f64 {
    let mut first: f64 = 1.0;
    for trigger in triggers {
        // The hull reaches HULL_HALF round the origin, from 24 below it to 40 above.
        if from.y + 40.0 < trigger.min.y || from.y - 24.0 > trigger.max.y {
            continue;
        }
        let min = [trigger.min.x - HULL_HALF, trigger.min.z - HULL_HALF];
        let max = [trigger.max.x + HULL_HALF, trigger.max.z + HULL_HALF];
        let (start, stop) = ([from.x, from.z], [end.x, end.z]);
        if (0..2).all(|axis| start[axis] >= min[axis] && start[axis] <= max[axis]) {
            continue;
        }
        let (mut enter, mut exit): (f64, f64) = (0.0, 1.0);
        for axis in 0..2 {
            let delta = stop[axis] - start[axis];
            if delta.abs() < 1e-9 {
                if start[axis] < min[axis] || start[axis] > max[axis] {
                    enter = 2.0;
                }
                continue;
            }
            let a = (min[axis] - start[axis]) / delta;
            let b = (max[axis] - start[axis]) / delta;
            enter = enter.max(a.min(b));
            exit = exit.min(a.max(b));
        }
        if enter <= exit && enter < first {
            first = enter;
        }
    }
    first
}

/// A turn away from the walls ahead and at the sides, radians: each wall
/// pushes the harder the nearer it is and the more nearly ahead. Walls
/// either side of a corridor push against each other, so the bot runs its
/// middle. Surfaces facing up do not push.
fn lean(world: &mut dyn CollisionWorld, from: Vec3, yaw: f64, speed: f64, cap: f64) -> f64 {
    let room = (speed * FAN_SECONDS).clamp(FAN_MIN_REACH, FAN_MAX_REACH);
    let reach = HULL_HALF + room;
    let mut turn = 0.0;
    for (angle, weight, comfort) in FAN {
        for side in [-1.0, 1.0] {
            let a = yaw + side * angle * DEG;
            let hit = world.trace(
                sim(from),
                SimVec3 {
                    x: from.x + a.sin() * reach,
                    y: from.y,
                    z: from.z + a.cos() * reach,
                },
                None,
                None,
            );
            if hit.fraction >= 1.0 || hit.normal.is_some_and(|normal| normal.y >= FLOOR_NORMAL_Y) {
                continue;
            }
            let gap = hit.fraction * reach - HULL_HALF;
            let push = (1.0 - gap / (comfort * room)).max(0.0);
            // A wall on the right (+) turns the heading left (-).
            turn -= side * weight * push;
        }
    }
    (turn * FAN_GAIN).clamp(-cap, cap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::agents::player::navigation::rollout::box_world::{Boxes, world};

    /// A floor whose top is at y = 0, out to z = `to_z`, and solid boxes.
    fn room(to_z: f64, walls: &[([f64; 3], [f64; 3])]) -> Boxes {
        let mut solids = vec![([-2000.0, -16.0, -2000.0], [2000.0, 0.0, to_z])];
        solids.extend_from_slice(walls);
        Boxes {
            solids,
            ..Boxes::default()
        }
    }

    fn trigger(kind: TriggerType, min: [f64; 3], max: [f64; 3]) -> TriggerVolume {
        let at = |[x, y, z]: [f64; 3]| SimVec3 { x, y, z };
        TriggerVolume {
            id: 0,
            kind,
            min: at(min),
            max: at(max),
            dest: at([0.0, 300.0, 900.0]),
            dest_yaw: None,
        }
    }

    fn request(yaw: f64) -> Request {
        Request {
            tick: 0,
            position: Vec3::new(0.0, 24.0, 0.0),
            velocity: Vec3::new(0.0, 0.0, 320.0),
            grounded: true,
            yaw,
            target_distance: None,
            dropping: false,
            through_triggers: false,
            on_line: false,
        }
    }

    #[test]
    fn an_open_floor_keeps_the_heading() {
        let mut world = world(room(2000.0, &[]));
        let steered = Clearance::default()
            .steer(&mut world, &[], &request(0.0))
            .unwrap();
        assert_eq!(
            steered,
            Steered {
                yaw: 0.0,
                clear: 1.0,
                turned: false
            }
        );
        let mut flying = request(0.0);
        flying.grounded = false;
        assert!(
            Clearance::default()
                .steer(&mut world, &[], &flying)
                .is_none()
        );
    }

    #[test]
    fn leans_off_a_wall_close_at_one_side() {
        // A wall along the run 30 units to the right (+x).
        let wall = ([30.0, 0.0, -500.0], [60.0, 200.0, 2000.0]);
        let mut walled = world(room(2000.0, &[wall]));
        let mut clearance = Clearance::default();
        let push = lean(&mut walled, request(0.0).position, 0.0, 320.0, MAX_LEAN);
        assert!((-MAX_LEAN..-0.05).contains(&push), "{push}");
        // The lean eases in over a few decisions and holds at the push.
        let first = clearance.steer(&mut walled, &[], &request(0.0)).unwrap();
        assert!(!first.turned);
        assert!(
            first.yaw > push && first.yaw < 0.0,
            "{first:?} toward {push}"
        );
        let held = (0..12)
            .map(|_| {
                clearance
                    .steer(&mut walled, &[], &request(0.0))
                    .unwrap()
                    .yaw
            })
            .last()
            .unwrap();
        assert!((held - push).abs() < 0.01, "{held} vs {push}");
        // Past the wall, it eases out, and a small remainder is none.
        let mut open = world(room(2000.0, &[]));
        let after: Vec<f64> = (0..12)
            .map(|_| clearance.steer(&mut open, &[], &request(0.0)).unwrap().yaw)
            .collect();
        assert!(after[0] < 0.0 && after[11] == 0.0, "{after:?}");
    }

    #[test]
    fn turns_round_a_wall_ahead_toward_the_open_side_and_keeps_that_side() {
        // A wall across the run 100 ahead, open past its left (-x) end.
        let wall = ([-60.0, 0.0, 100.0], [2000.0, 200.0, 130.0]);
        let mut world = world(room(2000.0, &[wall]));
        let mut clearance = Clearance::default();
        let steered = clearance.steer(&mut world, &[], &request(0.0)).unwrap();
        assert!(steered.turned && steered.yaw < 0.0, "{steered:?}");
        // Committed: a heading where both sides would do still goes left.
        assert_eq!(clearance.side.map(|(sign, _)| sign), Some(-1.0));
    }

    #[test]
    fn a_ledge_ahead_turns_the_bot_unless_it_means_to_drop() {
        // The floor ends 100 ahead, with floor still on to the left.
        let mut boxes = room(100.0, &[]);
        boxes
            .solids
            .push(([-2000.0, -16.0, 100.0], [-60.0, 0.0, 2000.0]));
        let mut world = world(boxes);
        let steered = Clearance::default()
            .steer(&mut world, &[], &request(0.0))
            .unwrap();
        assert!(steered.turned && steered.yaw < 0.0, "{steered:?}");
        let mut dropping = request(0.0);
        dropping.dropping = true;
        let steered = Clearance::default()
            .steer(&mut world, &[], &dropping)
            .unwrap();
        assert!(!steered.turned, "{steered:?}");
    }

    #[test]
    fn a_jump_pad_counts_as_a_wall_unless_the_move_uses_it() {
        let mut world = world(room(2000.0, &[]));
        let pad = trigger(TriggerType::Push, [-200.0, 0.0, 100.0], [40.0, 8.0, 140.0]);
        let steered = Clearance::default()
            .steer(&mut world, std::slice::from_ref(&pad), &request(0.0))
            .unwrap();
        assert!(steered.turned && steered.yaw > 0.0, "{steered:?}");
        let mut through = request(0.0);
        through.through_triggers = true;
        let steered = Clearance::default()
            .steer(&mut world, std::slice::from_ref(&pad), &through)
            .unwrap();
        assert!(!steered.turned, "{steered:?}");
        // From inside the pad, the way out is the way it is going.
        let mut inside = request(0.0);
        inside.position = Vec3::new(0.0, 24.0, 120.0);
        let steered = Clearance::default()
            .steer(&mut world, std::slice::from_ref(&pad), &inside)
            .unwrap();
        assert!(!steered.turned, "{steered:?}");
    }

    #[test]
    fn a_flight_into_a_wall_turns_to_the_open_side() {
        // A wall 60 ahead of a flight at 320 u/s, open past its left end.
        let wall = ([-10.0, -500.0, 60.0], [2000.0, 500.0, 90.0]);
        let mut world = world(room(2000.0, &[wall]));
        let at = Vec3::new(0.0, 100.0, 0.0);
        assert_eq!(
            wall_ahead_side(&mut world, at, Vec3::new(0.0, 0.0, 320.0)),
            -1.0
        );
        // Too slow to bend, or flying clear of it.
        assert_eq!(
            wall_ahead_side(&mut world, at, Vec3::new(0.0, 0.0, 100.0)),
            0.0
        );
        assert_eq!(
            wall_ahead_side(&mut world, at, Vec3::new(0.0, 0.0, -320.0)),
            0.0
        );
    }

    #[test]
    fn finds_the_pad_just_thrown_from() {
        let pad = trigger(TriggerType::Push, [0.0, 0.0, 0.0], [64.0, 8.0, 64.0]);
        let teleporter = trigger(TriggerType::Teleport, [0.0, 0.0, 0.0], [64.0, 8.0, 64.0]);
        let pads = [pad];
        assert_eq!(
            pad_under(&pads, Vec3::new(32.0, 90.0, 32.0)),
            Some((0, [0.0, 300.0, 900.0]))
        );
        assert!(on_pad(&pads, 0, Vec3::new(32.0, 24.0, 32.0)));
        assert_eq!(pad_under(&pads, Vec3::new(32.0, 200.0, 32.0)), None);
        assert_eq!(pad_under(&[teleporter], Vec3::new(32.0, 40.0, 32.0)), None);
        // A pad throwing straight up, as washoi_dm2's does, is no ride.
        let mut up = pad;
        up.dest = SimVec3 {
            x: 40.0,
            y: 312.0,
            z: 20.0,
        };
        assert_eq!(pad_under(&[up], Vec3::new(32.0, 90.0, 32.0)), None);
    }

    #[test]
    fn stops_short_of_the_point_it_makes_for() {
        // A wall 150 ahead, the point 60 ahead: nothing is in the way.
        let wall = ([-2000.0, 0.0, 150.0], [2000.0, 200.0, 180.0]);
        let mut world = world(room(2000.0, &[wall]));
        let mut near = request(0.0);
        near.target_distance = Some(60.0);
        let steered = Clearance::default().steer(&mut world, &[], &near).unwrap();
        assert!(!steered.turned && steered.clear >= 1.0, "{steered:?}");
    }
}

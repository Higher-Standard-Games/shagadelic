//! Strafe-jump chains down open stretches of route, after TypeScript
//! `sequences/RouteStrafeJumpSequence.ts` and
//! `BotTraversal.tryStartRouteStrafeJump`. A bot running down a straight lane
//! jumps, and in the air holds forward and a side key with its view at the
//! angle that gains the most speed. Jump stays held through each landing, so
//! the next hop goes without losing a tick to ground friction, as long as the
//! route ahead has room for it.
//!
//! The keys stay ones a human can hold. Each decision works out the view and
//! keys TypeScript's air control wants, turns the sent view toward that view at
//! TypeScript's hop turn rate, 2.5 + 4.5 × skill radians a second, and presses
//! the key chord nearest the wanted wish in the view actually sent. TypeScript
//! sends its keys in a frame of their own, apart from the view. Washoi's
//! 400 u/s ground cap doesn't apply in the air, so strafe keys gain speed there.
//!
//! Deviations: only straight lanes, not chains bent along a held route line,
//! and the walk to each landing's next lane comes from the route follower or
//! the order's walk. Each landing takes the next lane only past the gates a
//! start has, speed, heading and allies, as `routeStrafeNext` asks
//! `routeStrafeLane` again. An ally in the lane ahead stops a chain from
//! starting, as `routeStrafeLane`'s `allyBlocksRoute` does, measured along the
//! lane's heading for 600 units rather than to the route step's end. A chain
//! is planned once per decision, so its keys hold for the decision's input
//! ticks.

use std::f64::consts::{FRAC_1_SQRT_2, FRAC_PI_2, FRAC_PI_4, PI};

use shagger_shared::net::game_state::{Vec3, Velocity};

use crate::games::washoi::{Sight, hosting::net::protocol::PlayerInput};

const TICK_RATE: f64 = 125.0;
// `DEFAULT_FPS_MOVEMENT_CONFIG` and `Acceleration.ts`.
const MAX_SPEED: f64 = 320.0;
const RUN_SPEED: f64 = 400.0;
const TOP_SPEED: f64 = 1000.0;
const RADICAL_SPEED: f64 = 600.0;
const AIR_ACCELERATION: f64 = 1.0;
const MAX_AIR_SPEED: f64 = 30.0;
const SIDE_ACCEL: f64 = 50.0;
const JUMP_VELOCITY: f64 = 320.0;
const GRAVITY: f64 = 1000.0;
const FALLING_GRAVITY_MULTIPLIER: f64 = 1.25;

const TAKEOFF_CONFIRM_TICKS: u32 = 18;
const TAKEOFF_COMMIT_TICKS: u32 = 10;
const MIN_ENTRY_SPEED: f64 = 320.0;
const MIN_CONTROL_SPEED: f64 = 160.0;
const ENDPOINT_EXIT_DISTANCE: f64 = 260.0;
const MIN_ROUTE_ERROR_FOR_SIDE: f64 = 0.025;
const STEER_ERROR: f64 = 0.35;
const MAX_FORWARD_LEAD: f64 = 0.17;
const LINE_RETURN_DISTANCE: f64 = 300.0;
const UNSKILLED_ANGLE_ERROR: f64 = 0.35;
const SIDE_WALL_GAP: f64 = 28.0;
const TAKEOFF_SIDE_GAP: f64 = 72.0;
const SIDE_LOOK: f64 = 128.0;
const LANDED_GROUND_TICKS: u32 = 3;
const CHAIN_MARGIN: f64 = 64.0;
const MAX_TICKS: u32 = 10 * 125;
/// `BOT_STRAFE_RETRY_TICKS`.
const RETRY_TICKS: u32 = 30;
/// The least `strafeJumpSkill` that hops at all.
const MIN_SKILL: f64 = 0.35;
/// `ROUTE_STRAFE_ALLY_LOOKAHEAD` and `ROUTE_STRAFE_ALLY_RADIUS`: an ally this
/// far along the lane and this close to its line blocks a chain's start.
const ALLY_LOOKAHEAD: f64 = 600.0;
const ALLY_RADIUS: f64 = 80.0;
const ALLY_HEIGHT: f64 = 96.0;
// `BotVisionTracer` probes.
const HULL_HALF: f64 = 16.0;
const AIR_LOOKAHEAD_SECONDS: f64 = 0.3;
const AIR_MIN_SPEED: f64 = 150.0;
/// A ray this far up that clears where the low one hit means a slope or a
/// step, not a wall. The sight geometry gives no surface normals.
const STEP_PROBE: f64 = 18.0;

const FORWARD: u16 = 1;
const BACK: u16 = 1 << 1;
const LEFT: u16 = 1 << 2;
const RIGHT: u16 = 1 << 3;
const JUMP: u16 = 1 << 4;
const CROUCH: u16 = 1 << 5;
const SPRINT: u16 = 1 << 6;
const FIRE: u16 = 1 << 8;
const ALT_FIRE: u16 = 1 << 9;
const MOVE_KEYS: u16 = FORWARD | BACK | LEFT | RIGHT;

/// An open stretch of route to hop down (`RouteFollower::hop_lane`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::games::washoi) struct HopLane {
    pub(in crate::games::washoi) yaw: f64,
    pub(in crate::games::washoi) end: [f64; 3],
}

/// What a decision offers the hop chain.
pub(in crate::games::washoi) struct HopView<'a> {
    pub(in crate::games::washoi) tick: u32,
    pub(in crate::games::washoi) position: Vec3,
    pub(in crate::games::washoi) velocity: Option<Velocity>,
    /// The open stretch ahead on the route from here, if any.
    pub(in crate::games::washoi) lane: Option<HopLane>,
    pub(in crate::games::washoi) sight: Option<&'a Sight>,
    pub(in crate::games::washoi) skill: f64,
    /// The view the bot sends now, where a chain's first turn starts.
    pub(in crate::games::washoi) view_yaw: f64,
    /// A fight or chase at the last decision. No chain starts in one.
    pub(in crate::games::washoi) fighting: bool,
    /// Living teammates. None in the lane ahead may stand in a chain's way.
    pub(in crate::games::washoi) allies: &'a [Vec3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Takeoff,
    Air,
}

#[derive(Clone, Copy)]
struct Hop {
    phase: Phase,
    started: u32,
    route_yaw: f64,
    route_end: [f64; 3],
    start: Vec3,
    start_speed: f64,
    target_speed: f64,
    max_cross_track: f64,
    side: f64,
    landed: Option<u32>,
    /// The view sent at the last decision, and when.
    view_yaw: f64,
    view_tick: u32,
}

#[derive(Default)]
pub(in crate::games::washoi) struct HopChain {
    running: Option<Hop>,
    cooldown_until: u32,
    next_side: f64,
    /// For traces: why the last decision's chain didn't start, or how a
    /// running one started, landed, or ended, and the speed at a landing.
    note: Option<&'static str>,
    landing_speed: Option<f64>,
}

impl HopChain {
    pub(in crate::games::washoi) fn active(&self) -> bool {
        self.running.is_some()
    }

    /// A fight may take over unless the hop is in its takeoff commitment.
    pub(in crate::games::washoi) fn interruptible(&self, tick: u32) -> bool {
        self.running.is_none_or(|hop| {
            hop.phase != Phase::Takeoff || tick.saturating_sub(hop.started) >= TAKEOFF_COMMIT_TICKS
        })
    }

    pub(in crate::games::washoi) fn reset(&mut self) {
        self.running = None;
    }

    /// The note for traces since the last record (see `HopChain::note`),
    /// and the speed at the landing it records, if it landed.
    pub(in crate::games::washoi) fn note(&self) -> (Option<&'static str>, Option<f64>) {
        (self.note, self.landing_speed)
    }

    /// Clears the note once a trace has recorded it.
    pub(in crate::games::washoi) fn clear_note(&mut self) {
        self.note = None;
        self.landing_speed = None;
    }

    /// The exact controls for this decision while a chain runs, or None to
    /// leave the walk's `input` alone. A chain starts from a walk that runs
    /// forward down an open lane at walking pace or faster.
    pub(in crate::games::washoi) fn steer(
        &mut self,
        view: &HopView<'_>,
        input: PlayerInput,
    ) -> Option<PlayerInput> {
        let velocity = view.velocity.unwrap_or_default();
        if self.running.is_none() {
            match self.start(view, velocity, input) {
                Ok(hop) => self.running = Some(hop),
                Err(gate) => {
                    self.note = Some(gate);
                    return None;
                }
            }
        }
        let mut hop = self.running?;
        let (wanted, done, note) = tick(&mut hop, view, velocity);
        if let Some(note) = note {
            self.note = Some(note);
            if note.starts_with("land") {
                self.landing_speed = Some(horizontal_speed(velocity));
            }
        }
        let controls = turn_view(&mut hop, view, wanted);
        self.running = (!done).then_some(hop);
        Some(PlayerInput {
            weapon_slot: input.weapon_slot,
            ..controls
        })
    }

    /// A chain from this decision, or the first gate that refused one.
    fn start(
        &mut self,
        view: &HopView<'_>,
        velocity: Velocity,
        input: PlayerInput,
    ) -> Result<Hop, &'static str> {
        let speed = horizontal_speed(velocity);
        let [x, _, z] = input.direction;
        let refused = if view.tick < self.cooldown_until {
            Some("cooldown")
        } else if view.skill < MIN_SKILL {
            Some("skill")
        } else if view.fighting {
            Some("fighting")
        } else if !velocity.grounded {
            Some("airborne")
        } else if velocity.is_sliding {
            Some("sliding")
        } else if x == 0.0 && z == 0.0 {
            Some("no_wish")
        } else if input.buttons & (BACK | LEFT | RIGHT | CROUCH | FIRE | ALT_FIRE | JUMP) != 0 {
            Some("keys")
        } else {
            None
        };
        if let Some(gate) = refused {
            return Err(gate);
        }
        let lane = view.lane.ok_or("no_lane")?;
        lane_takes(view, velocity, &lane)?;
        let fallback = if self.next_side == 0.0 {
            1.0
        } else {
            self.next_side
        };
        let mut hop = Hop {
            phase: Phase::Takeoff,
            started: view.tick,
            route_yaw: lane.yaw,
            route_end: lane.end,
            start: view.position,
            start_speed: speed,
            target_speed: 360.0 + view.skill * 600.0,
            max_cross_track: 96.0 + view.skill * 64.0,
            side: fallback,
            landed: None,
            view_yaw: view.view_yaw,
            view_tick: view.tick,
        };
        let velocity_yaw = heading(velocity, lane.yaw);
        let side = choose_side(
            normalize(aim_yaw(&hop, view.position) - velocity_yaw),
            fallback,
        );
        hop.side = side_away_from_wall(view, velocity_yaw, side);
        self.next_side = -fallback;
        self.cooldown_until = view.tick + RETRY_TICKS;
        self.note = Some("start_straight");
        Ok(hop)
    }
}

/// One decision of the chain: the controls, whether it ended, and a note for
/// traces when it lands or ends.
fn tick(
    hop: &mut Hop,
    view: &HopView<'_>,
    velocity: Velocity,
) -> (PlayerInput, bool, Option<&'static str>) {
    let elapsed = view.tick.saturating_sub(hop.started);
    if elapsed >= MAX_TICKS {
        return (forward(hop, false), true, Some("end_long"));
    }
    if hop.phase == Phase::Takeoff {
        if !velocity.grounded {
            hop.phase = Phase::Air;
            return (strafe(hop, view, velocity, false), false, None);
        }
        if elapsed >= TAKEOFF_CONFIRM_TICKS {
            return (forward(hop, false), true, Some("end_takeoff"));
        }
        return (forward(hop, true), false, None);
    }
    if velocity.grounded {
        let landed = *hop.landed.get_or_insert(view.tick);
        // Jump held through the landing hops straight off it. Still down
        // after a few ticks, the ground there wouldn't let it.
        if view.tick.saturating_sub(landed) >= LANDED_GROUND_TICKS {
            return (forward(hop, false), true, Some("end_ground"));
        }
        if view.tick == landed {
            let room = horizontal_speed(velocity) * hop_seconds() + CHAIN_MARGIN;
            let Some(lane) = view.lane else {
                return (forward(hop, false), true, Some("land_no_lane"));
            };
            if (lane.end[0] - view.position.x).hypot(lane.end[2] - view.position.z) < room {
                return (forward(hop, false), true, Some("land_short"));
            }
            // TypeScript asks `routeStrafeLane` again on each landing, with
            // every gate a start has.
            if let Err(gate) = lane_takes(view, velocity, &lane) {
                let note = match gate {
                    "slow" => "land_slow",
                    "heading" => "land_heading",
                    _ => "land_ally",
                };
                return (forward(hop, false), true, Some(note));
            }
            take_lane(hop, view, velocity, lane);
            return (
                strafe(hop, view, velocity, false),
                false,
                Some("land_straight"),
            );
        }
        return (strafe(hop, view, velocity, false), false, None);
    }
    hop.landed = None;
    let speed = horizontal_speed(velocity);
    if speed < MIN_CONTROL_SPEED || speed < hop.start_speed * 0.65 {
        return (forward(hop, false), true, Some("end_slow"));
    }
    let (sin, cos) = hop.route_yaw.sin_cos();
    let cross_track =
        ((view.position.x - hop.start.x) * cos - (view.position.z - hop.start.z) * sin).abs();
    if cross_track > hop.max_cross_track {
        return (forward(hop, false), true, Some("end_off_line"));
    }
    let to_end = (hop.route_end[0] - view.position.x).hypot(hop.route_end[2] - view.position.z);
    (
        strafe(hop, view, velocity, to_end <= ENDPOINT_EXIT_DISTANCE),
        false,
        None,
    )
}

/// Whether a chain may take `lane` from here, at a start or a landing: the
/// rest of TypeScript `BotTraversal.routeStrafeLane` once a lane is open.
/// At running speed (`ROUTE_STRAFE_ENTRY_SPEED`), already heading along it
/// (`ROUTE_STRAFE_MAX_VELOCITY_ERROR_RADIANS`), and no ally in the way.
fn lane_takes(view: &HopView<'_>, velocity: Velocity, lane: &HopLane) -> Result<(), &'static str> {
    if horizontal_speed(velocity) < MIN_ENTRY_SPEED {
        return Err("slow");
    }
    if normalize(heading(velocity, lane.yaw) - lane.yaw).abs() > FRAC_PI_4 {
        return Err("heading");
    }
    if ally_in_lane(view.allies, view.position, lane.yaw) {
        return Err("ally");
    }
    Ok(())
}

fn take_lane(hop: &mut Hop, view: &HopView<'_>, velocity: Velocity, lane: HopLane) {
    hop.route_yaw = lane.yaw;
    hop.route_end = lane.end;
    hop.start = view.position;
    hop.start_speed = horizontal_speed(velocity);
    let velocity_yaw = heading(velocity, lane.yaw);
    let fallback = -hop.side;
    let side = choose_side(
        normalize(aim_yaw(hop, view.position) - velocity_yaw),
        fallback,
    );
    hop.side = side_away_from_wall(view, velocity_yaw, side);
}

/// Forward down the lane, with jump held for a takeoff.
fn forward(hop: &Hop, jump: bool) -> PlayerInput {
    keys(
        hop.route_yaw,
        0.0,
        1.0,
        FORWARD | SPRINT | if jump { JUMP } else { 0 },
    )
}

/// One decision of air control, `strafeInput`: forward and a side key at the
/// gain angle accelerate, a side key alone turns the velocity when the
/// heading is off, and forward alone just ahead of the velocity holds the
/// speed and lines the hop up at the end of its stretch.
fn strafe(hop: &Hop, view: &HopView<'_>, velocity: Velocity, steer_only: bool) -> PlayerInput {
    let speed = horizontal_speed(velocity).max(MIN_CONTROL_SPEED);
    let velocity_yaw = heading(velocity, hop.route_yaw);
    let mut wall = view
        .sight
        .map_or(0.0, |sight| wall_ahead_side(sight, view.position, velocity));
    if wall == 0.0
        && let Some(sight) = view.sight
    {
        let (left, right) = side_gaps(sight, view.position, velocity_yaw, SIDE_LOOK);
        if right < SIDE_WALL_GAP && left > right {
            wall = -1.0;
        } else if left < SIDE_WALL_GAP && right > left {
            wall = 1.0;
        }
    }
    let error = if wall != 0.0 {
        wall * FRAC_PI_2
    } else {
        normalize(aim_yaw(hop, view.position) - velocity_yaw)
    };
    if error.abs() > STEER_ERROR {
        let side = error.signum();
        // The side key is a quarter turn off the view.
        let yaw = velocity_yaw + side * side_turn_angle(speed) - side * FRAC_PI_2;
        return keys(yaw, side, 0.0, JUMP | if side < 0.0 { LEFT } else { RIGHT });
    }
    if steer_only || speed >= hop.target_speed {
        let yaw = velocity_yaw + error.signum() * error.abs().min(MAX_FORWARD_LEAD);
        return keys(yaw, 0.0, 1.0, JUMP | FORWARD);
    }
    let angle = strafe_gain_angle(speed) + (1.0 - view.skill) * UNSKILLED_ANGLE_ERROR;
    // Forward and one side: the wish is an eighth turn off the view.
    let yaw = velocity_yaw + hop.side * angle - hop.side * FRAC_PI_4;
    keys(
        yaw,
        hop.side * FRAC_1_SQRT_2,
        FRAC_1_SQRT_2,
        JUMP | FORWARD | if hop.side < 0.0 { LEFT } else { RIGHT },
    )
}

/// The controls air control wants, sent with the view turned toward the wanted
/// view at the skill's rate and the key chord nearest the wanted wish in the
/// view actually sent.
fn turn_view(hop: &mut Hop, view: &HopView<'_>, wanted: PlayerInput) -> PlayerInput {
    let elapsed = f64::from(view.tick.saturating_sub(hop.view_tick).max(1));
    let rate = 2.5 + 4.5 * view.skill;
    let turn = normalize(wanted.yaw - hop.view_yaw);
    let max = rate * elapsed / TICK_RATE;
    let yaw = normalize(hop.view_yaw + turn.clamp(-max, max));
    hop.view_yaw = yaw;
    hop.view_tick = view.tick;
    let [x, _, z] = wanted.direction;
    let mut buttons = wanted.buttons & !MOVE_KEYS;
    if x == 0.0 && z == 0.0 {
        return keys(yaw, 0.0, 0.0, buttons);
    }
    // The wish in the sent view's frame, snapped to the nearest of the eight
    // chords a player can hold.
    let wish = x.atan2(z);
    let eighth = (normalize(wish - yaw) / FRAC_PI_4).round();
    let (right, forward) = (eighth * FRAC_PI_4).sin_cos();
    let (right, forward) = (snap(right), snap(forward));
    if forward > 0.0 {
        buttons |= FORWARD;
    } else if forward < 0.0 {
        buttons |= BACK;
    }
    if right > 0.0 {
        buttons |= RIGHT;
    } else if right < 0.0 {
        buttons |= LEFT;
    }
    // Sprint counts only for forward alone.
    if buttons & (LEFT | RIGHT | BACK) != 0 {
        buttons &= !SPRINT;
    }
    keys(yaw, right, forward, buttons)
}

/// A chord's component: 0, or ±1 alone, or ±1/√2 in a diagonal.
fn snap(value: f64) -> f64 {
    if value.abs() < 0.3 {
        0.0
    } else if value.abs() > 0.9 {
        value.signum()
    } else {
        value.signum() * FRAC_1_SQRT_2
    }
}

/// The view `yaw` with movement keys `right` and `forward` in its frame: the
/// world-space wish the server reads, as a human client would send it.
fn keys(yaw: f64, right: f64, forward: f64, buttons: u16) -> PlayerInput {
    let (sin, cos) = yaw.sin_cos();
    PlayerInput {
        direction: [
            right * cos + forward * sin,
            0.0,
            forward * cos - right * sin,
        ],
        yaw,
        pitch: 0.0,
        buttons: buttons & (MOVE_KEYS | JUMP | SPRINT),
        weapon_slot: 0,
    }
}

/// The heading the hop makes for: back onto the straight lane it started on.
fn aim_yaw(hop: &Hop, position: Vec3) -> f64 {
    let (sin, cos) = hop.route_yaw.sin_cos();
    let right = (position.x - hop.start.x) * cos - (position.z - hop.start.z) * sin;
    hop.route_yaw - right.atan2(LINE_RETURN_DISTANCE)
}

fn choose_side(route_error: f64, fallback: f64) -> f64 {
    if route_error.abs() >= MIN_ROUTE_ERROR_FOR_SIDE {
        if route_error < 0.0 { -1.0 } else { 1.0 }
    } else {
        fallback
    }
}

/// The hop's side, turned away from a wall close by on it when the other side
/// has more room.
fn side_away_from_wall(view: &HopView<'_>, yaw: f64, side: f64) -> f64 {
    let Some(sight) = view.sight else {
        return side;
    };
    let (left, right) = side_gaps(sight, view.position, yaw, SIDE_LOOK);
    let (near, far) = if side > 0.0 {
        (right, left)
    } else {
        (left, right)
    };
    if near < TAKEOFF_SIDE_GAP && far > near {
        -side
    } else {
        side
    }
}

/// `calcAccelFalloff`.
fn accel_falloff(speed: f64) -> f64 {
    if speed <= RUN_SPEED {
        1.0
    } else if speed < RADICAL_SPEED {
        1.0 - (speed - RUN_SPEED) / (RADICAL_SPEED - RUN_SPEED) * 0.5
    } else {
        let ratio = (speed - RADICAL_SPEED) / (TOP_SPEED - RADICAL_SPEED);
        (0.5 * (1.0 - ratio * ratio * ratio)).max(0.0)
    }
}

/// `strafeGainAngle`: the wish just far enough round from the velocity that
/// air acceleration adds its whole step.
fn strafe_gain_angle(speed: f64) -> f64 {
    let step = AIR_ACCELERATION * MAX_SPEED * accel_falloff(speed) / TICK_RATE;
    ((MAX_SPEED - step) / speed.max(1.0))
        .clamp(-1.0, 1.0)
        .acos()
}

/// `sideTurnAngle`: a side key alone, nearly square to the velocity, turns
/// it fast without losing speed.
pub(in crate::games::washoi) fn side_turn_angle(speed: f64) -> f64 {
    let step = SIDE_ACCEL * accel_falloff(speed) / TICK_RATE;
    let along = MAX_AIR_SPEED * (1.0 - step).max(0.0);
    (along / speed.max(1.0)).clamp(-1.0, 1.0).acos()
}

/// `allyBlocksRoute`: a living ally ahead along `yaw` from `position`, within
/// `ALLY_LOOKAHEAD`, `ALLY_RADIUS` of the line, and `ALLY_HEIGHT` of the floor.
fn ally_in_lane(allies: &[Vec3], position: Vec3, yaw: f64) -> bool {
    let (sin, cos) = yaw.sin_cos();
    allies.iter().any(|ally| {
        let (dx, dz) = (ally.x - position.x, ally.z - position.z);
        let along = dx * sin + dz * cos;
        let lateral = (dx * cos - dz * sin).abs();
        (ally.y - position.y).abs() <= ALLY_HEIGHT
            && along > 0.0
            && along < ALLY_LOOKAHEAD
            && lateral < ALLY_RADIUS
    })
}

/// Seconds in the air for a hop off flat ground.
fn hop_seconds() -> f64 {
    JUMP_VELOCITY / GRAVITY
        + (JUMP_VELOCITY * JUMP_VELOCITY / GRAVITY / (GRAVITY * FALLING_GRAVITY_MULTIPLIER)).sqrt()
}

/// A wall coming up in the flight, `wallAheadSide`: the side with more room,
/// 1 for right and -1 for left, or 0 when the way ahead is clear.
fn wall_ahead_side(sight: &Sight, position: Vec3, velocity: Velocity) -> f64 {
    let speed = horizontal_speed(velocity);
    if speed < AIR_MIN_SPEED {
        return 0.0;
    }
    let yaw = velocity.linear.x.atan2(velocity.linear.z);
    let reach = speed * AIR_LOOKAHEAD_SECONDS;
    let ray = |angle: f64, offset: f64, height: f64| {
        let (sin, cos) = angle.sin_cos();
        let (ox, oz) = (cos * offset, -sin * offset);
        let from = [position.x + ox, position.y + height, position.z + oz];
        sight.reach(
            from,
            [from[0] + sin * reach, from[1], from[2] + cos * reach],
        )
    };
    // The hull's width, at its middle: three rays across it.
    let blocked = [-15.0, 0.0, 15.0]
        .into_iter()
        .any(|offset| ray(yaw, offset, 8.0) < 1.0 && ray(yaw, offset, 8.0 + STEP_PROBE) < 1.0);
    if !blocked {
        return 0.0;
    }
    let room =
        |side: f64| ray(yaw + side * PI / 6.0, 0.0, 8.0) + ray(yaw + side * PI / 3.0, 0.0, 8.0);
    if room(1.0) >= room(-1.0) { 1.0 } else { -1.0 }
}

/// The gap from the hull to the nearest wall square to each side of `yaw`,
/// out to `reach` past the hull, `sideGaps`: (left, right).
fn side_gaps(sight: &Sight, position: Vec3, yaw: f64, reach: f64) -> (f64, f64) {
    let length = reach + HULL_HALF;
    let gap = |side: f64| {
        let (sin, cos) = (yaw + side * FRAC_PI_2).sin_cos();
        let from = [position.x, position.y, position.z];
        let to = [
            position.x + sin * length,
            position.y,
            position.z + cos * length,
        ];
        let fraction = sight.reach(from, to);
        if fraction < 1.0
            && sight.clear(
                [from[0], from[1] + STEP_PROBE, from[2]],
                [to[0], to[1] + STEP_PROBE, to[2]],
            )
        {
            return reach;
        }
        (fraction * length - HULL_HALF).max(0.0)
    };
    (gap(-1.0), gap(1.0))
}

fn horizontal_speed(velocity: Velocity) -> f64 {
    velocity.linear.x.hypot(velocity.linear.z)
}

fn heading(velocity: Velocity, fallback: f64) -> f64 {
    if horizontal_speed(velocity) > 0.001 {
        velocity.linear.x.atan2(velocity.linear.z)
    } else {
        fallback
    }
}

fn normalize(angle: f64) -> f64 {
    angle.sin().atan2(angle.cos())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moving(z: f64, grounded: bool) -> Option<Velocity> {
        Some(Velocity {
            linear: Vec3::new(0.0, 0.0, z),
            grounded,
            ..Velocity::default()
        })
    }

    fn walk() -> PlayerInput {
        PlayerInput {
            direction: [0.0, 0.0, 1.0],
            buttons: FORWARD | SPRINT,
            weapon_slot: 3,
            ..PlayerInput::default()
        }
    }

    const LANE: HopLane = HopLane {
        yaw: 0.0,
        end: [0.0, 0.0, 1000.0],
    };

    fn view(
        tick: u32,
        z: f64,
        velocity: Option<Velocity>,
        lane: Option<HopLane>,
    ) -> HopView<'static> {
        HopView {
            tick,
            position: Vec3::new(0.0, 0.0, z),
            velocity,
            lane,
            sight: None,
            skill: 0.7,
            view_yaw: 0.0,
            fighting: false,
            allies: &[],
        }
    }

    #[test]
    fn gain_angles_match_washoi_air_acceleration() {
        // At 400 u/s, a 2.56-unit step past 320 along the wish.
        assert!((strafe_gain_angle(400.0) - (317.44f64 / 400.0).acos()).abs() < 1e-9);
        assert!(strafe_gain_angle(800.0) > strafe_gain_angle(400.0));
        assert!((accel_falloff(500.0) - 0.75).abs() < 1e-12);
        assert!((hop_seconds() - (0.32 + (0.32f64 * 0.32 / 1.25).sqrt())).abs() < 1e-12);
    }

    #[test]
    fn hops_from_a_fast_walk_strafes_in_the_air_and_chains_on_landing() {
        let mut chain = HopChain::default();
        // Slow, laneless, unskilled, or firing: no hop.
        assert!(
            chain
                .steer(&view(0, 0.0, moving(300.0, true), Some(LANE)), walk())
                .is_none()
        );
        assert!(
            chain
                .steer(&view(0, 0.0, moving(400.0, true), None), walk())
                .is_none()
        );
        let mut clumsy = view(0, 0.0, moving(400.0, true), Some(LANE));
        clumsy.skill = 0.2;
        assert!(chain.steer(&clumsy, walk()).is_none());
        let mut fighting = view(0, 0.0, moving(400.0, true), Some(LANE));
        fighting.fighting = true;
        assert!(chain.steer(&fighting, walk()).is_none());

        let takeoff = chain
            .steer(&view(10, 0.0, moving(400.0, true), Some(LANE)), walk())
            .unwrap();
        assert_eq!(takeoff.buttons, FORWARD | SPRINT | JUMP);
        assert_eq!(takeoff.weapon_slot, 3);
        assert!(!chain.interruptible(12));
        assert!(chain.interruptible(20));

        // Airborne at 400: forward and a side key, the view at the gain angle.
        let air = chain
            .steer(&view(14, 40.0, moving(400.0, false), Some(LANE)), walk())
            .unwrap();
        assert_eq!(air.buttons & (FORWARD | JUMP), FORWARD | JUMP);
        assert_ne!(air.buttons & (LEFT | RIGHT), 0);
        // The keys are relative to the view: a diagonal chord in its frame.
        let (sin, cos) = air.yaw.sin_cos();
        let forward = air.direction[0] * sin + air.direction[2] * cos;
        assert!((forward - FRAC_1_SQRT_2).abs() < 1e-9);

        // A landing with lane left hops on; one without stops after 3 ticks.
        let on = chain
            .steer(&view(60, 300.0, moving(450.0, true), Some(LANE)), walk())
            .unwrap();
        assert_ne!(on.buttons & JUMP, 0);
        assert!(chain.active());
        chain.steer(&view(70, 360.0, moving(450.0, false), Some(LANE)), walk());
        let short = HopLane {
            end: [0.0, 0.0, 400.0],
            ..LANE
        };
        let down = chain
            .steer(&view(100, 380.0, moving(450.0, true), Some(short)), walk())
            .unwrap();
        assert_eq!(down.buttons, FORWARD | SPRINT);
        assert!(!chain.active());
    }

    #[test]
    fn notes_why_a_chain_did_not_start_and_how_it_started_and_landed() {
        let mut chain = HopChain::default();
        assert!(
            chain
                .steer(&view(0, 0.0, moving(200.0, true), Some(LANE)), walk())
                .is_none()
        );
        assert_eq!(chain.note(), (Some("slow"), None));
        chain.clear_note();
        assert!(
            chain
                .steer(&view(0, 0.0, moving(400.0, true), None), walk())
                .is_none()
        );
        assert_eq!(chain.note().0, Some("no_lane"));
        chain.steer(&view(0, 0.0, moving(400.0, true), Some(LANE)), walk());
        assert_eq!(chain.note().0, Some("start_straight"));
        chain.clear_note();
        // Off the ground, then down again with the lane still open.
        chain.steer(&view(4, 20.0, moving(420.0, false), Some(LANE)), walk());
        chain.steer(&view(80, 300.0, moving(450.0, true), Some(LANE)), walk());
        assert_eq!(chain.note(), (Some("land_straight"), Some(450.0)));
    }

    #[test]
    fn a_landing_too_slow_turned_off_or_behind_an_ally_hands_back() {
        // Each landing asks the lane's start gates again, as TypeScript's
        // `routeStrafeNext` does: at 450 straight on it hops on; at 280,
        // heading 76 degrees off the lane, or with an ally ahead it ends.
        let started = || {
            let mut chain = HopChain::default();
            chain.steer(&view(0, 0.0, moving(400.0, true), Some(LANE)), walk());
            chain.steer(&view(4, 20.0, moving(420.0, false), Some(LANE)), walk());
            chain.clear_note();
            chain
        };
        let mut chain = started();
        chain.steer(&view(80, 300.0, moving(450.0, true), Some(LANE)), walk());
        assert_eq!(chain.note().0, Some("land_straight"));
        assert!(chain.active());
        let mut chain = started();
        chain.steer(&view(80, 300.0, moving(280.0, true), Some(LANE)), walk());
        assert_eq!(chain.note().0, Some("land_slow"));
        assert!(!chain.active());
        let turned = Some(Velocity {
            linear: Vec3::new(400.0, 0.0, 100.0),
            grounded: true,
            ..Velocity::default()
        });
        let mut chain = started();
        chain.steer(&view(80, 300.0, turned, Some(LANE)), walk());
        assert_eq!(chain.note().0, Some("land_heading"));
        assert!(!chain.active());
        let ahead = [Vec3::new(0.0, 0.0, 600.0)];
        let blocked = HopView {
            allies: &ahead,
            ..view(80, 300.0, moving(450.0, true), Some(LANE))
        };
        let mut chain = started();
        chain.steer(&blocked, walk());
        assert_eq!(chain.note().0, Some("land_ally"));
        assert!(!chain.active());
    }

    #[test]
    fn an_ally_in_the_lane_ahead_holds_the_chain_back() {
        // On the line 300 ahead: no chain. Behind, 200 to the side, or a
        // floor up: the chain starts.
        let ahead = [Vec3::new(10.0, 0.0, 300.0)];
        let blocked = HopView {
            allies: &ahead,
            ..view(0, 0.0, moving(400.0, true), Some(LANE))
        };
        assert!(HopChain::default().steer(&blocked, walk()).is_none());
        for clear in [
            Vec3::new(0.0, 0.0, -100.0),
            Vec3::new(200.0, 0.0, 300.0),
            Vec3::new(0.0, 200.0, 300.0),
            Vec3::new(0.0, 0.0, 700.0),
        ] {
            let allies = [clear];
            let view = HopView {
                allies: &allies,
                ..view(0, 0.0, moving(400.0, true), Some(LANE))
            };
            assert!(
                HopChain::default().steer(&view, walk()).is_some(),
                "{clear:?}"
            );
        }
    }

    #[test]
    fn a_hop_that_loses_its_line_or_momentum_hands_back() {
        let mut chain = HopChain::default();
        chain.steer(&view(0, 0.0, moving(400.0, true), Some(LANE)), walk());
        chain.steer(&view(4, 20.0, moving(400.0, false), Some(LANE)), walk());
        let drifted = HopView {
            position: Vec3::new(200.0, 0.0, 100.0),
            ..view(8, 0.0, moving(400.0, false), Some(LANE))
        };
        chain.steer(&drifted, walk());
        assert!(!chain.active());

        let mut chain = HopChain::default();
        chain.steer(&view(0, 0.0, moving(400.0, true), Some(LANE)), walk());
        chain.steer(&view(4, 20.0, moving(400.0, false), Some(LANE)), walk());
        chain.steer(&view(8, 40.0, moving(150.0, false), Some(LANE)), walk());
        assert!(!chain.active());
    }
}

//! Sliding off a ledge into a jump, after TypeScript `LedgeSlideJump.ts`
//! (`planLedgeSlideJump`) and `sequences/LedgeSlideJumpSequence.ts`. A bot
//! running at slide speed toward a drop or a jump on its route crouches into
//! a slide, jumps at the ledge, and carries the slide's speed across. The
//! whole script is proven first, tick by tick, with Washoi's own movement:
//! it must stay grounded until the jump, keep out of slime and lava, and
//! land within 96 units across and 64 up or down of the route's landing.
//!
//! TypeScript runs the script along the walk's heading under whatever view
//! the bot has. Here it runs forward along the view, with forward held, as
//! a player's keys would, and only when the walk already heads within
//! `MAX_VIEW_OFF` of the view.

use shagger_shared::{
    collision::collision_world::{CollisionWorld, contents},
    net::game_state::Vec3,
    server::simulation::types::Vec3 as SimVec3,
};

use super::rollout::Rollout;
use crate::games::washoi::{
    SimTime, hosting::net::protocol::PlayerInput, observation::INPUT_PERIOD,
};

const FORWARD: u16 = 1;
const JUMP: u16 = 1 << 4;
const CROUCH: u16 = 1 << 5;
const SPRINT: u16 = 1 << 6;

/// `PHYSICS.SLIDE_MIN_SPEED`.
const SLIDE_MIN_SPEED: f64 = 375.0;
/// How far above or below the bot a landing may be.
const MAX_CLIMB: f64 = 160.0;
/// The run must already head along the walk: cosine of the angle between.
const MIN_ALIGNMENT: f64 = 0.96;
/// How far the walk may head off the view, radians.
const MAX_VIEW_OFF: f64 = 8.0 * std::f64::consts::PI / 180.0;
/// The ledge is looked for from 16 to 224 units ahead, every 8.
const EDGE_PROBE: (u32, u32, u32) = (16, 224, 8);
/// Ticks of crouching before a jump may come: the slide has to start.
const MIN_SLIDE_TICKS: u32 = 14;
const MAX_TICKS: u32 = 150;
/// A landing counts this close to the route's.
const LANDING_RADIUS: f64 = 96.0;
const LANDING_HEIGHT: f64 = 64.0;

/// The bot's own state the plan starts from.
#[derive(Clone, Copy, Debug)]
pub(in crate::games::washoi) struct Start {
    pub(in crate::games::washoi) position: Vec3,
    pub(in crate::games::washoi) velocity: Vec3,
    pub(in crate::games::washoi) grounded: bool,
    pub(in crate::games::washoi) sliding: bool,
    pub(in crate::games::washoi) yaw: f64,
    pub(in crate::games::washoi) pitch: f64,
}

/// `LedgeSlideJumpPlan`: one input a tick, and where it lands.
#[derive(Clone, Debug)]
pub(in crate::games::washoi) struct Plan {
    pub(in crate::games::washoi) inputs: Vec<PlayerInput>,
    /// The body's predicted position and velocity before the first input,
    /// then after each: `inputs.len() + 1` states.
    pub(in crate::games::washoi) states: Vec<(Vec3, Vec3)>,
    pub(in crate::games::washoi) landing: Vec3,
    pub(in crate::games::washoi) jump_tick: u32,
}

/// How far ahead along `yaw` the floor ends: the first of every 8 units
/// from 16 to 224 with no floor within 64 below.
fn edge_ahead(position: Vec3, yaw: f64, world: &mut dyn CollisionWorld) -> Option<f64> {
    let (first, last, step) = EDGE_PROBE;
    (first..=last).step_by(step as usize).find_map(|distance| {
        let distance = f64::from(distance);
        let point = SimVec3::new(
            position.x + yaw.sin() * distance,
            position.y,
            position.z + yaw.cos() * distance,
        );
        let below = SimVec3::new(point.x, point.y - 64.0, point.z);
        let floor = world.trace(point, below, None, None);
        (!floor.hit || floor.fraction >= 1.0).then_some(distance)
    })
}

/// A proven slide off the ledge ahead onto `target`, or None when the bot
/// is too slow, not running along `walk`, the ledge is too near to slide
/// to, or the script misses.
pub(in crate::games::washoi) fn plan(
    start: Start,
    walk: [f64; 3],
    target: Vec3,
    world: &mut dyn CollisionWorld,
) -> Option<Plan> {
    let Start {
        position,
        velocity,
        yaw,
        pitch,
        ..
    } = start;
    let speed = velocity.x.hypot(velocity.z);
    if !start.grounded
        || start.sliding
        || speed < SLIDE_MIN_SPEED
        || (target.y - position.y).abs() > MAX_CLIMB
    {
        return None;
    }
    let travel = walk[0].atan2(walk[2]);
    let off = (travel - yaw + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU)
        - std::f64::consts::PI;
    if walk[0].hypot(walk[2]) == 0.0 || off.abs() > MAX_VIEW_OFF {
        return None;
    }
    let direction = [yaw.sin(), 0.0, yaw.cos()];
    if (velocity.x * direction[0] + velocity.z * direction[2]) / speed < MIN_ALIGNMENT {
        return None;
    }
    let edge = edge_ahead(position, yaw, world)?;
    if edge < speed * 0.14 + 24.0 {
        return None;
    }

    let mut body = Rollout::new(position, velocity, true);
    let mut sliding = false;
    let mut jump_tick = None;
    let mut airborne = false;
    let mut stable = 0;
    let mut inputs = Vec::new();
    let mut states = vec![(position, velocity)];
    for tick in 0..MAX_TICKS {
        let progress = (body.position.x - position.x) * direction[0]
            + (body.position.z - position.z) * direction[2];
        let jump = jump_tick.is_none()
            && sliding
            && tick >= MIN_SLIDE_TICKS
            && edge - progress <= (body.velocity.x.hypot(body.velocity.z) * 2.0 / 125.0).max(24.0);
        let buttons = FORWARD
            | SPRINT
            | if jump_tick.is_none() && !jump {
                CROUCH
            } else {
                0
            }
            | if jump { JUMP } else { 0 };
        let input = PlayerInput {
            direction,
            yaw,
            pitch,
            buttons,
            weapon_slot: 0,
        };
        let step = body.step(&input, world);
        sliding = step.sliding;
        let at = SimVec3::new(body.position.x, body.position.y, body.position.z);
        if world.point_contents(at) & (contents::LAVA | contents::SLIME) != 0 {
            return None;
        }
        if jump {
            jump_tick = Some(tick);
        }
        inputs.push(input);
        states.push((body.position, body.velocity));
        if !step.grounded {
            jump_tick?;
            airborne = true;
        }
        if airborne && step.grounded {
            if (body.position.x - target.x).hypot(body.position.z - target.z) > LANDING_RADIUS
                || (body.position.y - target.y).abs() > LANDING_HEIGHT
            {
                return None;
            }
            stable += 1;
            if stable >= 2 {
                return Some(Plan {
                    inputs,
                    states,
                    landing: body.position,
                    jump_tick: jump_tick?,
                });
            }
        }
    }
    None
}

/// `tryStartLedgeSlideJump`'s gates: `strafeJumpSkill`, and how often the
/// ledge ahead is probed.
const MIN_SKILL: f64 = 0.55;
const PROBE_TICKS: u32 = 31;
/// `BOT_SLIDE_COOLDOWN_TICKS`: Washoi's 1.5 s slide cooldown at 125 Hz.
const SLIDE_COOLDOWN_TICKS: u32 = 188;
/// A script whose body strays this far from every state the inputs sent so
/// far could have reached is abandoned: the rollout no longer describes the
/// game, and the rest of the script would play into a wall or a pit.
const MAX_DRIFT: f64 = 24.0;
const MAX_VELOCITY_DRIFT: f64 = 80.0;
/// How many predicted states back a snapshot may show: the inputs still on
/// their way to the server, as the backlog bounds them.
const LAG_STATES: usize = 32;
/// After the script, how long the landing may take: the sequence's 24 ticks.
const LANDING_TICKS: u32 = 24;
/// A ledge whose script failed is not tried again for five seconds, so a
/// misprediction never chains.
const FAILED_LEDGE_TICKS: u32 = 625;
const MAX_BURST: usize = 4;
const MAX_BACKLOG: u128 = 25;

/// What a decision knows when it may start a ledge slide jump.
pub(in crate::games::washoi) struct StartView<'a> {
    pub(in crate::games::washoi) tick: u32,
    pub(in crate::games::washoi) start: Start,
    /// The walk's input this decision, and the landing of the drop ahead.
    pub(in crate::games::washoi) walk: PlayerInput,
    pub(in crate::games::washoi) landing: Option<Vec3>,
    pub(in crate::games::washoi) skill: f64,
    pub(in crate::games::washoi) fighting: bool,
    pub(in crate::games::washoi) world: Option<&'a mut dyn CollisionWorld>,
}

struct Run {
    plan: Plan,
    target: Vec3,
    started: SimTime,
    sent: usize,
    /// The server tick of the first snapshot after the last input went out.
    finished: Option<u32>,
}

/// `LedgeSlideJumpSequence`: plays a proven script one input a tick, checks
/// every snapshot against it, and confirms the landing.
#[derive(Default)]
pub(in crate::games::washoi) struct LedgeJump {
    run: Option<Run>,
    probe_at: u32,
    cooldown_until: u32,
    failed: Vec<(Vec3, u32)>,
}

/// How a snapshot left the script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::games::washoi) enum Check {
    Running,
    Landed,
    /// The body strayed from the rollout, or never landed where it should.
    Failed,
}

fn near(a: Vec3, b: Vec3, limit: f64) -> bool {
    (a.x - b.x).hypot(a.z - b.z).hypot(a.y - b.y) <= limit
}

impl LedgeJump {
    pub(in crate::games::washoi) fn active(&self) -> bool {
        self.run.is_some()
    }

    /// Past the jump: from here the script runs to the landing whatever
    /// else happens, as a player can't take a jump back.
    pub(in crate::games::washoi) fn committed(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|run| run.sent > run.plan.jump_tick as usize)
    }

    /// Dead, respawned, or out of the map: nothing carries over.
    pub(in crate::games::washoi) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Give the script up and fall back to the route. A failure puts the
    /// ledge on cooldown; yielding to a fight before the jump does not.
    pub(in crate::games::washoi) fn abort(&mut self, tick: u32, failed: bool) {
        if let Some(run) = self.run.take()
            && failed
        {
            self.failed.push((run.target, tick + FAILED_LEDGE_TICKS));
        }
    }

    /// Start a script toward the drop ahead when every TypeScript gate
    /// passes and the rollout proves it. True when it started.
    ///
    /// TypeScript also wants sprint and forward on the walk's input, which
    /// sprints whenever it moves. Here the walk lets sprint go within 128
    /// units of a drop, to step off it at a landable speed, so the bot's run
    /// is judged by `plan`'s 375 u/s and heading instead. Live, every probe
    /// near a drop saw sprint released, at up to 403 u/s.
    pub(in crate::games::washoi) fn try_start(
        &mut self,
        view: StartView<'_>,
        now: SimTime,
    ) -> bool {
        const FIRE: u16 = 1 << 8;
        const ALT_FIRE: u16 = 1 << 9;
        let tick = view.tick;
        self.failed.retain(|&(_, until)| tick < until);
        let (Some(landing), Some(world)) = (view.landing, view.world) else {
            return false;
        };
        if self.run.is_some()
            || view.fighting
            || !view.start.grounded
            || view.skill < MIN_SKILL
            || tick < self.probe_at
            || tick < self.cooldown_until
            || view.walk.buttons & (FIRE | ALT_FIRE) != 0
            || view.walk.direction[0].hypot(view.walk.direction[2]) == 0.0
            || self
                .failed
                .iter()
                .any(|&(ledge, _)| near(ledge, landing, LANDING_RADIUS))
        {
            return false;
        }
        // TypeScript probes every 31 ticks regardless. Here the walk brakes
        // within 96 units of a ledge, so a probe spent before the edge comes
        // into range left the next one too slow: live, the few that reached
        // the planner ran at 53 to 179 u/s. The wait starts once the edge is
        // in range.
        if edge_ahead(view.start.position, view.start.yaw, &mut *world).is_none() {
            return false;
        }
        self.probe_at = tick + PROBE_TICKS;
        let Some(plan) = plan(view.start, view.walk.direction, landing, world) else {
            return false;
        };
        self.cooldown_until = tick + SLIDE_COOLDOWN_TICKS;
        self.run = Some(Run {
            plan,
            target: landing,
            started: now,
            sent: 0,
            finished: None,
        });
        true
    }

    /// The script's inputs due by `now`, one per input period from its
    /// start, a few at a time after a late wakeup. A backlog the scheduler
    /// can't clear abandons it.
    pub(in crate::games::washoi) fn drive(&mut self, now: SimTime, tick: u32) -> Vec<PlayerInput> {
        let Some(run) = &mut self.run else {
            return Vec::new();
        };
        let owed = 1 + (now - run.started).as_millis() / INPUT_PERIOD.as_millis();
        if owed.saturating_sub(run.sent as u128) > MAX_BACKLOG {
            self.abort(tick, true);
            return Vec::new();
        }
        let total = run.plan.inputs.len();
        let due = (owed.min(total as u128) as usize).saturating_sub(run.sent);
        let inputs = run.plan.inputs[run.sent..run.sent + due.min(MAX_BURST)].to_vec();
        run.sent += inputs.len();
        inputs
    }

    /// Check a snapshot of the bot's own body against the script: it must
    /// be near a state the inputs sent so far could have reached, moving as
    /// that state moves, and after the script it must land.
    pub(in crate::games::washoi) fn check(
        &mut self,
        tick: u32,
        position: Vec3,
        velocity: Vec3,
        grounded: bool,
    ) -> Check {
        let Some(run) = &mut self.run else {
            return Check::Running;
        };
        let reachable = &run.plan.states[run.sent.saturating_sub(LAG_STATES)..=run.sent];
        let on_track = reachable.iter().any(|&(at, moving)| {
            near(at, position, MAX_DRIFT)
                && (moving.x - velocity.x).hypot(moving.z - velocity.z) <= MAX_VELOCITY_DRIFT
        });
        let landing = run.plan.landing;
        let result = if run.sent == run.plan.inputs.len() {
            let finished = *run.finished.get_or_insert(tick);
            if grounded
                && (position.x - landing.x).hypot(position.z - landing.z) <= LANDING_RADIUS
                && (position.y - landing.y).abs() <= LANDING_HEIGHT
            {
                Check::Landed
            } else if tick > finished + LANDING_TICKS || !on_track {
                Check::Failed
            } else {
                Check::Running
            }
        } else if on_track {
            Check::Running
        } else {
            Check::Failed
        };
        match result {
            Check::Landed => self.run = None,
            Check::Failed => self.abort(tick, true),
            Check::Running => {}
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::agents::player::navigation::rollout::box_world::{Boxes, world};

    /// A floor ending at z = 400, then a gap to a lower floor from z = 700.
    fn gap(lower_from: f64) -> Boxes {
        Boxes {
            solids: vec![
                ([-512.0, -16.0, -1024.0], [512.0, 0.0, 400.0]),
                ([-512.0, -80.0, lower_from], [512.0, -64.0, 2048.0]),
            ],
            ..Boxes::default()
        }
    }

    fn running(z: f64, speed: f64) -> Start {
        Start {
            position: Vec3::new(0.0, 24.0, z),
            velocity: Vec3::new(0.0, 0.0, speed),
            grounded: true,
            sliding: false,
            yaw: 0.0,
            pitch: 0.0,
        }
    }

    const NORTH: [f64; 3] = [0.0, 0.0, 1.0];

    #[test]
    fn slides_off_the_ledge_and_jumps_the_gap_onto_the_landing() {
        let mut world = world(gap(560.0));
        let landing = Vec3::new(0.0, -40.0, 760.0);
        let plan = plan(running(200.0, 400.0), NORTH, landing, &mut world).expect("a proven jump");
        let jump = plan.jump_tick as usize;
        // Crouched into the slide until the jump, then neither.
        assert!(
            plan.inputs[..jump]
                .iter()
                .all(|input| input.buttons == FORWARD | SPRINT | CROUCH)
        );
        assert_eq!(plan.inputs[jump].buttons, FORWARD | SPRINT | JUMP);
        assert!(
            plan.inputs[jump + 1..]
                .iter()
                .all(|input| input.buttons == FORWARD | SPRINT)
        );
        // Forward along the view, the view unturned: keys a player could press.
        assert!(
            plan.inputs
                .iter()
                .all(|input| input.direction == NORTH && input.yaw == 0.0)
        );
        // The state after the jump input: the slide's speed carried off.
        let (_, takeoff) = plan.states[jump + 1];
        let takeoff = takeoff.x.hypot(takeoff.z);
        assert!(takeoff >= SLIDE_MIN_SPEED, "{takeoff}");
        assert!(
            plan.landing.z > 560.0 && (plan.landing.y + 40.0).abs() < 1.0,
            "{:?}",
            plan.landing
        );
    }

    #[test]
    fn refuses_what_it_cannot_prove() {
        let landing = Vec3::new(0.0, -40.0, 760.0);
        // Too slow to slide.
        assert!(
            plan(
                running(200.0, 320.0),
                NORTH,
                landing,
                &mut world(gap(560.0))
            )
            .is_none()
        );
        // The ledge too near to slide to first.
        assert!(
            plan(
                running(360.0, 400.0),
                NORTH,
                landing,
                &mut world(gap(560.0))
            )
            .is_none()
        );
        // Walking off the view: the keys would not run it.
        let aside = [0.5_f64.sin(), 0.0, 0.5_f64.cos()];
        assert!(
            plan(
                running(200.0, 400.0),
                aside,
                landing,
                &mut world(gap(560.0))
            )
            .is_none()
        );
        // A gap too wide: it lands short, in the pit below.
        let mut wide = gap(1400.0);
        wide.solids
            .push(([-512.0, -400.0, 400.0], [512.0, -384.0, 1400.0]));
        assert!(
            plan(
                running(200.0, 400.0),
                NORTH,
                Vec3::new(0.0, -40.0, 1450.0),
                &mut world(wide)
            )
            .is_none()
        );
        // Lava on the landing.
        let mut hot = gap(560.0);
        hot.liquids.push((
            ([-512.0, -64.0, 560.0], [512.0, 0.0, 2048.0]),
            contents::LAVA,
        ));
        assert!(plan(running(200.0, 400.0), NORTH, landing, &mut world(hot)).is_none());
    }

    fn start_view<'a>(world: &'a mut dyn CollisionWorld, tick: u32) -> StartView<'a> {
        StartView {
            tick,
            start: running(200.0, 400.0),
            walk: PlayerInput {
                direction: NORTH,
                buttons: SPRINT,
                ..PlayerInput::default()
            },
            landing: Some(Vec3::new(0.0, -40.0, 760.0)),
            skill: 0.7,
            fighting: false,
            world: Some(world),
        }
    }

    #[test]
    fn starts_only_when_typescripts_gates_pass() {
        let mut world = world(gap(560.0));
        let refused = |change: &dyn Fn(&mut StartView<'_>)| {
            let mut world = super::super::rollout::box_world::world(gap(560.0));
            let mut view = start_view(&mut world, 0);
            change(&mut view);
            !LedgeJump::default().try_start(view, SimTime::ZERO)
        };
        assert!(refused(&|view| view.fighting = true));
        assert!(refused(&|view| view.skill = 0.5));
        assert!(refused(&|view| view.walk.direction = [0.0; 3]));
        assert!(refused(&|view| view.walk.buttons = SPRINT | (1 << 8)));
        assert!(refused(&|view| view.landing = None));
        assert!(refused(&|view| view.world = None));
        let mut ledge = LedgeJump::default();
        assert!(ledge.try_start(start_view(&mut world, 0), SimTime::ZERO));
        assert!(ledge.active() && !ledge.committed());
    }

    #[test]
    fn plays_the_script_in_tick_order_and_confirms_the_landing() {
        let mut world = world(gap(560.0));
        let mut ledge = LedgeJump::default();
        assert!(ledge.try_start(start_view(&mut world, 0), SimTime::ZERO));
        let plan = ledge.run.as_ref().unwrap().plan.clone();
        let period = INPUT_PERIOD;
        let mut sent = Vec::new();
        // A late wakeup gets at most four inputs at once.
        let mut now = SimTime::ZERO + period * 6;
        sent.extend(ledge.drive(now, 0));
        assert_eq!(sent.len(), 4);
        for tick in 1.. {
            now = now + period;
            sent.extend(ledge.drive(now, tick));
            // The server shows the body where the rollout put it a few
            // inputs back, as latency would, and landed once all are in.
            let done = sent.len() == plan.inputs.len();
            let shown = if done {
                plan.states.len() - 1
            } else {
                sent.len().saturating_sub(3)
            };
            let (position, velocity) = plan.states[shown];
            let grounded = done;
            let result = ledge.check(tick, position, velocity, grounded);
            if result == Check::Landed {
                break;
            }
            assert_eq!(result, Check::Running, "tick {tick}");
            assert_eq!(ledge.committed(), sent.len() > plan.jump_tick as usize);
            assert!(tick < 400, "never landed");
        }
        assert_eq!(sent.len(), plan.inputs.len());
        assert!(sent.iter().zip(&plan.inputs).all(|(a, b)| a == b));
        assert!(!ledge.active());
    }

    #[test]
    fn a_body_off_the_rollout_stops_the_script_and_cools_the_ledge() {
        let mut world = world(gap(560.0));
        let mut ledge = LedgeJump::default();
        assert!(ledge.try_start(start_view(&mut world, 0), SimTime::ZERO));
        ledge.drive(SimTime::ZERO + INPUT_PERIOD * 3, 1);
        let (position, velocity) = ledge.run.as_ref().unwrap().plan.states[2];
        let pushed = Vec3::new(position.x + 40.0, position.y, position.z);
        assert_eq!(ledge.check(2, pushed, velocity, true), Check::Failed);
        assert!(!ledge.active());
        // That ledge waits; a later probe elsewhere would not.
        assert!(!ledge.try_start(start_view(&mut world, 400), SimTime::ZERO));
        assert!(ledge.try_start(start_view(&mut world, 700), SimTime::ZERO));
        // A backlog the scheduler can't clear abandons the script too.
        assert!(
            ledge
                .drive(SimTime::ZERO + INPUT_PERIOD * 40, 701)
                .is_empty()
        );
        assert!(!ledge.active());
    }

    #[test]
    fn yielding_to_a_fight_before_the_jump_cools_nothing() {
        let mut world = world(gap(560.0));
        let mut ledge = LedgeJump::default();
        assert!(ledge.try_start(start_view(&mut world, 0), SimTime::ZERO));
        ledge.abort(5, false);
        assert!(!ledge.active());
        assert!(ledge.try_start(start_view(&mut world, 200), SimTime::ZERO));
    }
}

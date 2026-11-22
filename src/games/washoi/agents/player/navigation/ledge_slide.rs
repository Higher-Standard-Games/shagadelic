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
use crate::games::washoi::hosting::net::protocol::PlayerInput;

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
    pub(in crate::games::washoi) landing: Vec3,
    pub(in crate::games::washoi) jump_tick: u32,
    pub(in crate::games::washoi) takeoff_speed: f64,
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
    let (first, last, step) = EDGE_PROBE;
    let edge = (first..=last).step_by(step as usize).find_map(|distance| {
        let distance = f64::from(distance);
        let point = SimVec3::new(
            position.x + direction[0] * distance,
            position.y,
            position.z + direction[2] * distance,
        );
        let below = SimVec3::new(point.x, point.y - 64.0, point.z);
        let floor = world.trace(point, below, None, None);
        (!floor.hit || floor.fraction >= 1.0).then_some(distance)
    })?;
    if edge < speed * 0.14 + 24.0 {
        return None;
    }

    let mut body = Rollout::new(position, velocity, true);
    let mut sliding = false;
    let mut jump_tick = None;
    let mut takeoff_speed = 0.0;
    let mut airborne = false;
    let mut stable = 0;
    let mut inputs = Vec::new();
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
            takeoff_speed = body.velocity.x.hypot(body.velocity.z);
        }
        inputs.push(input);
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
                    landing: body.position,
                    jump_tick: jump_tick?,
                    takeoff_speed,
                });
            }
        }
    }
    None
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
        assert!(
            plan.takeoff_speed >= SLIDE_MIN_SPEED,
            "{}",
            plan.takeoff_speed
        );
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
}

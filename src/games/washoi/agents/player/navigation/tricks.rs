//! Whether a climb or a wall jump works from where the bot is, proven by
//! running Washoi's own movement forward, from TypeScript
//! `bot/BotVisionTracer.ts` (`climbByJump`, `wallJumpGain`, `noWallJump`,
//! and `wallWithin`). A trick is taken because it works here, not because
//! a waypoint says so.

use shagger_shared::{
    collision::collision_world::CollisionWorld, net::game_state::Vec3,
    server::simulation::types::Vec3 as SimVec3,
};

use super::rollout::Rollout;
use crate::games::washoi::hosting::net::protocol::PlayerInput;

const FORWARD: u16 = 1;
const JUMP: u16 = 1 << 4;
/// A climb is looked for over this long after the jump: the jump, a ledge
/// grab, and its pull-up, ticks.
const CLIMB_TICKS: u32 = 150;
/// A wall jump's flight is followed this long, ticks.
const WALL_JUMP_TICKS: u32 = 125;
/// A climb has to end on ground at least this much higher than the bot stood.
const MIN_CLIMB: f64 = 20.0;
/// A flight rising this much faster than the same flight without the press
/// had a wall jump, units/s.
const WALL_JUMP_RISE: f64 = 100.0;
/// Ticks the two flights are stepped side by side before `wall_jump_gain`
/// runs them in full.
const WALL_JUMP_PROBE_TICKS: u32 = 8;
const HULL_HALF: f64 = 16.0;

/// Where a rollout ends.
#[derive(Clone, Copy, Debug)]
pub(in crate::games::washoi) struct End {
    pub(in crate::games::washoi) position: Vec3,
    pub(in crate::games::washoi) velocity: Vec3,
    pub(in crate::games::washoi) grounded: bool,
    pub(in crate::games::washoi) ticks: u32,
}

/// One tick's command: the keys move along `yaw`, and the view looks that
/// way, which the ledge grab reads for forward.
fn command(yaw: f64, buttons: u16) -> PlayerInput {
    PlayerInput {
        direction: [yaw.sin(), 0.0, yaw.cos()],
        yaw,
        pitch: 0.0,
        buttons,
        ..PlayerInput::default()
    }
}

/// `rollout`: run the movement forward one command a tick, until `stop`
/// says so or `max_ticks` pass.
fn rollout(
    world: &mut dyn CollisionWorld,
    position: Vec3,
    velocity: Vec3,
    grounded: bool,
    mut keys: impl FnMut(u32, &End) -> (f64, u16),
    max_ticks: u32,
    stop: impl Fn(&End) -> bool,
) -> End {
    let mut body = Rollout::with_tricks(position, velocity, grounded);
    let mut at = End {
        position,
        velocity,
        grounded,
        ticks: 0,
    };
    for tick in 1..=max_ticks {
        let (yaw, buttons) = keys(tick, &at);
        let step = body.step(&command(yaw, buttons), world);
        at = End {
            position: body.position,
            velocity: body.velocity,
            grounded: step.grounded,
            ticks: tick,
        };
        if stop(&at) {
            break;
        }
    }
    at
}

/// `climbByJump`: whether jumping now along `yaw`, holding forward, ends
/// standing on ground higher than here, onto a box by the jump or a ledge by
/// the grab and its pull-up. Where the bot stands then.
pub(in crate::games::washoi) fn climb_by_jump(
    world: &mut dyn CollisionWorld,
    position: Vec3,
    velocity: Vec3,
    yaw: f64,
) -> Option<Vec3> {
    let end = rollout(
        world,
        position,
        velocity,
        true,
        |tick, _| (yaw, FORWARD | if tick <= 2 { JUMP } else { 0 }),
        CLIMB_TICKS,
        |at| at.ticks > 4 && at.grounded,
    );
    (end.grounded && end.position.y - position.y >= MIN_CLIMB).then_some(end.position)
}

/// `noWallJump`: the two flights `wall_jump_gain` compares, a few ticks side
/// by side. True when they come to the same state with no wall jump thrown,
/// so the press did nothing and, with the same keys from then on, never
/// will. False, and the whole comparison runs, when they differ for
/// `WALL_JUMP_PROBE_TICKS` or a wall jump throws the bot up meanwhile.
fn no_wall_jump(world: &mut dyn CollisionWorld, position: Vec3, velocity: Vec3, yaw: f64) -> bool {
    let mut plain = Rollout::with_tricks(position, velocity, false);
    let mut jumped = Rollout::with_tricks(position, velocity, false);
    for tick in 1..=WALL_JUMP_PROBE_TICKS {
        // `wall_jump_gain`'s launch test, made before each tick's step.
        if tick > 1 && jumped.velocity.y > plain.velocity.y + WALL_JUMP_RISE {
            return false;
        }
        let a = plain.step(&command(yaw, FORWARD), world).grounded;
        let press = if tick == 1 { JUMP } else { 0 };
        let b = jumped.step(&command(yaw, FORWARD | press), world).grounded;
        // A landing ends a flight: the comparison's own stopping rule decides.
        if (tick > 2 && (a || b)) || a != b {
            return false;
        }
        if tick > 1
            && plain.position == jumped.position
            && plain.velocity == jumped.velocity
            && plain.state() == jumped.state()
        {
            return true;
        }
    }
    false
}

/// `wallJumpGain`: in the air, how much nearer `goal` a wall jump pressed
/// now ends than carrying on without one (negative: further), or `None`
/// when no wall jump would happen here. Both flights hold forward along
/// `yaw` and end on landing or after `WALL_JUMP_TICKS`.
pub(in crate::games::washoi) fn wall_jump_gain(
    world: &mut dyn CollisionWorld,
    position: Vec3,
    velocity: Vec3,
    yaw: f64,
    goal: Vec3,
) -> Option<f64> {
    let landed = |at: &End| at.ticks > 2 && at.grounded;
    if no_wall_jump(world, position, velocity, yaw) {
        return None;
    }
    let mut plain_rise = vec![f64::NEG_INFINITY; WALL_JUMP_TICKS as usize + 1];
    let plain = rollout(
        world,
        position,
        velocity,
        false,
        |tick, at| {
            plain_rise[tick as usize] = at.velocity.y;
            (yaw, FORWARD)
        },
        WALL_JUMP_TICKS,
        landed,
    );
    let mut launched = false;
    let jumped = rollout(
        world,
        position,
        velocity,
        false,
        |tick, at| {
            // A wall jump throws the bot up off the wall, far faster than the
            // flight without it.
            if tick > 1 && at.velocity.y > plain_rise[tick as usize] + WALL_JUMP_RISE {
                launched = true;
            }
            (yaw, FORWARD | if tick == 1 { JUMP } else { 0 })
        },
        WALL_JUMP_TICKS,
        landed,
    );
    if !launched {
        return None;
    }
    let distance = |at: &End| {
        (goal.x - at.position.x)
            .hypot((goal.y - at.position.y) * 1.5)
            .hypot(goal.z - at.position.z)
    };
    Some(distance(&plain) - distance(&jumped))
}

/// `wallWithin`: a wall stands within `reach` of the hull at the origin's
/// height, any way round.
pub(in crate::games::washoi) fn wall_within(
    world: &mut dyn CollisionWorld,
    position: Vec3,
    reach: f64,
) -> bool {
    let length = reach + HULL_HALF;
    (0..8).any(|i| {
        let a = f64::from(i) / 8.0 * std::f64::consts::TAU;
        let hit = world.trace(
            SimVec3 {
                x: position.x,
                y: position.y,
                z: position.z,
            },
            SimVec3 {
                x: position.x + a.sin() * length,
                y: position.y,
                z: position.z + a.cos() * length,
            },
            None,
            None,
        );
        hit.fraction < 1.0 && !hit.normal.is_some_and(|normal| normal.y.abs() > 0.3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::agents::player::navigation::rollout::box_world::{Boxes, world};

    /// A floor whose top is at y = 0, and a box `height` tall 40 ahead.
    fn step_up(height: f64) -> Boxes {
        Boxes {
            solids: vec![
                ([-512.0, -16.0, -512.0], [512.0, 0.0, 512.0]),
                ([-512.0, 0.0, 40.0], [512.0, height, 512.0]),
            ],
            ..Boxes::default()
        }
    }

    #[test]
    fn a_jump_climbs_a_box_within_reach_and_not_a_wall_too_tall() {
        let start = Vec3::new(0.0, 24.0, 0.0);
        let mut low = world(step_up(40.0));
        let top = climb_by_jump(&mut low, start, Vec3::default(), 0.0).expect("climbs");
        // Standing on it: the hull reaches 15 past the origin, over the edge at z 40.
        assert!(top.y >= 24.0 + 40.0 - 1.0 && top.z > 40.0 - 15.0, "{top:?}");
        let mut tall = world(step_up(400.0));
        assert_eq!(climb_by_jump(&mut tall, start, Vec3::default(), 0.0), None);
        // A jump on flat ground lands where it took off: no climb.
        let mut flat = world(step_up(0.0));
        assert_eq!(climb_by_jump(&mut flat, start, Vec3::default(), 0.0), None);
    }

    #[test]
    fn no_wall_jump_in_open_air() {
        let mut open = world(Boxes {
            solids: vec![([-512.0, -16.0, -512.0], [512.0, 0.0, 512.0])],
            ..Boxes::default()
        });
        let at = Vec3::new(0.0, 200.0, 0.0);
        let goal = Vec3::new(0.0, 200.0, 400.0);
        assert_eq!(
            wall_jump_gain(&mut open, at, Vec3::new(0.0, 0.0, 300.0), 0.0, goal),
            None
        );
        assert!(!wall_within(&mut open, at, 12.0));
        let mut walled = world(Boxes {
            solids: vec![([-512.0, -16.0, 20.0], [512.0, 400.0, 60.0])],
            ..Boxes::default()
        });
        assert!(wall_within(&mut walled, Vec3::new(0.0, 200.0, 0.0), 12.0));
    }
}

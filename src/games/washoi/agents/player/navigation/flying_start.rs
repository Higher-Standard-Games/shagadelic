//! Joining a learned route's script on the run, after TypeScript
//! `sequences/FlyingStart.ts`. A learned route's script starts from a
//! standstill on its mark: a sprint run-up along the route's heading, then
//! the recorded trick. A player doesn't stop on a mark before a trickjump. So
//! the run-up is replayed once from the mark, keeping the position and
//! velocity after each command, and a bot running in along the line that
//! matches one of those states can take over the script from there.
//!
//! Nothing is trusted to the match alone: before joining, the rest of the
//! script is simulated from the bot's own position and velocity with
//! Washoi's own movement, and the bot joins only if that lands where the
//! route lands.

use shagger_shared::{
    bsp::trigger_table::TriggerVolume, collision::collision_world::CollisionWorld,
    net::game_state::Vec3,
};

use super::rollout::Rollout;
use crate::games::washoi::hosting::net::protocol::PlayerInput;

const JUMP: u16 = 1 << 4;
const CROUCH: u16 = 1 << 5;

/// A run-up state matches the bot within this far, units...
const JOIN_DISTANCE: f64 = 16.0;
/// ...this far up or down...
const JOIN_HEIGHT: f64 = 18.0;
/// ...this much speed difference, u/s...
const JOIN_SPEED: f64 = 60.0;
/// ...and this much heading difference.
const JOIN_HEADING: f64 = 10.0 * std::f64::consts::PI / 180.0;
/// The run-in starts this far behind the mark, units.
pub(in crate::games::washoi) const ENTRY_BACK: f64 = 192.0;
/// Landed when this close to the route's landing, as the sequence judges it.
const LANDING_RADIUS: f64 = 64.0;
const LANDING_HEIGHT: f64 = 48.0;
/// Ticks after the script in which a landing counts.
const LANDING_TICKS: usize = 40;

/// A body's position and velocity.
pub(in crate::games::washoi) type State = (Vec3, Vec3);

/// `runUpLength`: how many leading commands are the plain run-up, all equal
/// to the first. A jump or a crouch is already the trick, and a script that
/// is all run-up has no trick to join.
pub(in crate::games::washoi) fn run_up_length(commands: &[PlayerInput]) -> usize {
    let Some(first) = commands.first() else {
        return 0;
    };
    if first.buttons & (JUMP | CROUCH) != 0 {
        return 0;
    }
    let length = commands
        .iter()
        .take_while(|command| *command == first)
        .count();
    if length == commands.len() { 0 } else { length }
}

/// `runUpStates`: the state after each run-up command, from standing on the
/// mark.
pub(in crate::games::washoi) fn run_up_states(
    start: Vec3,
    commands: &[PlayerInput],
    world: &mut dyn CollisionWorld,
    triggers: &[TriggerVolume],
) -> Vec<State> {
    let mut body = Rollout::full(start, Vec3::default(), true, triggers);
    (0..run_up_length(commands))
        .map(|i| {
            body.step(&commands[i], world);
            (body.position, body.velocity)
        })
        .collect()
}

/// `findJoin`: the run-up state the bot matches, by index: nearest within
/// the join distance, at the same height, as fast, heading the same way.
pub(in crate::games::washoi) fn find_join(
    run_up: &[State],
    position: Vec3,
    velocity: Vec3,
) -> Option<usize> {
    let speed = velocity.x.hypot(velocity.z);
    if speed < 1.0 {
        return None;
    }
    let mut best = None;
    let mut best_distance = JOIN_DISTANCE;
    for (k, &(at, moving)) in run_up.iter().enumerate() {
        let distance = (at.x - position.x).hypot(at.z - position.z);
        if distance > best_distance || (at.y - position.y).abs() > JOIN_HEIGHT {
            continue;
        }
        let run_speed = moving.x.hypot(moving.z);
        if (run_speed - speed).abs() > JOIN_SPEED || run_speed < 1.0 {
            continue;
        }
        let off = (moving.x * velocity.z - moving.z * velocity.x)
            .atan2(moving.x * velocity.x + moving.z * velocity.z);
        if off.abs() > JOIN_HEADING {
            continue;
        }
        best = Some(k);
        best_distance = distance;
    }
    best
}

/// `joinLands`: whether the script after run-up state `k`, from the bot's
/// own state, lands where the route lands. After the script the bot stands
/// still, with no keys.
pub(in crate::games::washoi) fn join_lands(
    commands: &[PlayerInput],
    k: usize,
    start: State,
    landing: Vec3,
    world: &mut dyn CollisionWorld,
    triggers: &[TriggerVolume],
) -> bool {
    let rest = &commands[k + 1..];
    let Some(&last) = rest.last() else {
        return false;
    };
    let idle = PlayerInput {
        direction: [0.0; 3],
        buttons: 0,
        ..last
    };
    let mut body = Rollout::full(start.0, start.1, true, triggers);
    for i in 0..rest.len() + LANDING_TICKS {
        let step = body.step(rest.get(i).unwrap_or(&idle), world);
        if i >= rest.len() / 2
            && step.grounded
            && (body.position.x - landing.x).hypot(body.position.z - landing.z) <= LANDING_RADIUS
            && (body.position.y - landing.y).abs() <= LANDING_HEIGHT
        {
            return true;
        }
    }
    false
}

/// `predict`: where the bot will be after `ticks` more of `command`. The
/// snapshot it decides on is that far behind the server, and it keeps
/// sprinting meanwhile. Also whether it is on the ground then.
pub(in crate::games::washoi) fn predict(
    start: State,
    command: &PlayerInput,
    ticks: usize,
    world: &mut dyn CollisionWorld,
    triggers: &[TriggerVolume],
) -> (State, bool) {
    let mut body = Rollout::full(start.0, start.1, true, triggers);
    let mut grounded = true;
    for _ in 0..ticks {
        grounded = body.step(command, world).grounded;
    }
    ((body.position, body.velocity), grounded)
}

/// `entryPoint`: where the run-in starts, behind the mark on the route's
/// line.
pub(in crate::games::washoi) fn entry_point(start: Vec3, yaw: f64) -> Vec3 {
    Vec3::new(
        start.x - yaw.sin() * ENTRY_BACK,
        start.y,
        start.z - yaw.cos() * ENTRY_BACK,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::agents::player::navigation::rollout::box_world::{Boxes, world};

    const FORWARD: u16 = 1;
    const SPRINT: u16 = 1 << 6;

    /// A run north along a long floor, then a jump onto a raised step.
    fn script() -> Vec<PlayerInput> {
        let run = PlayerInput {
            direction: [0.0, 0.0, 1.0],
            buttons: FORWARD | SPRINT,
            ..PlayerInput::default()
        };
        let jump = PlayerInput {
            buttons: FORWARD | SPRINT | JUMP,
            ..run
        };
        let mut commands = vec![run; 60];
        commands.push(jump);
        // Long enough to come down and run on after the landing.
        commands.extend(vec![run; 150]);
        commands
    }

    fn floor() -> Boxes {
        Boxes {
            solids: vec![([-512.0, -16.0, -1024.0], [512.0, 0.0, 4096.0])],
            ..Boxes::default()
        }
    }

    #[test]
    fn the_run_up_is_the_plain_running_before_the_trick() {
        let commands = script();
        assert_eq!(run_up_length(&commands), 60);
        assert_eq!(run_up_length(&commands[..60]), 0, "all run-up, no trick");
        assert_eq!(run_up_length(&commands[60..]), 0, "starts with the trick");
        let mut world = world(floor());
        let run_up = run_up_states(Vec3::new(0.0, 24.0, 0.0), &commands, &mut world, &[]);
        assert_eq!(run_up.len(), 60);
        // From standing it runs up to the sprint's 400 u/s along the line.
        let (at, moving) = run_up[59];
        assert!(at.z > 100.0 && at.x.abs() < 1e-6, "{at:?}");
        assert!((moving.z - 400.0).abs() < 5.0, "{moving:?}");
    }

    #[test]
    fn joins_a_matching_run_up_state_only_where_the_rest_lands() {
        let commands = script();
        let mut world = world(floor());
        let start = Vec3::new(0.0, 24.0, 0.0);
        let run_up = run_up_states(start, &commands, &mut world, &[]);
        // The route's own landing, from standing on the mark.
        let mut body = Rollout::full(start, Vec3::default(), true, &[]);
        let mut landed = start;
        for (i, command) in commands.iter().enumerate() {
            let step = body.step(command, &mut world);
            if i > commands.len() / 2 && step.grounded {
                landed = body.position;
            }
        }
        // Running in as the run-up runs at state 40: it joins there, and
        // the rest of the script lands.
        let (at, moving) = run_up[40];
        let k = find_join(&run_up, at, moving).expect("a join");
        assert!((39..=41).contains(&k), "{k}");
        assert!(join_lands(
            &commands,
            k,
            (at, moving),
            landed,
            &mut world,
            &[]
        ));
        // Too slow, off the line, or heading away: no join.
        assert_eq!(
            find_join(&run_up, at, Vec3::new(0.0, 0.0, moving.z - 100.0)),
            None
        );
        assert_eq!(
            find_join(&run_up, Vec3::new(at.x + 30.0, at.y, at.z), moving),
            None
        );
        assert_eq!(
            find_join(&run_up, at, Vec3::new(moving.z * 0.3, 0.0, moving.z)),
            None
        );
        // From far behind, the rest of the script falls short of the landing.
        let behind = (Vec3::new(0.0, 24.0, at.z - 300.0), moving);
        assert!(!join_lands(&commands, k, behind, landed, &mut world, &[]));
    }

    #[test]
    fn predicts_the_run_ahead_and_finds_the_entry_behind_the_mark() {
        let mut world = world(floor());
        let ((at, _), grounded) = predict(
            (Vec3::new(0.0, 24.0, 0.0), Vec3::new(0.0, 0.0, 400.0)),
            &script()[0],
            3,
            &mut world,
            &[],
        );
        assert!(grounded && (at.z - 9.6).abs() < 1.0, "{at:?}");
        let entry = entry_point(Vec3::new(100.0, 24.0, 0.0), std::f64::consts::FRAC_PI_2);
        assert!((entry.x - (100.0 - ENTRY_BACK)).abs() < 1e-9 && entry.z.abs() < 1e-9);
    }
}

//! A bot that asks to move and goes nowhere turns away and thinks again.
//!
//! Ported from TypeScript `BotBlockedMove.ts` and `StateMachineBrain.ts`
//! (`_turnFromBlock`, `_openHeading`, `_runOpen`, and the walk to an open
//! point in `_startRecovery`). A wall, a corner, or a lip the steering does
//! not know about can hold a grounded bot still while whatever it is doing
//! keeps asking to move. This runs last over every decision's input.
//!
//! Deviations: TypeScript also flips the fight strafe's direction and turns
//! away from the step it was walking. Here the strafe keeps its own rhythm,
//! and the bot turns away from the direction it was asking to move.

use std::{f64::consts::PI, time::Duration};

use shagger_shared::net::game_state::{Vec3, Velocity};

use crate::games::washoi::{
    Sight, SimTime, agents::player::navigation::local_walk, hosting::net::protocol::PlayerInput,
};

/// How long asking to move without going anywhere counts as blocked.
const BLOCKED_FOR: Duration = Duration::from_millis(600);
/// Moving less than this far across the floor is going nowhere.
const BLOCKED_RADIUS: f64 = 24.0;
/// A rise or fall of more than this, a lift or a step, is going somewhere.
const BLOCKED_RISE: f64 = 16.0;
/// Faster than this across the floor is moving, whatever the position says.
const BLOCKED_SPEED: f64 = 60.0;
/// In a fight, how long to run the open way.
const UNSTICK_FOR: Duration = Duration::from_millis(500);
const UNSTICK_PROBE: f64 = 96.0;
/// Out of a fight, how long to walk to the open point found.
const RECOVERY_FOR: Duration = Duration::from_secs(1);
const RECOVERY_REACHED: f64 = 16.0;

const FORWARD: u16 = 1;
const BACK: u16 = 1 << 1;
const LEFT: u16 = 1 << 2;
const RIGHT: u16 = 1 << 3;
const JUMP: u16 = 1 << 4;
const CROUCH: u16 = 1 << 5;
const SPRINT: u16 = 1 << 6;
const MOVE_KEYS: u16 = FORWARD | BACK | LEFT | RIGHT;

/// What the filter needs from this decision.
pub(in crate::games::washoi) struct UnstickView<'a> {
    pub(in crate::games::washoi) now: SimTime,
    pub(in crate::games::washoi) position: Vec3,
    pub(in crate::games::washoi) velocity: Option<Velocity>,
    /// Fighting, dodging, an enemy in sight, or hit just now.
    pub(in crate::games::washoi) fighting: bool,
    /// An enemy within body range: pressing into it is no wall.
    pub(in crate::games::washoi) enemy_near: bool,
    pub(in crate::games::washoi) sight: Option<&'a Sight>,
}

#[derive(Default)]
pub(in crate::games::washoi) struct Unstick {
    anchor: Option<(Vec3, SimTime)>,
    run: Option<(f64, SimTime)>,
    recovery: Option<(Vec3, SimTime)>,
}

impl Unstick {
    /// Forget everything, such as on death or a recorded maneuver.
    pub(in crate::games::washoi) fn reset(&mut self) {
        *self = Self::default();
    }

    /// The decision's input, turned away from what holds the bot when it is
    /// blocked. True when the walk that led here should be given up, so the
    /// next plan goes another way.
    pub(in crate::games::washoi) fn filter(
        &mut self,
        view: &UnstickView<'_>,
        input: PlayerInput,
    ) -> (PlayerInput, bool) {
        let now = view.now;
        if let Some((yaw, until)) = self.run {
            if now < until {
                return (run_open(input, yaw), false);
            }
            // The watch starts over once the turn is done.
            self.run = None;
            self.anchor = None;
        }
        if let Some((target, until)) = self.recovery {
            if now < until && horizontal(view.position, target) > RECOVERY_REACHED {
                return (move_toward(input, view.position, target), false);
            }
            self.recovery = None;
            self.anchor = None;
        }
        let grounded = view.velocity.is_none_or(|velocity| velocity.grounded);
        let speed = view
            .velocity
            .map_or(0.0, |velocity| velocity.linear.x.hypot(velocity.linear.z));
        let [x, _, z] = input.direction;
        let asking = grounded
            && speed < BLOCKED_SPEED
            && (x != 0.0 || z != 0.0 || input.buttons & MOVE_KEYS != 0)
            && !view.enemy_near;
        if !self.blocked(now, view.position, asking) {
            return (input, false);
        }
        let heading = x.atan2(z);
        if view.fighting {
            let yaw = open_heading(view.sight, view.position, heading);
            self.run = Some((yaw, now + UNSTICK_FOR));
            return (run_open(input, yaw), true);
        }
        let away = heading + PI;
        let target = [128.0, 64.0, 32.0]
            .into_iter()
            .flat_map(|distance| [0.0, PI / 2.0, -PI / 2.0, PI].map(|turn| (distance, away + turn)))
            .map(|(distance, yaw)| ahead(view.position, yaw, distance))
            .find(|&target| open(view.sight, view.position, target))
            .unwrap_or_else(|| ahead(view.position, away, 32.0));
        self.recovery = Some((target, now + RECOVERY_FOR));
        (move_toward(input, view.position, target), true)
    }

    /// `BotBlockedMove.update`: true on the decision the bot is found
    /// blocked, and the watch starts over from there.
    fn blocked(&mut self, now: SimTime, position: Vec3, asking: bool) -> bool {
        if !asking {
            self.anchor = None;
            return false;
        }
        match self.anchor {
            Some((anchor, since))
                if horizontal(position, anchor) <= BLOCKED_RADIUS
                    && (position.y - anchor.y).abs() <= BLOCKED_RISE =>
            {
                if now - since < BLOCKED_FOR {
                    return false;
                }
                self.anchor = Some((position, now));
                true
            }
            _ => {
                self.anchor = Some((position, now));
                false
            }
        }
    }
}

/// The first of behind, the sides, and the back diagonals that has a clear
/// walk with floor, or straight back.
fn open_heading(sight: Option<&Sight>, position: Vec3, heading: f64) -> f64 {
    [PI, PI / 2.0, -PI / 2.0, 3.0 * PI / 4.0, -3.0 * PI / 4.0]
        .into_iter()
        .map(|turn| heading + turn)
        .find(|&yaw| open(sight, position, ahead(position, yaw, UNSTICK_PROBE)))
        .unwrap_or(heading + PI)
}

/// A straight walk with floor under it. `local_walk` samples both. Without
/// the map's collision every way counts as open, as in TypeScript.
fn open(sight: Option<&Sight>, from: Vec3, to: Vec3) -> bool {
    sight.is_none_or(|sight| local_walk(sight, from, to))
}

/// `input` with its movement replaced by a run toward `yaw`. Aim and fire stay.
fn run_open(mut input: PlayerInput, yaw: f64) -> PlayerInput {
    input.direction = [yaw.sin(), 0.0, yaw.cos()];
    input.buttons = (input.buttons & !(MOVE_KEYS | JUMP | CROUCH)) | FORWARD | SPRINT;
    input
}

fn move_toward(mut input: PlayerInput, position: Vec3, target: Vec3) -> PlayerInput {
    let yaw = (target.x - position.x).atan2(target.z - position.z);
    input.direction = [yaw.sin(), 0.0, yaw.cos()];
    input.buttons = (input.buttons & !(MOVE_KEYS | JUMP | SPRINT)) | FORWARD;
    input
}

fn ahead(position: Vec3, yaw: f64, distance: f64) -> Vec3 {
    Vec3::new(
        position.x + yaw.sin() * distance,
        position.y,
        position.z + yaw.cos() * distance,
    )
}

fn horizontal(a: Vec3, b: Vec3) -> f64 {
    (a.x - b.x).hypot(a.z - b.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRE: u16 = 1 << 8;

    fn view(now_ms: u64, fighting: bool, enemy_near: bool) -> UnstickView<'static> {
        UnstickView {
            now: SimTime::ZERO + Duration::from_millis(now_ms),
            position: Vec3::new(10.0, 0.0, 10.0),
            velocity: Some(Velocity {
                grounded: true,
                ..Velocity::default()
            }),
            fighting,
            enemy_near,
            sight: None,
        }
    }

    fn walk_north() -> PlayerInput {
        PlayerInput {
            direction: [0.0, 0.0, 1.0],
            yaw: 0.3,
            buttons: FORWARD | FIRE,
            ..PlayerInput::default()
        }
    }

    #[test]
    fn a_bot_held_still_while_asking_to_move_is_blocked_after_0_6_s() {
        let mut unstick = Unstick::default();
        for ms in [0, 200, 400, 592] {
            assert_eq!(
                unstick.filter(&view(ms, false, false), walk_north()),
                (walk_north(), false)
            );
        }
        // Out of a fight it gives up the walk and heads for open ground behind.
        let (input, give_up) = unstick.filter(&view(600, false, false), walk_north());
        assert!(give_up);
        assert!(input.direction[2] < -0.99);
        assert_eq!(input.yaw, 0.3);
        // It keeps walking there for a second, then asks afresh.
        let (still, again) = unstick.filter(&view(900, false, false), walk_north());
        assert_eq!((still.direction, again), (input.direction, false));
        assert_eq!(
            unstick.filter(&view(1700, false, false), walk_north()).0,
            walk_north()
        );
    }

    #[test]
    fn in_a_fight_it_runs_the_open_way_and_keeps_aim_and_fire() {
        let mut unstick = Unstick::default();
        unstick.filter(&view(0, true, false), walk_north());
        let (input, give_up) = unstick.filter(&view(600, true, false), walk_north());
        assert!(give_up);
        assert!(input.direction[2] < -0.99);
        assert_eq!(input.buttons, FORWARD | SPRINT | FIRE);
        assert_eq!(input.yaw, 0.3);
        let (running, _) = unstick.filter(&view(1000, true, false), walk_north());
        assert_eq!(running, input);
        assert_eq!(
            unstick.filter(&view(1100, true, false), walk_north()).0,
            walk_north()
        );
    }

    #[test]
    fn pressing_into_an_enemy_standing_or_moving_is_not_blocked() {
        let mut unstick = Unstick::default();
        for ms in [0, 700, 1400] {
            assert!(!unstick.filter(&view(ms, true, true), walk_north()).1);
        }
        let standing = PlayerInput::default();
        for ms in [0, 700, 1400] {
            assert!(!unstick.filter(&view(ms, false, false), standing).1);
        }
        let mut moving = view(0, false, false);
        moving.velocity = Some(Velocity {
            grounded: true,
            linear: Vec3::new(200.0, 0.0, 0.0),
            ..Velocity::default()
        });
        for ms in [0, 700, 1400] {
            moving.now = SimTime::ZERO + Duration::from_millis(ms);
            assert!(!unstick.filter(&moving, walk_north()).1);
        }
    }
}

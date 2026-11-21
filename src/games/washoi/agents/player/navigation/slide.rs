//! Crouch-slides along straight walks, after TypeScript
//! `sequences/RouteCrouchslideSequence.ts` and
//! `BotTraversal.tryStartRouteCrouchslide`. A bot running fast down a plain,
//! straight walk crouches into a slide, holds it for three quarters of its
//! duration, and skilled bots jump out of it to keep their speed. A failed
//! slide doesn't spoil the walk: the bot simply walks on.
//!
//! Deviations: TypeScript also requires the sprint key, which Washoi's route
//! walker here doesn't press, so the speed check stands in for it. Dodge
//! slides belong to fight movement.

use shagger_shared::net::game_state::Velocity;

use crate::games::washoi::hosting::net::protocol::PlayerInput;

const TICK_RATE: f64 = 125.0;
/// `sv_slideMinSpeed`.
const SLIDE_MIN_SPEED: f64 = 375.0;
/// `sv_runSpeed`, the speed a jump cancel needs to be worth it.
const SPRINT_SPEED: f64 = 400.0;
/// `sv_slideDuration`, 800 ms, held for three quarters.
const SLIDE_HOLD_TICKS: u32 = 75;
/// `sv_slideCooldown`, 1.5 s.
const SLIDE_COOLDOWN_TICKS: u32 = (1.5 * TICK_RATE) as u32;
const SLIDE_CONFIRM_TICKS: u32 = 20;
const JUMP_CANCEL_CONFIRM_TICKS: u32 = 16;
/// The longest a slide may run, `RouteCrouchslideSequence.maxTicks`.
const MAX_TICKS: u32 = 140;
/// Skill from which a bot jumps out of a slide.
const JUMP_CANCEL_SKILL: f64 = 0.55;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Engage,
    Slide,
    NaturalExit,
    JumpCancel,
}

#[derive(Clone, Copy)]
struct Running {
    phase: Phase,
    started: u32,
    phase_started: u32,
    confirmed: Option<u32>,
    was_sliding: bool,
    jump_cancel: bool,
    /// The walk's heading when the slide began. A slide barely steers.
    heading: f64,
}

/// One bot's route slide.
#[derive(Default)]
pub(in crate::games::washoi) struct RouteSlide {
    running: Option<Running>,
    cooldown_until: u32,
}

/// What a decision offers the slide.
pub(in crate::games::washoi) struct SlideView {
    /// The authoritative server tick.
    pub(in crate::games::washoi) tick: u32,
    pub(in crate::games::washoi) velocity: Option<Velocity>,
    /// The heading of a plain, straight, walkable lane ahead on the route,
    /// when there is one (`RouteFollower::slide_lane`).
    pub(in crate::games::washoi) lane: Option<f64>,
    /// `strafeJumpSkill`, which decides the jump cancel.
    pub(in crate::games::washoi) strafe_jump_skill: f64,
}

impl RouteSlide {
    #[cfg(test)]
    fn active(&self) -> bool {
        self.running.is_some()
    }

    pub(in crate::games::washoi) fn reset(&mut self) {
        self.running = None;
    }

    /// The route walk's input, turned into a slide when one is under way or
    /// may start now.
    pub(in crate::games::washoi) fn filter(
        &mut self,
        view: &SlideView,
        input: PlayerInput,
    ) -> PlayerInput {
        let velocity = view.velocity.unwrap_or_default();
        let speed = velocity.linear.x.hypot(velocity.linear.z);
        if self.running.is_none() {
            let [x, _, z] = input.direction;
            let asking = x != 0.0 || z != 0.0;
            if view.tick < self.cooldown_until
                || !asking
                || !velocity.grounded
                || input.buttons & (FIRE | ALT_FIRE | BACK) != 0
                || speed < SLIDE_MIN_SPEED
            {
                return input;
            }
            let Some(heading) = view.lane else {
                return input;
            };
            self.cooldown_until = view.tick + SLIDE_COOLDOWN_TICKS;
            self.running = Some(Running {
                phase: Phase::Engage,
                started: view.tick,
                phase_started: view.tick,
                confirmed: None,
                was_sliding: velocity.is_sliding,
                jump_cancel: view.strafe_jump_skill >= JUMP_CANCEL_SKILL,
                heading,
            });
        }
        let Some(mut run) = self.running else {
            return input;
        };
        let (buttons, done) = step(&mut run, view.tick, velocity, speed);
        self.running = (!done).then_some(run);
        let mut slide = input;
        slide.direction = [run.heading.sin(), 0.0, run.heading.cos()];
        slide.buttons = (input.buttons & !(MOVE_KEYS | JUMP | CROUCH | SPRINT)) | buttons;
        slide
    }
}

/// One tick of the slide: the movement keys to press, and whether it ended.
fn step(run: &mut Running, tick: u32, velocity: Velocity, speed: f64) -> (u16, bool) {
    let forward = FORWARD | SPRINT;
    let sliding = forward | CROUCH;
    if tick.saturating_sub(run.started) >= MAX_TICKS {
        return (forward, true);
    }
    if speed < SPRINT_SPEED {
        run.jump_cancel = false;
    }
    if velocity.is_sliding {
        run.was_sliding = true;
        run.confirmed.get_or_insert(tick);
    }
    if run.phase == Phase::Engage && !velocity.is_sliding {
        // The slide never began: walk on.
        if tick.saturating_sub(run.started) >= SLIDE_CONFIRM_TICKS {
            return (forward, true);
        }
        return (sliding, false);
    }
    if run.phase == Phase::JumpCancel {
        if !run.jump_cancel || !velocity.grounded || !velocity.is_sliding {
            return (forward | if run.jump_cancel { JUMP } else { 0 }, true);
        }
        if tick.saturating_sub(run.phase_started) >= JUMP_CANCEL_CONFIRM_TICKS {
            return (forward, true);
        }
        return (forward | JUMP, false);
    }
    if run.was_sliding && !velocity.is_sliding {
        return (forward, true);
    }
    let confirmed = run.confirmed.unwrap_or(tick);
    if tick.saturating_sub(confirmed) >= SLIDE_HOLD_TICKS {
        if run.jump_cancel {
            run.phase = Phase::JumpCancel;
            run.phase_started = tick;
            return (forward | JUMP, false);
        }
        run.phase = Phase::NaturalExit;
        return (forward, false);
    }
    if run.phase == Phase::NaturalExit {
        return (forward, false);
    }
    run.phase = Phase::Slide;
    (sliding, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shagger_shared::net::game_state::Vec3;

    fn moving(speed: f64, grounded: bool, sliding: bool) -> Option<Velocity> {
        Some(Velocity {
            linear: Vec3::new(0.0, 0.0, speed),
            grounded,
            is_sliding: sliding,
            ..Velocity::default()
        })
    }

    fn walk() -> PlayerInput {
        PlayerInput {
            direction: [0.0, 0.0, 1.0],
            yaw: 0.4,
            buttons: FORWARD,
            ..PlayerInput::default()
        }
    }

    fn view(tick: u32, velocity: Option<Velocity>, lane: Option<f64>, skill: f64) -> SlideView {
        SlideView {
            tick,
            velocity,
            lane,
            strafe_jump_skill: skill,
        }
    }

    #[test]
    fn slides_down_a_straight_lane_holds_it_and_jumps_out_when_skilled() {
        let mut slide = RouteSlide::default();
        // Too slow, no lane, or firing: just walk.
        assert_eq!(
            slide.filter(&view(0, moving(300.0, true, false), Some(0.0), 1.0), walk()),
            walk()
        );
        assert_eq!(
            slide.filter(&view(0, moving(400.0, true, false), None, 1.0), walk()),
            walk()
        );
        let firing = PlayerInput {
            buttons: FORWARD | FIRE,
            ..walk()
        };
        assert_eq!(
            slide.filter(&view(0, moving(400.0, true, false), Some(0.0), 1.0), firing),
            firing
        );

        let crouch = slide.filter(
            &view(100, moving(420.0, true, false), Some(0.0), 1.0),
            walk(),
        );
        assert_eq!(crouch.buttons, FORWARD | SPRINT | CROUCH);
        assert_eq!(crouch.yaw, 0.4);
        assert!(slide.active());
        // Confirmed at 104, held 75 ticks, then a jump out while still sliding.
        for tick in 104..179 {
            let input = slide.filter(&view(tick, moving(420.0, true, true), None, 1.0), walk());
            assert_eq!(input.buttons, FORWARD | SPRINT | CROUCH, "tick {tick}");
        }
        let jump = slide.filter(&view(179, moving(420.0, true, true), None, 1.0), walk());
        assert_eq!(jump.buttons, FORWARD | SPRINT | JUMP);
        let off = slide.filter(&view(180, moving(420.0, false, false), None, 1.0), walk());
        assert_eq!(off.buttons, FORWARD | SPRINT | JUMP);
        assert!(!slide.active());
        // Cooling down for 1.5 s.
        assert_eq!(
            slide.filter(
                &view(200, moving(420.0, true, false), Some(0.0), 1.0),
                walk()
            ),
            walk()
        );
    }

    #[test]
    fn a_slide_that_never_starts_walks_on_and_unskilled_bots_stand_up() {
        let mut slide = RouteSlide::default();
        slide.filter(&view(0, moving(420.0, true, false), Some(0.0), 0.2), walk());
        for tick in 1..20 {
            slide.filter(&view(tick, moving(420.0, true, false), None, 0.2), walk());
        }
        let walked = slide.filter(&view(20, moving(420.0, true, false), None, 0.2), walk());
        assert_eq!(walked.buttons, FORWARD | SPRINT);
        assert!(!slide.active());

        let mut slide = RouteSlide::default();
        slide.filter(&view(0, moving(420.0, true, false), Some(0.0), 0.2), walk());
        for tick in 1..76 {
            slide.filter(&view(tick, moving(420.0, true, true), None, 0.2), walk());
        }
        let up = slide.filter(&view(76, moving(420.0, true, true), None, 0.2), walk());
        assert_eq!(up.buttons, FORWARD | SPRINT);
        let ended = slide.filter(&view(77, moving(380.0, true, false), None, 0.2), walk());
        assert_eq!(ended.buttons, FORWARD | SPRINT);
        assert!(!slide.active());
    }
}

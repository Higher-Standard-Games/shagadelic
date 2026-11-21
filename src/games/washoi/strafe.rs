//! How a bot moves while it fights, from TypeScript `bot/BotCombatMovement.ts`:
//! strafe runs of 0.25 to 0.9 s, with skilled bots sometimes stopping dead
//! or doubling back, and a jump or slide dodge when hit. Sidestepping
//! incoming rockets and taking high ground are not ported yet.
//!
//! TypeScript keeps the feet clear of walls and ledges with collision sweeps
//! (`BotVisionTracer.ts`). Here a strafe side is taken only when a static
//! sight ray finds no wall beside the bot and a ray down finds floor there.

use super::{Sight, random::Random};

/// Commands and server ticks per second.
const TICK_RATE: f64 = 125.0;
const MIN_RUN_TICKS: u32 = 31;
const MAX_RUN_TICKS: u32 = 113;
const STEADY_RUN_TICKS: f64 = 75.0;
const STOP_TICKS: u32 = 19;
/// `PHYSICS.SLIDE_MIN_SPEED`, from `FPS.slideMinSpeed`.
const SLIDE_MIN_SPEED: f64 = 200.0;
/// How far beside the bot a strafe side must be clear.
const STRAFE_CLEARANCE: f64 = 48.0;
/// From the origin, the feet are this far down; floor must be within
/// `FLOOR_DEPTH` below them.
const ORIGIN_HEIGHT: f64 = 24.0;
const FLOOR_DEPTH: f64 = 40.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Moves {
    /// -1 left, 0 none, 1 right of the view.
    pub(super) strafe: i8,
    pub(super) jump: bool,
    pub(super) crouch: bool,
}

pub(super) struct CombatMovement {
    side: i8,
    run_until: u32,
    stop_until: u32,
    dodge_until: u32,
    dodge: Moves,
    dodge_jump_sent: bool,
    last_jump: Option<u32>,
}

impl Default for CombatMovement {
    fn default() -> Self {
        Self {
            side: 1,
            run_until: 0,
            stop_until: 0,
            dodge_until: 0,
            dodge: Moves::default(),
            dodge_jump_sent: false,
            last_jump: None,
        }
    }
}

/// What the bot knows of itself this tick.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Body {
    pub(super) hit: bool,
    pub(super) grounded: bool,
    pub(super) speed: f64,
}

impl CombatMovement {
    pub(super) fn reset(&mut self) {
        *self = Self {
            side: self.side,
            ..Self::default()
        };
    }

    /// `step`, at server tick `tick`.
    pub(super) fn step(&mut self, tick: u32, skill: f64, body: Body, random: &mut Random) -> Moves {
        let skill = skill.clamp(0.0, 1.0);
        // `_reactToHit`.
        if body.hit && !self.dodging(tick) && body.grounded && random.next() < skill {
            let fast = body.speed >= SLIDE_MIN_SPEED;
            let slide = fast && skill >= 0.35 && random.next() < 0.35;
            let side = if random.next() < 0.5 { -1 } else { 1 };
            self.start_dodge(tick, side, !slide, slide, if slide { 0.5 } else { 0.3 });
        }
        if self.dodging(tick) {
            let jump = self.dodge.jump
                && !self.dodge_jump_sent
                && body.grounded
                && self
                    .last_jump
                    .is_none_or(|at| tick.wrapping_sub(at) >= TICK_RATE as u32);
            if jump {
                self.dodge_jump_sent = true;
                self.last_jump = Some(tick);
            }
            return Moves { jump, ..self.dodge };
        }
        if tick >= self.run_until {
            self.next_run(tick, skill, random);
        }
        Moves {
            strafe: if tick < self.stop_until { 0 } else { self.side },
            ..Moves::default()
        }
    }

    fn dodging(&self, tick: u32) -> bool {
        tick < self.dodge_until
    }

    /// `_nextRun`: the side, length, and whether the run starts with a stop.
    fn next_run(&mut self, tick: u32, skill: f64, random: &mut Random) {
        let double_back = random.next() < 0.15 * skill;
        let stop = !double_back && random.next() < 0.1 * skill;
        self.side = -self.side;
        let varied =
            f64::from(MIN_RUN_TICKS) + random.next() * f64::from(MAX_RUN_TICKS - MIN_RUN_TICKS);
        let steady = STEADY_RUN_TICKS * (0.85 + random.next() * 0.3);
        let length = if double_back {
            STOP_TICKS
        } else {
            (steady + (varied - steady) * skill).round() as u32
        };
        let pause = if stop { STOP_TICKS } else { 0 };
        self.stop_until = if stop { tick + STOP_TICKS } else { 0 };
        self.run_until = tick + length + pause;
    }

    fn start_dodge(&mut self, tick: u32, side: i8, jump: bool, crouch: bool, seconds: f64) {
        self.dodge_jump_sent = false;
        self.dodge = Moves {
            strafe: side,
            jump,
            crouch,
        };
        self.dodge_until = tick + (seconds * TICK_RATE).round() as u32;
        self.side = side;
        self.run_until = self.dodge_until + MIN_RUN_TICKS;
    }
}

/// The strafe side to take, `wanted` first, if the static geometry has room
/// for it: no wall beside the origin and floor under the spot.
pub(super) fn clear_side(sight: &Sight, origin: [f64; 3], yaw: f64, wanted: i8) -> i8 {
    if wanted == 0 {
        return 0;
    }
    // Right of the view is (cos yaw, -sin yaw).
    let room = |side: i8| {
        let side = f64::from(side) * STRAFE_CLEARANCE;
        let beside = [
            origin[0] + side * yaw.cos(),
            origin[1],
            origin[2] - side * yaw.sin(),
        ];
        let below = [
            beside[0],
            beside[1] - ORIGIN_HEIGHT - FLOOR_DEPTH,
            beside[2],
        ];
        sight.clear(origin, beside) && !sight.clear(beside, below)
    };
    [wanted, -wanted]
        .into_iter()
        .find(|&side| room(side))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::sight::tests::{cube, encode};

    #[test]
    fn strafe_runs_alternate_within_the_typescript_lengths() {
        let mut moves = CombatMovement::default();
        let mut random = Random::new(3);
        let body = Body {
            grounded: true,
            ..Body::default()
        };
        let mut runs = Vec::new();
        let mut last = (0, 0);
        for tick in 0..5_000 {
            let strafe = moves.step(tick, 0.0, body, &mut random).strafe;
            if strafe != last.0 {
                runs.push((last.0, tick - last.1));
                last = (strafe, tick);
            }
        }
        // An unskilled bot never stops, so it alternates at a steady rhythm.
        let runs = &runs[1..];
        assert!(runs.windows(2).all(|pair| pair[0].0 == -pair[1].0));
        assert!(runs.iter().all(|&(_, length)| (63..=87).contains(&length)));
    }

    #[test]
    fn a_hit_sets_off_one_jump_dodge_for_a_skilled_bot() {
        let mut moves = CombatMovement::default();
        let mut random = Random::new(5);
        let walking = Body {
            hit: true,
            grounded: true,
            speed: 100.0,
        };
        let first = moves.step(10, 1.0, walking, &mut random);
        assert!(first.jump && !first.crouch && first.strafe != 0);
        let after = Body {
            hit: false,
            ..walking
        };
        assert!(!moves.step(11, 1.0, after, &mut random).jump);
        // The dodge lasts 0.3 s, then the strafe rhythm resumes.
        assert_eq!(moves.step(48, 1.0, after, &mut random).strafe, first.strafe);
        assert!(!moves.step(60, 1.0, after, &mut random).jump);
        // An unskilled bot never dodges.
        let mut steady = CombatMovement::default();
        assert!(!steady.step(10, 0.0, walking, &mut random).jump);
    }

    #[test]
    fn strafes_only_toward_open_floor() {
        // Floor from x = -100 to 30 under the origin, and a wall at z = -40.
        let sight = Sight::parse(&encode(
            "test",
            &[
                cube([-100.0, -10.0, -100.0], [30.0, 0.0, 100.0]),
                cube([-100.0, 0.0, -50.0], [100.0, 100.0, -40.0]),
            ],
            &[],
        ))
        .unwrap();
        let origin = [0.0, 24.0, 0.0];
        // Facing +z, right is +x and runs off the edge; left has floor.
        let facing = 0.0;
        assert_eq!(clear_side(&sight, origin, facing, 1), -1);
        assert_eq!(clear_side(&sight, origin, facing, -1), -1);
        // Facing +x, right is -z, into the wall; left is open.
        let facing = std::f64::consts::FRAC_PI_2;
        assert_eq!(clear_side(&sight, origin, facing, 1), -1);
        assert_eq!(clear_side(&sight, origin, facing, 0), 0);
        // Near the edge only the inside has floor; out past it, neither side.
        let edge = [28.0, 24.0, 0.0];
        assert_eq!(clear_side(&sight, edge, 0.0, 1), -1);
        assert_eq!(clear_side(&sight, [200.0, 24.0, 0.0], 0.0, 1), 0);
    }
}

//! How a bot moves while it fights, from TypeScript `bot/BotCombatMovement.ts`:
//! strafe runs of 0.25 to 0.9 s, with skilled bots sometimes stopping dead
//! or doubling back, a jump or slide dodge when hit, and a sidestep from a
//! rocket or grenade on course for the bot once it has had time to notice
//! it. Taking high ground is not ported yet.
//!
//! Departing from TypeScript, which jumps in a fight only to dodge, some
//! long strafe runs carry one hop a moment after the run starts, as a human
//! jump-strafes, and a run long enough for it takes a second hop once the
//! one-a-second limit allows. In the user's Duel demo the human, within 900 units of the
//! enemy, pressed jump 27.9 times a minute and was airborne 44% of the time
//! while moving; the Rust bot pressed 6.8 times and was airborne 32%. The
//! caller sends the hop only when the floor under the landing is walkable.
//!
//! TypeScript keeps the feet clear of walls and ledges with collision sweeps
//! (`BotVisionTracer.ts`). Here a strafe side is taken only when a static
//! sight ray finds no wall beside the bot and a ray down finds floor there.

use crate::games::washoi::{Sight, random::Random};

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
/// A projectile first seen this close to the bot is its own.
/// A run this long or longer may carry a hop, this many ticks in, once
/// the turn back has some speed behind it, for `HOP_CHANCE` times the
/// dodge skill. A hop not taken by `HOP_WINDOW` ticks later is dropped, and
/// another comes a second after one when the run has `HOP_MIN_RUN_TICKS`
/// left past it. A run that starts with a stop or doubles back carries none.
const HOP_MIN_RUN_TICKS: u32 = 45;
const HOP_DELAY_TICKS: u32 = 20;
const HOP_WINDOW_TICKS: u32 = 20;
const HOP_CHANCE: f64 = 1.2;
const OWN_PROJECTILE_RADIUS: f64 = 120.0;
/// `isIncoming` in `BotGameState.ts`: a path passing this close within this
/// many seconds is a threat.
const THREAT_RADIUS: f64 = 150.0;
const THREAT_SECONDS: f64 = 1.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::games::washoi) struct Moves {
    /// -1 left, 0 none, 1 right of the view.
    pub(in crate::games::washoi) strafe: i8,
    pub(in crate::games::washoi) jump: bool,
    pub(in crate::games::washoi) crouch: bool,
    /// The jump is a strafe hop, not a dodge.
    pub(in crate::games::washoi) hop: bool,
}

pub(in crate::games::washoi) struct CombatMovement {
    side: i8,
    run_until: u32,
    stop_until: u32,
    dodge_until: u32,
    dodge: Moves,
    dodge_jump_sent: bool,
    last_jump: Option<u32>,
    /// The last strafe hop. Hops wait a second after any jump, but a dodge
    /// waits only on dodges, so a hop never holds one back.
    last_hop: Option<u32>,
    /// When this run's hop is due.
    hop_at: Option<u32>,
    /// Projectiles by entity: the tick first seen, whether the bot's own,
    /// and whether already dodged.
    projectiles: Vec<Tracked>,
}

struct Tracked {
    id: String,
    first: u32,
    own: bool,
    handled: bool,
}

/// A projectile in the bot's sight.
pub(in crate::games::washoi) struct Projectile<'a> {
    pub(in crate::games::washoi) id: &'a str,
    pub(in crate::games::washoi) position: [f64; 3],
    pub(in crate::games::washoi) velocity: [f64; 3],
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
            last_hop: None,
            hop_at: None,
            projectiles: Vec::new(),
        }
    }
}

/// What the bot knows of itself this tick.
#[derive(Clone, Copy, Debug, Default)]
pub(in crate::games::washoi) struct Body {
    pub(in crate::games::washoi) hit: bool,
    pub(in crate::games::washoi) grounded: bool,
    pub(in crate::games::washoi) speed: f64,
    pub(in crate::games::washoi) position: [f64; 3],
    pub(in crate::games::washoi) yaw: f64,
}

/// `dodgeSkill` and `reactionTimeMs`.
#[derive(Clone, Copy, Debug)]
pub(in crate::games::washoi) struct Skills {
    pub(in crate::games::washoi) dodge: f64,
    pub(in crate::games::washoi) reaction_ms: f64,
}

impl CombatMovement {
    pub(in crate::games::washoi) fn reset(&mut self) {
        *self = Self {
            side: self.side,
            ..Self::default()
        };
    }

    /// `step`, at server tick `tick`.
    pub(in crate::games::washoi) fn step(
        &mut self,
        tick: u32,
        skills: Skills,
        body: Body,
        projectiles: &[Projectile<'_>],
        random: &mut Random,
    ) -> Moves {
        let skill = skills.dodge.clamp(0.0, 1.0);
        self.track(tick, body.position, projectiles);
        self.react_to_projectiles(tick, skills, body, projectiles, random);
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
        let hop = match self.hop_at {
            Some(at) if tick > at + HOP_WINDOW_TICKS => {
                self.hop_at = None;
                false
            }
            Some(at) => {
                tick >= at
                    && body.grounded
                    && [self.last_jump, self.last_hop]
                        .into_iter()
                        .flatten()
                        .all(|last| tick.wrapping_sub(last) >= TICK_RATE as u32)
            }
            None => false,
        };
        if hop {
            let again = tick + TICK_RATE as u32;
            self.hop_at = (self.run_until >= again + HOP_MIN_RUN_TICKS).then_some(again);
            self.last_hop = Some(tick);
        }
        Moves {
            strafe: if tick < self.stop_until { 0 } else { self.side },
            jump: hop,
            hop,
            ..Moves::default()
        }
    }

    /// `_trackProjectiles`.
    fn track(&mut self, tick: u32, position: [f64; 3], projectiles: &[Projectile<'_>]) {
        self.projectiles.retain(|tracked| {
            projectiles
                .iter()
                .any(|projectile| projectile.id == tracked.id)
        });
        for projectile in projectiles {
            if !self
                .projectiles
                .iter()
                .any(|tracked| tracked.id == projectile.id)
            {
                self.projectiles.push(Tracked {
                    id: projectile.id.to_owned(),
                    first: tick,
                    own: distance(projectile.position, position) <= OWN_PROJECTILE_RADIUS,
                    handled: false,
                });
            }
        }
    }

    /// `_reactToRocket`: step off the path of the first noticed threat.
    fn react_to_projectiles(
        &mut self,
        tick: u32,
        skills: Skills,
        body: Body,
        projectiles: &[Projectile<'_>],
        random: &mut Random,
    ) {
        if self.dodging(tick) {
            return;
        }
        let notice = (skills.reaction_ms / 1000.0 * TICK_RATE).round() as u32;
        let skill = skills.dodge.clamp(0.0, 1.0);
        for projectile in projectiles {
            let Some(tracked) = self
                .projectiles
                .iter_mut()
                .find(|tracked| tracked.id == projectile.id)
            else {
                continue;
            };
            if tracked.own
                || tracked.handled
                || !incoming(body.position, projectile.position, projectile.velocity)
                || tick.wrapping_sub(tracked.first) < notice
            {
                continue;
            }
            tracked.handled = true;
            if random.next() >= skill {
                continue;
            }
            let side = away_side(body.position, body.yaw, projectile);
            let jump = body.grounded && random.next() < skill;
            self.start_dodge(tick, side, jump, false, 0.4);
            return;
        }
    }

    pub(in crate::games::washoi) fn dodging(&self, tick: u32) -> bool {
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
        self.hop_at = (!double_back
            && !stop
            && length >= HOP_MIN_RUN_TICKS
            && random.next() < HOP_CHANCE * skill)
            .then_some(tick + HOP_DELAY_TICKS);
    }

    fn start_dodge(&mut self, tick: u32, side: i8, jump: bool, crouch: bool, seconds: f64) {
        self.dodge_jump_sent = false;
        self.dodge = Moves {
            strafe: side,
            jump,
            crouch,
            hop: false,
        };
        self.hop_at = None;
        self.dodge_until = tick + (seconds * TICK_RATE).round() as u32;
        self.side = side;
        self.run_until = self.dodge_until + MIN_RUN_TICKS;
    }
}

/// `isIncoming`.
fn incoming(target: [f64; 3], position: [f64; 3], velocity: [f64; 3]) -> bool {
    let speed = velocity.iter().map(|v| v * v).sum::<f64>();
    if speed < 1.0 {
        return false;
    }
    let to = [0, 1, 2].map(|axis| target[axis] - position[axis]);
    let t = (0..3).map(|axis| to[axis] * velocity[axis]).sum::<f64>() / speed;
    if t <= 0.0 || t > THREAT_SECONDS {
        return false;
    }
    let closest = [0, 1, 2].map(|axis| position[axis] + velocity[axis] * t);
    distance(closest, target) <= THREAT_RADIUS
}

/// `awaySide`: which strafe side steps off the projectile's path.
fn away_side(position: [f64; 3], yaw: f64, projectile: &Projectile<'_>) -> i8 {
    let [vx, _, vz] = projectile.velocity;
    let (ox, oz) = (
        position[0] - projectile.position[0],
        position[2] - projectile.position[2],
    );
    let sign = if vx * oz - vz * ox >= 0.0 { 1.0 } else { -1.0 };
    let length = vx.hypot(vz);
    let length = if length == 0.0 { 1.0 } else { length };
    let (away_x, away_z) = (-vz / length * sign, vx / length * sign);
    if away_x * yaw.cos() - away_z * yaw.sin() >= 0.0 {
        1
    } else {
        -1
    }
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3)
        .map(|axis| (a[axis] - b[axis]).powi(2))
        .sum::<f64>()
        .sqrt()
}

/// The strafe side to take, `wanted` first, if the static geometry has room
/// for it: no wall beside the origin and floor under the spot.
pub(in crate::games::washoi) fn clear_side(
    sight: &Sight,
    origin: [f64; 3],
    yaw: f64,
    wanted: i8,
) -> i8 {
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
    use crate::games::washoi::perception::sight::tests::{cube, encode};

    fn skills(dodge: f64) -> Skills {
        Skills {
            dodge,
            reaction_ms: 280.0,
        }
    }

    #[test]
    fn sidesteps_a_noticed_rocket_away_from_its_path() {
        let mut moves = CombatMovement::default();
        let mut random = Random::new(9);
        // A rocket flies down -x along z = -40, so it passes 40 units
        // from the bot.
        let body = Body {
            grounded: true,
            position: [0.0, 24.0, 0.0],
            ..Body::default()
        };
        let rocket = |x: f64| Projectile {
            id: "proj_1",
            position: [x, 24.0, -40.0],
            velocity: [-900.0, 0.0, 0.0],
        };
        // Not before the 280 ms reaction: 35 ticks.
        for tick in 0..35 {
            let at = 800.0 - 900.0 * f64::from(tick) / TICK_RATE;
            let step = moves.step(tick, skills(1.0), body, &[rocket(at)], &mut random);
            assert!(!step.jump || step.hop);
        }
        let dodge = moves.step(35, skills(1.0), body, &[rocket(548.0)], &mut random);
        // A skilled bot jumps it and strafes to the side away from its path.
        assert!(dodge.jump);
        assert_eq!(dodge.strafe, away_side(body.position, 0.0, &rocket(548.0)));
        // Its own rocket, first seen beside it, is never dodged.
        let mut own = CombatMovement::default();
        let mine = Projectile {
            id: "proj_2",
            position: [0.0, 24.0, 50.0],
            velocity: [0.0, 0.0, 900.0],
        };
        for tick in 0..100 {
            let step = own.step(tick, skills(1.0), body, &[mine.clone_at(tick)], &mut random);
            assert!(!step.jump || step.hop);
        }
        assert!(incoming([0.0; 3], [500.0, 0.0, 0.0], [-900.0, 0.0, 0.0]));
        assert!(!incoming([0.0; 3], [500.0, 0.0, 0.0], [900.0, 0.0, 0.0]));
        assert!(!incoming([0.0; 3], [500.0, 0.0, 400.0], [-900.0, 0.0, 0.0]));
    }

    impl Projectile<'_> {
        fn clone_at(&self, tick: u32) -> Self {
            let t = f64::from(tick) / TICK_RATE;
            Projectile {
                id: self.id,
                position: [0, 1, 2].map(|axis| self.position[axis] + self.velocity[axis] * t),
                velocity: self.velocity,
            }
        }
    }

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
            let strafe = moves.step(tick, skills(0.0), body, &[], &mut random).strafe;
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
            ..Body::default()
        };
        let first = moves.step(10, skills(1.0), walking, &[], &mut random);
        assert!(first.jump && !first.crouch && first.strafe != 0);
        let after = Body {
            hit: false,
            ..walking
        };
        assert!(!moves.step(11, skills(1.0), after, &[], &mut random).jump);
        // The dodge lasts 0.3 s, then the strafe rhythm resumes.
        assert_eq!(
            moves.step(48, skills(1.0), after, &[], &mut random).strafe,
            first.strafe
        );
        assert!(!moves.step(60, skills(1.0), after, &[], &mut random).jump);
        // An unskilled bot never dodges.
        let mut steady = CombatMovement::default();
        assert!(!steady.step(10, skills(0.0), walking, &[], &mut random).jump);
    }

    #[test]
    fn a_skilled_bot_hops_some_long_runs_and_an_unskilled_one_never() {
        let body = Body {
            grounded: true,
            speed: 300.0,
            ..Body::default()
        };
        let hops = |skill: f64| {
            let mut moves = CombatMovement::default();
            let mut random = Random::new(11);
            let mut hops = Vec::new();
            let mut run_start = 0;
            let mut side = 0;
            for tick in 0..12_500 {
                let step = moves.step(tick, skills(skill), body, &[], &mut random);
                if step.strafe != side {
                    side = step.strafe;
                    run_start = tick;
                }
                if step.jump {
                    assert!(step.hop && step.strafe != 0);
                    hops.push(tick - run_start);
                }
            }
            hops
        };
        assert!(hops(0.0).is_empty());
        let skilled = hops(0.9);
        // A hop comes 20 ticks into a run, later when the last jump was
        // under a second ago, or a second after the run's first; over 100 s,
        // close to one a run.
        assert!(
            skilled
                .iter()
                .all(|&into| (20..=40).contains(&into) || (145..=165).contains(&into))
        );
        assert!((60..=170).contains(&skilled.len()), "{}", skilled.len());
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

//! Fighting from the edge of cover, from TypeScript `bot/BotCoverPeek.ts`.
//! At range a good player stands by a pillar or a doorframe, steps out to
//! shoot, and steps back while the weapon reloads.
//!
//! Every half second of a ranged fight, at the bot's skill, it looks either
//! side of itself, square to the enemy, for the nearest spot the enemy can't
//! see and that it can walk straight to. With one, it ducks there after each
//! shot of a slow weapon and steps out just before the next is ready. With a
//! fast weapon it ducks between bursts. Without one it fights in the open.

use super::random::Random;

/// Server ticks per second.
const TICK_RATE: f64 = 125.0;
/// How often the sides are looked at again: 0.5 s.
const COVER_CHECK_TICKS: u32 = 63;
/// How far to the side a hidden spot is looked for, in units.
const COVER_OFFSETS: [f64; 4] = [64.0, 112.0, 160.0, 208.0];
/// Strafing speed the step timing assumes.
const STEP_SPEED: f64 = 320.0;
/// A weapon this slow or slower is peeked shot by shot: 0.5 s.
const SLOW_REFIRE_TICKS: u32 = 63;
/// Bursts of a fast weapon, and the ducks between them.
const BURST_TICKS: (u32, u32) = (75, 125);
const DUCK_TICKS: (u32, u32) = (44, 63);
/// Out of sight in cover, the fight is held this long, or a reload and a
/// step, before it counts as lost.
const HOLD_TICKS: u32 = 125;
/// A peek this recent still counts as using cover. TypeScript allows two
/// ticks, as it decides every server tick; the Rust brain decides per
/// snapshot and may skip a few.
const STEP_FRESH_TICKS: u32 = 16;

/// One tick of a ranged fight, for `step`.
pub(super) struct Query<'a> {
    pub(super) tick: u32,
    pub(super) position: [f64; 3],
    /// Yaw toward the enemy.
    pub(super) yaw: f64,
    /// Whether a spot is out of the enemy's sight.
    pub(super) hidden: &'a dyn Fn([f64; 3]) -> bool,
    /// Whether the bot can walk straight from where it is to there.
    pub(super) walkable: &'a dyn Fn([f64; 3]) -> bool,
    /// Ticks between shots of the weapon out.
    pub(super) refire_ticks: u32,
    /// When the bot last fired it.
    pub(super) fired_tick: Option<u32>,
    /// Whether the enemy is in sight now.
    pub(super) visible: bool,
}

pub(super) struct CoverPeek {
    side: i8,
    offset: f64,
    check_tick: Option<u32>,
    cycle_start: u32,
    burst: u32,
    duck: u32,
    last_step: Option<u32>,
    last_visible: Option<u32>,
    hold_ticks: u32,
}

impl Default for CoverPeek {
    fn default() -> Self {
        Self {
            side: 0,
            offset: 0.0,
            check_tick: None,
            cycle_start: 0,
            burst: 0,
            duck: 0,
            last_step: None,
            last_visible: None,
            hold_ticks: HOLD_TICKS,
        }
    }
}

impl CoverPeek {
    pub(super) fn reset(&mut self) {
        self.side = 0;
        self.check_tick = None;
        self.last_step = None;
    }

    /// While the bot uses cover and saw the enemy lately: a duck out of
    /// sight is still the same fight.
    pub(super) fn holding(&self, tick: u32) -> bool {
        self.side != 0
            && self
                .last_step
                .is_some_and(|at| tick.wrapping_sub(at) <= STEP_FRESH_TICKS)
            && self
                .last_visible
                .is_some_and(|at| tick.wrapping_sub(at) <= self.hold_ticks)
    }

    /// The strafe that peeks from cover (−1 left, 1 right, 0 stand), or none
    /// to fight in the open.
    pub(super) fn step(&mut self, q: &Query<'_>, skill: f64, random: &mut Random) -> Option<i8> {
        if q.visible {
            self.last_visible = Some(q.tick);
        }
        if self.check_tick.is_none_or(|at| q.tick >= at) {
            self.check_tick = Some(q.tick + COVER_CHECK_TICKS);
            let had = self.side;
            self.side = 0;
            // Looked for from the open only: from cover every side looks hidden.
            if q.visible && random.next() < skill {
                self.find(q, random);
            } else if !q.visible && had != 0 {
                self.side = had;
            }
            if self.side != 0 && had == 0 {
                self.new_cycle(q.tick, random);
            }
        }
        if self.side == 0 {
            return None;
        }
        self.last_step = Some(q.tick);
        let step = ((self.offset / STEP_SPEED * TICK_RATE).round() as u32).clamp(15, 63);
        self.hold_ticks = HOLD_TICKS.max(q.refire_ticks + step);
        let side = self.side;
        if q.refire_ticks >= SLOW_REFIRE_TICKS {
            if q.refire_ticks < 2 * step + 10 {
                return None;
            }
            let since = q.fired_tick.map_or(u32::MAX, |at| q.tick.wrapping_sub(at));
            if since < step {
                return Some(side);
            }
            if since < q.refire_ticks - step {
                return Some(0);
            }
            // Out again. In sight, fight as in the open until the shot is gone.
            return if q.visible { None } else { Some(-side) };
        }
        // A fast weapon: a burst in sight, then a duck into cover and back.
        let at = q.tick.wrapping_sub(self.cycle_start);
        if at < self.burst {
            return if q.visible { None } else { Some(-side) };
        }
        if at < self.burst + step {
            return Some(side);
        }
        if at < self.burst + self.duck {
            return Some(0);
        }
        if at < self.burst + self.duck + step {
            return Some(-side);
        }
        self.new_cycle(q.tick, random);
        None
    }

    fn new_cycle(&mut self, tick: u32, random: &mut Random) {
        self.cycle_start = tick;
        let between = |(low, high): (u32, u32), draw: f64| {
            (f64::from(low) + draw * f64::from(high - low)).round() as u32
        };
        self.burst = between(BURST_TICKS, random.next());
        self.duck = between(DUCK_TICKS, random.next());
    }

    /// The nearer side with a hidden spot, and how far it is.
    fn find(&mut self, q: &Query<'_>, random: &mut Random) {
        // Right of the view is (cos yaw, −sin yaw).
        let (rx, rz) = (q.yaw.cos(), -q.yaw.sin());
        let first: i8 = if random.next() < 0.5 { 1 } else { -1 };
        for offset in COVER_OFFSETS {
            for side in [first, -first] {
                let d = f64::from(side) * offset;
                let at = [
                    q.position[0] + rx * d,
                    q.position[1],
                    q.position[2] + rz * d,
                ];
                if (q.hidden)(at) && (q.walkable)(at) {
                    self.side = side;
                    self.offset = offset;
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pillar hides everything 100 units or more to the bot's right (+x
    /// when facing +z).
    fn query<'a>(
        tick: u32,
        refire: u32,
        fired: Option<u32>,
        visible: bool,
        hidden: &'a dyn Fn([f64; 3]) -> bool,
        walkable: &'a dyn Fn([f64; 3]) -> bool,
    ) -> Query<'a> {
        Query {
            tick,
            position: [0.0; 3],
            yaw: 0.0,
            hidden,
            walkable,
            refire_ticks: refire,
            fired_tick: fired,
            visible,
        }
    }

    #[test]
    fn ducks_behind_the_nearest_hidden_spot_between_slow_shots() {
        let right = |at: [f64; 3]| at[0] >= 100.0;
        let open = |_: [f64; 3]| true;
        let mut cover = CoverPeek::default();
        let mut random = Random::new(1);
        // A bolt, 156 ticks between shots. Fired at tick 0: duck right, then
        // stand in cover, then step back out before the next shot.
        let first = cover.step(
            &query(0, 156, Some(0), true, &right, &open),
            1.0,
            &mut random,
        );
        assert_eq!(first, Some(1));
        // The nearest hidden spot is 112 units away: a 44-tick step.
        assert_eq!(cover.offset, 112.0);
        assert_eq!(
            cover.step(
                &query(43, 156, Some(0), false, &right, &open),
                1.0,
                &mut random
            ),
            Some(1)
        );
        assert_eq!(
            cover.step(
                &query(60, 156, Some(0), false, &right, &open),
                1.0,
                &mut random
            ),
            Some(0)
        );
        assert!(cover.holding(60));
        assert_eq!(
            cover.step(
                &query(120, 156, Some(0), false, &right, &open),
                1.0,
                &mut random
            ),
            Some(-1)
        );
        // Back in sight, the shot is taken as in the open.
        assert_eq!(
            cover.step(
                &query(121, 156, Some(0), true, &right, &open),
                1.0,
                &mut random
            ),
            None
        );
        // No hidden spot to walk to: fight in the open.
        let mut exposed = CoverPeek::default();
        let walls = |_: [f64; 3]| false;
        assert_eq!(
            exposed.step(
                &query(0, 156, Some(0), true, &right, &walls),
                1.0,
                &mut random
            ),
            None
        );
        assert!(!exposed.holding(0));
    }

    #[test]
    fn ducks_between_bursts_of_a_fast_weapon_and_never_without_skill() {
        let left = |at: [f64; 3]| at[0] <= -64.0;
        let open = |_: [f64; 3]| true;
        let mut cover = CoverPeek::default();
        let mut random = Random::new(2);
        // A machinegun, 13 ticks between shots: a burst, then a duck left.
        assert_eq!(
            cover.step(&query(0, 13, None, true, &left, &open), 1.0, &mut random),
            None
        );
        assert_eq!(cover.side, -1);
        let burst = cover.burst;
        assert!((75..=125).contains(&burst));
        assert_eq!(
            cover.step(
                &query(burst, 13, None, true, &left, &open),
                1.0,
                &mut random
            ),
            Some(-1)
        );
        let mut unskilled = CoverPeek::default();
        assert_eq!(
            unskilled.step(&query(0, 13, None, true, &left, &open), 0.0, &mut random),
            None
        );
        assert_eq!(unskilled.side, 0);
    }
}

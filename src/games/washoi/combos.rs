//! The weapon a good player switches to before the distance says so, from
//! TypeScript `bot/BotWeaponCombos.ts`:
//!
//! - A rocket that throws the enemy up is followed by the electrobolt at
//!   range or the riotgun close, while they hang in the air. A rocket hit at
//!   arm's length is followed by the blade.
//! - Long plasma pressure at range is broken by a rail shot now and then.
//! - A slow gun just fired is swapped for another gun carried while it
//!   reloads, when that one fits the distance and can shoot well before the
//!   slow one could again. The server keeps one refire clock across a switch.
//!
//! Each chance is rolled once, at `predictionSkill` odds. Finishing an enemy
//! with the gun whose one hit kills needs TypeScript's `MatchRead` health
//! estimate and is not ported.

use super::{random::Random, weapons};

/// Server ticks per second.
#[cfg(test)]
const TICK_RATE: f64 = 125.0;
/// A hit this recent still sets up a combo: 0.3 s.
const HIT_FRESH_TICKS: u32 = 38;
/// Rising this fast after a rocket hit counts as thrown up.
const LAUNCH_SPEED: f64 = 150.0;
const BLADE_REACH: f64 = 150.0;
const BOLT_MIN_RANGE: f64 = 400.0;
/// Plasma held this long at rail range invites a rail shot: 1.5 s.
const PLASMA_SPELL_TICKS: u32 = 188;
const RAIL_RANGE: f64 = 500.0;
/// A swap after a shot must let the other gun fire this much sooner: 0.25 s.
const REFIRE_MIN_GAIN_TICKS: i64 = 31;
/// From the decision to swap until the server has the other gun out.
const SWAP_TICKS: i64 = 4;
const SWAP_RANGE_SLACK: f64 = 0.25;
const LAUNCH_HOLD: u32 = 100;
const BLADE_HOLD: u32 = 75;
const RAIL_HOLD: u32 = 88;

/// What the bot knows this tick, for choosing a combo.
pub(super) struct Situation<'a> {
    pub(super) tick: u32,
    pub(super) target: &'a str,
    pub(super) distance: f64,
    pub(super) target_rising: f64,
    pub(super) current: &'a str,
    /// The weapon the distance picked.
    pub(super) picked: &'a str,
    /// Whether the bot carries a weapon with ammo for it.
    pub(super) has: &'a dyn Fn(&str) -> bool,
    pub(super) weapons: &'a [String],
}

#[derive(Default)]
pub(super) struct Combos {
    held: Option<Held>,
    last_hit: Option<u32>,
    last_shot: Option<u32>,
    plasma_since: Option<u32>,
    /// The bot's own latest hit (weapon, victim, tick) and shot (weapon, tick).
    hit: Option<(String, String, u32)>,
    shot: Option<(String, u32)>,
}

struct Held {
    weapon: String,
    target: String,
    until: u32,
}

impl Combos {
    pub(super) fn reset(&mut self) {
        self.held = None;
        self.plasma_since = None;
    }

    pub(super) fn hit(&mut self, weapon: &str, victim: &str, tick: u32) {
        self.hit = Some((weapon.to_owned(), victim.to_owned(), tick));
    }

    pub(super) fn shot(&mut self, weapon: &str, tick: u32) {
        self.shot = Some((weapon.to_owned(), tick));
    }

    /// `choose`: the weapon to hold in place of the distance pick, if any.
    pub(super) fn choose(
        &mut self,
        now: &Situation<'_>,
        skill: f64,
        random: &mut Random,
    ) -> Option<String> {
        if self.held.as_ref().is_some_and(|held| {
            now.tick >= held.until || held.target != now.target || !(now.has)(&held.weapon)
        }) {
            self.held = None;
        }
        if self.held.is_none() {
            self.rocket_combo(now, skill, random);
        }
        if self.held.is_none() {
            self.rail_break(now, skill, random);
        }
        if self.held.is_none() {
            self.refire_swap(now, skill, random);
        }
        self.held.as_ref().map(|held| held.weapon.clone())
    }

    fn hold(&mut self, now: &Situation<'_>, weapon: &str, ticks: u32) {
        self.held = Some(Held {
            weapon: weapon.to_owned(),
            target: now.target.to_owned(),
            until: now.tick + ticks,
        });
    }

    /// `_rocketCombo`, rolled once per hit.
    fn rocket_combo(&mut self, now: &Situation<'_>, skill: f64, random: &mut Random) {
        let Some((weapon, victim, tick)) = self.hit.clone() else {
            return;
        };
        if self.last_hit.is_some_and(|last| tick <= last) {
            return;
        }
        self.last_hit = Some(tick);
        if weapon != "rlauncher"
            || victim != now.target
            || now.tick.wrapping_sub(tick) > HIT_FRESH_TICKS
        {
            return;
        }
        let follow = if now.distance < BLADE_REACH && (now.has)("blade") {
            Some(("blade", BLADE_HOLD))
        } else if now.target_rising > LAUNCH_SPEED {
            let gun = if now.distance >= BOLT_MIN_RANGE {
                "electrobolt"
            } else {
                "riotgun"
            };
            (now.has)(gun).then_some((gun, LAUNCH_HOLD))
        } else {
            None
        };
        if let Some((weapon, ticks)) = follow
            && random.next() < skill
        {
            self.hold(now, weapon, ticks);
        }
    }

    /// `_railBreak`.
    fn rail_break(&mut self, now: &Situation<'_>, skill: f64, random: &mut Random) {
        let plasma = now.picked == "plasmagun" && now.current == "plasmagun";
        if !plasma || now.distance < RAIL_RANGE {
            self.plasma_since = None;
            return;
        }
        let since = *self.plasma_since.get_or_insert(now.tick);
        // A start in the future, after a break, has not begun yet.
        if now.tick < since || now.tick - since < PLASMA_SPELL_TICKS {
            return;
        }
        self.plasma_since = Some(now.tick + RAIL_HOLD);
        if (now.has)("electrobolt") && random.next() < skill {
            self.hold(now, "electrobolt", RAIL_HOLD);
        }
    }

    /// `_refireSwap`, rolled once per shot of the gun in hand.
    fn refire_swap(&mut self, now: &Situation<'_>, skill: f64, random: &mut Random) {
        let Some((fired, tick)) = self.shot.clone() else {
            return;
        };
        if self.last_shot.is_some_and(|last| tick <= last) {
            return;
        }
        self.last_shot = Some(tick);
        if fired != now.current {
            return;
        }
        let ready_again =
            i64::from(tick) + i64::from(weapons::profile_of(&fired).fire_interval_ticks);
        let out = i64::from(now.tick) + SWAP_TICKS;
        let best = now
            .weapons
            .iter()
            .filter(|weapon| **weapon != fired && (now.has)(weapon))
            .map(|weapon| (weapon, weapons::profile_of(weapon)))
            .filter(|(_, profile)| {
                let fits = if profile.melee {
                    now.distance < BLADE_REACH
                } else {
                    profile.fits(now.distance, SWAP_RANGE_SLACK)
                };
                let ready = (i64::from(tick) + i64::from(profile.fire_interval_ticks)).max(out);
                fits && ready_again - ready >= REFIRE_MIN_GAIN_TICKS
            })
            .max_by(|a, b| a.1.dps().total_cmp(&b.1.dps()));
        if let Some((weapon, _)) = best
            && random.next() < skill
        {
            let ticks = u32::try_from(ready_again - i64::from(now.tick)).unwrap_or(0);
            self.hold(now, &weapon.clone(), ticks);
        }
    }
}

/// Seconds as server ticks, for tests.
#[cfg(test)]
fn ticks(seconds: f64) -> u32 {
    (seconds * TICK_RATE).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn situation<'a>(
        tick: u32,
        distance: f64,
        rising: f64,
        current: &'a str,
        picked: &'a str,
        has: &'a dyn Fn(&str) -> bool,
        weapons: &'a [String],
    ) -> Situation<'a> {
        Situation {
            tick,
            target: "foe",
            distance,
            target_rising: rising,
            current,
            picked,
            has,
            weapons,
        }
    }

    #[test]
    fn follows_a_launching_rocket_with_the_right_gun_and_lets_it_go() {
        let weapons: Vec<String> = ["rlauncher", "riotgun", "electrobolt", "blade"]
            .map(String::from)
            .into();
        let all = |_: &str| true;
        let mut random = Random::new(1);
        let mut combos = Combos::default();
        combos.hit("rlauncher", "foe", 100);
        // Thrown up at mid range: the riotgun, held for 0.8 s.
        let now = situation(105, 300.0, 300.0, "rlauncher", "rlauncher", &all, &weapons);
        assert_eq!(
            combos.choose(&now, 1.0, &mut random).as_deref(),
            Some("riotgun")
        );
        let later = situation(205, 300.0, 0.0, "riotgun", "rlauncher", &all, &weapons);
        assert_eq!(combos.choose(&later, 1.0, &mut random), None);
        // Far off, the electrobolt; up close, the blade.
        let mut far = Combos::default();
        far.hit("rlauncher", "foe", 100);
        let now = situation(101, 800.0, 300.0, "rlauncher", "rlauncher", &all, &weapons);
        assert_eq!(
            far.choose(&now, 1.0, &mut random).as_deref(),
            Some("electrobolt")
        );
        let mut close = Combos::default();
        close.hit("rlauncher", "foe", 100);
        let now = situation(101, 100.0, 0.0, "rlauncher", "rlauncher", &all, &weapons);
        assert_eq!(
            close.choose(&now, 1.0, &mut random).as_deref(),
            Some("blade")
        );
        // A stale hit, or no skill, sets up nothing.
        let mut stale = Combos::default();
        stale.hit("rlauncher", "foe", 100);
        let now = situation(
            100 + ticks(0.5),
            300.0,
            300.0,
            "rlauncher",
            "rlauncher",
            &all,
            &weapons,
        );
        assert_eq!(stale.choose(&now, 1.0, &mut random), None);
        let mut unskilled = Combos::default();
        unskilled.hit("rlauncher", "foe", 100);
        let now = situation(105, 300.0, 300.0, "rlauncher", "rlauncher", &all, &weapons);
        assert_eq!(unskilled.choose(&now, 0.0, &mut random), None);
    }

    #[test]
    fn breaks_long_plasma_with_a_rail_and_swaps_a_slow_gun_while_it_reloads() {
        let weapons: Vec<String> = ["plasmagun", "electrobolt", "machinegun"]
            .map(String::from)
            .into();
        let all = |_: &str| true;
        let mut random = Random::new(2);
        let mut combos = Combos::default();
        for tick in 0..PLASMA_SPELL_TICKS {
            let now = situation(tick, 800.0, 0.0, "plasmagun", "plasmagun", &all, &weapons);
            assert_eq!(combos.choose(&now, 1.0, &mut random), None);
        }
        let now = situation(
            PLASMA_SPELL_TICKS,
            800.0,
            0.0,
            "plasmagun",
            "plasmagun",
            &all,
            &weapons,
        );
        assert_eq!(
            combos.choose(&now, 1.0, &mut random).as_deref(),
            Some("electrobolt")
        );

        // A bolt just fired takes 156 ticks to come back; the machinegun
        // fits 800 units and shoots after 13, so it fills the gap.
        let pair: Vec<String> = ["electrobolt", "machinegun"].map(String::from).into();
        let weapons = &pair[..];
        let mut swap = Combos::default();
        swap.shot("electrobolt", 500);
        let now = situation(501, 800.0, 0.0, "electrobolt", "electrobolt", &all, weapons);
        assert_eq!(
            swap.choose(&now, 1.0, &mut random).as_deref(),
            Some("machinegun")
        );
        // Rolled once per shot: it lasts until the bolt is ready again.
        let now = situation(600, 800.0, 0.0, "machinegun", "electrobolt", &all, weapons);
        assert_eq!(
            swap.choose(&now, 1.0, &mut random).as_deref(),
            Some("machinegun")
        );
        let now = situation(656, 800.0, 0.0, "machinegun", "electrobolt", &all, weapons);
        assert_eq!(swap.choose(&now, 1.0, &mut random), None);
    }
}

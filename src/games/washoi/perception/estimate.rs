//! What a bot believes about each enemy's health and armor, from TypeScript
//! `strategy/MatchRead.ts`. An enemy first seen alive, or seen again after it
//! died, has spawn values. Each hit the bot lands takes armor's share by the
//! server's absorption rule, and the pain sound the hit gives away clamps
//! the health range. The estimate grows unsure while nothing new is learned.
//!
//! A major item credited to an enemy raises the estimate to at least what
//! it grants (`picked`, fed by `strategy::opponents` through
//! `Combat::credit_pickup`). No one hears an armor or health take (the
//! server sends `item_pickup` for weapons and ammo only), so credits come
//! from an item seen vanishing or, in a Duel, found gone.
//!
//! Departure from TypeScript: health over 100 bleeds 1 a second, as the
//! server's overheal decay does (`PickupSystem.ts`, `decayOverheal`), so
//! a credited Mega Health no longer reads as 200 for the rest of a life.

/// `ARMOR_ABSORPTION`: armor takes this share of a hit while it lasts.
pub(in crate::games::washoi) const ARMOR_ABSORPTION: f64 = 0.66;
const SPAWN_HEALTH: f64 = 100.0;
/// `ESTIMATE_FADE_TICKS`: 15 s without news makes the estimate unknown.
const FADE_TICKS: f64 = 1875.0;
/// `PAIN_TIER_RANGE`: the health range each pain tier stands for.
const PAIN_TIER_RANGE: [(f64, f64); 4] = [(1.0, 20.0), (21.0, 35.0), (36.0, 60.0), (61.0, 200.0)];
/// `HEALTH_MAX`: overheal bleeds back to it at 1 a second.
const HEALTH_MAX: f64 = 100.0;
const TICK_RATE: f64 = 125.0;

/// Health over the maximum after the overheal decay from `since` to `tick`.
fn decayed(health: f64, since: u32, tick: u32) -> f64 {
    if health <= HEALTH_MAX {
        return health;
    }
    let seconds = (f64::from(tick.saturating_sub(since)) / TICK_RATE).floor();
    (health - seconds).max(HEALTH_MAX)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::games::washoi) struct Estimate {
    pub(in crate::games::washoi) health: f64,
    pub(in crate::games::washoi) armor: f64,
    /// 0 just learned, 1 long without news.
    pub(in crate::games::washoi) uncertainty: f64,
}

struct Enemy {
    entity: String,
    health: f64,
    /// When `health` was last set, for the overheal decay since.
    health_at: u32,
    armor: f64,
    known: u32,
    /// When it was first known alive this life.
    spawn: u32,
    alive: bool,
}

impl Enemy {
    /// Apply the overheal decay up to `tick`, keeping the part second.
    fn settle(&mut self, tick: u32) {
        let health = decayed(self.health, self.health_at, tick);
        if health < self.health {
            let seconds = (self.health - health) as u32;
            self.health_at += seconds * TICK_RATE as u32;
            self.health = health;
        }
        if self.health <= HEALTH_MAX {
            self.health_at = tick;
        }
    }
}

#[derive(Default)]
pub(in crate::games::washoi) struct Estimates {
    enemies: Vec<Enemy>,
}

impl Estimates {
    fn get_or_spawn(&mut self, entity: &str, tick: u32) -> &mut Enemy {
        let index = match self.enemies.iter().position(|enemy| enemy.entity == entity) {
            Some(index) => index,
            None => {
                self.enemies.push(Enemy {
                    entity: entity.to_owned(),
                    health: SPAWN_HEALTH,
                    health_at: tick,
                    armor: 0.0,
                    known: tick,
                    spawn: tick,
                    alive: true,
                });
                self.enemies.len() - 1
            }
        };
        &mut self.enemies[index]
    }

    /// An enemy in sight. One not known alive has just spawned.
    pub(in crate::games::washoi) fn seen(&mut self, entity: &str, tick: u32) {
        let enemy = self.get_or_spawn(entity, tick);
        if !enemy.alive {
            *enemy = Enemy {
                entity: entity.to_owned(),
                health: SPAWN_HEALTH,
                health_at: tick,
                armor: 0.0,
                known: tick,
                spawn: tick,
                alive: true,
            };
        }
    }

    pub(in crate::games::washoi) fn died(&mut self, entity: &str) {
        if let Some(enemy) = self.enemies.iter_mut().find(|enemy| enemy.entity == entity) {
            enemy.alive = false;
        }
    }

    /// `applyHit`: a hit the bot landed, before armor, and its pain tier.
    pub(in crate::games::washoi) fn hit(
        &mut self,
        entity: &str,
        damage: f64,
        pain_tier: Option<u8>,
        tick: u32,
    ) {
        let enemy = self.get_or_spawn(entity, tick);
        enemy.settle(tick);
        let absorbed = enemy.armor.min(damage * ARMOR_ABSORPTION);
        enemy.armor = (enemy.armor - absorbed).max(0.0);
        enemy.health = (enemy.health - (damage - absorbed)).max(1.0);
        if let Some(&(low, high)) =
            pain_tier.and_then(|tier| PAIN_TIER_RANGE.get(usize::from(tier)))
        {
            enemy.health = enemy.health.clamp(low, high);
        }
        enemy.known = tick;
    }

    /// `applyPickup`: an enemy took an item, so its health or armor is at
    /// least what the item grants on top of the estimate.
    pub(in crate::games::washoi) fn picked(
        &mut self,
        entity: &str,
        tick: u32,
        grant: impl Fn(f64, f64) -> (f64, f64),
    ) {
        let enemy = self.get_or_spawn(entity, tick);
        enemy.settle(tick);
        let (health, armor) = grant(enemy.health, enemy.armor);
        enemy.health = enemy.health.max(health);
        enemy.armor = enemy.armor.max(armor);
    }

    /// How many enemies the bot has met and believes alive, of those still
    /// on the scoreboard.
    pub(in crate::games::washoi) fn alive(&self, scoreboard: &[&str]) -> usize {
        self.enemies
            .iter()
            .filter(|enemy| enemy.alive && scoreboard.contains(&enemy.entity.as_str()))
            .count()
    }

    /// The living enemy the bot heard of last, and its estimate.
    pub(in crate::games::washoi) fn freshest(&self, tick: u32) -> Option<(&str, Estimate)> {
        let enemy = self
            .enemies
            .iter()
            .filter(|enemy| enemy.alive)
            .max_by_key(|enemy| enemy.known)?;
        Some((enemy.entity.as_str(), self.of(&enemy.entity, tick)?))
    }

    /// Seconds since the enemy was first known alive this life.
    pub(in crate::games::washoi) fn alive_for(&self, entity: &str, tick: u32) -> Option<f64> {
        self.enemies
            .iter()
            .find(|enemy| enemy.entity == entity && enemy.alive)
            .map(|enemy| f64::from(tick.saturating_sub(enemy.spawn)) / 125.0)
    }

    /// Every living enemy the bot has met, and its estimate.
    pub(in crate::games::washoi) fn all(&self, tick: u32) -> Vec<(&str, Estimate)> {
        self.enemies
            .iter()
            .filter(|enemy| enemy.alive)
            .filter_map(|enemy| Some((enemy.entity.as_str(), self.of(&enemy.entity, tick)?)))
            .collect()
    }

    /// The estimate for a living enemy the bot has met.
    pub(in crate::games::washoi) fn of(&self, entity: &str, tick: u32) -> Option<Estimate> {
        let enemy = self
            .enemies
            .iter()
            .find(|enemy| enemy.entity == entity && enemy.alive)?;
        Some(Estimate {
            health: decayed(enemy.health, enemy.health_at, tick),
            armor: enemy.armor,
            uncertainty: (f64::from(tick.wrapping_sub(enemy.known)) / FADE_TICKS).min(1.0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_enemies_on_the_scoreboard_count_as_alive() {
        let mut estimates = Estimates::default();
        estimates.seen("foe", 0);
        estimates.seen("gone", 0);
        assert_eq!(estimates.alive(&["foe", "gone"]), 2);
        assert_eq!(estimates.alive(&["foe"]), 1);
    }

    #[test]
    fn hits_split_by_armor_and_pain_and_respawns_reset() {
        let mut estimates = Estimates::default();
        estimates.seen("foe", 0);
        assert_eq!(
            estimates.of("foe", 0),
            Some(Estimate {
                health: 100.0,
                armor: 0.0,
                uncertainty: 0.0
            })
        );
        estimates.hit("foe", 75.0, None, 10);
        assert_eq!(estimates.of("foe", 10).unwrap().health, 25.0);
        // The pain of a hit at tier 2 says 36 to 60 health remain.
        estimates.hit("foe", 10.0, Some(2), 20);
        assert_eq!(estimates.of("foe", 20).unwrap().health, 36.0);
        // Unsure after a while without news; gone when dead; fresh on return.
        assert!((estimates.of("foe", 20 + 1875 / 2).unwrap().uncertainty - 0.5).abs() < 1e-3);
        estimates.died("foe");
        assert_eq!(estimates.of("foe", 30), None);
        estimates.seen("foe", 40);
        assert_eq!(estimates.of("foe", 40).unwrap().health, 100.0);
        // Armor takes 66% of a hit while it lasts.
        let mut armored = Estimates::default();
        armored.hit("foe", 0.0, None, 0);
        armored.enemies[0].armor = 50.0;
        armored.hit("foe", 100.0, None, 1);
        let after = armored.of("foe", 1).unwrap();
        assert_eq!((after.health, after.armor), (50.0, 0.0));
    }

    #[test]
    fn a_take_raises_the_estimate_to_what_it_grants() {
        let mut estimates = Estimates::default();
        estimates.seen("foe", 0);
        estimates.picked("foe", 10, |health, armor| (health, armor + 100.0));
        estimates.picked("foe", 11, |health, armor| (health - 50.0, armor));
        let estimate = estimates.of("foe", 11).unwrap();
        assert_eq!((estimate.health, estimate.armor), (100.0, 100.0));
    }

    #[test]
    fn overheal_bleeds_a_point_a_second_and_armor_stays() {
        let mut estimates = Estimates::default();
        estimates.seen("foe", 0);
        estimates.picked("foe", 1000, |health, armor| (health + 100.0, armor + 100.0));
        let at = |estimates: &Estimates, tick| {
            let e = estimates.of("foe", tick).unwrap();
            (e.health, e.armor)
        };
        assert_eq!(at(&estimates, 1000), (200.0, 100.0));
        assert_eq!(at(&estimates, 1000 + 30 * 125), (170.0, 100.0));
        assert_eq!(at(&estimates, 1000 + 500 * 125), (100.0, 100.0));
        // A hit lands on the decayed health, and the decay goes on from it.
        estimates.hit("foe", 0.0, None, 1000 + 40 * 125 + 60);
        assert_eq!(at(&estimates, 1000 + 40 * 125 + 60), (160.0, 100.0));
        assert_eq!(at(&estimates, 1000 + 50 * 125), (150.0, 100.0));
        estimates.hit("foe", 150.0, None, 1000 + 50 * 125);
        let (health, _) = at(&estimates, 1000 + 50 * 125);
        assert!((health - (150.0 - 150.0 * (1.0 - ARMOR_ABSORPTION))).abs() < 1e-9);
        assert_eq!(
            at(&estimates, 1000 + 90 * 125).0,
            health,
            "under 100 it stays"
        );
    }
}

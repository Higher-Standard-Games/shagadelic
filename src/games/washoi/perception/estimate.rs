//! What a bot believes about each enemy's health and armor, from TypeScript
//! `strategy/MatchRead.ts`. An enemy first seen alive, or seen again after it
//! died, has spawn values. Each hit the bot lands takes armor's share by the
//! server's absorption rule, and the pain sound the hit gives away clamps
//! the health range. The estimate grows unsure while nothing new is learned.
//!
//! Major items an enemy was seen or heard taking also feed TypeScript's
//! estimate. That needs pickup hearing by item and is not ported, so the
//! estimate can run low after an unseen Mega Health.

/// `ARMOR_ABSORPTION`: armor takes this share of a hit while it lasts.
pub(in crate::games::washoi) const ARMOR_ABSORPTION: f64 = 0.66;
const SPAWN_HEALTH: f64 = 100.0;
/// `ESTIMATE_FADE_TICKS`: 15 s without news makes the estimate unknown.
const FADE_TICKS: f64 = 1875.0;
/// `PAIN_TIER_RANGE`: the health range each pain tier stands for.
const PAIN_TIER_RANGE: [(f64, f64); 4] = [(1.0, 20.0), (21.0, 35.0), (36.0, 60.0), (61.0, 200.0)];

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
    armor: f64,
    known: u32,
    alive: bool,
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
                    armor: 0.0,
                    known: tick,
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
                armor: 0.0,
                known: tick,
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
        let (health, armor) = grant(enemy.health, enemy.armor);
        enemy.health = enemy.health.max(health);
        enemy.armor = enemy.armor.max(armor);
    }

    /// How many enemies the bot has met and believes alive.
    pub(in crate::games::washoi) fn alive(&self) -> usize {
        self.enemies.iter().filter(|enemy| enemy.alive).count()
    }

    /// The living enemy the bot heard of last, with its estimate.
    pub(in crate::games::washoi) fn freshest(&self, tick: u32) -> Option<Estimate> {
        let enemy = self
            .enemies
            .iter()
            .filter(|enemy| enemy.alive)
            .max_by_key(|enemy| enemy.known)?;
        self.of(&enemy.entity, tick)
    }

    /// The estimate for a living enemy the bot has met.
    pub(in crate::games::washoi) fn of(&self, entity: &str, tick: u32) -> Option<Estimate> {
        let enemy = self
            .enemies
            .iter()
            .find(|enemy| enemy.entity == entity && enemy.alive)?;
        Some(Estimate {
            health: enemy.health,
            armor: enemy.armor,
            uncertainty: (f64::from(tick.wrapping_sub(enemy.known)) / FADE_TICKS).min(1.0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

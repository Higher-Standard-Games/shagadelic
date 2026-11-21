//! How a bot's last few fights colour the next one, and the odd mistake a
//! person makes, from TypeScript `bot/BotMood.ts`.
//!
//! Moods: calm by default, fading back after 20 s with nothing new.
//! Confident after two kills without dying. Tilted after three deaths in a
//! row, with a looser aim and slower reactions. Cautious when low on health.
//! Cautious too when an enemy is known to carry a major item, as in
//! item, which needs pickup hearing by item and is not ported. A mood also
//! scales the retreat threshold, used once retreating is ported.
//!
//! Mistakes: each new fight, at `mistakeRate / 16` odds (one in two on easy,
//! about one in eighty on insane), the bot makes one of these. It opens fire
//! a few degrees off (`WildShot`), fights a second with whatever it had out
//! (`WrongWeapon`), looks off at an item (`ItemGlance`), or stops chasing as
//! soon as the enemy is out of sight (`Forget`).

use crate::games::washoi::{protocol::SnapshotEvent, random::Random};

/// Server ticks per second.
const TICK_RATE: f64 = 125.0;
const FADE_TICKS: u32 = 2500;
const CONFIDENT_KILLS: u32 = 2;
const TILTED_DEATHS: u32 = 3;
const CAUTIOUS_HEALTH: u16 = 40;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::games::washoi) enum Mood {
    #[default]
    Calm,
    Confident,
    Tilted,
    Cautious,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::games::washoi) enum Mistake {
    WildShot,
    WrongWeapon,
    ItemGlance,
    Forget,
}

impl Mistake {
    const ALL: [Self; 4] = [
        Self::WildShot,
        Self::WrongWeapon,
        Self::ItemGlance,
        Self::Forget,
    ];

    /// `MISTAKE_TICKS`: how long it lasts. Forgetting lasts the fight.
    fn ticks(self) -> Option<u32> {
        let seconds = match self {
            Self::WildShot => 0.4,
            Self::WrongWeapon => 1.0,
            Self::ItemGlance => 0.5,
            Self::Forget => return None,
        };
        Some((seconds * TICK_RATE).round() as u32)
    }
}

#[derive(Default)]
pub(in crate::games::washoi) struct BotMood {
    mood: Mood,
    kills: u32,
    deaths: u32,
    last_event: Option<u32>,
    mistake: Option<(Mistake, u32, Option<u32>)>,
}

impl BotMood {
    #[cfg(test)]
    pub(in crate::games::washoi) fn mood(&self) -> Mood {
        self.mood
    }

    /// `update`: kills and deaths involving the bot, then the mood.
    pub(in crate::games::washoi) fn update(
        &mut self,
        own: &str,
        events: &[SnapshotEvent],
        health: Option<u16>,
        dead: bool,
        armed_enemy: bool,
        tick: u32,
    ) {
        for event in events {
            if let SnapshotEvent::Death {
                entity_id,
                killer_id,
                ..
            } = event
            {
                if entity_id == own {
                    self.deaths += 1;
                    self.kills = 0;
                    self.last_event = Some(tick);
                } else if killer_id == own {
                    self.kills += 1;
                    self.deaths = 0;
                    self.last_event = Some(tick);
                }
            }
        }
        if self
            .last_event
            .is_none_or(|at| tick.wrapping_sub(at) >= FADE_TICKS)
        {
            self.kills = 0;
            self.deaths = 0;
        }
        self.mood = if self.deaths >= TILTED_DEATHS {
            Mood::Tilted
        } else if !dead && (health.is_some_and(|health| health < CAUTIOUS_HEALTH) || armed_enemy) {
            Mood::Cautious
        } else if self.kills >= CONFIDENT_KILLS {
            Mood::Confident
        } else {
            Mood::Calm
        };
    }

    /// `retreatHealth`: the health below which the bot backs off.
    #[allow(dead_code)]
    pub(in crate::games::washoi) fn retreat_health(&self, base: f64) -> f64 {
        match self.mood {
            Mood::Confident => base * 0.6,
            Mood::Tilted => base * 0.4,
            Mood::Cautious => base * 1.5,
            Mood::Calm => base,
        }
    }

    /// `aimDifficulty`: tilted aims looser and reacts slower. The accuracy
    /// and reaction time to use.
    pub(in crate::games::washoi) fn aim(&self, accuracy: f64, reaction_ms: f64) -> (f64, f64) {
        if self.mood == Mood::Tilted {
            (accuracy * 0.85, reaction_ms * 1.3)
        } else {
            (accuracy, reaction_ms)
        }
    }

    /// `startFight`: a new fight began, so maybe roll a mistake for it.
    pub(in crate::games::washoi) fn start_fight(
        &mut self,
        tick: u32,
        mistake_rate: f64,
        random: &mut Random,
    ) {
        self.mistake = None;
        if random.next() >= mistake_rate / 16.0 {
            return;
        }
        let index = ((random.next() * Mistake::ALL.len() as f64) as usize) % Mistake::ALL.len();
        let mistake = Mistake::ALL[index];
        self.mistake = Some((mistake, tick, mistake.ticks().map(|ticks| tick + ticks)));
    }

    /// The mistake being made now, if any, and when it began.
    pub(in crate::games::washoi) fn mistake(&mut self, tick: u32) -> Option<(Mistake, u32)> {
        if self
            .mistake
            .is_some_and(|(_, _, until)| until.is_some_and(|until| tick >= until))
        {
            self.mistake = None;
        }
        self.mistake.map(|(mistake, began, _)| (mistake, began))
    }

    /// `endFight`: a fight ended. Forgetting outlasts it.
    pub(in crate::games::washoi) fn end_fight(&mut self) {
        if !matches!(self.mistake, Some((Mistake::Forget, _, _))) {
            self.mistake = None;
        }
    }

    pub(in crate::games::washoi) fn clear_mistake(&mut self) {
        self.mistake = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::protocol::Point;

    fn death(victim: &str, killer: &str) -> SnapshotEvent {
        let origin = Point {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        SnapshotEvent::Death {
            entity_id: victim.into(),
            killer_id: killer.into(),
            weapon_id: "machinegun".into(),
            damage_type: String::new(),
            final_damage: 0,
            at: origin,
            vel: origin,
            kb_dir: origin,
            kill_weapon: None,
            hit_location: None,
        }
    }

    #[test]
    fn streaks_set_the_mood_and_fade() {
        let mut mood = BotMood::default();
        mood.update(
            "me",
            &[death("a", "me"), death("b", "me")],
            Some(100),
            false,
            false,
            10,
        );
        assert_eq!(mood.mood(), Mood::Confident);
        let deaths = [death("me", "a"), death("me", "a"), death("me", "a")];
        mood.update("me", &deaths, Some(100), false, false, 20);
        assert_eq!(mood.mood(), Mood::Tilted);
        assert_eq!(mood.aim(0.5, 280.0), (0.5 * 0.85, 280.0 * 1.3));
        // Twenty seconds later the streak is gone; low health is cautious.
        mood.update("me", &[], Some(30), false, false, 20 + 2500);
        assert_eq!(mood.mood(), Mood::Cautious);
        assert_eq!(mood.retreat_health(40.0), 60.0);
        mood.update("me", &[], Some(100), false, false, 20 + 2500);
        assert_eq!(mood.mood(), Mood::Calm);
    }

    #[test]
    fn mistakes_are_rolled_per_fight_and_forgetting_outlasts_it() {
        let mut random = Random::new(1);
        let mut mood = BotMood::default();
        // A rate of 16 always errs; 0 never does.
        mood.start_fight(100, 16.0, &mut random);
        let (made, began) = mood.mistake(100).expect("a mistake");
        assert_eq!(began, 100);
        let mut never = BotMood::default();
        never.start_fight(100, 0.0, &mut random);
        assert_eq!(never.mistake(100), None);
        // Timed mistakes expire; forgetting lasts past the fight's end.
        let mut seen = std::collections::HashSet::new();
        for tick in 0..200 {
            mood.start_fight(tick * 1000, 16.0, &mut random);
            let (mistake, _) = mood.mistake(tick * 1000).unwrap();
            seen.insert(format!("{mistake:?}"));
            match mistake.ticks() {
                Some(ticks) => assert_eq!(mood.mistake(tick * 1000 + ticks), None),
                None => {
                    mood.end_fight();
                    assert_eq!(
                        mood.mistake(tick * 1000 + 99_999).map(|m| m.0),
                        Some(Mistake::Forget)
                    );
                }
            }
        }
        assert_eq!(seen.len(), 4, "{seen:?}");
        let _ = made;
    }
}

//! Washoi named presets. The `itemTimingSkill` and `matchSense` values come
//! from `DIFFICULTY_PRESETS` in TypeScript `bot/BotConfig.ts`.
//!
//! The mappings below are SHAGADELIC approximations, not ports. TypeScript
//! uses `itemTimingSkill` in `BotPerception.ts` to blur remembered respawn
//! times, and `matchSense` gates the `strategy/Strategist.ts` stances. The
//! Rust bot has neither yet, so the skill shortens how long a missing pickup
//! is remembered and match sense scales how strongly health and armor need
//! shapes pickup choice.
//!
//! Unlike `resolveDifficulty`, an unknown name is rejected rather than read as
//! medium, so a host typo can't silently weaken a bot.

use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Difficulty {
    Easy,
    #[default]
    Medium,
    Hard,
    Insane,
}

impl Difficulty {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "easy" => Some(Self::Easy),
            "medium" => Some(Self::Medium),
            "hard" => Some(Self::Hard),
            "insane" => Some(Self::Insane),
            _ => None,
        }
    }

    pub(super) fn pickup_recheck(self) -> Duration {
        let item_timing = match self {
            Self::Easy => 0.1,
            Self::Medium => 0.4,
            Self::Hard => 0.7,
            Self::Insane => 0.9,
        };
        Duration::from_secs_f64(6.0 - 5.0 * item_timing)
    }

    pub(super) fn resource_awareness(self) -> f64 {
        let match_sense = match self {
            Self::Easy => 0.0,
            Self::Medium => 0.45,
            Self::Hard => 0.8,
            Self::Insane => 1.0,
        };
        0.55 + match_sense
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_presets_give_more_weight_to_resource_need_and_recheck_sooner() {
        assert_eq!(Difficulty::Medium.pickup_recheck(), Duration::from_secs(4));
        for (low, high) in [
            (Difficulty::Easy, Difficulty::Medium),
            (Difficulty::Medium, Difficulty::Hard),
            (Difficulty::Hard, Difficulty::Insane),
        ] {
            assert!(low.pickup_recheck() > high.pickup_recheck());
            assert!(low.resource_awareness() < high.resource_awareness());
        }
    }
}

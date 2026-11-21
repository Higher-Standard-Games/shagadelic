//! Washoi named presets. The `matchSense` values come from
//! `DIFFICULTY_PRESETS` in TypeScript `bot/BotConfig.ts`.
//!
//! The mapping below is a SHAGADELIC approximation, not a port. TypeScript
//! `matchSense` gates the `strategy/Strategist.ts` stances. The Rust bot has
//! no strategist yet, so match sense scales how strongly health and armor
//! need shapes pickup choice. `itemTimingSkill` blurs remembered respawn
//! times in `BotPerception.ts` and awaits a per-bot random seed.
//!
//! Unlike `resolveDifficulty`, an unknown name is rejected rather than read as
//! medium, so a host typo can't silently weaken a bot.

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
    fn higher_presets_give_more_weight_to_resource_need() {
        for (low, high) in [
            (Difficulty::Easy, Difficulty::Medium),
            (Difficulty::Medium, Difficulty::Hard),
            (Difficulty::Hard, Difficulty::Insane),
        ] {
            assert!(low.resource_awareness() < high.resource_awareness());
        }
    }
}

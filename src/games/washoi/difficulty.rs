//! Washoi named presets. The `matchSense` values come from
//! `DIFFICULTY_PRESETS` in TypeScript `bot/BotConfig.ts`.
//!
//! The aim values are ported as they are. The resource mapping below is a
//! SHAGADELIC approximation, not a port. TypeScript
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

    /// `aimAccuracy`, `reactionTimeMs`, `trackingSpeed`, and
    /// `predictionSkill` from `DIFFICULTY_PRESETS`, as `BotAimController.ts`
    /// and `states/BotFight.ts` use them.
    pub(super) fn aim(self) -> AimSkill {
        let (accuracy, reaction_ms, tracking, prediction) = match self {
            Self::Easy => (0.25, 450.0, 1.5, 0.2),
            Self::Medium => (0.5, 280.0, 3.0, 0.5),
            Self::Hard => (0.75, 150.0, 5.0, 0.75),
            Self::Insane => (0.92, 60.0, 8.0, 0.9),
        };
        AimSkill {
            accuracy,
            reaction_ms,
            tracking,
            prediction,
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct AimSkill {
    pub(super) accuracy: f64,
    pub(super) reaction_ms: f64,
    pub(super) tracking: f64,
    pub(super) prediction: f64,
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

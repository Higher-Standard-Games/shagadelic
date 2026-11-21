//! Washoi named presets. The `matchSense` values come from
//! `DIFFICULTY_PRESETS` in TypeScript `bot/BotConfig.ts`.
//!
//! The aim values are ported as they are. The resource mapping below is a
//! SHAGADELIC approximation, not a port. TypeScript
//! `matchSense` gates the `strategy/Strategist.ts` stances. The Rust bot has
//! no strategist yet, so match sense scales how strongly health and armor
//! need shapes pickup choice. `itemTimingSkill` blurs remembered respawn
//! times as in `BotPerception.ts`.
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
    pub(in crate::games::washoi) fn aim(self) -> AimSkill {
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
            dodge: self.dodge_skill(),
        }
    }

    /// `matchSense`: how well the bot reads the match, from 0 for an easy
    /// bot to 1 for an insane one. From 0.6 it contests the items an
    /// opponent keeps taking (`CONTEST_SENSE`).
    pub(super) fn match_sense(self) -> f64 {
        match self {
            Self::Easy => 0.0,
            Self::Medium => 0.45,
            Self::Hard => 0.8,
            Self::Insane => 1.0,
        }
    }

    /// `mistakeRate`: a new fight errs at `mistakeRate / 16` odds (`BotMood`).
    pub(super) fn mistake_rate(self) -> f64 {
        match self {
            Self::Easy => 8.0,
            Self::Medium => 3.0,
            Self::Hard => 1.0,
            Self::Insane => 0.2,
        }
    }

    /// `dodgeSkill`, as `BotCombatMovement.ts` uses it.
    pub(in crate::games::washoi) fn dodge_skill(self) -> f64 {
        match self {
            Self::Easy => 0.15,
            Self::Medium => 0.4,
            Self::Hard => 0.7,
            Self::Insane => 0.9,
        }
    }

    /// `strafeJumpSkill`, as the route slide's jump cancel reads it.
    pub(in crate::games::washoi) fn strafe_jump_skill(self) -> f64 {
        match self {
            Self::Easy => 0.1,
            Self::Medium => 0.4,
            Self::Hard => 0.7,
            Self::Insane => 0.9,
        }
    }

    /// How far off a remembered take time may be, from `itemTimingSkill`
    /// as `BotPerception._believeItems` scales it.
    pub(in crate::games::washoi) fn item_timing_error(self) -> std::time::Duration {
        let skill: f64 = match self {
            Self::Easy => 0.1,
            Self::Medium => 0.4,
            Self::Hard => 0.7,
            Self::Insane => 0.9,
        };
        std::time::Duration::from_secs_f64((7.83 - 8.33 * skill).max(0.5))
    }

    pub(in crate::games::washoi) fn resource_awareness(self) -> f64 {
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
pub(in crate::games::washoi) struct AimSkill {
    pub(in crate::games::washoi) accuracy: f64,
    pub(in crate::games::washoi) reaction_ms: f64,
    pub(in crate::games::washoi) tracking: f64,
    pub(in crate::games::washoi) prediction: f64,
    /// `dodgeSkill`, which also rolls cover use in `BotFight`.
    pub(in crate::games::washoi) dodge: f64,
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

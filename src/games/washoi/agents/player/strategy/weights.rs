//! How much a gametype cares about each reason to pursue a goal, from
//! TypeScript `shared/bot/WeightProfiles.ts`, and the match context that
//! selects a profile, from the `match_state` event as `BotGameState.ts`
//! reads it.

use shagger_shared::json::Value;

/// `BotWeightProfile`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::games::washoi) struct Weights {
    pub(in crate::games::washoi) item_greed: f64,
    pub(in crate::games::washoi) item_denial: f64,
    pub(in crate::games::washoi) major_item_bias: f64,
    pub(in crate::games::washoi) item_timing: f64,
    pub(in crate::games::washoi) objective_push: f64,
    pub(in crate::games::washoi) team_cohesion: f64,
    pub(in crate::games::washoi) carrier_protect: f64,
    pub(in crate::games::washoi) map_control: f64,
    pub(in crate::games::washoi) aggression: f64,
    pub(in crate::games::washoi) risk_tolerance: f64,
}

/// `DEFAULT_WEIGHT_PROFILE`.
pub(in crate::games::washoi) const DEFAULT: Weights = Weights {
    item_greed: 1.0,
    item_denial: 0.3,
    major_item_bias: 1.0,
    item_timing: 0.0,
    objective_push: 0.0,
    team_cohesion: 0.0,
    carrier_protect: 0.0,
    map_control: 0.3,
    aggression: 0.8,
    risk_tolerance: 0.5,
};

/// `WEIGHT_PROFILES`, by gametype key, then role, then round state.
const PROFILES: [(&str, Weights); 8] = [
    (
        "amphi",
        Weights {
            item_greed: 0.6,
            item_denial: 0.1,
            major_item_bias: 0.5,
            aggression: 1.0,
            risk_tolerance: 0.7,
            ..DEFAULT
        },
    ),
    (
        "deathmatch",
        Weights {
            item_greed: 1.0,
            item_denial: 0.3,
            item_timing: 0.5,
            aggression: 0.8,
            risk_tolerance: 0.5,
            ..DEFAULT
        },
    ),
    (
        "team_deathmatch",
        Weights {
            item_greed: 0.9,
            item_denial: 0.4,
            item_timing: 0.5,
            team_cohesion: 0.5,
            map_control: 0.4,
            aggression: 0.7,
            risk_tolerance: 0.4,
            ..DEFAULT
        },
    ),
    (
        "search_destroy",
        Weights {
            item_greed: 0.7,
            item_denial: 0.2,
            major_item_bias: 0.7,
            objective_push: 1.0,
            team_cohesion: 0.6,
            carrier_protect: 0.8,
            map_control: 0.4,
            aggression: 0.7,
            risk_tolerance: 0.6,
            ..DEFAULT
        },
    ),
    (
        "search_destroy:defender",
        Weights {
            item_greed: 0.6,
            item_denial: 0.2,
            major_item_bias: 0.7,
            objective_push: 0.8,
            team_cohesion: 0.3,
            carrier_protect: 0.0,
            map_control: 1.0,
            aggression: 0.4,
            risk_tolerance: 0.3,
            ..DEFAULT
        },
    ),
    (
        "search_destroy:defender:post_plant",
        Weights {
            item_greed: 0.1,
            item_denial: 0.0,
            objective_push: 1.0,
            team_cohesion: 0.2,
            carrier_protect: 0.0,
            map_control: 0.1,
            aggression: 0.5,
            risk_tolerance: 1.0,
            ..DEFAULT
        },
    ),
    (
        "search_destroy:attacker:post_plant",
        Weights {
            item_greed: 0.2,
            item_denial: 0.0,
            objective_push: 0.9,
            team_cohesion: 0.5,
            carrier_protect: 0.0,
            map_control: 0.9,
            aggression: 0.4,
            risk_tolerance: 0.35,
            ..DEFAULT
        },
    ),
    (
        "duel",
        Weights {
            item_greed: 1.0,
            item_denial: 0.9,
            major_item_bias: 1.8,
            item_timing: 1.0,
            map_control: 0.9,
            aggression: 0.5,
            risk_tolerance: 0.3,
            ..DEFAULT
        },
    ),
];

/// The match as the latest `match_state` event describes it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(in crate::games::washoi) struct MatchContext {
    pub(in crate::games::washoi) gametype: String,
    pub(in crate::games::washoi) phase: u8,
    /// Each team's role, such as `attacker` or `defender`.
    pub(in crate::games::washoi) team_roles: Vec<(String, String)>,
    pub(in crate::games::washoi) round_state: Option<String>,
    pub(in crate::games::washoi) time_remaining: Option<f64>,
}

impl MatchContext {
    /// Read a `match_state` event defensively, as an older or newer server
    /// may omit any field.
    pub(in crate::games::washoi) fn read(event: &Value) -> Self {
        let team_roles = match event.get("teamRoles") {
            Some(Value::Object(roles)) => roles
                .iter()
                .filter_map(|(team, role)| Some((team.clone(), role.as_str()?.to_owned())))
                .collect(),
            _ => Vec::new(),
        };
        Self {
            gametype: event
                .get("gametypeName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            phase: event
                .get("phase")
                .and_then(Value::as_f64)
                .map_or(0, |phase| phase as u8),
            team_roles,
            round_state: event
                .get("roundState")
                .and_then(Value::as_str)
                .map(str::to_owned),
            time_remaining: event.get("timeRemaining").and_then(Value::as_f64),
        }
    }

    /// `getOwnRole`: the role of the bot's team, outside FFA and spectating.
    pub(in crate::games::washoi) fn role(&self, team: &str) -> Option<&str> {
        if team.is_empty() || team == "ffa" || team == "spectator" {
            return None;
        }
        self.team_roles
            .iter()
            .find(|(known, _)| known == team)
            .map(|(_, role)| role.as_str())
    }

    /// `GAMETYPE_HABITS.keepsTerritory`: a Duel is won by holding ground and
    /// timing items over it.
    pub(in crate::games::washoi) fn keeps_territory(&self) -> bool {
        profile_key(&self.gametype) == "duel"
    }

    /// `selectWeightProfile`: the most specific profile for the gametype,
    /// the bot's role, and the round state.
    pub(in crate::games::washoi) fn weights(&self, team: &str) -> Weights {
        let base = profile_key(&self.gametype);
        if base.is_empty() {
            return DEFAULT;
        }
        let role = self.role(team);
        let mut keys = Vec::with_capacity(3);
        if let (Some(role), Some(round)) = (role, &self.round_state) {
            keys.push(format!("{base}:{role}:{round}"));
        }
        if let Some(role) = role {
            keys.push(format!("{base}:{role}"));
        }
        keys.push(base);
        keys.iter()
            .find_map(|key| {
                PROFILES
                    .iter()
                    .find(|(known, _)| known == key)
                    .map(|(_, weights)| *weights)
            })
            .unwrap_or(DEFAULT)
    }
}

/// `weightProfileKey`: lower case, with runs of other characters as one `_`.
fn profile_key(gametype: &str) -> String {
    let mut key = String::with_capacity(gametype.len());
    for c in gametype.trim().chars() {
        if c.is_ascii_alphanumeric() {
            key.push(c.to_ascii_lowercase());
        } else if !key.is_empty() && !key.ends_with('_') {
            key.push('_');
        }
    }
    key.trim_end_matches('_').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shagger_shared::json;

    #[test]
    fn picks_the_most_specific_profile_for_mode_role_and_round() {
        let context = MatchContext::read(
            &json::parse(
                r#"{"gametypeName":"Search & Destroy","phase":2,
                    "teamRoles":{"red":"defender","blue":"attacker"},
                    "roundState":"post_plant","timeRemaining":31.5}"#,
            )
            .unwrap(),
        );
        assert_eq!(profile_key(&context.gametype), "search_destroy");
        assert_eq!(context.weights("red").risk_tolerance, 1.0);
        assert_eq!(context.weights("blue").map_control, 0.9);
        // No role for a spectator: the mode's base profile.
        assert_eq!(context.weights("spectator").objective_push, 1.0);
        let duel = MatchContext {
            gametype: "Duel".into(),
            ..MatchContext::default()
        };
        assert_eq!(duel.weights("").major_item_bias, 1.8);
        assert_eq!(MatchContext::default().weights(""), DEFAULT);
    }
}

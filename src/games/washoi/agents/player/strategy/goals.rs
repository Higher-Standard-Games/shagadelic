//! First Washoi pickup policy. Pickup IDs index the map's snapshot active mask.

use std::time::Duration;

use crate::games::washoi::{
    Difficulty,
    agents::player::strategy::items::{self, ARMOR_MAX, HEALTH_MAX},
};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::games::washoi) enum PickupKind {
    Health,
    Armor,
    Weapon,
    Ammo,
    Powerup,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub(in crate::games::washoi) struct PickupGoal {
    pub(in crate::games::washoi) id: usize,
    pub(in crate::games::washoi) kind: PickupKind,
    /// The item within its kind, as the map's pickup table names it: a
    /// health or armor grant, a weapon, an ammo pool, or a powerup.
    #[serde(default)]
    pub(in crate::games::washoi) item: Item,
}

/// A pickup table item name, interned so a goal stays `Copy`. Empty when the
/// name is unknown or missing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::games::washoi) struct Item(pub(in crate::games::washoi) &'static str);

impl<'de> Deserialize<'de> for Item {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = Option::<String>::deserialize(deserializer)?;
        Ok(Self(name.map_or("", |name| items::intern(&name))))
    }
}

#[derive(Default, Clone, Copy)]
pub(in crate::games::washoi) struct OwnVitals {
    pub(in crate::games::washoi) health: Option<u16>,
    pub(in crate::games::washoi) armor: Option<u16>,
}

impl PickupGoal {
    /// `isMajorItem` in `goals/considerations/ItemFacts.ts`: powerups,
    /// green, yellow, and red armor, Mega Health, and Ultra Health.
    pub(in crate::games::washoi) fn is_major(self) -> bool {
        items::is_major(self.kind, self.item.0)
    }

    /// How long a taken item stays away, from Washoi
    /// `shared/items/ItemRespawn.ts` `itemRespawnSeconds` with its default
    /// weapon time. The TypeScript bot uses the same defaults.
    pub(in crate::games::washoi) fn respawn(self) -> Duration {
        Duration::from_secs(match self.kind {
            PickupKind::Weapon | PickupKind::Ammo => 15,
            PickupKind::Armor => 25,
            PickupKind::Powerup => 120,
            PickupKind::Health if matches!(self.item.0, "mega" | "ultra") => 60,
            PickupKind::Health => 20,
        })
    }

    /// Whether the server would let the bot take this item now. Washoi's
    /// `PickupSystem.ts` leaves health and armor in place when they add
    /// nothing, by the `ItemGrants.ts` rules. A bot sent to one would wait on it indefinitely. Weapon and
    /// ammo limits depend on inventory the bot doesn't track yet.
    pub(in crate::games::washoi) fn benefits(self, vitals: OwnVitals) -> bool {
        match self.kind {
            PickupKind::Health => vitals
                .health
                .is_none_or(|health| items::health_after(health, self.item.0) > health),
            PickupKind::Armor => vitals
                .armor
                .is_none_or(|armor| items::armor_after(armor, self.item.0) > armor),
            PickupKind::Weapon | PickupKind::Ammo | PickupKind::Powerup => true,
        }
    }

    pub(in crate::games::washoi) fn value(self, vitals: OwnVitals, difficulty: Difficulty) -> f64 {
        // Missing health or armor is unknown until our own CombatState arrives.
        match self.kind {
            PickupKind::Health => {
                0.3 + 1.4 * deficit(vitals.health, HEALTH_MAX) * difficulty.resource_awareness()
            }
            PickupKind::Armor => {
                0.4 + 0.9 * deficit(vitals.armor, ARMOR_MAX) * difficulty.resource_awareness()
            }
            PickupKind::Weapon => 0.75,
            PickupKind::Ammo => 0.25,
            PickupKind::Powerup => 1.05,
        }
    }
}

fn deficit(value: Option<u16>, baseline: u16) -> f64 {
    f64::from(value.map_or(0, |amount| baseline.saturating_sub(amount))) / f64::from(baseline)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(json: &str) -> PickupGoal {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn full_bots_skip_health_and_armor_the_server_would_leave() {
        let full = OwnVitals {
            health: Some(100),
            armor: Some(100),
        };
        let medium = goal(r#"{"id":0,"kind":"health","item":"medium"}"#);
        let unlabeled = goal(r#"{"id":0,"kind":"health"}"#);
        let mega = goal(r#"{"id":0,"kind":"health","item":"mega"}"#);
        let armor = goal(r#"{"id":0,"kind":"armor","item":"ra"}"#);
        assert!(!medium.benefits(full));
        assert!(!unlabeled.benefits(full));
        assert!(mega.benefits(full));
        assert!(!mega.benefits(OwnVitals {
            health: Some(200),
            ..full
        }));
        assert!(!armor.benefits(full));
        let unknown = OwnVitals::default();
        assert!(medium.benefits(unknown) && armor.benefits(unknown));
        assert!(goal(r#"{"id":0,"kind":"weapon","item":"rl"}"#).benefits(full));
    }
}

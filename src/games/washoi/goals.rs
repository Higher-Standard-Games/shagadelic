//! First Washoi pickup policy. Pickup IDs index the map's snapshot active mask.

use super::Difficulty;
use serde::Deserialize;

// Caps from Washoi `shared/items/ItemGrants.ts`.
const HEALTH_MAX: u16 = 100;
const HEALTH_OVERHEAL_MAX: u16 = 200;
const ARMOR_MAX: u16 = 100;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PickupKind {
    Health,
    Armor,
    Weapon,
    Ammo,
    Powerup,
}

/// The item within its category, as in the map's pickup table. Only the
/// health kinds that overheal change what the bot may take.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PickupItem {
    Small,
    Mega,
    Ultra,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub(super) struct PickupGoal {
    pub(super) id: usize,
    kind: PickupKind,
    #[serde(default)]
    item: Option<PickupItem>,
}

#[derive(Default, Clone, Copy)]
pub(super) struct OwnVitals {
    pub(super) health: Option<u16>,
    pub(super) armor: Option<u16>,
}

impl PickupGoal {
    /// Whether the server would let the bot take this item now. Washoi's
    /// `PickupSystem.ts` leaves health and armor in place when they add
    /// nothing. A bot sent to one would wait on it indefinitely. Weapon and
    /// ammo limits depend on inventory the bot doesn't track yet.
    pub(super) fn benefits(self, vitals: OwnVitals) -> bool {
        match self.kind {
            PickupKind::Health => {
                let cap = match self.item {
                    Some(PickupItem::Small | PickupItem::Mega | PickupItem::Ultra) => {
                        HEALTH_OVERHEAL_MAX
                    }
                    // Without the item kind, assume the stricter cap.
                    _ => HEALTH_MAX,
                };
                vitals.health.is_none_or(|health| health < cap)
            }
            PickupKind::Armor => vitals.armor.is_none_or(|armor| armor < ARMOR_MAX),
            PickupKind::Weapon | PickupKind::Ammo | PickupKind::Powerup => true,
        }
    }

    pub(super) fn value(self, vitals: OwnVitals, difficulty: Difficulty) -> f64 {
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

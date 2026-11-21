//! First Washoi pickup policy. Pickup IDs index the map's snapshot active mask.

use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PickupKind {
    Health,
    Armor,
    Weapon,
    Ammo,
    Powerup,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub(super) struct PickupGoal {
    pub(super) id: usize,
    kind: PickupKind,
}

#[derive(Default, Clone, Copy)]
pub(super) struct OwnVitals {
    pub(super) health: Option<u16>,
    pub(super) armor: Option<u16>,
}

impl PickupGoal {
    pub(super) fn value(self, vitals: OwnVitals) -> f64 {
        // Missing health or armor is unknown until our own CombatState arrives.
        match self.kind {
            PickupKind::Health => 0.3 + 1.4 * deficit(vitals.health, 100),
            PickupKind::Armor => 0.4 + 0.9 * deficit(vitals.armor, 100),
            PickupKind::Weapon => 0.75,
            PickupKind::Ammo => 0.25,
            PickupKind::Powerup => 1.05,
        }
    }
}

fn deficit(value: Option<u16>, baseline: u16) -> f64 {
    f64::from(value.map_or(0, |amount| baseline.saturating_sub(amount))) / f64::from(baseline)
}

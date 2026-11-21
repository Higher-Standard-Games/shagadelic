//! What items grant and what they are worth, the facts the goal
//! considerations read. Ported from TypeScript `shared/items/ItemGrants.ts`,
//! the ammo classnames of `shared/bsp/PickupTable.ts`, the ammo pools and
//! capacities of `shared/entities/definitions/weapons/`, and
//! `bot/goals/considerations/ItemFacts.ts`.

use crate::games::washoi::agents::player::strategy::goals::PickupKind;

pub(in crate::games::washoi) const HEALTH_MAX: u16 = 100;
pub(in crate::games::washoi) const HEALTH_OVERHEAL_MAX: u16 = 200;
pub(in crate::games::washoi) const ARMOR_MAX: u16 = 100;
const ARMOR_PLATE: u16 = 25;

/// Every weapon, its ammo pool, and the pool's reserve capacity (`maxAmmo`).
const WEAPONS: [(&str, &str, f64); 8] = [
    ("blade", "blade", 1.0),
    ("electrobolt", "bolts", 10.0),
    ("glauncher", "grenades", 20.0),
    ("machinegun", "bullets", 120.0),
    ("plasmagun", "cells", 150.0),
    ("riotgun", "shells", 24.0),
    ("rlauncher", "rockets", 20.0),
    ("voltaic", "lasers", 60.0),
];

/// Ammo pickups by pool, and how much each grants (`AMMO_CLASSNAMES`).
const AMMO: [(&str, f64); 7] = [
    ("bullets", 50.0),
    ("shells", 10.0),
    ("grenades", 10.0),
    ("rockets", 10.0),
    ("cells", 50.0),
    ("lasers", 100.0),
    ("bolts", 10.0),
];

const HEALTH: [(&str, u16); 5] = [
    ("small", 5),
    ("medium", 25),
    ("large", 50),
    ("mega", 100),
    ("ultra", 100),
];
const ARMOR: [&str; 4] = ["shard", "ga", "ya", "ra"];
const POWERUPS: [&str; 1] = ["quad"];

/// The pickup table's item name as a static string, so a pickup goal stays
/// `Copy`. An item this table doesn't know reads as the empty name.
pub(in crate::games::washoi) fn intern(name: &str) -> &'static str {
    HEALTH
        .iter()
        .map(|(kind, _)| *kind)
        .chain(ARMOR)
        .chain(POWERUPS)
        .chain(WEAPONS.iter().map(|(id, _, _)| *id))
        .chain(AMMO.iter().map(|(pool, _)| *pool))
        .find(|known| *known == name)
        .unwrap_or("")
}

/// `healthOverheals`: small, Mega, and Ultra Health go past 100.
pub(in crate::games::washoi) fn health_overheals(kind: &str) -> bool {
    matches!(kind, "small" | "mega" | "ultra")
}

/// `healthAfterPickup`. An unknown kind counts as medium, the strict cap.
pub(in crate::games::washoi) fn health_after(current: u16, kind: &str) -> u16 {
    let amount = HEALTH
        .iter()
        .find(|(known, _)| *known == kind)
        .map_or(25, |(_, amount)| *amount);
    let cap = if health_overheals(kind) {
        HEALTH_OVERHEAL_MAX
    } else {
        HEALTH_MAX
    };
    cap.min(current.saturating_add(amount))
}

/// `armorAfterPickup`: a shard adds 5, and an armor adds its plates (green 1,
/// yellow 2, red 4) to the plates already full. An unknown kind counts as
/// green.
pub(in crate::games::washoi) fn armor_after(current: u16, kind: &str) -> u16 {
    let plates = match kind {
        "shard" => return ARMOR_MAX.min(current.saturating_add(5)),
        "ya" => 2,
        "ra" => 4,
        _ => 1,
    };
    ARMOR_MAX.min((current / ARMOR_PLATE + plates) * ARMOR_PLATE)
}

/// The ammo pool a weapon draws from, or the key itself when it names a pool.
pub(in crate::games::washoi) fn ammo_pool(key: &str) -> &str {
    WEAPONS
        .iter()
        .find(|(id, _, _)| *id == key)
        .map_or(key, |(_, pool, _)| pool)
}

/// The weapons that draw from a pool.
pub(in crate::games::washoi) fn ammo_consumers(pool: &str) -> impl Iterator<Item = &'static str> {
    WEAPONS
        .iter()
        .filter(move |(_, known, _)| *known == pool)
        .map(|(id, _, _)| *id)
}

/// The largest reserve any consumer of the pool holds, zero when unknown.
pub(in crate::games::washoi) fn ammo_capacity(pool: &str) -> f64 {
    WEAPONS
        .iter()
        .filter(|(_, known, _)| *known == pool)
        .map(|(_, _, capacity)| *capacity)
        .fold(0.0, f64::max)
}

/// What an ammo pickup of this pool grants.
pub(in crate::games::washoi) fn ammo_amount(pool: &str) -> Option<f64> {
    AMMO.iter()
        .find(|(known, _)| *known == pool)
        .map(|(_, amount)| *amount)
}

/// `ammoNeedFraction`: how badly the bot needs this ammo, 1 empty, 0 full or
/// unknown. The key is a pool or a weapon. The pool level is the largest
/// holding among its consumers, as the bot's ammo is kept per weapon. With
/// a grant, only the part that would not be wasted counts.
pub(in crate::games::washoi) fn ammo_need(
    held: impl Fn(&str) -> Option<f64>,
    key: &str,
    grant: Option<f64>,
) -> f64 {
    let pool = ammo_pool(key);
    let capacity = ammo_capacity(pool);
    if capacity <= 0.0 {
        return 0.0;
    }
    let level = ammo_consumers(pool).filter_map(&held).fold(0.0, f64::max);
    let room = (capacity - level).max(0.0);
    let usable = grant.map_or(room, |grant| room.min(grant));
    clamp01(usable / capacity)
}

/// `powerupValue`: Quad just below 1, as a desperate health need may still
/// outrank it, anything else low but not zero. Washoi's Quad multiplies by 4.
pub(in crate::games::washoi) fn powerup_value(kind: &str) -> f64 {
    if kind == "quad" { 0.95 } else { 0.4 }
}

/// `denialValue`: how much the bot would rather the enemy did not get this.
pub(in crate::games::washoi) fn denial_value(kind: PickupKind, item: &str) -> f64 {
    match kind {
        PickupKind::Powerup => powerup_value(item),
        PickupKind::Health if matches!(item, "mega" | "ultra") => 0.7,
        PickupKind::Health => 0.15,
        PickupKind::Armor => match item {
            "ra" => 0.8,
            "ya" => 0.4,
            _ => 0.1,
        },
        PickupKind::Weapon => 0.25,
        PickupKind::Ammo => 0.05,
    }
}

/// `isMajorItem`: powerups, green, yellow, and red armor, Mega and Ultra
/// Health, what an item route is planned around.
pub(in crate::games::washoi) fn is_major(kind: PickupKind, item: &str) -> bool {
    match kind {
        PickupKind::Powerup => true,
        PickupKind::Armor => matches!(item, "ga" | "ya" | "ra"),
        PickupKind::Health => matches!(item, "mega" | "ultra"),
        PickupKind::Weapon | PickupKind::Ammo => false,
    }
}

/// `clamp01`: the consideration contract's [0, 1], and 0 for NaN.
pub(in crate::games::washoi) fn clamp01(value: f64) -> f64 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_follow_the_shared_item_rules() {
        assert_eq!(health_after(98, "small"), 103);
        assert_eq!(health_after(90, "medium"), 100);
        assert_eq!(health_after(150, "mega"), 200);
        assert_eq!(health_after(100, "large"), 100);
        // Plates: 30 armor holds one full plate, so green tops it to 50.
        assert_eq!(armor_after(30, "ga"), 50);
        assert_eq!(armor_after(30, "ra"), 100);
        assert_eq!(armor_after(99, "shard"), 100);
        assert_eq!(armor_after(100, "ya"), 100);
    }

    #[test]
    fn ammo_need_reads_the_pool_and_caps_by_the_grant() {
        let held = |weapon: &str| (weapon == "rlauncher").then_some(18.0);
        // 2 of a 10-rocket box land at 18/20.
        assert!((ammo_need(held, "rockets", Some(10.0)) - 0.1).abs() < 1e-9);
        assert!((ammo_need(held, "rlauncher", None) - 0.1).abs() < 1e-9);
        assert_eq!(ammo_need(|_| None, "cells", None), 1.0);
        assert_eq!(ammo_need(|_| None, "unknown", None), 0.0);
        assert_eq!(ammo_amount("lasers"), Some(100.0));
        assert_eq!(intern("rlauncher"), "rlauncher");
        assert_eq!(intern("bfg"), "");
    }
}

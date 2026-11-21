//! Weapon knowledge from TypeScript `bot/BotWeaponProfile.ts`. The table
//! holds what `getWeaponProfile` builds from Washoi's weapon definitions,
//! dumped through vite-node from the SHAG checkout. `select` ports
//! `selectBestWeapon`.

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::games::washoi) struct Profile {
    pub(in crate::games::washoi) id: &'static str,
    pub(in crate::games::washoi) melee: bool,
    /// Zero for hitscan weapons.
    pub(in crate::games::washoi) projectile_speed: f64,
    pub(in crate::games::washoi) splash: bool,
    dps: f64,
    min_range: f64,
    max_range: f64,
    /// Alt-fire aims down sights and tightens the shot.
    pub(in crate::games::washoi) aims_down_sights: bool,
    /// Server ticks aim must be held before a shot counts as aimed.
    pub(in crate::games::washoi) ads_ready_ticks: u32,
    /// `fireIntervalTicks`: server ticks between shots.
    pub(in crate::games::washoi) fire_interval_ticks: u32,
    /// `chargeMinTicks` and `chargeMaxTicks` of an alt-fire charged
    /// projectile, in server ticks.
    pub(in crate::games::washoi) charge: Option<(u32, u32)>,
    /// `damagePerHit`, all pellets together, and `pelletCount`.
    pub(in crate::games::washoi) damage_per_hit: f64,
    pub(in crate::games::washoi) pellets: u32,
}

impl Profile {
    const fn every(self, fire_interval_ticks: u32) -> Self {
        Self {
            fire_interval_ticks,
            ..self
        }
    }

    const fn hits(self, damage_per_hit: f64, pellets: u32) -> Self {
        Self {
            damage_per_hit,
            pellets,
            ..self
        }
    }

    /// `isHitscan`: no projectile to fly.
    pub(in crate::games::washoi) fn hitscan(&self) -> bool {
        self.projectile_speed == 0.0
    }

    pub(in crate::games::washoi) fn max_range(&self) -> f64 {
        self.max_range
    }

    pub(in crate::games::washoi) fn dps(&self) -> f64 {
        self.dps
    }

    /// Within the ideal range, widened by `slack` of it on each side.
    pub(in crate::games::washoi) fn fits(&self, distance: f64, slack: f64) -> bool {
        distance >= self.min_range * (1.0 - slack) && distance <= self.max_range * (1.0 + slack)
    }
}

const fn profile(
    id: &'static str,
    projectile_speed: f64,
    splash: bool,
    dps: f64,
    (min_range, max_range): (f64, f64),
    ads_ready_ticks: Option<u32>,
) -> Profile {
    Profile {
        id,
        melee: false,
        projectile_speed,
        splash,
        dps,
        min_range,
        max_range,
        aims_down_sights: ads_ready_ticks.is_some(),
        ads_ready_ticks: match ads_ready_ticks {
            Some(ticks) => ticks,
            None => 0,
        },
        fire_interval_ticks: 100,
        charge: None,
        damage_per_hit: 10.0,
        pellets: 1,
    }
}

/// Alt-fire charges a blast that flies at 2,500 u/s, so it aims with lead.
const BLADE: Profile = Profile {
    melee: true,
    charge: Some((63, 125)),
    ..profile("blade", 2500.0, false, 160.0, (0.0, 100.0), None)
        .every(63)
        .hits(80.0, 1)
};

const PROFILES: [Profile; 8] = [
    BLADE,
    profile("machinegun", 0.0, false, 100.0, (100.0, 3000.0), Some(0))
        .every(13)
        .hits(10.0, 1),
    profile("riotgun", 0.0, false, 112.5, (50.0, 500.0), Some(0))
        .every(100)
        .hits(90.0, 18),
    profile("plasmagun", 1750.0, true, 150.0, (200.0, 1200.0), Some(0))
        .every(13)
        .hits(15.0, 1),
    profile("electrobolt", 0.0, false, 60.0, (400.0, 5000.0), Some(47))
        .every(156)
        .hits(75.0, 1),
    profile("voltaic", 0.0, false, 140.0, (50.0, 850.0), Some(0))
        .every(6)
        .hits(7.0, 1),
    profile("rlauncher", 1814.0, true, 100.0, (200.0, 1200.0), Some(20))
        .every(100)
        .hits(80.0, 1),
    profile("glauncher", 1080.0, true, 100.0, (200.0, 1200.0), Some(20))
        .every(100)
        .hits(80.0, 1),
];

/// `DEFAULT_PROFILE`, for weapons the table doesn't know.
const UNKNOWN: Profile = profile("unknown", 0.0, false, 50.0, (100.0, 2000.0), None);

/// `getWeaponProfile`, with `canonicalWeaponId`'s one alias.
pub(in crate::games::washoi) fn profile_of(id: &str) -> Profile {
    let id = if id == "lasergun" { "voltaic" } else { id };
    PROFILES
        .into_iter()
        .find(|profile| profile.id == id)
        .unwrap_or(UNKNOWN)
}

/// `selectBestWeapon`: the 1-based slot to switch to, or 0 to keep the
/// current weapon, and the profile chosen. Weapons without ammo are skipped.
/// A missing ammo count means unlimited.
pub(in crate::games::washoi) fn select(
    weapons: &[String],
    ammo: impl Fn(&str) -> Option<f64>,
    current: &str,
    distance: f64,
) -> (u8, Profile) {
    let mut best = (0usize, profile_of(current));
    let mut best_score = f64::NEG_INFINITY;
    for (index, id) in weapons.iter().enumerate() {
        if id.is_empty() || ammo(id).is_some_and(|ammo| ammo <= 0.0) {
            continue;
        }
        let profile = profile_of(id);
        let mut score = profile.dps;
        if (profile.min_range..=profile.max_range).contains(&distance) {
            score *= 2.0;
        } else if distance < profile.min_range {
            score *= distance / profile.min_range;
        } else {
            score *= profile.max_range / distance;
        }
        if profile.melee && distance < 150.0 {
            score *= 3.0;
        }
        if profile.melee && distance > 200.0 {
            score = 0.0;
        }
        if score > best_score {
            best_score = score;
            best = (index + 1, profile);
        }
    }
    let (slot, profile) = best;
    if slot == 0 || weapons[slot - 1] == current {
        return (0, profile);
    }
    (u8::try_from(slot).unwrap_or(0), profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_by_range_and_skips_empty_weapons() {
        let weapons: Vec<String> = ["blade", "machinegun", "riotgun", "electrobolt"]
            .map(String::from)
            .into();
        let full = |_: &str| None;
        // Up close the blade wins, at riot range the riotgun, far off the machinegun.
        assert_eq!(select(&weapons, full, "machinegun", 80.0).1.id, "blade");
        assert_eq!(
            select(&weapons, full, "blade", 300.0),
            (3, profile_of("riotgun"))
        );
        assert_eq!(select(&weapons, full, "blade", 2500.0).0, 2);
        // Already holding the best: keep it.
        assert_eq!(select(&weapons, full, "riotgun", 300.0).0, 0);
        let no_riot = |id: &str| (id == "riotgun").then_some(0.0);
        assert_eq!(select(&weapons, no_riot, "blade", 300.0).0, 2);
        assert_eq!(profile_of("lasergun").id, "voltaic");
        assert_eq!(profile_of("mystery").id, "unknown");
    }
}

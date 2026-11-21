//! Pure scores over one candidate goal, from TypeScript
//! `bot/goals/Consideration.ts` and `bot/goals/considerations/`.
//!
//! An attractor is a reason to pursue the goal and competes for the
//! maximum. A discount says the goal is expensive or unwise and multiplies
//! the result, so a close but worthless item can never win on proximity.
//! Every score is in [0, 1] and depends on its context alone.

use std::collections::HashMap;

use crate::games::washoi::{
    Vec3,
    agents::player::{
        combat::weapons,
        strategy::{
            goals::{PickupGoal, PickupKind},
            items::{self, clamp01},
            weights::Weights,
        },
    },
};

/// A candidate the arbiter scores: a map pickup or a gametype order.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::games::washoi) struct Goal {
    /// Stable identity from the target, never from the ranking, as the
    /// commitment window keys on it: `pickup:<id>` or the order's ID.
    pub(in crate::games::washoi) id: String,
    pub(in crate::games::washoi) position: Vec3,
    pub(in crate::games::washoi) pickup: Option<PickupGoal>,
    pub(in crate::games::washoi) order: Option<Order>,
    /// For a major item that is down but known to return: the server tick
    /// it comes back.
    pub(in crate::games::washoi) respawn_tick: Option<u32>,
}

/// What the considerations read from an `AgentDirective`.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::games::washoi) struct Order {
    pub(in crate::games::washoi) priority: f64,
    pub(in crate::games::washoi) urgency: f64,
    /// The teammate an `escort` order protects.
    pub(in crate::games::washoi) escort: Option<String>,
}

/// The bot itself, as it knows itself from its own combat and weapon state.
#[derive(Debug, Clone, Copy)]
pub(in crate::games::washoi) struct Me<'a> {
    pub(in crate::games::washoi) position: Vec3,
    pub(in crate::games::washoi) health: u16,
    pub(in crate::games::washoi) armor: u16,
    pub(in crate::games::washoi) weapons: &'a [String],
    /// Ammo held, by weapon ID.
    pub(in crate::games::washoi) ammo: &'a [(String, f64)],
}

impl Me<'_> {
    fn held(&self, weapon: &str) -> Option<f64> {
        self.ammo
            .iter()
            .find(|(id, _)| id == weapon)
            .map(|(_, ammo)| *ammo)
    }
}

/// What the bot perceives of the match around it.
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::games::washoi) struct Situation<'a> {
    pub(in crate::games::washoi) tick: u32,
    /// Server ticks the bot was recently hit.
    pub(in crate::games::washoi) recent_damage: &'a [u32],
    /// Enemies the bot believes are alive, by sight and hearing.
    pub(in crate::games::washoi) known_enemies: usize,
    /// Enemies on the scoreboard, seen or not.
    pub(in crate::games::washoi) scoreboard_enemies: usize,
    /// Living teammates and where they are.
    pub(in crate::games::washoi) allies: &'a [(&'a str, Vec3)],
    /// `OpponentModel`: how often an opponent takes each pickup, 0 to 1.
    pub(in crate::games::washoi) contested: Option<&'a HashMap<usize, f64>>,
}

/// `ConsiderationContext`. The route cost is injected, in world units, and
/// None means no route is known.
#[derive(Debug, Clone, Copy)]
pub(in crate::games::washoi) struct Context<'a> {
    pub(in crate::games::washoi) goal: &'a Goal,
    pub(in crate::games::washoi) me: Me<'a>,
    pub(in crate::games::washoi) situation: Situation<'a>,
    pub(in crate::games::washoi) route_cost: Option<f64>,
    pub(in crate::games::washoi) weights: Weights,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::games::washoi) enum Role {
    Attractor,
    Discount,
}

/// One consideration: its trace ID, role, the gametype weight that scales
/// it (none for physics or modifiers), and its score.
#[derive(Clone, Copy)]
pub(in crate::games::washoi) struct Consideration {
    pub(in crate::games::washoi) id: &'static str,
    pub(in crate::games::washoi) role: Role,
    pub(in crate::games::washoi) weight: Option<fn(&Weights) -> f64>,
    pub(in crate::games::washoi) score: fn(&Context) -> f64,
}

/// `ITEM_CONSIDERATIONS`, in the TypeScript order, which the trace keeps.
pub(in crate::games::washoi) const ALL: [Consideration; 9] = [
    Consideration {
        id: "item_value",
        role: Role::Attractor,
        weight: Some(|w| w.item_greed),
        score: item_value,
    },
    Consideration {
        id: "item_denial",
        role: Role::Attractor,
        weight: Some(|w| w.item_denial),
        score: item_denial,
    },
    Consideration {
        id: "contested_item",
        role: Role::Attractor,
        weight: Some(|w| w.item_denial),
        score: contested_item,
    },
    Consideration {
        id: "objective_push",
        role: Role::Attractor,
        weight: Some(|w| w.objective_push),
        score: objective_push,
    },
    Consideration {
        id: "objective_urgency",
        role: Role::Attractor,
        weight: Some(|w| w.objective_push),
        score: objective_urgency,
    },
    Consideration {
        id: "team_cohesion",
        role: Role::Attractor,
        weight: Some(|w| w.team_cohesion),
        score: team_cohesion,
    },
    Consideration {
        id: "carrier_protect",
        role: Role::Attractor,
        weight: Some(|w| w.carrier_protect),
        score: carrier_protect,
    },
    Consideration {
        id: "route_cost",
        role: Role::Discount,
        weight: None,
        score: route_cost,
    },
    Consideration {
        id: "danger",
        role: Role::Discount,
        weight: None,
        score: danger,
    },
];

// `ItemValue.ts`.
/// Health and armor gains count against this, above the 100 a Mega grants,
/// so that Quad can still outrank the largest ordinary pickup.
const POINTS_FULL_VALUE: f64 = 150.0;
/// The extra pull on health below 50.
const HEALTH_URGENCY: f64 = 1.5;
const HEALTH_URGENCY_THRESHOLD: f64 = 50.0;
/// Armor absorbs most damage and stacks on health.
const ARMOR_POINT_WEIGHT: f64 = 1.15;
const DPS_FULL_VALUE: f64 = 150.0;
const MELEE_WEAPON: &str = "blade";
/// A new weapon's value by guns already owned: 0, 1, 2, then 3 or more.
const EXTRA_WEAPON_SCALE: [f64; 4] = [1.0, 1.0, 0.5, 0.3];
const AMMO_UNOWNED_WEAPON_SCALE: f64 = 0.1;

/// `ItemValue`: what this bot gains from the item now, computed from the
/// grant rules, so overcap waste scores nothing. Major items take the
/// mode's bias.
pub(in crate::games::washoi) fn item_value(context: &Context) -> f64 {
    let Some(pickup) = context.goal.pickup else {
        return 0.0;
    };
    let value = raw_item_value(pickup, context.me);
    if pickup.is_major() {
        major_item_value(value, context.weights.major_item_bias)
    } else {
        value
    }
}

/// `majorItemValue`: above 1 the bias raises every major item toward 1,
/// below 1 it lowers them, and their order stays.
pub(in crate::games::washoi) fn major_item_value(value: f64, bias: f64) -> f64 {
    1.0 - (1.0 - clamp01(value)).powf(bias)
}

fn raw_item_value(pickup: PickupGoal, me: Me) -> f64 {
    let item = pickup.item.0;
    match pickup.kind {
        PickupKind::Health => {
            let gain = f64::from(items::health_after(me.health, item)) - f64::from(me.health);
            if gain <= 0.0 {
                return 0.0;
            }
            let deficit = clamp01(
                (HEALTH_URGENCY_THRESHOLD - f64::from(me.health)) / HEALTH_URGENCY_THRESHOLD,
            );
            clamp01(gain / POINTS_FULL_VALUE * (1.0 + HEALTH_URGENCY * deficit))
        }
        PickupKind::Armor => {
            let gain = f64::from(items::armor_after(me.armor, item)) - f64::from(me.armor);
            if gain <= 0.0 {
                return 0.0;
            }
            clamp01(gain / POINTS_FULL_VALUE * ARMOR_POINT_WEIGHT)
        }
        PickupKind::Weapon => {
            let dps = clamp01(weapons::profile_of(item).dps() / DPS_FULL_VALUE);
            if !me.weapons.iter().any(|owned| owned == item) {
                // The first gun is a capability; each one after is an option.
                let owned = me.weapons.iter().filter(|w| *w != MELEE_WEAPON).count();
                let scale = EXTRA_WEAPON_SCALE[owned.min(EXTRA_WEAPON_SCALE.len() - 1)];
                return clamp01((0.5 + 0.5 * dps) * scale);
            }
            clamp01(items::ammo_need(|w| me.held(w), item, None) * dps)
        }
        PickupKind::Ammo => {
            let need = items::ammo_need(|w| me.held(w), item, items::ammo_amount(item));
            if need <= 0.0 {
                return 0.0;
            }
            let owns = items::ammo_consumers(items::ammo_pool(item))
                .any(|consumer| me.weapons.iter().any(|w| w == consumer));
            clamp01(need * if owns { 1.0 } else { AMMO_UNOWNED_WEAPON_SCALE })
        }
        PickupKind::Powerup => items::powerup_value(item),
    }
}

/// `ItemDenial`: denial wanes below 60 health, where the bot should solve
/// its own problem first.
const HEALTH_COMFORT: f64 = 60.0;

/// `ItemDenial`: the worth of the enemy not having this, while there is an
/// enemy to deny.
pub(in crate::games::washoi) fn item_denial(context: &Context) -> f64 {
    let Some(pickup) = context.goal.pickup else {
        return 0.0;
    };
    let base = items::denial_value(pickup.kind, pickup.item.0);
    if base <= 0.0 || context.situation.scoreboard_enemies == 0 {
        return 0.0;
    }
    clamp01(base * clamp01(f64::from(context.me.health) / HEALTH_COMFORT))
}

/// `ContestedItem`: an item the opponent keeps taking, as this match taught.
pub(in crate::games::washoi) fn contested_item(context: &Context) -> f64 {
    let (Some(pickup), Some(contested)) = (context.goal.pickup, context.situation.contested) else {
        return 0.0;
    };
    clamp01(contested.get(&pickup.id).copied().unwrap_or(0.0))
}

/// `ObjectivePush`: an order that exists at all is worth a floor.
const DIRECTIVE_FLOOR: f64 = 0.25;

/// `ObjectivePush`: the order's own priority, floored.
pub(in crate::games::washoi) fn objective_push(context: &Context) -> f64 {
    context.goal.order.as_ref().map_or(0.0, |order| {
        clamp01(DIRECTIVE_FLOOR + (1.0 - DIRECTIVE_FLOOR) * clamp01(order.priority))
    })
}

/// `ObjectiveUrgency`: the order's time pressure.
pub(in crate::games::washoi) fn objective_urgency(context: &Context) -> f64 {
    context
        .goal
        .order
        .as_ref()
        .map_or(0.0, |order| clamp01(order.urgency))
}

// `TeamPlay.ts`.
const SUPPORT_RADIUS: f64 = 900.0;
const CROWDING_RADIUS: f64 = 200.0;

/// `TeamCohesion`: whether going there brings the bot into support range
/// of its nearest teammate, peaking just outside crowding range.
pub(in crate::games::washoi) fn team_cohesion(context: &Context) -> f64 {
    let nearest = context
        .situation
        .allies
        .iter()
        .map(|(_, at)| distance(context.goal.position, *at))
        .fold(f64::INFINITY, f64::min);
    if !nearest.is_finite() || nearest <= CROWDING_RADIUS || nearest >= SUPPORT_RADIUS {
        return 0.0;
    }
    clamp01(1.0 - (nearest - CROWDING_RADIUS) / (SUPPORT_RADIUS - CROWDING_RADIUS))
}

/// `CarrierProtect`: an escort order matters most while the carrier is far
/// from the bot with enemies about.
pub(in crate::games::washoi) fn carrier_protect(context: &Context) -> f64 {
    let Some(carrier) = context
        .goal
        .order
        .as_ref()
        .and_then(|order| order.escort.as_deref())
    else {
        return 0.0;
    };
    let Some((_, at)) = context
        .situation
        .allies
        .iter()
        .find(|(entity, _)| *entity == carrier)
    else {
        return 0.0;
    };
    let need = clamp01(distance(context.me.position, *at) / SUPPORT_RADIUS);
    let threat = if context.situation.known_enemies > 0 {
        1.0
    } else {
        0.3
    };
    clamp01(need * threat)
}

// `RouteCost.ts`.
/// The route cost at which the discount halves.
pub(in crate::games::washoi) const ROUTE_HALF_COST: f64 = 1200.0;
/// An order's half cost grows by this times its priority.
const OBJECTIVE_ROUTE_SCALE: f64 = 4.0;
const VERTICAL_PENALTY: f64 = 2.0;
pub(in crate::games::washoi) const DETOUR_FACTOR: f64 = 1.35;

/// `RouteCost`: distance discounts what the bot already wants. Orders
/// without a known route fall back to the straight line, as they must be
/// attempted; other goals without a route are gated out.
pub(in crate::games::washoi) fn route_cost(context: &Context) -> f64 {
    let half = route_half_cost(context);
    match context.route_cost {
        None if context.goal.order.is_some() => clamp01(
            1.0 / (1.0 + estimate_route_cost(context.me.position, context.goal.position) / half),
        ),
        None => 0.0,
        Some(cost) if cost <= 0.0 => 1.0,
        Some(cost) => clamp01(1.0 / (1.0 + cost / half)),
    }
}

/// An order is the job, so it tolerates a longer route by its priority. A
/// major item is worth a longer run where the mode favors major items.
fn route_half_cost(context: &Context) -> f64 {
    if let Some(order) = &context.goal.order {
        return ROUTE_HALF_COST * (1.0 + OBJECTIVE_ROUTE_SCALE * order.priority.max(0.0));
    }
    match context.goal.pickup {
        Some(pickup) if pickup.is_major() => {
            ROUTE_HALF_COST * context.weights.major_item_bias.max(1.0)
        }
        _ => ROUTE_HALF_COST,
    }
}

/// `estimateRouteCost`: the straight line, with climbing dearer, inflated by
/// a detour factor.
pub(in crate::games::washoi) fn estimate_route_cost(from: Vec3, to: Vec3) -> f64 {
    let horizontal = (to.x - from.x).hypot(to.z - from.z);
    (horizontal + (to.y - from.y).abs() * VERTICAL_PENALTY) * DETOUR_FACTOR
}

// `Danger.ts`.
/// A hit counts as recent for 250 ticks, 2 seconds.
const DAMAGE_MEMORY_TICKS: f64 = 250.0;
const CROWD_SATURATION: f64 = 3.0;
const WEIGHT_DAMAGE: f64 = 0.6;
const WEIGHT_CROWD: f64 = 0.4;

/// `Danger`: recent hits and known enemies, felt as much as the mode's risk
/// tolerance allows. 1 is safe.
pub(in crate::games::washoi) fn danger(context: &Context) -> f64 {
    let situation = context.situation;
    let heat = situation
        .recent_damage
        .iter()
        .filter_map(|&hit| situation.tick.checked_sub(hit))
        .map(f64::from)
        .filter(|age| *age <= DAMAGE_MEMORY_TICKS)
        .map(|age| 1.0 - age / DAMAGE_MEMORY_TICKS)
        .fold(0.0, f64::max);
    let crowd = clamp01(situation.known_enemies as f64 / CROWD_SATURATION);
    let danger = clamp01(WEIGHT_DAMAGE * heat + WEIGHT_CROWD * crowd);
    clamp01(1.0 - danger * (1.0 - clamp01(context.weights.risk_tolerance)))
}

fn distance(a: Vec3, b: Vec3) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}

#[cfg(test)]
pub(in crate::games::washoi) mod tests {
    use super::*;
    use crate::games::washoi::agents::player::strategy::{goals::Item, weights};

    pub(in crate::games::washoi) fn pickup(id: usize, kind: PickupKind, item: &str) -> Goal {
        Goal {
            id: format!("pickup:{id}"),
            position: Vec3::new(1000.0, 0.0, 0.0),
            pickup: Some(PickupGoal {
                id,
                kind,
                item: Item(items::intern(item)),
            }),
            order: None,
            respawn_tick: None,
        }
    }

    pub(in crate::games::washoi) fn me(health: u16, armor: u16) -> Me<'static> {
        Me {
            position: Vec3::new(0.0, 0.0, 0.0),
            health,
            armor,
            weapons: &[],
            ammo: &[],
        }
    }

    fn context<'a>(goal: &'a Goal, me: Me<'a>, weights: Weights) -> Context<'a> {
        Context {
            goal,
            me,
            situation: Situation {
                scoreboard_enemies: 1,
                ..Situation::default()
            },
            route_cost: Some(0.0),
            weights,
        }
    }

    #[test]
    fn item_value_counts_only_what_the_bot_would_gain() {
        let bubble = pickup(0, PickupKind::Health, "small");
        let mega = pickup(1, PickupKind::Health, "mega");
        let red = pickup(2, PickupKind::Armor, "ra");
        let quad = pickup(3, PickupKind::Powerup, "quad");
        let at =
            |goal, health, armor| item_value(&context(goal, me(health, armor), weights::DEFAULT));
        // 2 points of a 5 HP bubble at 98, and nothing at full overheal.
        assert!((at(&bubble, 98, 0) - 5.0 / 150.0).abs() < 1e-9);
        assert_eq!(at(&bubble, 200, 0), 0.0);
        // Dying lifts health past Quad; at 60 it doesn't.
        assert!(at(&mega, 10, 0) > at(&quad, 10, 0));
        assert!(at(&mega, 60, 0) < at(&quad, 60, 0));
        assert_eq!(at(&red, 100, 100), 0.0);
        assert!(at(&red, 100, 0) > at(&mega, 100, 0));
        // Duel's bias raises major items and keeps their order.
        let duel = weights::MatchContext {
            gametype: "duel".into(),
            ..Default::default()
        }
        .weights("");
        let biased = item_value(&context(&red, me(100, 50), duel));
        assert!(biased > at(&red, 100, 50));
    }

    #[test]
    fn weapons_matter_most_first_and_ammo_needs_its_gun() {
        let rl = pickup(0, PickupKind::Weapon, "rlauncher");
        let rockets = pickup(1, PickupKind::Ammo, "rockets");
        let blade = [String::from("blade")];
        let armed = [
            String::from("blade"),
            String::from("machinegun"),
            String::from("riotgun"),
            String::from("rlauncher"),
        ];
        let first = item_value(&context(
            &rl,
            Me {
                weapons: &blade,
                ..me(100, 0)
            },
            weights::DEFAULT,
        ));
        let ammo_unowned = item_value(&context(
            &rockets,
            Me {
                weapons: &blade,
                ..me(100, 0)
            },
            weights::DEFAULT,
        ));
        let held = [(String::from("rlauncher"), 5.0)];
        let ammo_owned = item_value(&context(
            &rockets,
            Me {
                weapons: &armed,
                ammo: &held,
                ..me(100, 0)
            },
            weights::DEFAULT,
        ));
        assert!(first > 0.5);
        assert!((ammo_owned - 0.5).abs() < 1e-9);
        assert!((ammo_unowned - 0.05).abs() < 1e-9);
    }

    #[test]
    fn discounts_shrink_with_distance_and_danger() {
        let red = pickup(0, PickupKind::Armor, "ra");
        let mut near = context(&red, me(100, 0), weights::DEFAULT);
        assert_eq!(route_cost(&near), 1.0);
        near.route_cost = Some(ROUTE_HALF_COST);
        assert!((route_cost(&near) - 0.5).abs() < 1e-9);
        near.route_cost = None;
        assert_eq!(route_cost(&near), 0.0);
        let hits = [100];
        near.situation.tick = 100;
        near.situation.recent_damage = &hits;
        near.situation.known_enemies = 3;
        // Full danger, half felt at risk tolerance 0.5.
        assert!((danger(&near) - 0.5).abs() < 1e-9);
        near.situation.tick = 400;
        assert!((danger(&near) - 0.8).abs() < 1e-9);
    }

    #[test]
    fn orders_pull_by_priority_and_teams_by_support_range() {
        let order = Goal {
            id: "plant".into(),
            position: Vec3::new(500.0, 0.0, 0.0),
            pickup: None,
            order: Some(Order {
                priority: 1.0,
                urgency: 0.4,
                escort: Some("carrier".into()),
            }),
            respawn_tick: None,
        };
        let allies = [("carrier", Vec3::new(900.0, 0.0, 0.0))];
        let mut at = context(&order, me(100, 0), weights::DEFAULT);
        at.situation.allies = &allies;
        assert_eq!(objective_push(&at), 1.0);
        assert_eq!(objective_urgency(&at), 0.4);
        // The carrier is 400 from the goal: inside support range.
        assert!((team_cohesion(&at) - (1.0 - 200.0 / 700.0)).abs() < 1e-9);
        // 900 away with nobody about: need 1, threat 0.3.
        assert!((carrier_protect(&at) - 0.3).abs() < 1e-9);
        // No route known: the straight line still counts for an order.
        at.route_cost = None;
        assert!(route_cost(&at) > 0.5);
        assert_eq!(item_value(&at), 0.0);
    }
}

//! HUNCH's feature schema `washoi.goal.v0` (`docs/research/HUNCH.md`): one
//! fixed-order vector per goal candidate, built from what the arbiter
//! already knows. Raw consideration scores, not weighted ones, so a model
//! learns its own weights.

use crate::games::washoi::agents::player::strategy::{
    arbiter::Trace, considerations::Context, goals::PickupKind, items,
};

pub(in crate::games::washoi) const SCHEMA: &str = "washoi.goal.v0";

/// Feature names, in vector order.
pub(in crate::games::washoi) const NAMES: [&str; 22] = [
    "kind_pickup",
    "kind_order",
    "item_value",
    "item_denial",
    "contested_item",
    "objective_push",
    "objective_urgency",
    "team_cohesion",
    "carrier_protect",
    "route_cost",
    "danger",
    "log_route_units",
    "reachable",
    "major",
    "health_gain",
    "armor_gain",
    "ammo_need",
    "timed",
    "timed_seconds_left",
    "health_deficit_x_route",
    "health_deficit_x_danger",
    "timed_slack",
];

pub(in crate::games::washoi) const WIDTH: usize = NAMES.len();

const TICK_RATE: f64 = 125.0;
const RUN_SPEED: f64 = 320.0;

/// Append one candidate's row. The trace holds its raw consideration
/// scores, in `considerations::ALL` order.
pub(in crate::games::washoi) fn push_row(context: &Context, trace: &Trace, out: &mut Vec<f32>) {
    let goal = context.goal;
    let me = context.me;
    let feature = |id: &str| trace.feature(id).unwrap_or(0.0);
    let pickup = goal.pickup;
    let route_units = context.route_cost;
    let (health_gain, armor_gain, ammo_need, major) =
        pickup.map_or((0.0, 0.0, 0.0, false), |pickup| {
            let item = pickup.item.0;
            let held = |weapon: &str| {
                me.ammo
                    .iter()
                    .find(|(id, _)| id == weapon)
                    .map(|(_, ammo)| *ammo)
            };
            match pickup.kind {
                PickupKind::Health => (
                    f64::from(items::health_after(me.health, item).saturating_sub(me.health))
                        / 100.0,
                    0.0,
                    0.0,
                    pickup.is_major(),
                ),
                PickupKind::Armor => (
                    0.0,
                    f64::from(items::armor_after(me.armor, item).saturating_sub(me.armor)) / 100.0,
                    0.0,
                    pickup.is_major(),
                ),
                PickupKind::Ammo | PickupKind::Weapon => {
                    (0.0, 0.0, items::ammo_need(held, item, None), false)
                }
                PickupKind::Powerup => (0.0, 0.0, 0.0, true),
            }
        });
    let seconds_left = goal.respawn_tick.map_or(0.0, |back| {
        f64::from(back.saturating_sub(context.situation.tick)) / TICK_RATE
    });
    let travel = route_units.map_or(0.0, |units| units / RUN_SPEED);
    let deficit = items::clamp01((100.0 - f64::from(me.health)) / 100.0);
    let row: [f64; WIDTH] = [
        f64::from(u8::from(pickup.is_some())),
        f64::from(u8::from(goal.order.is_some())),
        feature("item_value"),
        feature("item_denial"),
        feature("contested_item"),
        feature("objective_push"),
        feature("objective_urgency"),
        feature("team_cohesion"),
        feature("carrier_protect"),
        feature("route_cost"),
        feature("danger"),
        route_units.map_or(0.0, |units| (1.0 + units.max(0.0)).ln()),
        f64::from(u8::from(route_units.is_some())),
        f64::from(u8::from(major)),
        health_gain,
        armor_gain,
        ammo_need,
        f64::from(u8::from(goal.respawn_tick.is_some())),
        seconds_left,
        deficit * feature("route_cost"),
        deficit * feature("danger"),
        if goal.respawn_tick.is_some() {
            seconds_left - travel
        } else {
            0.0
        },
    ];
    out.extend(row.iter().map(|value| *value as f32));
}

/// The names as a model artifact stores them.
#[cfg(test)]
pub(in crate::games::washoi) fn names() -> Vec<String> {
    NAMES.iter().map(|name| (*name).to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::agents::player::strategy::{
        arbiter::Arbiter,
        considerations::{
            self, Situation,
            tests::{me, pickup},
        },
        weights,
    };
    use crate::games::washoi::random::Random;

    #[test]
    fn builds_one_row_per_candidate_in_schema_order() {
        assert_eq!(considerations::ALL.len(), 9);
        let goals = [pickup(0, PickupKind::Armor, "ra")];
        let me = me(40, 0);
        let context = Context {
            goal: &goals[0],
            me,
            situation: Situation::default(),
            route_cost: Some(1200.0),
            weights: weights::DEFAULT,
        };
        let mut arbiter = Arbiter::new(&considerations::ALL, 0.0);
        let choice = arbiter.select(&goals, |_| context, 0, &[], &mut Random::new(1));
        let mut row = Vec::new();
        push_row(&context, &choice.traces[0], &mut row);
        assert_eq!(row.len(), WIDTH);
        assert_eq!(row[0], 1.0);
        assert!((row[9] - 0.5).abs() < 1e-6, "route_cost {}", row[9]);
        assert_eq!(row[12], 1.0);
        assert_eq!((row[13], row[15]), (1.0, 1.0));
        assert!((row[19] - 0.6 * 0.5).abs() < 1e-6);
    }
}

//! Maps one reconstructed Washoi snapshot into the brain's `Observation`.
//!
//! Reference: `src/games/fps/bot/BotClient.ts` hands the brain the client's
//! reconstructed `GameState` as is. The fields here are the ones the
//! pre-separation brain at `a272bda` read from it: each entity's legacy
//! `position` component and its `velocity`, the scoreboard teams, the pickup
//! mask, and the snapshot's one-tick events in wire order, with an item
//! picker named by its entity key.

use shagger_shared::net::game_state::GameState;

use super::SnapshotEvent;
use crate::games::washoi::{Body, Observation, PlayerView};

/// One bot's view of a reconstructed snapshot. Entities and players keep
/// snapshot order, and events keep wire order.
///
/// The position is the legacy `position` component only, as the brain read
/// it before the separation. A rotation-only `transform` doesn't move a body.
pub fn observe(state: &GameState<SnapshotEvent>) -> Observation {
    Observation {
        tick: state.tick,
        players: state
            .players
            .values()
            .map(|player| PlayerView {
                entity_id: player.entity_id.clone(),
                name: player.name.clone(),
                team: player.team.clone(),
                score: player.score,
                deaths: player.deaths,
            })
            .collect(),
        bodies: state
            .entities
            .iter()
            .map(|(id, entity)| {
                (
                    id.to_owned(),
                    Body {
                        position: entity.position,
                        velocity: entity.velocity,
                    },
                )
            })
            .collect(),
        pickup_active: state.pickup_active_mask.clone(),
        events: state.events.iter().map(entity_keys).collect(),
        acked_input: state.acked_input_seq,
    }
}

/// `CLIENT_KEY_PREFIX` in `ClientEntityKey.ts`.
const CLIENT_KEY_PREFIX: &str = "player_";

/// The server names an `item_pickup` picker by its numeric entity ID, and
/// every other event by the client entity key the snapshot's entities use.
/// Give the picker as its key too, as `BotPerception`'s `pickerKey` does.
pub fn entity_keys(event: &SnapshotEvent) -> SnapshotEvent {
    match event {
        SnapshotEvent::ItemPickup {
            pickup_id,
            weapon_id,
            entity_id,
            dropped_weapon_id,
            at,
        } if !entity_id.is_empty() && !entity_id.starts_with(CLIENT_KEY_PREFIX) => {
            SnapshotEvent::ItemPickup {
                pickup_id: *pickup_id,
                weapon_id: weapon_id.clone(),
                entity_id: format!("{CLIENT_KEY_PREFIX}{entity_id}"),
                dropped_weapon_id: dropped_weapon_id.clone(),
                at: *at,
            }
        }
        _ => event.clone(),
    }
}

#[cfg(test)]
mod tests {
    use shagger_shared::net::game_state::{Entity, PlayerState, Transform, Vec3, Velocity};

    use super::*;

    fn at(x: f64) -> Vec3 {
        Vec3 { x, y: 0.0, z: 0.0 }
    }

    #[test]
    fn keeps_snapshot_order_bodies_teams_mask_and_wire_order_events() {
        let mut state = GameState::<SnapshotEvent> {
            tick: 42,
            pickup_active_mask: vec![true, false, true],
            ..GameState::default()
        };
        let moving = Velocity {
            linear: at(3.0),
            grounded: true,
            ..Velocity::default()
        };
        state.entities.insert(
            "player_2",
            Entity {
                position: Some(at(2.0)),
                velocity: Some(moving),
                ..Entity::default()
            },
        );
        state.entities.insert(
            "proj_7",
            Entity {
                position: Some(at(7.0)),
                ..Entity::default()
            },
        );
        // A turn without movement sends a transform but no position.
        state.entities.insert(
            "player_1",
            Entity {
                transform: Some(Transform {
                    position: at(9.0),
                    rotation: [0.1, 0.2, 0.0, 1.0],
                }),
                ..Entity::default()
            },
        );
        for (id, team) in [("player_2", "red"), ("player_1", "blue")] {
            state.players.insert(
                id,
                PlayerState {
                    entity_id: id.to_owned(),
                    team: team.to_owned(),
                    ..PlayerState::default()
                },
            );
        }
        let combat = |entity_id: &str, dead| SnapshotEvent::CombatState {
            entity_id: entity_id.to_owned(),
            health: 0,
            armor: 0,
            dead,
        };
        state.events = vec![combat("player_1", true), combat("player_1", false)];

        let observation = observe(&state);

        assert_eq!(observation.tick, 42);
        let ids: Vec<_> = observation
            .bodies
            .iter()
            .map(|(id, _)| id.as_str())
            .collect();
        assert_eq!(ids, ["player_2", "proj_7", "player_1"]);
        assert_eq!(
            observation.body("player_2"),
            Some(&Body {
                position: Some(at(2.0)),
                velocity: Some(moving),
            })
        );
        assert_eq!(observation.body("player_1"), Some(&Body::default()));
        assert_eq!(observation.player("player_1").unwrap().team, "blue");
        assert_eq!(observation.players[0].entity_id, "player_2");
        assert_eq!(observation.pickup_active, [true, false, true]);
        assert_eq!(observation.events, state.events);
    }

    #[test]
    fn an_empty_snapshot_observes_nothing() {
        let observation = observe(&GameState::default());
        assert_eq!(observation, Observation::default());
    }

    #[test]
    fn names_an_item_picker_by_its_entity_key() {
        use crate::games::washoi::hosting::net::Point;
        let at = Point {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        let pickup = |entity: &str| SnapshotEvent::ItemPickup {
            pickup_id: 3,
            weapon_id: String::new(),
            entity_id: entity.into(),
            dropped_weapon_id: String::new(),
            at,
        };
        assert_eq!(entity_keys(&pickup("7")), pickup("player_7"));
        assert_eq!(entity_keys(&pickup("player_7")), pickup("player_7"));
    }
}

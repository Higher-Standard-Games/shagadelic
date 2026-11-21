//! What the bot learns of its opponents' item habits: the major items it
//! heard or saw them take, from TypeScript `bot/BotPerception.ts`
//! (`enemyMajorItems`), the takes of `strategy/MatchRead.ts`, and
//! `strategy/OpponentModel.ts`.
//!
//! No event names who takes armor or health, for a person's client either.
//! A major pickup is heard across 2,500 units and the event names the
//! picker. An item seen vanishing is credited to the enemy standing at it,
//! or in a Duel to the one opponent.

use std::collections::HashMap;

use crate::games::washoi::{
    Observation, Sight, Vec3, agents::player::strategy::goals::PickupGoal,
    hosting::net::protocol::SnapshotEvent,
};

const TICK_RATE: u32 = 125;
/// `HEARING_RANGE.majorPickup`.
const MAJOR_PICKUP_RANGE: f64 = 2500.0;
/// `TAKER_RADIUS`: an enemy this close to an item seen vanishing took it.
const TAKER_RADIUS: f64 = 200.0;
/// `SELF_TAKE_RADIUS`: one vanishing this close, the bot took itself.
const SELF_TAKE_RADIUS: f64 = 180.0;
/// `HABIT_WINDOW_SECONDS`.
const HABIT_WINDOW_TICKS: u32 = 180 * TICK_RATE;
/// `MAJOR_ITEM_MEMORY_TICKS` in `BotMood.ts`: an enemy carries a major item
/// for 30 seconds after taking it, unless it dies.
const ARMED_TICKS: u32 = 30 * TICK_RATE;
const EYE_HEIGHT: f64 = 32.0;

#[derive(Debug, Clone, PartialEq)]
struct Take {
    by: String,
    pickup: usize,
    tick: u32,
}

#[derive(Default)]
pub(in crate::games::washoi) struct Opponents {
    takes: Vec<Take>,
    /// The last take credited to each enemy until it dies, for the mood.
    armed: HashMap<String, u32>,
    /// The pickup mask of the last snapshot, to see items vanish.
    last_mask: Vec<bool>,
}

/// What the bot knows of the scene for one snapshot.
pub(in crate::games::washoi) struct Scene<'a> {
    pub(in crate::games::washoi) own: &'a str,
    pub(in crate::games::washoi) position: Option<Vec3>,
    /// Living teammates, who are never credited.
    pub(in crate::games::washoi) allies: &'a [&'a str],
    /// Opponents on the scoreboard, by entity.
    pub(in crate::games::washoi) opponents: &'a [&'a str],
    pub(in crate::games::washoi) pickups: &'a [(PickupGoal, [f64; 3])],
    pub(in crate::games::washoi) sight: Option<&'a Sight>,
}

impl Opponents {
    /// Read one snapshot. Returns the takes credited in it: who took which
    /// pickup.
    pub(in crate::games::washoi) fn observe(
        &mut self,
        world: &Observation,
        scene: &Scene,
    ) -> Vec<(String, usize)> {
        let tick = world.tick;
        let before = self.takes.len();
        let major = |id: usize| {
            scene
                .pickups
                .iter()
                .find(|(pickup, _)| pickup.id == id && pickup.is_major())
        };
        let enemy = |entity: &str| {
            entity != scene.own && !scene.allies.contains(&entity) && !entity.is_empty()
        };
        for event in &world.events {
            match event {
                SnapshotEvent::ItemPickup {
                    pickup_id,
                    entity_id,
                    at,
                    ..
                } => {
                    let Ok(id) = usize::try_from(*pickup_id) else {
                        continue;
                    };
                    let heard = scene.position.is_some_and(|me| {
                        let at = Vec3::new(at.x, at.y, at.z);
                        distance(me, at) <= MAJOR_PICKUP_RANGE
                    });
                    if heard && enemy(entity_id) && major(id).is_some() {
                        self.credit(entity_id, id, tick);
                    }
                }
                SnapshotEvent::Death { entity_id, .. } => {
                    self.armed.remove(entity_id);
                }
                _ => {}
            }
        }
        // Items seen vanishing.
        if let Some(me) = scene.position {
            let lone = match scene.opponents {
                [one] => Some(*one),
                _ => None,
            };
            let vanished: Vec<usize> = self
                .last_mask
                .iter()
                .zip(&world.pickup_active)
                .enumerate()
                .filter(|(_, (before, now))| **before && !**now)
                .map(|(id, _)| id)
                .collect();
            for id in vanished {
                let Some((_, at)) = major(id) else {
                    continue;
                };
                let at = Vec3::new(at[0], at[1], at[2]);
                if distance(me, at) <= SELF_TAKE_RADIUS || !in_sight(scene.sight, me, at) {
                    continue;
                }
                let taker = world
                    .bodies
                    .iter()
                    .filter(|(entity, _)| scene.opponents.contains(&entity.as_str()))
                    .filter_map(|(entity, body)| {
                        Some((entity.as_str(), distance(body.position?, at)))
                    })
                    .filter(|(_, d)| *d <= TAKER_RADIUS)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(entity, _)| entity)
                    .or(lone);
                if let Some(taker) = taker {
                    self.credit(taker, id, tick);
                }
            }
        }
        if !world.pickup_active.is_empty() {
            self.last_mask.clone_from(&world.pickup_active);
        }
        let credited = self.takes[before..]
            .iter()
            .map(|take| (take.by.clone(), take.pickup))
            .collect();
        self.takes
            .retain(|take| tick.saturating_sub(take.tick) <= HABIT_WINDOW_TICKS);
        credited
    }

    fn credit(&mut self, by: &str, pickup: usize, tick: u32) {
        // One pickup heard and seen in the same snapshot counts once.
        if self
            .takes
            .iter()
            .any(|take| take.by == by && take.pickup == pickup && take.tick == tick)
        {
            return;
        }
        self.takes.push(Take {
            by: by.to_owned(),
            pickup,
            tick,
        });
        self.armed.insert(by.to_owned(), tick);
    }

    /// `OpponentModel.contested`: pickups an opponent took at least twice in
    /// the last three minutes, 0.5 at two takes and 1 from three.
    pub(in crate::games::washoi) fn contested(&self) -> HashMap<usize, f64> {
        let mut counts: HashMap<(&str, usize), u32> = HashMap::new();
        for take in &self.takes {
            *counts.entry((take.by.as_str(), take.pickup)).or_default() += 1;
        }
        let mut contested: HashMap<usize, f64> = HashMap::new();
        for ((_, pickup), count) in counts {
            if count >= 2 {
                let strength = (f64::from(count - 1) / 2.0).min(1.0);
                let entry = contested.entry(pickup).or_default();
                *entry = entry.max(strength);
            }
        }
        contested
    }

    /// Major items enemies took in the last seconds, before `tick`.
    pub(in crate::games::washoi) fn takes(&self, tick: u32, seconds: f64) -> usize {
        let from = tick.saturating_sub((seconds * f64::from(TICK_RATE)).round() as u32);
        self.takes.iter().filter(|take| take.tick >= from).count()
    }

    /// An enemy took a major item in the last 30 seconds and has not died
    /// since: `BotMood`'s armed enemy.
    pub(in crate::games::washoi) fn armed_enemy(&self, tick: u32) -> bool {
        self.armed
            .values()
            .any(|&at| tick.saturating_sub(at) < ARMED_TICKS)
    }
}

/// The bot sees the item's spot from its eye, without the static map in
/// the way. Without a sight file, nothing is seen.
fn in_sight(sight: Option<&Sight>, me: Vec3, at: Vec3) -> bool {
    sight.is_some_and(|sight| {
        sight.clear(
            [me.x, me.y + EYE_HEIGHT, me.z],
            [at.x, at.y + EYE_HEIGHT, at.z],
        )
    })
}

fn distance(a: Vec3, b: Vec3) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::hosting::net::protocol::Point;
    use crate::games::washoi::observation::Body;

    fn pickups() -> Vec<(PickupGoal, [f64; 3])> {
        vec![
            (
                serde_json::from_str(r#"{"id":0,"kind":"armor","item":"ra"}"#).unwrap(),
                [1000.0, 0.0, 0.0],
            ),
            (
                serde_json::from_str(r#"{"id":1,"kind":"ammo","item":"rockets"}"#).unwrap(),
                [0.0, 0.0, 1000.0],
            ),
        ]
    }

    fn heard(tick: u32, entity: &str, id: i16) -> Observation {
        Observation {
            tick,
            events: vec![SnapshotEvent::ItemPickup {
                pickup_id: id,
                weapon_id: String::new(),
                entity_id: entity.into(),
                dropped_weapon_id: String::new(),
                at: Point {
                    x: 1000.0,
                    y: 0.0,
                    z: 0.0,
                },
            }],
            ..Observation::default()
        }
    }

    #[test]
    fn learns_which_major_items_an_opponent_keeps_taking() {
        let pickups = pickups();
        let scene = Scene {
            own: "bot",
            position: Some(Vec3::new(0.0, 0.0, 0.0)),
            allies: &["mate"],
            opponents: &["foe"],
            pickups: &pickups,
            sight: None,
        };
        let mut opponents = Opponents::default();
        opponents.observe(&heard(100, "foe", 0), &scene);
        // Ammo is not a major item, and allies and the bot are not credited.
        opponents.observe(&heard(110, "foe", 1), &scene);
        opponents.observe(&heard(120, "mate", 0), &scene);
        opponents.observe(&heard(130, "bot", 0), &scene);
        assert!(opponents.contested().is_empty());
        assert!(opponents.armed_enemy(200));
        opponents.observe(&heard(4000, "foe", 0), &scene);
        assert_eq!(opponents.contested().get(&0), Some(&0.5));
        opponents.observe(&heard(8000, "foe", 0), &scene);
        assert_eq!(opponents.contested().get(&0), Some(&1.0));
        // Death disarms; three minutes on the habit is forgotten.
        let mut death = Observation {
            tick: 8100,
            ..Observation::default()
        };
        let origin = Point {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        death.events.push(SnapshotEvent::Death {
            entity_id: "foe".into(),
            killer_id: "bot".into(),
            weapon_id: "rlauncher".into(),
            damage_type: String::new(),
            final_damage: 0,
            at: origin,
            vel: origin,
            kb_dir: origin,
            kill_weapon: None,
            hit_location: None,
        });
        opponents.observe(&death, &scene);
        assert!(!opponents.armed_enemy(8101));
        let later = Observation {
            tick: 8000 + HABIT_WINDOW_TICKS + 1,
            ..Observation::default()
        };
        opponents.observe(&later, &scene);
        assert!(opponents.contested().is_empty());
    }

    #[test]
    fn credits_an_item_seen_vanishing_to_the_enemy_beside_it() {
        let pickups = pickups();
        let sight = Sight::parse(&crate::games::washoi::perception::sight::tests::encode(
            "test",
            &[],
            &[],
        ))
        .unwrap();
        let scene = Scene {
            own: "bot",
            position: Some(Vec3::new(0.0, 0.0, 0.0)),
            allies: &[],
            opponents: &["foe", "other"],
            pickups: &pickups,
            sight: Some(&sight),
        };
        let mut opponents = Opponents::default();
        let up = Observation {
            tick: 10,
            pickup_active: vec![true, true],
            ..Observation::default()
        };
        opponents.observe(&up, &scene);
        let mut gone = Observation {
            tick: 11,
            pickup_active: vec![false, true],
            ..Observation::default()
        };
        gone.bodies.push((
            "foe".into(),
            Body {
                position: Some(Vec3::new(1050.0, 0.0, 0.0)),
                velocity: None,
            },
        ));
        opponents.observe(&gone, &scene);
        assert!(opponents.armed_enemy(12));
        assert_eq!(opponents.takes.len(), 1);
        assert_eq!(opponents.takes[0].by, "foe");
    }
}

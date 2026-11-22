//! What a bot notices of its enemies, after TypeScript `bot/BotPerception.ts`
//! `_hear` and `_sortPlayers`. An enemy counts as seen when it is in line of
//! sight and was in view, seen within the last second, or just heard, and
//! stays seen for half the reaction time. The view is 55° to each side,
//! 70° for an enemy running past. Sounds carry as far as the client would
//! play them loud enough: shots, beams, pain, pickups, footsteps, jumps, and
//! explosions next to someone. Being hit reveals the attacker.

pub(in crate::games::washoi) mod collision;
pub(in crate::games::washoi) mod estimate;
pub(in crate::games::washoi) mod sight;

use std::f64::consts::PI;

use shagger_shared::net::game_state::Vec3;

use crate::games::washoi::{
    Sight,
    hosting::net::protocol::{Point, SnapshotEvent},
};

/// Server ticks per second.
const TICK_RATE: f64 = 125.0;
const DEG: f64 = PI / 180.0;
const VIEW_HALF_ANGLE: f64 = 55.0 * DEG;
const PERIPHERAL_HALF_ANGLE: f64 = 70.0 * DEG;
/// `KEEP_SEEN_TICKS`: one second.
const KEEP_SEEN_TICKS: u32 = 125;
const RUNNING_SPEED: f64 = 250.0;
const FOOTSTEP_SPEED: f64 = 70.0;
const JUMP_SPEED: f64 = 60.0;
const MAX_SIGHT_CHECKS: usize = 8;
/// `HEARING_RANGE`. Movement sounds carry `125 · volume / 0.125` units.
const FIRE_RANGE: f64 = 1200.0;
const LOUD_FIRE_RANGE: f64 = 2000.0;
const EXPLOSION_RANGE: f64 = 1500.0;
const FOOTSTEP_RANGE: f64 = 500.0;
const JUMP_RANGE: f64 = 720.0;
const PAIN_RANGE: f64 = 800.0;
const PICKUP_RANGE: f64 = 900.0;
const EXPLOSION_REVEAL_RADIUS: f64 = 200.0;
const LOUD_WEAPONS: [&str; 3] = ["rlauncher", "glauncher", "electrobolt"];
/// `BotVision.ts` `EYE_HEIGHT`: sight runs from here to 0.8 of it on the target.
const EYE_HEIGHT: f64 = 32.0;

/// An enemy the snapshot holds, whether or not the bot notices it.
#[derive(Clone, Copy, Debug)]
pub(in crate::games::washoi) struct Enemy<'a> {
    pub(in crate::games::washoi) entity: &'a str,
    pub(in crate::games::washoi) at: Vec3,
    pub(in crate::games::washoi) velocity: Vec3,
    pub(in crate::games::washoi) grounded: bool,
    pub(in crate::games::washoi) sliding: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::games::washoi) enum Source {
    Sound,
    /// It hit the bot.
    Damage,
}

/// What the bot noticed this snapshot. `seen` indexes the enemies given.
#[derive(Debug, Default, PartialEq)]
pub(in crate::games::washoi) struct Noticed {
    pub(in crate::games::washoi) seen: Vec<usize>,
    pub(in crate::games::washoi) heard: Vec<(String, Vec3, Source)>,
}

/// The bot's own side of a snapshot.
#[derive(Clone, Copy)]
pub(in crate::games::washoi) struct Senses<'a> {
    pub(in crate::games::washoi) own: &'a str,
    pub(in crate::games::washoi) position: Vec3,
    pub(in crate::games::washoi) view_yaw: f64,
    pub(in crate::games::washoi) reaction_ms: f64,
    pub(in crate::games::washoi) tick: u32,
    pub(in crate::games::washoi) sight: &'a Sight,
}

#[derive(Default)]
pub(in crate::games::washoi) struct Perception {
    /// By entity: first and last tick in sight while noticed.
    sightings: Vec<(String, u32, u32)>,
    /// By entity: grounded last snapshot.
    movers: Vec<(String, bool)>,
}

impl Perception {
    pub(in crate::games::washoi) fn perceive(
        &mut self,
        senses: Senses<'_>,
        events: &[SnapshotEvent],
        enemies: &[Enemy<'_>],
    ) -> Noticed {
        let heard = self.hear(senses, events, enemies);
        let notice = (senses.reaction_ms * 0.5 / 1000.0 * TICK_RATE).round() as u32;
        let tick = senses.tick;
        let eye = [
            senses.position.x,
            senses.position.y + EYE_HEIGHT,
            senses.position.z,
        ];
        let mut order: Vec<usize> = (0..enemies.len()).collect();
        order.sort_by(|&a, &b| {
            distance(senses.position, enemies[a].at)
                .total_cmp(&distance(senses.position, enemies[b].at))
        });
        let mut noticed = Noticed::default();
        let mut checks = 0;
        for index in order {
            let enemy = enemies[index];
            let sighting = self.sightings.iter().position(|(id, _, last)| {
                id == enemy.entity && tick.wrapping_sub(*last) <= KEEP_SEEN_TICKS
            });
            let off = angle_to(senses.position, enemy.at, senses.view_yaw).abs();
            let running = enemy.velocity.x.hypot(enemy.velocity.z) > RUNNING_SPEED;
            let in_view = off <= VIEW_HALF_ANGLE || (running && off <= PERIPHERAL_HALF_ANGLE);
            let sound = heard.iter().find(|(id, _, _)| id == enemy.entity);
            let mut seen = false;
            if checks < MAX_SIGHT_CHECKS && (sighting.is_some() || in_view || sound.is_some()) {
                checks += 1;
                let target = [enemy.at.x, enemy.at.y + 0.8 * EYE_HEIGHT, enemy.at.z];
                if senses.sight.clear(eye, target) {
                    let first = match sighting {
                        Some(at) => {
                            self.sightings[at].2 = tick;
                            self.sightings[at].1
                        }
                        None => {
                            self.sightings.retain(|(id, _, _)| id != enemy.entity);
                            self.sightings.push((enemy.entity.to_owned(), tick, tick));
                            tick
                        }
                    };
                    seen = tick.wrapping_sub(first) >= notice;
                }
            }
            if seen {
                noticed.seen.push(index);
            } else if let Some((id, at, source)) = sound {
                noticed.heard.push((id.clone(), *at, *source));
            }
        }
        self.sightings
            .retain(|(_, _, last)| tick.wrapping_sub(*last) <= KEEP_SEEN_TICKS);
        noticed
    }

    /// `_hear`: who made a sound this snapshot, and where.
    fn hear(
        &mut self,
        senses: Senses<'_>,
        events: &[SnapshotEvent],
        enemies: &[Enemy<'_>],
    ) -> Vec<(String, Vec3, Source)> {
        let me = senses.position;
        let mut heard: Vec<(String, Vec3, Source)> = Vec::new();
        let mut hear = |entity: &str, at: Vec3, range: f64, source: Source| {
            if entity == senses.own
                || !enemies.iter().any(|enemy| enemy.entity == entity)
                || !finite(at)
                || distance(me, at) > range
            {
                return;
            }
            heard.retain(|(id, _, _)| id != entity);
            heard.push((entity.to_owned(), at, source));
        };
        let point = |p: &Point| Vec3::new(p.x, p.y, p.z);
        let mut explosions = Vec::new();
        for event in events {
            match event {
                SnapshotEvent::WeaponFire {
                    entity_id,
                    weapon_id,
                    origin,
                    ..
                } => {
                    let range = if LOUD_WEAPONS.contains(&weapon_id.as_str()) {
                        LOUD_FIRE_RANGE
                    } else {
                        FIRE_RANGE
                    };
                    hear(entity_id, point(origin), range, Source::Sound);
                }
                SnapshotEvent::Beam {
                    electro_rail,
                    entity_id,
                    origin,
                    ..
                } => {
                    let range = if *electro_rail {
                        LOUD_FIRE_RANGE
                    } else {
                        FIRE_RANGE
                    };
                    hear(entity_id, point(origin), range, Source::Sound);
                }
                SnapshotEvent::Damage {
                    entity_id,
                    attacker_id,
                    at,
                    ..
                } => {
                    hear(entity_id, point(at), PAIN_RANGE, Source::Sound);
                    if entity_id == senses.own
                        && let Some(attacker) =
                            enemies.iter().find(|enemy| enemy.entity == attacker_id)
                    {
                        hear(attacker_id, attacker.at, f64::INFINITY, Source::Damage);
                    }
                }
                SnapshotEvent::ItemPickup { entity_id, at, .. } => {
                    hear(entity_id, point(at), PICKUP_RANGE, Source::Sound);
                }
                SnapshotEvent::ProjectileImpact { at, .. } => {
                    let at = point(at);
                    if finite(at) && distance(me, at) <= EXPLOSION_RANGE {
                        explosions.push(at);
                    }
                }
                _ => {}
            }
        }
        // Movement sounds, worked out from the snapshot as a client does
        // for footsteps, and explosions next to someone.
        let mut movers = Vec::with_capacity(enemies.len());
        for enemy in enemies {
            let before = self
                .movers
                .iter()
                .find(|(id, _)| id == enemy.entity)
                .map(|(_, grounded)| *grounded);
            let d = distance(me, enemy.at);
            let speed = enemy.velocity.x.hypot(enemy.velocity.z);
            if (enemy.grounded && !enemy.sliding && speed > FOOTSTEP_SPEED && d <= FOOTSTEP_RANGE)
                || (before == Some(true)
                    && !enemy.grounded
                    && enemy.velocity.y > JUMP_SPEED
                    && d <= JUMP_RANGE)
                || explosions
                    .iter()
                    .any(|&at| distance(at, enemy.at) <= EXPLOSION_REVEAL_RADIUS)
            {
                hear(enemy.entity, enemy.at, f64::INFINITY, Source::Sound);
            }
            movers.push((enemy.entity.to_owned(), enemy.grounded));
        }
        self.movers = movers;
        heard
    }
}

/// The horizontal angle from the view to `to`, in (−π, π].
fn angle_to(from: Vec3, to: Vec3, yaw: f64) -> f64 {
    let wanted = (to.x - from.x).atan2(to.z - from.z);
    let off = wanted - yaw;
    off.sin().atan2(off.cos())
}

fn finite(at: Vec3) -> bool {
    at.x.is_finite() && at.y.is_finite() && at.z.is_finite()
}

fn distance(a: Vec3, b: Vec3) -> f64 {
    (a.x - b.x).hypot(a.z - b.z).hypot(a.y - b.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::perception::sight::tests::{cube, encode};

    fn open() -> Sight {
        Sight::parse(&encode("test", &[cube([1000.0; 3], [1001.0; 3])], &[])).unwrap()
    }

    fn enemy(entity: &str, x: f64, z: f64) -> Enemy<'_> {
        Enemy {
            entity,
            at: Vec3::new(x, 0.0, z),
            velocity: Vec3::default(),
            grounded: true,
            sliding: false,
        }
    }

    fn senses<'a>(sight: &'a Sight, tick: u32) -> Senses<'a> {
        Senses {
            own: "me",
            position: Vec3::default(),
            view_yaw: 0.0,
            reaction_ms: 280.0,
            tick,
            sight,
        }
    }

    #[test]
    fn notices_enemies_in_view_after_half_the_reaction_time() {
        let sight = open();
        let mut perception = Perception::default();
        // Ahead (+z), and behind the bot.
        let enemies = [enemy("ahead", 0.0, 400.0), enemy("behind", 0.0, -400.0)];
        // 140 ms at 125 Hz: 18 ticks.
        assert!(
            perception
                .perceive(senses(&sight, 0), &[], &enemies)
                .seen
                .is_empty()
        );
        assert!(
            perception
                .perceive(senses(&sight, 17), &[], &enemies)
                .seen
                .is_empty()
        );
        assert_eq!(
            perception.perceive(senses(&sight, 18), &[], &enemies).seen,
            [0]
        );
        // Turned away, the one in sight within the last second stays seen.
        let turned = Senses {
            view_yaw: PI,
            ..senses(&sight, 100)
        };
        assert_eq!(perception.perceive(turned, &[], &enemies).seen, [0]);
        // Gone from sight for over a second, it must come into view again.
        let later = Senses {
            view_yaw: PI,
            ..senses(&sight, 300)
        };
        let noticed = perception.perceive(later, &[], &enemies[..1]);
        assert!(noticed.seen.is_empty() && noticed.heard.is_empty());
    }

    #[test]
    fn hears_shots_footsteps_and_attackers_out_of_view() {
        let sight = open();
        let mut perception = Perception::default();
        let behind = enemy("behind", 0.0, -1000.0);
        let shot = SnapshotEvent::WeaponFire {
            entity_id: "behind".into(),
            weapon_id: "machinegun".into(),
            origin: Point {
                x: 0.0,
                y: 0.0,
                z: -1000.0,
            },
            dir: Point {
                x: 0.0,
                y: 0.0,
                z: 1.0,
            },
            pellet: None,
        };
        let noticed = perception.perceive(senses(&sight, 0), &[shot], &[behind]);
        assert_eq!(noticed.heard, [("behind".into(), behind.at, Source::Sound)]);
        // Heard, it was looked for. Still in sight a moment later, it is
        // noticed though out of view.
        assert_eq!(
            perception.perceive(senses(&sight, 30), &[], &[behind]).seen,
            [0]
        );
        // Footsteps carry 500 units.
        let walking = Enemy {
            velocity: Vec3::new(200.0, 0.0, 0.0),
            ..enemy("walker", 0.0, -450.0)
        };
        let far = Enemy {
            velocity: Vec3::new(200.0, 0.0, 0.0),
            ..enemy("far", 0.0, -600.0)
        };
        let noticed = perception.perceive(senses(&sight, 40), &[], &[walking, far]);
        assert_eq!(noticed.heard.len(), 1);
        assert_eq!(noticed.heard[0].0, "walker");
        // A hit reveals the attacker however far.
        let hit = SnapshotEvent::Damage {
            entity_id: "me".into(),
            attacker_id: "behind".into(),
            damage: 10,
            weapon_id: "machinegun".into(),
            at: Point {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            hit_location: None,
            pain_tier: None,
        };
        let sniper = enemy("behind", 0.0, -3000.0);
        let noticed = Perception::default().perceive(senses(&sight, 50), &[hit], &[sniper]);
        assert_eq!(
            noticed.heard,
            [("behind".into(), sniper.at, Source::Damage)]
        );
    }
}

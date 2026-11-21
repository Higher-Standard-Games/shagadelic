//! First Washoi fight layer, after TypeScript `states/BotFight.ts` and
//! `bot/BotVision.ts`: pick the weapon for the range to the nearest enemy in
//! sight, aim at it from the fire origin, leading projectiles, and hold fire
//! while it stays in sight and the weapon has ammo. Sight is a static
//! line-of-sight trace, and the bot sees all around, as in TypeScript.
//! Strafing, cover, weapon combos, charged shots, and memory of enemies out
//! of sight remain open.

use std::collections::HashSet;

use shagger_shared::{
    json::Value,
    net::game_state::{GameState, Vec3},
};

use super::{
    Sight,
    aim::View,
    difficulty::AimSkill,
    protocol::SnapshotEvent,
    weapons::{self, Profile},
};

/// `BotVision.ts` `EYE_HEIGHT`: sight lines run from here to 0.8 of it on
/// the target.
const EYE_HEIGHT: f64 = 32.0;
/// `DEFAULT_VIEWHEIGHT` in `engine/physics/PlayerPhysicsConstants.ts`.
const FIRE_HEIGHT: f64 = 34.0;
/// `MAX_ENEMY_LOS_CHECKS`.
const MAX_SIGHT_CHECKS: usize = 5;
/// `predictTargetPosition` assumes this projectile speed.
const PREDICTED_SHOT_SPEED: f64 = 1400.0;
/// `aimWithLead` aims splash weapons this far below the target.
const SPLASH_DROP: f64 = 20.0;
/// `states/BotFight.ts` `MELEE_RANGE`.
const MELEE_RANGE: f64 = 150.0;
/// `BotVision.ts` enemy memory, in server ticks at 125 Hz: a visual track
/// fades from full confidence to none over 4 s, its position is
/// extrapolated for up to 3 s, and it is chased above 0.3.
const TRACK_DECAY_TICKS: f64 = 500.0;
const MAX_EXTRAPOLATION_TICKS: u32 = 375;
const TICK_RATE: f64 = 125.0;
const CHASE_CONFIDENCE: f64 = 0.3;
// `InputButtons` in `engine/network/components/InputComponent.ts`.
pub(super) const FIRE: u16 = 1 << 8;
pub(super) const ALT_FIRE: u16 = 1 << 9;

#[derive(Default)]
pub(super) struct Combat {
    dead: HashSet<String>,
    /// From our own `WeaponState`: the weapon out, the slots in order, and
    /// clip plus reserve per weapon, as `normalizeBotAmmo` sums them.
    weapon: String,
    weapons: Vec<String>,
    ammo: Vec<(String, f64)>,
    /// The last enemy fought, where and when it was last seen.
    track: Option<Track>,
    /// The weapon and server tick since which aim has been held with it out.
    /// TypeScript resets this during a switch, when the weapon out differs
    /// from the one chosen; keying it by weapon also covers a switch between
    /// two snapshots.
    ads_since: Option<(&'static str, u32)>,
}

struct Track {
    entity: String,
    at: Vec3,
    velocity: Vec3,
    tick: u32,
}

/// An enemy out of sight: where it should be by now, and how sure the bot is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Lost {
    pub(super) predicted: Vec3,
    pub(super) confidence: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Engagement {
    pub(super) view: View,
    /// `FIRE` and `ALT_FIRE`.
    pub(super) buttons: u16,
    /// Melee fire must be pressed afresh for each swing.
    pub(super) melee: bool,
    pub(super) weapon_slot: u8,
}

impl Combat {
    pub(super) fn observe(&mut self, events: &[SnapshotEvent], own: &str) {
        for event in events {
            match event {
                SnapshotEvent::CombatState {
                    entity_id, dead, ..
                } => {
                    if *dead {
                        self.dead.insert(entity_id.clone());
                    } else {
                        self.dead.remove(entity_id);
                    }
                }
                SnapshotEvent::Death { entity_id, .. } => {
                    self.dead.insert(entity_id.clone());
                }
                SnapshotEvent::WeaponState {
                    entity_id,
                    current_weapon,
                    weapons,
                    ammo,
                    ..
                } if entity_id == own => {
                    self.weapon.clone_from(current_weapon);
                    self.weapons = weapons
                        .as_array()
                        .unwrap_or_default()
                        .iter()
                        .map(|id| id.as_str().unwrap_or_default().to_owned())
                        .collect();
                    if let Value::Object(entries) = ammo {
                        self.ammo = entries
                            .iter()
                            .filter_map(|(id, value)| Some((id.clone(), total(value)?)))
                            .collect();
                    }
                }
                _ => {}
            }
        }
    }

    /// The fight against the nearest enemy in sight, if any.
    pub(super) fn engage(
        &mut self,
        world: &GameState<SnapshotEvent>,
        own: &str,
        position: Vec3,
        sight: &Sight,
        skill: AimSkill,
    ) -> Option<Engagement> {
        let team = world
            .players
            .values()
            .find(|player| player.entity_id == own)
            .map(|player| player.team.as_str());
        let mut enemies: Vec<_> = world
            .players
            .values()
            .filter(|player| {
                player.entity_id != own
                    && player.team != "spectator"
                    && !(on_a_team(team) && Some(player.team.as_str()) == team)
                    && !self.dead.contains(&player.entity_id)
            })
            .filter_map(|player| {
                let entity = world.entities.get(&player.entity_id)?;
                let at = entity.position.filter(|at| finite(*at))?;
                Some((
                    distance(position, at),
                    at,
                    entity.velocity.map(|v| v.linear),
                    player.entity_id.as_str(),
                ))
            })
            .collect();
        enemies.sort_by(|a, b| a.0.total_cmp(&b.0));
        let eye = [position.x, position.y + EYE_HEIGHT, position.z];
        let (distance, at, velocity, entity) = enemies
            .into_iter()
            .take(MAX_SIGHT_CHECKS)
            .find(|(_, at, _, _)| sight.clear(eye, [at.x, at.y + 0.8 * EYE_HEIGHT, at.z]))?;
        self.track = Some(Track {
            entity: entity.to_owned(),
            at,
            velocity: velocity.filter(|v| finite(*v)).unwrap_or_default(),
            tick: world.tick,
        });
        let ammo = |id: &str| {
            self.ammo
                .iter()
                .find(|(weapon, _)| weapon == id)
                .map(|(_, ammo)| *ammo)
        };
        let (weapon_slot, profile) = weapons::select(&self.weapons, ammo, &self.weapon, distance);
        let selected = match weapon_slot {
            0 => self.weapon.as_str(),
            slot => self.weapons[usize::from(slot) - 1].as_str(),
        };
        let has_ammo = ammo(selected).is_none_or(|ammo| ammo > 0.0);
        let selected_out = selected == self.weapon;
        // `aimWithLead` for projectiles. `aimAt` leads every other target by
        // the predicted travel time.
        let velocity = velocity.filter(|v| finite(*v)).unwrap_or_default();
        let (lead, drop) = if profile.projectile_speed > 0.0 {
            let drop = if profile.splash { SPLASH_DROP } else { 0.0 };
            (distance / profile.projectile_speed, drop)
        } else {
            (skill.prediction * distance / PREDICTED_SHOT_SPEED, 0.0)
        };
        let target = [
            at.x + velocity.x * lead,
            at.y + velocity.y * lead - drop,
            at.z + velocity.z * lead,
        ];
        let delta = [
            target[0] - position.x,
            target[1] - (position.y + FIRE_HEIGHT),
            target[2] - position.z,
        ];
        let horizontal = delta[0].hypot(delta[2]);
        if horizontal <= 0.01 {
            return None;
        }
        let view = View {
            yaw: delta[0].atan2(delta[2]),
            pitch: -delta[1].atan2(horizontal),
        };
        let buttons = self.buttons(profile, has_ammo, selected_out, distance, world.tick);
        Some(Engagement {
            view,
            buttons,
            melee: profile.melee,
            weapon_slot,
        })
    }

    /// `fight`'s buttons. Aiming down sights tightens the shot, and a weapon
    /// whose aim takes time to settle holds fire until the server counts the
    /// shot as aimed. Alt-fire waits for the weapon to be out, because on the
    /// blade it starts a charge that the switch cancels.
    fn buttons(
        &mut self,
        profile: Profile,
        has_ammo: bool,
        selected_out: bool,
        distance: f64,
        tick: u32,
    ) -> u16 {
        if profile.melee {
            self.ads_since = None;
            return if has_ammo && distance < MELEE_RANGE {
                FIRE
            } else {
                0
            };
        }
        let aims = profile.aims_down_sights && has_ammo && selected_out;
        if !aims
            || self
                .ads_since
                .is_some_and(|(weapon, _)| weapon != profile.id)
        {
            self.ads_since = None;
        }
        if aims {
            self.ads_since.get_or_insert((profile.id, tick));
        }
        let ready = profile.ads_ready_ticks == 0
            || self
                .ads_since
                .is_some_and(|(_, since)| tick.wrapping_sub(since) >= profile.ads_ready_ticks);
        let mut buttons = if aims { ALT_FIRE } else { 0 };
        if has_ammo && ready {
            buttons |= FIRE;
        }
        buttons
    }

    /// `_getActiveTrackedEnemies` for the last enemy fought, while it is out
    /// of sight: forgotten once dead or faded, chased above 0.3 confidence.
    pub(super) fn lost(&mut self, tick: u32) -> Option<Lost> {
        let track = self.track.as_ref()?;
        let elapsed = tick.wrapping_sub(track.tick);
        let confidence = 1.0 - f64::from(elapsed) / TRACK_DECAY_TICKS;
        if confidence <= 0.0 || self.dead.contains(&track.entity) {
            self.track = None;
            return None;
        }
        let seconds = f64::from(elapsed.min(MAX_EXTRAPOLATION_TICKS)) / TICK_RATE;
        let predicted = Vec3::new(
            track.at.x + track.velocity.x * seconds,
            track.at.y + track.velocity.y * seconds,
            track.at.z + track.velocity.z * seconds,
        );
        (confidence >= CHASE_CONFIDENCE).then_some(Lost {
            predicted,
            confidence,
        })
    }

    pub(super) fn holding_melee(&self) -> bool {
        weapons::profile_of(&self.weapon).melee
    }

    /// Out of a fight, aim is released.
    pub(super) fn disengage(&mut self) {
        self.ads_since = None;
    }

    /// Forget the weapon on death. Other players' state stays current.
    pub(super) fn respawned(&mut self) {
        self.weapon.clear();
        self.weapons.clear();
        self.ammo.clear();
        self.ads_since = None;
        self.track = None;
    }
}

/// `normalizeBotAmmo`: a count, or a clip and reserve.
fn total(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| {
        let part = |key| value.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        value.get("clip").or(value.get("reserve"))?;
        Some(part("clip") + part("reserve"))
    })
}

/// `onATeam` in `BotGameState.ts`.
fn on_a_team(team: Option<&str>) -> bool {
    team.is_some_and(|team| !matches!(team, "" | "spectator" | "ffa"))
}

fn finite(at: Vec3) -> bool {
    at.x.is_finite() && at.y.is_finite() && at.z.is_finite()
}

fn distance(a: Vec3, b: Vec3) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::{
        Difficulty,
        sight::tests::{cube, encode},
    };
    use shagger_shared::{
        json::Value,
        net::game_state::{PlayerState, Velocity},
    };

    fn world(players: &[(&str, &str, [f64; 3])]) -> GameState<SnapshotEvent> {
        let mut world = GameState::default();
        for &(id, team, [x, y, z]) in players {
            world.players.insert(
                id,
                PlayerState {
                    entity_id: id.into(),
                    team: team.into(),
                    ..PlayerState::default()
                },
            );
            world
                .entities
                .get_or_insert_with(id, Default::default)
                .position = Some(Vec3::new(x, y, z));
        }
        world
    }

    fn sight() -> Sight {
        // A wall across x = 0 for z > 0.
        Sight::parse(&encode(
            "test",
            &[cube([-8.0, -100.0, 0.0], [8.0, 200.0, 1000.0])],
            &[],
        ))
        .unwrap()
    }

    #[test]
    fn aims_at_the_nearest_enemy_in_sight_and_skips_allies_and_the_dead() {
        let skill = AimSkill {
            prediction: 0.0,
            ..Difficulty::Medium.aim()
        };
        let me = Vec3::new(-100.0, 0.0, 10.0);
        let mut combat = Combat::default();
        // Behind the wall, an ally, a dead enemy, and one in the open.
        let mut world = world(&[
            ("me", "red", [-100.0, 0.0, 10.0]),
            ("hidden", "blue", [100.0, 0.0, 10.0]),
            ("ally", "red", [-100.0, 0.0, -50.0]),
            ("dead", "blue", [-100.0, 0.0, -60.0]),
            ("open", "blue", [-100.0, 34.0, -300.0]),
        ]);
        world.events.push(SnapshotEvent::CombatState {
            entity_id: "dead".into(),
            health: 0,
            armor: 0,
            dead: true,
        });
        combat.observe(&world.events, "me");
        let fight = combat.engage(&world, "me", me, &sight(), skill).unwrap();
        // Straight down -z, level with the fire origin.
        assert!((fight.view.yaw.abs() - std::f64::consts::PI).abs() < 1e-9);
        assert!(fight.view.pitch.abs() < 1e-9);
        assert!(fight.buttons & FIRE != 0 && !fight.melee);

        // In free-for-all, the former ally is the target.
        let mut ffa = world.clone();
        for player in ["me", "ally"] {
            ffa.players.get_mut(player).unwrap().team = "ffa".into();
        }
        let fight = combat.engage(&ffa, "me", me, &sight(), skill).unwrap();
        assert!(fight.view.pitch > 0.0, "looks down at the closer player");

        // Nobody in sight but the hidden one: no fight.
        world.players.get_mut("open").unwrap().team = "red".into();
        assert!(combat.engage(&world, "me", me, &sight(), skill).is_none());
    }

    fn armed(weapon: &str, weapons: &[&str], ammo: &[(&str, Value)]) -> SnapshotEvent {
        SnapshotEvent::WeaponState {
            entity_id: "me".into(),
            current_weapon: weapon.into(),
            weapons: Value::Array(
                weapons
                    .iter()
                    .map(|id| Value::String((*id).into()))
                    .collect(),
            ),
            ammo: Value::Object(
                ammo.iter()
                    .map(|(id, value)| ((*id).into(), value.clone()))
                    .collect(),
            ),
            reloading: false,
            last_fire_tick: 0,
        }
    }

    fn rounds(clip: f64, reserve: f64) -> Value {
        Value::Object(vec![
            ("clip".into(), Value::Number(clip)),
            ("reserve".into(), Value::Number(reserve)),
        ])
    }

    #[test]
    fn leads_moving_targets_by_prediction_or_projectile_speed() {
        let me = Vec3::new(0.0, 0.0, -100.0);
        let mut world = world(&[
            ("me", "", [0.0, 0.0, -100.0]),
            ("foe", "", [0.0, 0.0, -1500.0]),
        ]);
        world.entities.get_mut("foe").unwrap().velocity = Some(Velocity {
            linear: Vec3::new(300.0, 0.0, 0.0),
            ..Velocity::default()
        });
        let mut combat = Combat::default();
        let skill = Difficulty::Insane.aim();
        let fight = combat.engage(&world, "me", me, &sight(), skill).unwrap();
        // 0.9 · 1400 / 1400 s ahead: 270 units to +x, so the yaw leans that way.
        assert!((fight.view.yaw - 270f64.atan2(-1400.0)).abs() < 1e-9);
        // A plasma bolt takes 1400 / 1750 s and splashes at the feet.
        combat.observe(&[armed("plasmagun", &["plasmagun"], &[])], "me");
        let fight = combat.engage(&world, "me", me, &sight(), skill).unwrap();
        assert!((fight.view.yaw - 240f64.atan2(-1400.0)).abs() < 1e-9);
        assert!(fight.view.pitch > 0.0);
    }

    #[test]
    fn switches_for_the_range_and_holds_fire_until_it_can_hit() {
        let me = Vec3::new(0.0, 0.0, -100.0);
        let mut world = world(&[
            ("me", "", [0.0, 0.0, -100.0]),
            ("foe", "", [0.0, 0.0, -1500.0]),
        ]);
        let skill = Difficulty::Medium.aim();
        let mut combat = Combat::default();
        let loadout = ["blade", "machinegun", "electrobolt"];
        // Spawned with the blade out: switch to the machinegun, and fire at once.
        combat.observe(
            &[armed(
                "blade",
                &loadout,
                &[
                    ("blade", rounds(1.0, 0.0)),
                    ("machinegun", rounds(30.0, 120.0)),
                    ("electrobolt", rounds(0.0, 0.0)),
                ],
            )],
            "me",
        );
        let fight = combat.engage(&world, "me", me, &sight(), skill).unwrap();
        assert_eq!((fight.weapon_slot, fight.buttons), (2, FIRE));
        // With it out, aim down sights while firing.
        combat.observe(
            &[armed(
                "machinegun",
                &loadout,
                &[
                    ("machinegun", rounds(30.0, 120.0)),
                    ("electrobolt", rounds(0.0, 0.0)),
                ],
            )],
            "me",
        );
        let fight = combat.engage(&world, "me", me, &sight(), skill).unwrap();
        assert_eq!((fight.weapon_slot, fight.buttons), (0, FIRE | ALT_FIRE));
        // The electrobolt counts a shot as aimed after 47 ticks of aim.
        combat.observe(
            &[armed(
                "electrobolt",
                &["electrobolt"],
                &[("electrobolt", rounds(5.0, 0.0))],
            )],
            "me",
        );
        world.tick = 100;
        assert_eq!(
            combat
                .engage(&world, "me", me, &sight(), skill)
                .unwrap()
                .buttons,
            ALT_FIRE
        );
        world.tick = 147;
        assert_eq!(
            combat
                .engage(&world, "me", me, &sight(), skill)
                .unwrap()
                .buttons,
            FIRE | ALT_FIRE
        );
        // Out of rounds, nothing is pressed.
        combat.observe(
            &[armed(
                "electrobolt",
                &["electrobolt"],
                &[("electrobolt", rounds(0.0, 0.0))],
            )],
            "me",
        );
        assert_eq!(
            combat
                .engage(&world, "me", me, &sight(), skill)
                .unwrap()
                .buttons,
            0
        );
        // The blade swings only within reach.
        combat.observe(&[armed("blade", &["blade"], &[])], "me");
        let far = combat.engage(&world, "me", me, &sight(), skill).unwrap();
        assert!(far.melee && far.buttons == 0);
        world.entities.get_mut("foe").unwrap().position = Some(Vec3::new(0.0, 0.0, -200.0));
        assert_eq!(
            combat
                .engage(&world, "me", me, &sight(), skill)
                .unwrap()
                .buttons,
            FIRE
        );
    }

    #[test]
    fn remembers_an_enemy_out_of_sight_until_it_fades_or_dies() {
        let me = Vec3::new(0.0, 0.0, -100.0);
        let mut world = world(&[
            ("me", "", [0.0, 0.0, -100.0]),
            ("foe", "", [0.0, 0.0, -500.0]),
        ]);
        world.entities.get_mut("foe").unwrap().velocity = Some(Velocity {
            linear: Vec3::new(250.0, 0.0, 0.0),
            ..Velocity::default()
        });
        let mut combat = Combat::default();
        let skill = Difficulty::Medium.aim();
        world.tick = 1000;
        assert!(combat.engage(&world, "me", me, &sight(), skill).is_some());
        // Behind the wall across x = 0 for z > 0, one second later.
        world.entities.get_mut("foe").unwrap().position = Some(Vec3::new(0.0, 0.0, 300.0));
        world.tick = 1125;
        assert!(combat.engage(&world, "me", me, &sight(), skill).is_none());
        let lost = combat.lost(1125).unwrap();
        assert!((lost.confidence - 0.75).abs() < 1e-9);
        assert_eq!(lost.predicted, Vec3::new(250.0, 0.0, -500.0));
        // The chase ends below 0.3 confidence, after 2.8 s.
        let last = combat.lost(1000 + 350).unwrap();
        assert!((last.predicted.x - 700.0).abs() < 1e-9);
        assert!(combat.lost(1000 + 351).is_none());
        // The memory is gone; seeing the foe again brings it back.
        world.entities.get_mut("foe").unwrap().position = Some(Vec3::new(0.0, 0.0, -500.0));
        assert!(combat.engage(&world, "me", me, &sight(), skill).is_some());
        // A death ends the memory.
        combat.observe(
            &[SnapshotEvent::CombatState {
                entity_id: "foe".into(),
                health: 0,
                armor: 0,
                dead: true,
            }],
            "me",
        );
        assert!(combat.lost(1125).is_none());
    }
}

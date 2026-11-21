//! First Washoi fight layer, after TypeScript `states/BotFight.ts` and
//! `bot/BotVision.ts`: pick the weapon for the range to the nearest enemy
//! the bot has noticed, aim at it from the fire origin, leading projectiles,
//! and hold fire while it stays noticed and the weapon has ammo. What the
//! bot notices comes from `perception.rs`: a view cone, a static
//! line-of-sight trace, and sounds.
//! Strafing, cover, weapon combos, charged shots, and memory of enemies out
//! of sight remain open.

pub(in crate::games::washoi) mod aim;
pub(in crate::games::washoi) mod combos;
pub(in crate::games::washoi) mod lunge;
pub(in crate::games::washoi) mod mood;
pub(in crate::games::washoi) mod safety;
pub(in crate::games::washoi) mod weapons;

use self::{
    aim::View,
    combos::{Combos, Situation},
    weapons::Profile,
};

use std::collections::HashSet;

use shagger_shared::{json::Value, net::game_state::Vec3};

use crate::games::washoi::{
    Observation, Sight,
    agents::player::movement::cover::{self, CoverPeek},
    agents::player::navigation::local_walk,
    difficulty::AimSkill,
    hosting::net::protocol::SnapshotEvent,
    perception::estimate::Estimates,
    perception::{Enemy, Perception, Senses, Source},
    random::Random,
};

/// `DEFAULT_VIEWHEIGHT` in `engine/physics/PlayerPhysicsConstants.ts`.
const FIRE_HEIGHT: f64 = 34.0;
/// `predictTargetPosition` assumes this projectile speed.
const PREDICTED_SHOT_SPEED: f64 = 1400.0;
/// `aimWithLead` aims splash weapons this far below the target.
const SPLASH_DROP: f64 = 20.0;
/// `states/BotFight.ts` `MELEE_RANGE`.
const MELEE_RANGE: f64 = 150.0;
/// `CHARGE_RELEASE_CONE`, `CHARGE_AIM_GRACE_TICKS`, `CHARGE_CARRY_TICKS`.
const CHARGE_RELEASE_CONE: f64 = 12.0 * std::f64::consts::PI / 180.0;
const CHARGE_AIM_GRACE_TICKS: u32 = 63;
const CHARGE_CARRY_TICKS: u32 = 250;
/// `READY_RANGE`, `READY_TICKS`, and `HOLSTER_RANGE` of `readyForHeard`.
const READY_RANGE: f64 = 1500.0;
const READY_TICKS: u32 = 250;
const HOLSTER_RANGE: f64 = 800.0;
/// `COVER_MIN_RANGE`: closer fights are fought in the open.
const COVER_MIN_RANGE: f64 = 350.0;
/// `BotVision.ts` enemy memory, in server ticks at 125 Hz: a visual track
/// fades from full confidence to none over 4 s, its position is
/// extrapolated for up to 3 s, and it is chased above 0.3.
const TRACK_DECAY_TICKS: f64 = 500.0;
/// Heard tracks start less sure and fade in 2.5 s: `SOUND_CONFIDENCE`,
/// `DAMAGE_CONFIDENCE`, and `SOUND_TRACK_DECAY_TICKS`.
const SOUND_CONFIDENCE: f64 = 0.6;
const DAMAGE_CONFIDENCE: f64 = 0.8;
const SOUND_DECAY_TICKS: f64 = 312.5;
const MAX_EXTRAPOLATION_TICKS: u32 = 375;
const TICK_RATE: f64 = 125.0;
const CHASE_CONFIDENCE: f64 = 0.3;
// `InputButtons` in `engine/network/components/InputComponent.ts`.
pub(in crate::games::washoi) const FIRE: u16 = 1 << 8;
pub(in crate::games::washoi) const ALT_FIRE: u16 = 1 << 9;

#[derive(Default)]
pub(in crate::games::washoi) struct Combat {
    dead: HashSet<String>,
    /// From our own `WeaponState`: the weapon out, the slots in order, and
    /// clip plus reserve per weapon, as `normalizeBotAmmo` sums them.
    weapon: String,
    weapons: Vec<String>,
    ammo: Vec<(String, f64)>,
    /// The enemy last fought or heard, where and when it was last noticed.
    track: Option<Track>,
    perception: Perception,
    combos: Combos,
    estimates: Estimates,
    cover: CoverPeek,
    /// A charge being held for a blast.
    charge: Option<Charge>,
    /// The nearest enemy heard but not seen this decision, and the gun got
    /// out for it until a tick.
    heard_nearest: Option<f64>,
    ready: Option<(String, u32)>,
    ready_roll_at: u32,
    /// When the bot last fired, for peeking between shots.
    fired_tick: Option<u32>,
    /// Combo rolls, seeded per bot.
    random: Random,
    /// The weapon and server tick since which aim has been held with it out.
    /// TypeScript resets this during a switch, when the weapon out differs
    /// from the one chosen; keying it by weapon also covers a switch between
    /// two snapshots.
    ads_since: Option<(&'static str, u32)>,
}

struct Charge {
    weapon: &'static str,
    target: String,
    start: u32,
    release: u32,
}

struct Track {
    entity: String,
    at: Vec3,
    velocity: Vec3,
    tick: u32,
    /// Confidence when noticed, and the ticks it takes to fade from 1.
    initial: f64,
    decay: f64,
}

impl Track {
    fn confidence(&self, tick: u32) -> f64 {
        self.initial - f64::from(tick.wrapping_sub(self.tick)) / self.decay
    }
}

/// An enemy out of sight: where it should be by now, and how sure the bot is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::games::washoi) struct Lost {
    pub(in crate::games::washoi) predicted: Vec3,
    pub(in crate::games::washoi) confidence: f64,
    /// Only heard, not seen or felt.
    pub(in crate::games::washoi) heard: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::games::washoi) struct Engagement {
    pub(in crate::games::washoi) view: View,
    /// `FIRE` and `ALT_FIRE`.
    pub(in crate::games::washoi) buttons: u16,
    /// Melee fire must be pressed afresh for each swing.
    pub(in crate::games::washoi) melee: bool,
    pub(in crate::games::washoi) weapon_slot: u8,
    /// A strafe that peeks from cover, `BotCoverPeek`: −1, 1, or 0 to stand.
    pub(in crate::games::washoi) peek: Option<i8>,
    /// False while ducked in cover with the enemy out of sight.
    pub(in crate::games::washoi) in_sight: bool,
    /// Where the enemy is, or was last seen while ducked.
    pub(in crate::games::washoi) target: Vec3,
    /// The chosen weapon splashes, so high ground pays.
    pub(in crate::games::washoi) splash: bool,
}

impl Combat {
    pub(in crate::games::washoi) fn seeded(random: Random) -> Self {
        Self {
            random,
            ..Self::default()
        }
    }

    pub(in crate::games::washoi) fn observe(
        &mut self,
        events: &[SnapshotEvent],
        own: &str,
        tick: u32,
    ) {
        for event in events {
            match event {
                SnapshotEvent::Damage {
                    entity_id,
                    attacker_id,
                    weapon_id,
                    damage,
                    pain_tier,
                    ..
                } if attacker_id == own && entity_id != own => {
                    self.combos.hit(weapon_id, entity_id, tick);
                    self.estimates
                        .hit(entity_id, f64::from(*damage), *pain_tier, tick);
                }
                SnapshotEvent::WeaponFire {
                    entity_id,
                    weapon_id,
                    ..
                } if entity_id == own => {
                    self.combos.shot(weapon_id, tick);
                    self.fired_tick = Some(tick);
                }
                SnapshotEvent::CombatState {
                    entity_id, dead, ..
                } => {
                    if *dead {
                        self.estimates.died(entity_id);
                        self.dead.insert(entity_id.clone());
                    } else {
                        self.dead.remove(entity_id);
                    }
                }
                SnapshotEvent::Death { entity_id, .. } => {
                    self.estimates.died(entity_id);
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
    pub(in crate::games::washoi) fn engage(
        &mut self,
        world: &Observation,
        own: &str,
        position: Vec3,
        sent: View,
        sight: &Sight,
        skill: AimSkill,
    ) -> Option<Engagement> {
        let view_yaw = sent.yaw;
        let team = world
            .players
            .iter()
            .find(|player| player.entity_id == own)
            .map(|player| player.team.as_str());
        let mut enemies: Vec<_> = world
            .players
            .iter()
            .filter(|player| {
                player.entity_id != own
                    && player.team != "spectator"
                    && !(on_a_team(team) && Some(player.team.as_str()) == team)
                    && !self.dead.contains(&player.entity_id)
            })
            .filter_map(|player| {
                let entity = world.body(&player.entity_id)?;
                let at = entity.position.filter(|at| finite(*at))?;
                let velocity = entity.velocity.unwrap_or_default();
                Some(Enemy {
                    entity: player.entity_id.as_str(),
                    at,
                    velocity: Some(velocity.linear)
                        .filter(|v| finite(*v))
                        .unwrap_or_default(),
                    grounded: entity.velocity.is_none_or(|v| v.grounded),
                    sliding: velocity.is_sliding,
                })
            })
            .collect();
        enemies.sort_by(|a, b| distance(position, a.at).total_cmp(&distance(position, b.at)));
        let senses = Senses {
            own,
            position,
            view_yaw,
            reaction_ms: skill.reaction_ms,
            tick: world.tick,
            sight,
        };
        let noticed = self.perception.perceive(senses, &world.events, &enemies);
        self.heard_nearest = None;
        for (entity, at, source) in &noticed.heard {
            self.heard(entity, *at, *source, world.tick);
            let heard_at = distance(position, *at);
            self.heard_nearest = Some(
                self.heard_nearest
                    .map_or(heard_at, |near: f64| near.min(heard_at)),
            );
        }
        let grounded = world
            .body(own)
            .and_then(|body| body.velocity)
            .is_none_or(|velocity| velocity.grounded);
        // `seen` is nearest first.
        let Some(&seen) = noticed.seen.first() else {
            return self.hold_cover(world.tick, position, sight, skill, grounded);
        };
        for &index in &noticed.seen {
            self.estimates.seen(enemies[index].entity, world.tick);
        }
        let enemy = enemies[seen];
        let (distance, at, velocity) =
            (distance(position, enemy.at), enemy.at, Some(enemy.velocity));
        self.track = Some(Track {
            entity: enemy.entity.to_owned(),
            at,
            velocity: enemy.velocity,
            tick: world.tick,
            initial: 1.0,
            decay: TRACK_DECAY_TICKS,
        });
        let ammo = |id: &str| {
            self.ammo
                .iter()
                .find(|(weapon, _)| weapon == id)
                .map(|(_, ammo)| *ammo)
        };
        let (mut weapon_slot, mut profile) =
            weapons::select(&self.weapons, ammo, &self.weapon, distance);
        let picked = match weapon_slot {
            0 => self.weapon.clone(),
            slot => self.weapons[usize::from(slot) - 1].clone(),
        };
        let has =
            |id: &str| self.weapons.iter().any(|w| w == id) && ammo(id).is_none_or(|a| a > 0.0);
        let combo = self.combos.choose(
            &Situation {
                tick: world.tick,
                target: enemy.entity,
                distance,
                target_rising: enemy.velocity.y,
                current: &self.weapon,
                picked: &picked,
                has: &has,
                weapons: &self.weapons,
                estimate: self.estimates.of(enemy.entity, world.tick),
            },
            skill.prediction,
            &mut self.random,
        );
        // `slotFor`: 0 when the combo's weapon is already out.
        if let Some(combo) = &combo {
            profile = weapons::profile_of(combo);
            weapon_slot = self
                .weapons
                .iter()
                .position(|weapon| weapon == combo && *weapon != self.weapon)
                .and_then(|index| u8::try_from(index + 1).ok())
                .unwrap_or(0);
        }
        let selected = combo.as_deref().unwrap_or(&picked);
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
        // `updateChargedFire`: beyond swing reach, a charged weapon holds
        // alt-fire for a blast and lets go on the aim.
        let charging = match profile.charge {
            Some(span) if distance >= MELEE_RANGE && selected_out => self.charged(
                world.tick,
                enemy.entity,
                profile.id,
                span,
                has_ammo,
                sent,
                view,
                skill,
            ),
            _ => {
                self.charge = None;
                false
            }
        };
        let mut buttons = if charging {
            ALT_FIRE
        } else {
            self.buttons(profile, has_ammo, selected_out, distance, world.tick)
        };
        // `peekFromCover`: ranged fights on the ground with the weapon out.
        let peek = if !profile.melee && distance >= COVER_MIN_RANGE && grounded && selected_out {
            self.peek(
                world.tick, position, enemy.at, view.yaw, sight, profile, skill, true,
            )
        } else {
            self.cover.reset();
            None
        };
        if peek.is_some() && !has_ammo {
            buttons = 0;
        }
        Some(Engagement {
            view,
            buttons,
            melee: profile.melee,
            weapon_slot,
            peek,
            in_sight: true,
            target: enemy.at,
            splash: profile.splash,
        })
    }

    /// Ducked in cover, the fight goes on out of sight for a while: watch
    /// where the enemy was last seen, hold fire, and keep peeking.
    fn hold_cover(
        &mut self,
        tick: u32,
        position: Vec3,
        sight: &Sight,
        skill: AimSkill,
        grounded: bool,
    ) -> Option<Engagement> {
        if !grounded || !self.cover.holding(tick) {
            return None;
        }
        let at = self.track.as_ref()?.at;
        let delta = [at.x - position.x, at.y - position.y, at.z - position.z];
        let horizontal = delta[0].hypot(delta[2]);
        if horizontal <= 0.01 {
            return None;
        }
        let view = View {
            yaw: delta[0].atan2(delta[2]),
            pitch: -delta[1].atan2(horizontal),
        };
        let profile = weapons::profile_of(&self.weapon);
        let peek = self.peek(tick, position, at, view.yaw, sight, profile, skill, false);
        Some(Engagement {
            view,
            buttons: 0,
            melee: false,
            weapon_slot: 0,
            peek,
            in_sight: false,
            target: at,
            splash: profile.splash,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn peek(
        &mut self,
        tick: u32,
        position: Vec3,
        enemy: Vec3,
        yaw: f64,
        sight: &Sight,
        profile: Profile,
        skill: AimSkill,
        visible: bool,
    ) -> Option<i8> {
        let enemy_eye = [enemy.x, enemy.y + FIRE_HEIGHT, enemy.z];
        let hidden = |at: [f64; 3]| !sight.clear([at[0], at[1] + FIRE_HEIGHT, at[2]], enemy_eye);
        let walkable = |to: [f64; 3]| local_walk(sight, position, Vec3::new(to[0], to[1], to[2]));
        self.cover.step(
            &cover::Query {
                tick,
                position: [position.x, position.y, position.z],
                yaw,
                hidden: &hidden,
                walkable: &walkable,
                refire_ticks: profile.fire_interval_ticks,
                fired_tick: self.fired_tick,
                visible,
            },
            skill.dodge,
            &mut self.random,
        )
    }

    /// Hold or let go of a charge against `target`. A charge starts only
    /// when the bot means to fire, and once started it is held through a
    /// moment's hesitation, because letting go before the minimum cancels it.
    #[allow(clippy::too_many_arguments)]
    fn charged(
        &mut self,
        tick: u32,
        target: &str,
        weapon: &'static str,
        (min, max): (u32, u32),
        wants_fire: bool,
        sent: View,
        wanted: View,
        skill: AimSkill,
    ) -> bool {
        let current = self
            .charge
            .as_ref()
            .filter(|charge| charge.target == target && charge.weapon == weapon);
        let Some(charge) = current else {
            if !wants_fire {
                self.charge = None;
                return false;
            }
            let hold = 0.35 + skill.prediction * 0.4 + (self.random.next() - 0.5) * 0.15;
            let ticks =
                (f64::from(min) + f64::from(max - min) * hold.clamp(0.0, 1.0)).round() as u32;
            self.charge = Some(Charge {
                weapon,
                target: target.to_owned(),
                start: tick,
                release: tick + ticks.max(min),
            });
            return true;
        };
        // Release on the aim: a blast let go while the view is still turning
        // misses. After the grace it goes anyway.
        let off = aim::wrap(wanted.yaw - sent.yaw)
            .abs()
            .max((wanted.pitch - sent.pitch).abs());
        let release = charge.release;
        if tick >= release
            && (off <= CHARGE_RELEASE_CONE || tick >= release + CHARGE_AIM_GRACE_TICKS)
        {
            self.charge = None;
            return false;
        }
        true
    }

    /// `readyForHeard`: out of a fight, the weapon slot to switch to, or 0.
    /// An enemy heard within 1,500 units but not seen gets the gun for that
    /// distance, rolled every two seconds at `predictionSkill` odds and kept
    /// for two seconds. Otherwise the blade goes away for the gun a fight at
    /// 800 units would pick: whoever comes round the next corner is more
    /// likely at mid range than in swing reach.
    pub(in crate::games::washoi) fn ready_slot(&mut self, tick: u32, skill: AimSkill) -> u8 {
        let ammo = |id: &str| {
            self.ammo
                .iter()
                .find(|(weapon, _)| weapon == id)
                .map(|(_, ammo)| *ammo)
        };
        let has =
            |id: &str| self.weapons.iter().any(|w| w == id) && ammo(id).is_none_or(|a| a > 0.0);
        if self
            .ready
            .as_ref()
            .is_some_and(|(weapon, until)| tick >= *until || !has(weapon))
        {
            self.ready = None;
        }
        if self.ready.is_none()
            && let Some(heard) = self.heard_nearest.filter(|&heard| heard <= READY_RANGE)
            && tick >= self.ready_roll_at
        {
            self.ready_roll_at = tick + READY_TICKS;
            if self.random.next() < skill.prediction {
                let (slot, _) = weapons::select(&self.weapons, ammo, &self.weapon, heard);
                let weapon = match slot {
                    0 => self.weapon.clone(),
                    slot => self.weapons[usize::from(slot) - 1].clone(),
                };
                if !weapon.is_empty() {
                    self.ready = Some((weapon, tick + READY_TICKS));
                }
            }
        }
        let slot_for = |weapon: &str| {
            if weapon == self.weapon {
                return 0;
            }
            self.weapons
                .iter()
                .position(|w| w == weapon)
                .and_then(|index| u8::try_from(index + 1).ok())
                .unwrap_or(0)
        };
        if let Some((weapon, _)) = &self.ready {
            let slot = slot_for(weapon);
            if slot != 0 {
                return slot;
            }
        }
        if self.weapon == "blade" && self.charge.is_none() {
            let (slot, profile) = weapons::select(&self.weapons, ammo, &self.weapon, HOLSTER_RANGE);
            if slot != 0 && !profile.melee {
                return slot;
            }
        }
        0
    }

    /// `carryCharge`: out of a fight, a charge is still held for up to two
    /// seconds while its weapon stays out, to let go when the enemy is back.
    pub(in crate::games::washoi) fn carry_charge(&mut self, tick: u32) -> bool {
        let carried = self.charge.as_ref().is_some_and(|charge| {
            charge.weapon == self.weapon && tick.wrapping_sub(charge.start) < CHARGE_CARRY_TICKS
        });
        if !carried {
            self.charge = None;
        }
        carried
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
    pub(in crate::games::washoi) fn lost(&mut self, tick: u32) -> Option<Lost> {
        let track = self.track.as_ref()?;
        let elapsed = tick.wrapping_sub(track.tick);
        let confidence = track.confidence(tick);
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
            heard: track.initial == SOUND_CONFIDENCE && track.decay == SOUND_DECAY_TICKS,
        })
    }

    /// A heard enemy leads the chase unless a surer track of another one
    /// is still fresh. TypeScript keeps several tracks and prefers seen ones.
    fn heard(&mut self, entity: &str, at: Vec3, source: Source, tick: u32) {
        let initial = match source {
            Source::Sound => SOUND_CONFIDENCE,
            Source::Damage => DAMAGE_CONFIDENCE,
        };
        if self
            .track
            .as_ref()
            .is_some_and(|track| track.entity != entity && track.confidence(tick) > initial)
        {
            return;
        }
        self.track = Some(Track {
            entity: entity.to_owned(),
            at,
            velocity: Vec3::default(),
            tick,
            initial,
            decay: SOUND_DECAY_TICKS,
        });
    }

    pub(in crate::games::washoi) fn holding_melee(&self) -> bool {
        weapons::profile_of(&self.weapon).melee
    }

    /// Out of a fight, aim is released.
    pub(in crate::games::washoi) fn disengage(&mut self) {
        self.ads_since = None;
    }

    /// Living teammates' positions, for the friendly-fire gate. None outside
    /// team play.
    pub(super) fn allies(&self, world: &Observation, own: &str) -> Vec<Vec3> {
        self.allies_by_id(world, own)
            .into_iter()
            .map(|(_, at)| at)
            .collect()
    }

    /// Living teammates and where they are, by entity ID.
    pub(in crate::games::washoi) fn allies_by_id<'a>(
        &self,
        world: &'a Observation,
        own: &str,
    ) -> Vec<(&'a str, Vec3)> {
        let team = world.player(own).map(|player| player.team.as_str());
        if !on_a_team(team) {
            return Vec::new();
        }
        world
            .players
            .iter()
            .filter(|player| {
                player.entity_id != own
                    && Some(player.team.as_str()) == team
                    && !self.dead.contains(&player.entity_id)
            })
            .filter_map(|player| {
                Some((
                    player.entity_id.as_str(),
                    world.body(&player.entity_id)?.position?,
                ))
            })
            .collect()
    }

    /// Opponents on the scoreboard, seen or not: everyone else outside team
    /// play, the other teams in it. Spectators don't count.
    pub(in crate::games::washoi) fn scoreboard_enemies<'a>(
        &self,
        world: &'a Observation,
        own: &str,
    ) -> Vec<&'a str> {
        let team = world.player(own).map(|player| player.team.as_str());
        world
            .players
            .iter()
            .filter(|player| {
                player.entity_id != own
                    && player.team != "spectator"
                    && (!on_a_team(team) || Some(player.team.as_str()) != team)
            })
            .map(|player| player.entity_id.as_str())
            .collect()
    }

    /// Enemies the bot believes alive, from what it saw, heard, and hit.
    pub(in crate::games::washoi) fn known_enemies(&self) -> usize {
        self.estimates.alive()
    }

    /// The enemy last fought or heard, while alive.
    pub(in crate::games::washoi) fn tracked(&self) -> Option<&str> {
        let track = self.track.as_ref()?;
        (!self.dead.contains(&track.entity)).then_some(track.entity.as_str())
    }

    /// Whether this enemy is known dead.
    pub(in crate::games::washoi) fn is_dead(&self, entity: &str) -> bool {
        self.dead.contains(entity)
    }

    /// The weapon out.
    pub(in crate::games::washoi) fn weapon(&self) -> &str {
        &self.weapon
    }

    /// Ammo for the weapon out, when known.
    pub(in crate::games::washoi) fn current_ammo(&self) -> Option<f64> {
        self.ammo
            .iter()
            .find(|(weapon, _)| *weapon == self.weapon)
            .map(|(_, ammo)| *ammo)
    }

    /// The bot's weapons, in slot order, and ammo by weapon.
    pub(in crate::games::washoi) fn inventory(&self) -> (&[String], &[(String, f64)]) {
        (&self.weapons, &self.ammo)
    }

    /// Forget the weapon on death. Other players' state stays current.
    pub(in crate::games::washoi) fn respawned(&mut self) {
        self.weapon.clear();
        self.weapons.clear();
        self.ammo.clear();
        self.ads_since = None;
        self.track = None;
        self.combos.reset();
        self.cover.reset();
        self.fired_tick = None;
        self.charge = None;
        self.ready = None;
        self.heard_nearest = None;
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
        perception::sight::tests::{cube, encode},
    };
    use crate::games::washoi::{PlayerView, random::Random};
    use shagger_shared::{json::Value, net::game_state::Velocity};

    fn world(players: &[(&str, &str, [f64; 3])]) -> Observation {
        let mut world = Observation::default();
        for &(id, team, [x, y, z]) in players {
            world.players.push(PlayerView {
                entity_id: id.into(),
                team: team.into(),
            });
            world.body_or_insert(id).position = Some(Vec3::new(x, y, z));
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

    /// The bot looks down -z, at every foe in these tests, for longer than
    /// it takes to notice one, then engages.
    fn toward(yaw: f64) -> View {
        View { yaw, pitch: 0.0 }
    }

    fn engaged(
        combat: &mut Combat,
        world: &Observation,
        me: Vec3,
        skill: AimSkill,
    ) -> Option<Engagement> {
        let facing = std::f64::consts::PI;
        let mut before = world.clone();
        before.tick = world.tick.wrapping_sub(30);
        combat.engage(&before, "me", me, toward(facing), &sight(), skill);
        combat.engage(world, "me", me, toward(facing), &sight(), skill)
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
        combat.observe(&world.events, "me", 0);
        let fight = engaged(&mut combat, &world, me, skill).unwrap();
        // Straight down -z, level with the fire origin.
        assert!((fight.view.yaw.abs() - std::f64::consts::PI).abs() < 1e-9);
        assert!(fight.view.pitch.abs() < 1e-9);
        assert!(fight.buttons & FIRE != 0 && !fight.melee);

        // In free-for-all, the former ally is the target.
        let mut ffa = world.clone();
        for player in ["me", "ally"] {
            ffa.player_mut(player).unwrap().team = "ffa".into();
        }
        let fight = engaged(&mut combat, &ffa, me, skill).unwrap();
        assert!(fight.view.pitch > 0.0, "looks down at the closer player");

        // Nobody in sight but the hidden one: no fight.
        world.player_mut("open").unwrap().team = "red".into();
        assert!(engaged(&mut combat, &world, me, skill).is_none());
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
        world.body_mut("foe").unwrap().velocity = Some(Velocity {
            linear: Vec3::new(300.0, 0.0, 0.0),
            ..Velocity::default()
        });
        let mut combat = Combat::default();
        let skill = Difficulty::Insane.aim();
        let fight = engaged(&mut combat, &world, me, skill).unwrap();
        // 0.9 · 1400 / 1400 s ahead: 270 units to +x, so the yaw leans that way.
        assert!((fight.view.yaw - 270f64.atan2(-1400.0)).abs() < 1e-9);
        // A plasma bolt takes 1400 / 1750 s and splashes at the feet.
        combat.observe(&[armed("plasmagun", &["plasmagun"], &[])], "me", 0);
        let fight = engaged(&mut combat, &world, me, skill).unwrap();
        assert!((fight.view.yaw - 240f64.atan2(-1400.0)).abs() < 1e-9);
        assert!(fight.view.pitch > 0.0);
    }

    #[test]
    fn follows_its_own_launching_rocket_with_the_electrobolt() {
        let me = Vec3::new(0.0, 0.0, -100.0);
        let mut world = world(&[
            ("me", "", [0.0, 0.0, -100.0]),
            ("foe", "", [0.0, 0.0, -900.0]),
        ]);
        world.body_mut("foe").unwrap().velocity = Some(Velocity {
            linear: Vec3::new(0.0, 400.0, 0.0),
            ..Velocity::default()
        });
        world.tick = 200;
        // Insane rolls its combos at 0.9, and this seed passes.
        let mut combat = Combat::seeded(Random::new(4));
        let loadout = ["rlauncher", "electrobolt"];
        let hit = SnapshotEvent::Damage {
            entity_id: "foe".into(),
            attacker_id: "me".into(),
            damage: 80,
            weapon_id: "rlauncher".into(),
            at: crate::games::washoi::hosting::net::protocol::Point {
                x: 0.0,
                y: 0.0,
                z: -900.0,
            },
            hit_location: None,
            pain_tier: None,
        };
        combat.observe(
            &[
                armed(
                    "rlauncher",
                    &loadout,
                    &[
                        ("rlauncher", rounds(5.0, 0.0)),
                        ("electrobolt", rounds(5.0, 0.0)),
                    ],
                ),
                hit,
            ],
            "me",
            200,
        );
        let fight = engaged(&mut combat, &world, me, Difficulty::Insane.aim()).unwrap();
        // At 800 units the distance keeps the rocket launcher out. The combo
        // takes the bolt in slot 2 while the foe hangs in the air.
        assert_eq!(fight.weapon_slot, 2);
    }

    #[test]
    fn peeks_from_a_pillar_and_holds_the_fight_while_ducked() {
        use crate::games::washoi::hosting::net::protocol::Point;
        // A floor, and a wall that hides the spot 112 units to the bot's
        // right (−x, facing the foe down −z) but not the spot at 64.
        let sight = Sight::parse(&encode(
            "test",
            &[
                cube([-1000.0, -10.0, -1000.0], [1000.0, 0.0, 1000.0]),
                cube([-200.0, 0.0, -60.0], [-80.0, 200.0, -40.0]),
            ],
            &[],
        ))
        .unwrap();
        let mut world = world(&[
            ("me", "", [0.0, 24.0, 0.0]),
            ("foe", "", [0.0, 24.0, -600.0]),
        ]);
        let skill = Difficulty::Insane.aim();
        let mut combat = Combat::seeded(Random::new(3));
        let me = |x: f64| Vec3::new(x, 24.0, 0.0);
        let bolt = armed(
            "electrobolt",
            &["electrobolt"],
            &[("electrobolt", rounds(5.0, 0.0))],
        );
        combat.observe(&[bolt], "me", 0);
        let facing = std::f64::consts::PI;
        for tick in [0, 10] {
            world.tick = tick;
            combat.engage(&world, "me", me(0.0), toward(facing), &sight, skill);
        }
        // Just fired the bolt: step right into cover.
        let shot = SnapshotEvent::WeaponFire {
            entity_id: "me".into(),
            weapon_id: "electrobolt".into(),
            origin: Point {
                x: 0.0,
                y: 58.0,
                z: 0.0,
            },
            dir: Point {
                x: 0.0,
                y: 0.0,
                z: -1.0,
            },
            pellet: None,
        };
        combat.observe(&[shot], "me", 40);
        world.tick = 40;
        let out = combat
            .engage(&world, "me", me(0.0), toward(facing), &sight, skill)
            .unwrap();
        assert_eq!((out.peek, out.in_sight), (Some(1), true));
        // Behind the wall the foe is out of sight, but the fight goes on:
        // no fire, still watching the foe's last spot.
        world.tick = 48;
        world.body_or_insert("me").position = Some(me(-112.0));
        let ducked = combat
            .engage(&world, "me", me(-112.0), toward(facing), &sight, skill)
            .unwrap();
        assert_eq!((ducked.in_sight, ducked.buttons), (false, 0));
        assert!(ducked.peek.is_some());
        // Long out of sight, the hold ends and the chase takes over.
        world.tick = 400;
        assert!(
            combat
                .engage(&world, "me", me(-112.0), toward(facing), &sight, skill)
                .is_none()
        );
    }

    #[test]
    fn holsters_the_blade_and_readies_the_gun_for_a_heard_enemy() {
        let skill = AimSkill {
            prediction: 1.0,
            ..Difficulty::Medium.aim()
        };
        let loadout = ["blade", "machinegun", "riotgun"];
        let full = [
            ("machinegun", rounds(30.0, 0.0)),
            ("riotgun", rounds(10.0, 0.0)),
        ];
        // Nobody about: the blade goes away for the 800-unit gun.
        let mut combat = Combat::default();
        combat.observe(&[armed("blade", &loadout, &full)], "me", 0);
        assert_eq!(combat.ready_slot(0, skill), 2);
        // Heard at 300 units: the riotgun, held for two seconds.
        combat.observe(&[armed("machinegun", &loadout, &full)], "me", 0);
        combat.heard_nearest = Some(300.0);
        assert_eq!(combat.ready_slot(10, skill), 3);
        combat.heard_nearest = None;
        assert_eq!(combat.ready_slot(200, skill), 3);
        assert_eq!(combat.ready_slot(260, skill), 0);
        // Too far to matter, or no prediction skill: nothing readied.
        combat.heard_nearest = Some(2000.0);
        assert_eq!(combat.ready_slot(600, skill), 0);
        let mut unskilled = Combat::default();
        unskilled.observe(&[armed("machinegun", &loadout, &full)], "me", 0);
        unskilled.heard_nearest = Some(300.0);
        let blind = AimSkill {
            prediction: 0.0,
            ..skill
        };
        assert_eq!(unskilled.ready_slot(0, blind), 0);
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
            0,
        );
        let fight = engaged(&mut combat, &world, me, skill).unwrap();
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
            0,
        );
        let fight = engaged(&mut combat, &world, me, skill).unwrap();
        assert_eq!((fight.weapon_slot, fight.buttons), (0, FIRE | ALT_FIRE));
        // The electrobolt counts a shot as aimed after 47 ticks of aim.
        combat.observe(
            &[armed(
                "electrobolt",
                &["electrobolt"],
                &[("electrobolt", rounds(5.0, 0.0))],
            )],
            "me",
            0,
        );
        world.tick = 100;
        assert_eq!(
            engaged(&mut combat, &world, me, skill).unwrap().buttons,
            ALT_FIRE
        );
        world.tick = 147;
        assert_eq!(
            engaged(&mut combat, &world, me, skill).unwrap().buttons,
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
            0,
        );
        assert_eq!(engaged(&mut combat, &world, me, skill).unwrap().buttons, 0);
        // The blade swings only within reach. Farther, it charges a blast.
        combat.observe(&[armed("blade", &["blade"], &[])], "me", 0);
        let far = engaged(&mut combat, &world, me, skill).unwrap();
        assert!(far.melee && far.buttons == ALT_FIRE);
        world.body_mut("foe").unwrap().position = Some(Vec3::new(0.0, 0.0, -200.0));
        assert_eq!(
            engaged(&mut combat, &world, me, skill).unwrap().buttons,
            FIRE
        );
    }

    #[test]
    fn charges_the_blade_out_of_reach_and_lets_go_on_the_aim() {
        let me = Vec3::new(0.0, 0.0, -100.0);
        let mut world = world(&[
            ("me", "", [0.0, 0.0, -100.0]),
            ("foe", "", [0.0, 0.0, -500.0]),
        ]);
        let skill = Difficulty::Medium.aim();
        let mut combat = Combat::seeded(Random::new(5));
        combat.observe(&[armed("blade", &["blade"], &[])], "me", 0);
        let facing = toward(std::f64::consts::PI);
        // Noticed by tick 30; the charge starts then.
        for tick in [0, 30] {
            world.tick = tick;
            combat.engage(&world, "me", me, facing, &sight(), skill);
        }
        // Held at least the 63-tick minimum, at most the 125-tick maximum
        // plus the half-second grace, then let go with the view on the aim.
        let mut released = None;
        for tick in 31..=30 + 125 + 63 {
            world.tick = tick;
            let fight = combat
                .engage(&world, "me", me, facing, &sight(), skill)
                .unwrap();
            if fight.buttons & ALT_FIRE == 0 {
                released = Some(tick);
                break;
            }
        }
        let released = released.expect("let go");
        assert!(
            (30 + 63..=30 + 125).contains(&released),
            "released at {released}"
        );
        // Looking away past the 12° cone, it holds through the grace.
        let mut held = Combat::seeded(Random::new(5));
        held.observe(&[armed("blade", &["blade"], &[])], "me", 0);
        let away = toward(std::f64::consts::PI + 0.5);
        for tick in [0, 30] {
            world.tick = tick;
            held.engage(&world, "me", me, away, &sight(), skill);
        }
        world.tick = released;
        assert_eq!(
            held.engage(&world, "me", me, away, &sight(), skill)
                .unwrap()
                .buttons,
            ALT_FIRE
        );
        // Out of the fight, the charge is carried for up to two seconds.
        assert!(held.carry_charge(released + 10));
        assert!(!held.carry_charge(30 + 250));
    }

    #[test]
    fn remembers_an_enemy_out_of_sight_until_it_fades_or_dies() {
        let me = Vec3::new(0.0, 0.0, -100.0);
        let mut world = world(&[
            ("me", "", [0.0, 0.0, -100.0]),
            ("foe", "", [0.0, 0.0, -500.0]),
        ]);
        world.body_mut("foe").unwrap().velocity = Some(Velocity {
            linear: Vec3::new(250.0, 0.0, 0.0),
            ..Velocity::default()
        });
        let mut combat = Combat::default();
        let skill = Difficulty::Medium.aim();
        world.tick = 1000;
        assert!(engaged(&mut combat, &world, me, skill).is_some());
        // Behind the wall across x = 0 for z > 0, one second later.
        world.body_mut("foe").unwrap().position = Some(Vec3::new(0.0, 0.0, 300.0));
        world.tick = 1125;
        assert!(engaged(&mut combat, &world, me, skill).is_none());
        let lost = combat.lost(1125).unwrap();
        assert!((lost.confidence - 0.75).abs() < 1e-9);
        assert_eq!(lost.predicted, Vec3::new(250.0, 0.0, -500.0));
        // The chase ends below 0.3 confidence, after 2.8 s.
        let last = combat.lost(1000 + 350).unwrap();
        assert!((last.predicted.x - 700.0).abs() < 1e-9);
        assert!(combat.lost(1000 + 351).is_none());
        // The memory is gone; seeing the foe again brings it back.
        world.body_mut("foe").unwrap().position = Some(Vec3::new(0.0, 0.0, -500.0));
        assert!(engaged(&mut combat, &world, me, skill).is_some());
        // A death ends the memory.
        combat.observe(
            &[SnapshotEvent::CombatState {
                entity_id: "foe".into(),
                health: 0,
                armor: 0,
                dead: true,
            }],
            "me",
            0,
        );
        assert!(combat.lost(1125).is_none());
    }
}

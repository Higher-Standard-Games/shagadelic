use std::{sync::Arc, time::Duration};

use shagger_shared::net::game_state::GameState;
use tokio::time::Instant;

use super::aim::{AimController, View};
use super::combat::{Combat, Engagement, FIRE};
use super::goals::OwnVitals;
use super::navigation::{RouteFollower, RouteObservation};
use super::orders::Orders;
use super::protocol::{PlayerInput, SnapshotEvent};
use super::random::Random;
use super::strafe::{self, Body, CombatMovement};
use super::{Difficulty, LearnedRoutes, Sight, WashoiNavigation, learned::Traversal};

pub(crate) const INPUT_PERIOD: Duration = Duration::from_millis(8);
// `InputButtons` in `engine/network/components/InputComponent.ts`.
const JUMP: u16 = 1 << 4;
const CROUCH: u16 = 1 << 5;
const MAX_BURST: u64 = 4;
const MAX_DEBT: u64 = 25;
const MAX_OBSERVATION_AGE: Duration = Duration::from_millis(500);
const RESPAWN_DELAY: Duration = Duration::from_millis(2500);

/// The pilot walks forward or follows an authored route while alive, and
/// fights enemies in sight when it has the map's sight file.
pub struct BotPlayer {
    entity_id: String,
    respawn_at: Option<Instant>,
    own_entity_present: bool,
    last_snapshot: Option<Instant>,
    started: Option<Instant>,
    sent: u64,
    navigation: Option<Arc<WashoiNavigation>>,
    route: RouteFollower,
    orders: Orders,
    planned_input: PlayerInput,
    learned: Option<Arc<LearnedRoutes>>,
    traversal: Traversal,
    linked_takeoffs: bool,
    vitals: OwnVitals,
    difficulty: Difficulty,
    sight: Option<Arc<Sight>>,
    combat: Combat,
    engagement: Option<Engagement>,
    moves: CombatMovement,
    aim: AimController,
    random: Random,
    /// A traversal sets the view itself, so the aim controller stands aside.
    exact_view: bool,
    pressed_melee: bool,
}

impl BotPlayer {
    pub fn new(entity_id: String) -> Self {
        Self {
            random: Random::for_bot(&entity_id),
            entity_id,
            respawn_at: None,
            own_entity_present: false,
            last_snapshot: None,
            started: None,
            sent: 0,
            navigation: None,
            route: RouteFollower::default(),
            orders: Orders::default(),
            learned: None,
            traversal: Traversal::default(),
            linked_takeoffs: false,
            vitals: OwnVitals::default(),
            difficulty: Difficulty::default(),
            sight: None,
            combat: Combat::default(),
            engagement: None,
            moves: CombatMovement::default(),
            aim: AimController::new(Difficulty::default().aim()),
            exact_view: false,
            pressed_melee: false,
            planned_input: PlayerInput {
                direction: [0.0, 0.0, 1.0],
                ..PlayerInput::default()
            },
        }
    }

    pub fn with_difficulty(mut self, difficulty: Difficulty) -> Self {
        self.difficulty = difficulty;
        self.aim = AimController::new(difficulty.aim());
        self
    }

    pub fn with_sight(mut self, sight: Arc<Sight>) -> Self {
        self.sight = Some(sight);
        self
    }

    pub fn with_learned_routes(mut self, library: Arc<LearnedRoutes>) -> Self {
        self.learned = Some(library);
        if self.navigation.is_none() {
            self.planned_input = PlayerInput::default();
        }
        self
    }

    pub fn learned_landing_result(&self) -> Option<bool> {
        self.traversal.landing_result()
    }

    pub(crate) fn uses_exact_movement(&self) -> bool {
        self.route.exact()
            || self.traversal.active()
            || matches!(self.traversal, Traversal::Approaching { .. })
    }

    pub fn with_navigation(mut self, navigation: Arc<WashoiNavigation>) -> Self {
        self.linked_takeoffs = navigation.has_learned_links();
        self.navigation = Some(navigation);
        self.planned_input = PlayerInput::default();
        self
    }

    pub(crate) fn observe(&mut self, world: &GameState<SnapshotEvent>, now: Instant) {
        self.combat.observe(&world.events, &self.entity_id);
        self.exact_view = self.plan(world, now);
        let position = world
            .entities
            .get(&self.entity_id)
            .and_then(|entity| entity.position);
        self.engagement = match (&self.sight, position) {
            (Some(sight), Some(position))
                if !self.exact_view && self.respawn_at.is_none() && !self.traversal.active() =>
            {
                self.combat.engage(
                    world,
                    &self.entity_id,
                    position,
                    sight,
                    self.difficulty.aim(),
                )
            }
            _ => None,
        };
        match (self.engagement, self.sight.clone(), position) {
            (Some(fight), Some(sight), Some(position)) => {
                self.fight_moves(world, fight, &sight, position);
            }
            _ => {
                self.combat.disengage();
                self.moves.reset();
            }
        }
    }

    /// Strafe across the walk while fighting, and dodge when hit. The strafe
    /// is relative to the wanted view, and the keys encoder snaps the sum to
    /// a held chord.
    fn fight_moves(
        &mut self,
        world: &GameState<SnapshotEvent>,
        fight: Engagement,
        sight: &Sight,
        position: shagger_shared::net::game_state::Vec3,
    ) {
        let own = &self.entity_id;
        let hit = world.events.iter().any(|event| {
            matches!(event, SnapshotEvent::Damage { entity_id, attacker_id, .. }
                if entity_id == own && attacker_id != own)
        });
        let velocity = world.entities.get(own).and_then(|entity| entity.velocity);
        let body = Body {
            hit,
            grounded: velocity.is_none_or(|velocity| velocity.grounded),
            speed: velocity.map_or(0.0, |velocity| velocity.linear.x.hypot(velocity.linear.z)),
        };
        let moves = self.moves.step(
            world.tick,
            self.difficulty.dodge_skill(),
            body,
            &mut self.random,
        );
        let yaw = fight.view.yaw;
        let side = f64::from(strafe::clear_side(
            sight,
            [position.x, position.y, position.z],
            yaw,
            moves.strafe,
        ));
        let [x, _, z] = self.planned_input.direction;
        let (x, z) = (x + side * yaw.cos(), z - side * yaw.sin());
        let length = x.hypot(z);
        self.planned_input.direction = if length > 1e-6 {
            [x / length, 0.0, z / length]
        } else {
            [0.0; 3]
        };
        if moves.jump {
            self.planned_input.buttons |= JUMP;
        }
        if moves.crouch {
            self.planned_input.buttons |= CROUCH;
        }
    }

    /// Movement for the coming commands. True when a traversal set the view.
    fn plan(&mut self, world: &GameState<SnapshotEvent>, now: Instant) -> bool {
        for event in &world.events {
            match event {
                SnapshotEvent::CombatState {
                    entity_id,
                    dead,
                    health,
                    armor,
                } if *entity_id == self.entity_id => {
                    self.vitals = OwnVitals {
                        health: Some(*health),
                        armor: Some(*armor),
                    };
                    if *dead {
                        self.respawn_at.get_or_insert(now + RESPAWN_DELAY);
                    } else {
                        self.respawn_at = None;
                    }
                }
                SnapshotEvent::Death { entity_id, .. } if *entity_id == self.entity_id => {
                    self.respawn_at.get_or_insert(now + RESPAWN_DELAY);
                }
                _ => {}
            }
        }
        self.last_snapshot = Some(now);
        let own = world.entities.get(&self.entity_id);
        self.own_entity_present = own.is_some();
        self.orders.observe(world, &self.entity_id);
        if self.respawn_at.is_some() || !self.own_entity_present {
            self.started = None;
            self.route.detach();
            self.orders = Orders::default();
            self.traversal = Traversal::default();
            self.aim.clear();
            self.combat.respawned();
        } else if !self.route.traversing()
            && let Some(library) = &self.learned
            && let Some(entity) = own
        {
            if let Some(input) = self.traversal.approach(library, entity, now) {
                self.planned_input = input;
                return true;
            }
            self.traversal
                .observe(library, entity, now, !self.linked_takeoffs);
            if self.traversal.active() {
                self.planned_input = PlayerInput::default();
                self.route.detach();
                return true;
            }
        }
        if !self.route.traversing()
            && self.respawn_at.is_none()
            && let Some(entity) = own
            && let Some(position) = entity.position
            && let Some(input) = self.orders.input(
                self.navigation.as_deref(),
                self.sight.as_deref(),
                position,
                entity.velocity,
                now,
            )
        {
            self.route.detach();
            self.planned_input = input;
            return false;
        }
        if let Some(navigation) = &self.navigation {
            self.planned_input = PlayerInput::default();
            if self.respawn_at.is_none()
                && let Some(position) = own.and_then(|entity| entity.position)
                && [position.x, position.y, position.z]
                    .into_iter()
                    .all(f64::is_finite)
            {
                let landed = self
                    .learned
                    .as_ref()
                    .and_then(|library| self.traversal.landed_id(library));
                self.planned_input = self.route.steer(
                    navigation,
                    position,
                    RouteObservation {
                        now,
                        vitals: self.vitals,
                        velocity: own.and_then(|entity| entity.velocity),
                        difficulty: self.difficulty,
                        pickup_active: &world.pickup_active_mask,
                        allow_takeoff: self.learned.is_some()
                            && self.traversal.landing_result().is_none(),
                        landed_id: landed,
                        timing_noise: 2.0 * self.random.next() - 1.0,
                        sight: self.sight.as_deref(),
                        fighting: self.engagement.is_some(),
                    },
                );
                if self.route.exact() {
                    return true;
                }
                if let Some(library) = &self.learned
                    && let Some(id) = self.route.takeoff(navigation)
                    && self.traversal.begin_approach(library, id, position, now)
                {
                    self.route.detach();
                    self.planned_input = PlayerInput::default();
                    return true;
                }
            }
        }
        false
    }

    pub(crate) fn request_respawn(&mut self, now: Instant) -> bool {
        if self.has_fresh_snapshot(now) && self.respawn_at.is_some_and(|at| now >= at) {
            self.respawn_at = Some(now + RESPAWN_DELAY);
            return true;
        }
        false
    }

    fn has_fresh_snapshot(&self, now: Instant) -> bool {
        self.last_snapshot
            .is_some_and(|at| now - at <= MAX_OBSERVATION_AGE)
    }

    pub(crate) fn inputs(&mut self, now: Instant) -> impl Iterator<Item = PlayerInput> {
        let ready =
            self.respawn_at.is_none() && self.own_entity_present && self.has_fresh_snapshot(now);
        let mut learned = None;
        let mut count = 0;
        if ready {
            if let Some(library) = &self.learned {
                learned = self.traversal.drive(library, now);
            }
            if learned.is_some() {
                self.started = None;
            } else {
                count = self.commands_due(now);
            }
        } else {
            self.started = None;
            self.traversal.abort_if_active();
        }
        let mut commands = learned.unwrap_or_default();
        if let Some(last) = commands.last() {
            self.aim.reset(View {
                yaw: last.yaw,
                pitch: last.pitch,
            });
        }
        commands.extend((0..count).map(|_| self.next_command()));
        commands.into_iter()
    }

    /// One planned command, with the view turned by the aim controller and
    /// fire pressed in a fight. A blade swings on each fresh press only, so
    /// melee fire alternates, as in TypeScript `pulseMeleeFire`.
    fn next_command(&mut self) -> PlayerInput {
        let mut input = self.planned_input;
        let planned = View {
            yaw: input.yaw,
            pitch: input.pitch,
        };
        if self.exact_view {
            self.aim.reset(planned);
            return input;
        }
        let Some(fight) = self.engagement else {
            self.pressed_melee = false;
            let view = self.aim.step(
                INPUT_PERIOD.as_secs_f64(),
                planned,
                false,
                true,
                &mut self.random,
            );
            input.yaw = view.yaw;
            input.pitch = view.pitch;
            return input;
        };
        let fire = fight.buttons & FIRE != 0;
        let view = self.aim.step(
            INPUT_PERIOD.as_secs_f64(),
            fight.view,
            fire,
            false,
            &mut self.random,
        );
        input.yaw = view.yaw;
        input.pitch = view.pitch;
        input.weapon_slot = fight.weapon_slot;
        let press = fire && !(fight.melee && self.pressed_melee);
        input.buttons |= fight.buttons & !FIRE;
        if press {
            input.buttons |= FIRE;
        }
        self.pressed_melee = press && fight.melee;
        input
    }

    fn commands_due(&mut self, now: Instant) -> u64 {
        let started = *self.started.get_or_insert_with(|| {
            self.sent = 0;
            now
        });
        let owed = ((now - started).as_millis() / INPUT_PERIOD.as_millis()) as u64 + 1;
        let backlog = owed.saturating_sub(self.sent);
        if backlog > MAX_DEBT {
            self.sent = owed;
            return 1;
        }
        let count = backlog.min(MAX_BURST);
        self.sent += count;
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_debt_is_bounded_and_stale_observations_stop_movement() {
        let start = Instant::now();
        let mut bot = BotPlayer::new("bot".into());
        assert_eq!(bot.inputs(start).count(), 0);
        bot.last_snapshot = Some(start);
        bot.own_entity_present = true;
        assert_eq!(bot.inputs(start).count(), 1);
        assert_eq!(bot.inputs(start + Duration::from_millis(4)).count(), 0);
        assert_eq!(bot.inputs(start + Duration::from_millis(40)).count(), 4);
        assert_eq!(bot.inputs(start + Duration::from_millis(48)).count(), 2);
        assert_eq!(bot.inputs(start + Duration::from_millis(300)).count(), 1);
        assert_eq!(bot.inputs(start + Duration::from_millis(501)).count(), 0);
    }

    #[test]
    fn turns_to_and_fires_at_an_enemy_in_sight_but_not_through_walls() {
        use crate::games::washoi::sight::tests::{cube, encode};
        use shagger_shared::net::game_state::{PlayerState, Vec3};
        let sight = Sight::parse(&encode(
            "test",
            &[cube([-8.0, -100.0, 200.0], [8.0, 200.0, 1000.0])],
            &[],
        ))
        .unwrap();
        let mut bot = BotPlayer::new("bot".into())
            .with_difficulty(Difficulty::Insane)
            .with_sight(Arc::new(sight));
        let mut world = GameState::default();
        // The foe stands level with the bot's fire origin.
        for (id, at) in [
            ("bot", Vec3::default()),
            ("foe", Vec3::new(500.0, 34.0, 0.0)),
        ] {
            world.players.insert(
                id,
                PlayerState {
                    entity_id: id.into(),
                    ..PlayerState::default()
                },
            );
            world
                .entities
                .get_or_insert_with(id, Default::default)
                .position = Some(at);
        }
        let start = Instant::now();
        let mut fired = 0;
        let mut last = PlayerInput::default();
        for step in 0..150u64 {
            let now = start + INPUT_PERIOD * step as u32;
            bot.observe(&world, now);
            for input in bot.inputs(now) {
                fired += usize::from(input.buttons & FIRE != 0);
                last = input;
            }
        }
        // Insane turns 90° well within 1.2 s, firing all the while.
        assert!((last.yaw - std::f64::consts::FRAC_PI_2).abs() < 0.05);
        assert!(fired >= 140);
        // Behind the wall, fire stops.
        world.entities.get_mut("foe").unwrap().position = Some(Vec3::new(0.0, 34.0, 500.0));
        let now = start + INPUT_PERIOD * 150;
        bot.observe(&world, now);
        assert!(bot.inputs(now).all(|input| input.buttons & FIRE == 0));
    }

    #[test]
    fn dead_bots_wait_for_a_fresh_snapshot_before_requesting_respawn() {
        let start = Instant::now();
        let mut bot = BotPlayer::new("bot".into());
        let mut world = GameState::default();
        world.entities.get_or_insert_with("bot", Default::default);
        world.events.push(SnapshotEvent::CombatState {
            entity_id: "bot".into(),
            health: 0,
            armor: 0,
            dead: true,
        });
        bot.observe(&world, start);
        assert_eq!(bot.inputs(start).count(), 0);
        let due = start + RESPAWN_DELAY;
        assert!(!bot.request_respawn(due));
        bot.observe(&world, due);
        assert!(bot.request_respawn(due));
        assert!(!bot.request_respawn(due));
        world.events[0] = SnapshotEvent::CombatState {
            entity_id: "bot".into(),
            health: 100,
            armor: 0,
            dead: false,
        };
        bot.observe(&world, due);
        assert_eq!(bot.inputs(due).count(), 1);
    }
}

use std::{collections::VecDeque, sync::Arc, time::Duration};

use crate::games::washoi::{Controls, Observation, SimTime};

use super::aim::{AimController, View};
use super::chase::Chase;
use super::combat::{ALT_FIRE, Combat, Engagement, FIRE};
use super::goals::OwnVitals;
use super::highground::HighGround;
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
/// Events kept for the next decision. A burst beyond this drops the oldest,
/// counted in `dropped_events`.
const MAX_PENDING_EVENTS: usize = 4096;

/// The pilot walks forward or follows an authored route while alive, and
/// fights enemies in sight when it has the map's sight file.
pub struct BotPlayer {
    entity_id: String,
    /// The latest observation without its events, and every event since the
    /// last decision with its snapshot tick, in order.
    latest: Option<Observation>,
    pending: VecDeque<(u32, SnapshotEvent)>,
    dropped_events: u64,
    respawn_at: Option<SimTime>,
    own_entity_present: bool,
    last_snapshot: Option<SimTime>,
    started: Option<SimTime>,
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
    chase: Chase,
    /// A ledge to fight from with a splash weapon, and the walk there.
    high_ground: HighGround,
    climb: Chase,
    /// A server order steered this tick, which takes over from a chase.
    ordered: bool,
    /// Where to look while chasing, and whether to fire into the spot.
    chasing: Option<(View, bool)>,
    aim: AimController,
    random: Random,
    /// A traversal sets the view itself, so the aim controller stands aside.
    exact_view: bool,
    pressed_melee: bool,
}

impl BotPlayer {
    pub fn new(entity_id: String) -> Self {
        let combat = Combat::seeded(Random::for_bot(&format!("{entity_id}/combos")));
        Self {
            random: Random::for_bot(&entity_id),
            entity_id,
            latest: None,
            pending: VecDeque::new(),
            dropped_events: 0,
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
            combat,
            engagement: None,
            moves: CombatMovement::default(),
            chase: Chase::default(),
            high_ground: HighGround::default(),
            climb: Chase::default(),
            ordered: false,
            chasing: None,
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

    fn uses_exact_movement(&self) -> bool {
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

    /// Take one snapshot's observation, received at `now`. This only keeps
    /// it: the latest bodies, players, and pickups replace the previous
    /// ones, and its events join the queue for the next `decide`.
    pub fn observe_at(&mut self, mut observation: Observation, now: SimTime) {
        self.last_snapshot = Some(now);
        let tick = observation.tick;
        for event in observation.events.drain(..) {
            if self.pending.len() == MAX_PENDING_EVENTS {
                self.pending.pop_front();
                self.dropped_events += 1;
            }
            self.pending.push_back((tick, event));
        }
        self.latest = Some(observation);
    }

    /// Events dropped from bursts longer than the pending queue.
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
    }

    /// Choose what to do from the latest observation and, in order, every
    /// event since the last decision. Without a new observation it plans
    /// again on the same view.
    pub fn decide(&mut self, now: SimTime) {
        let Some(mut world) = self.latest.take() else {
            return;
        };
        debug_assert!(
            self.pending
                .iter()
                .zip(self.pending.iter().skip(1))
                .all(|((a, _), (b, _))| a <= b)
        );
        world.events = self.pending.drain(..).map(|(_, event)| event).collect();
        self.decide_on(&world, now);
        world.events.clear();
        self.latest = Some(world);
    }

    fn decide_on(&mut self, world: &Observation, now: SimTime) {
        self.combat
            .observe(&world.events, &self.entity_id, world.tick);
        self.exact_view = self.plan(world, now);
        let position = world
            .body(&self.entity_id)
            .and_then(|entity| entity.position);
        self.engagement = match (&self.sight, position) {
            (Some(sight), Some(position))
                if !self.exact_view && self.respawn_at.is_none() && !self.traversal.active() =>
            {
                self.combat.engage(
                    world,
                    &self.entity_id,
                    position,
                    self.aim.current().unwrap_or(View {
                        yaw: self.planned_input.yaw,
                        pitch: self.planned_input.pitch,
                    }),
                    sight,
                    self.difficulty.aim(),
                )
            }
            _ => None,
        };
        match (self.engagement, self.sight.clone(), position) {
            (Some(fight), Some(sight), Some(position)) => {
                self.fight_moves(world, fight, &sight, position, now);
                // A guard answers the fight from the ground it was sent to hold.
                self.planned_input = self.orders.leash(position, self.planned_input);
            }
            _ => {
                self.combat.disengage();
                self.moves.reset();
                if !self.exact_view
                    && self.respawn_at.is_none()
                    && self.combat.carry_charge(world.tick)
                {
                    self.planned_input.buttons |= ALT_FIRE;
                }
            }
        }
        self.chasing = None;
        if self.engagement.is_none()
            && !self.exact_view
            && !self.ordered
            && self.respawn_at.is_none()
            && let (Some(navigation), Some(sight), Some(position)) =
                (self.navigation.clone(), self.sight.clone(), position)
            && let Some(lost) = self.combat.lost(world.tick)
        {
            self.chase_lost(world, now, &navigation, &sight, position, lost);
        } else {
            self.chase.reset();
        }
    }

    /// `chase`: walk toward where the enemy should be, watch it, and fire
    /// into it within 200 units while the memory is above 0.7.
    fn chase_lost(
        &mut self,
        world: &Observation,
        now: SimTime,
        navigation: &WashoiNavigation,
        sight: &Sight,
        position: shagger_shared::net::game_state::Vec3,
        lost: super::combat::Lost,
    ) {
        let velocity = world
            .body(&self.entity_id)
            .and_then(|entity| entity.velocity);
        let target = lost.predicted;
        let Some(input) = self
            .chase
            .steer(navigation, sight, position, velocity, target, now)
        else {
            return;
        };
        self.planned_input = input;
        let (dx, dy, dz) = (
            target.x - position.x,
            target.y - (position.y + 34.0),
            target.z - position.z,
        );
        let horizontal = dx.hypot(dz);
        if horizontal <= 0.01 {
            return;
        }
        let view = View {
            yaw: dx.atan2(dz),
            pitch: -dy.atan2(horizontal),
        };
        let prefire =
            horizontal.hypot(dy) < 200.0 && lost.confidence > 0.7 && !self.combat.holding_melee();
        self.chasing = Some((view, prefire));
    }

    /// Strafe across the walk while fighting, and dodge when hit. The strafe
    /// is relative to the wanted view, and the keys encoder snaps the sum to
    /// a held chord.
    fn fight_moves(
        &mut self,
        world: &Observation,
        fight: Engagement,
        sight: &Sight,
        position: shagger_shared::net::game_state::Vec3,
        now: SimTime,
    ) {
        let climb = self.climb_input(world, fight, sight, position, now);
        let own = &self.entity_id;
        let hit = world.events.iter().any(|event| {
            matches!(event, SnapshotEvent::Damage { entity_id, attacker_id, .. }
                if entity_id == own && attacker_id != own)
        });
        let velocity = world.body(own).and_then(|entity| entity.velocity);
        let origin = [position.x, position.y, position.z];
        let body = Body {
            hit,
            grounded: velocity.is_none_or(|velocity| velocity.grounded),
            speed: velocity.map_or(0.0, |velocity| velocity.linear.x.hypot(velocity.linear.z)),
            position: origin,
            yaw: fight.view.yaw,
        };
        // TypeScript reads every `proj_` entity in the snapshot. Here a
        // projectile counts only when a sight ray from the eye reaches it.
        let eye = [position.x, position.y + 32.0, position.z];
        let projectiles: Vec<_> = world
            .bodies
            .iter()
            .filter(|(id, _)| id.starts_with("proj_"))
            .filter_map(|(id, entity)| {
                let at = entity.position?;
                let velocity = entity.velocity?.linear;
                let at = [at.x, at.y, at.z];
                sight.clear(eye, at).then_some(strafe::Projectile {
                    id: id.as_str(),
                    position: at,
                    velocity: [velocity.x, velocity.y, velocity.z],
                })
            })
            .collect();
        let skills = strafe::Skills {
            dodge: self.difficulty.dodge_skill(),
            reaction_ms: self.difficulty.aim().reaction_ms,
        };
        let moves = self
            .moves
            .step(world.tick, skills, body, &projectiles, &mut self.random);
        let yaw = fight.view.yaw;
        let side = f64::from(strafe::clear_side(
            sight,
            [position.x, position.y, position.z],
            yaw,
            moves.strafe,
        ));
        let [x, _, z] = self.planned_input.direction;
        // An order that says stand means stand. Since 9507ab5 an order leaves
        // the bot still only on a `move_to` arrival: an S&D carrier planting
        // or a defender defusing. Washoi's `BombModel` starts either only
        // below `maxPlantSpeed`, so a strafe or dodge there would hold the
        // plant off until the fight ends. `hold_area`, `defend`, and `escort`
        // patrol with a direction and keep their strafe. Aim and fire go on.
        if self.ordered && x == 0.0 && z == 0.0 {
            return;
        }
        // Peeking from cover, the bot keeps its spot: only the side step.
        // Making for high ground, the walk there replaces the strafe. A server
        // order outranks both: the bot keeps walking it and only strafes.
        let peek = fight.peek.filter(|_| !self.ordered);
        let (x, z) = match (peek, climb) {
            (Some(peek), _) if !self.moves.dodging(world.tick) => {
                let peek = f64::from(peek);
                (peek * yaw.cos(), -peek * yaw.sin())
            }
            (None, Some([x, _, z])) => (x, z),
            _ => (x + side * yaw.cos(), z - side * yaw.sin()),
        };
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

    /// `BotFight.highGround`: with a splash weapon, out of cover, the walk to
    /// a ledge above the enemy, when one is found and the graph leads there.
    fn climb_input(
        &mut self,
        world: &Observation,
        fight: Engagement,
        sight: &Sight,
        position: shagger_shared::net::game_state::Vec3,
        now: SimTime,
    ) -> Option<[f64; 3]> {
        let navigation = self.navigation.clone()?;
        if self.ordered || !fight.splash || !fight.in_sight || fight.peek.is_some() {
            self.high_ground.reset();
            self.climb.reset();
            return None;
        }
        let can_see = |from: [f64; 3], to: [f64; 3]| sight.clear(from, to);
        let reachable = |node: super::Vec3| {
            navigation
                .objective_walk(position, node, 48.0, None, Some(sight))
                .is_some()
        };
        let spot = self.high_ground.spot(
            world.tick,
            navigation.nodes(),
            position,
            fight.target,
            self.difficulty.dodge_skill(),
            &can_see,
            &reachable,
            &mut self.random,
        );
        let Some(spot) = spot else {
            self.climb.reset();
            return None;
        };
        let velocity = world.body(&self.entity_id).and_then(|body| body.velocity);
        self.climb
            .steer(&navigation, sight, position, velocity, spot, now)
            .map(|input| input.direction)
            .filter(|direction| *direction != [0.0; 3])
    }

    /// Movement for the coming commands. True when a traversal set the view.
    fn plan(&mut self, world: &Observation, now: SimTime) -> bool {
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
        let own = world.body(&self.entity_id);
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
            self.ordered = true;
            return false;
        }
        self.ordered = false;
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
                        pickup_active: &world.pickup_active,
                        allow_takeoff: self.learned.is_some()
                            && self.traversal.landing_result().is_none(),
                        landed_id: landed,
                        timing_noise: 2.0 * self.random.next() - 1.0,
                        sight: self.sight.as_deref(),
                        fighting: self.engagement.is_some() || self.chasing.is_some(),
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

    /// Whether to ask the server for a respawn now. Asking starts its delay.
    pub fn request_respawn_at(&mut self, now: SimTime) -> bool {
        if self.has_fresh_snapshot(now) && self.respawn_at.is_some_and(|at| now >= at) {
            self.respawn_at = Some(now + RESPAWN_DELAY);
            return true;
        }
        false
    }

    fn has_fresh_snapshot(&self, now: SimTime) -> bool {
        self.last_snapshot
            .is_some_and(|at| now - at <= MAX_OBSERVATION_AGE)
    }

    /// The controls due by `now`, paced at the Washoi input period. Recorded
    /// traversals and precise approaches come out `exact`.
    pub fn inputs_at(&mut self, now: SimTime) -> Vec<Controls> {
        let exact = self.uses_exact_movement();
        self.paced_inputs(now)
            .into_iter()
            .map(|input| Controls {
                direction: input.direction,
                yaw: input.yaw,
                pitch: input.pitch,
                buttons: input.buttons,
                weapon_slot: input.weapon_slot,
                exact,
            })
            .collect()
    }

    fn paced_inputs(&mut self, now: SimTime) -> Vec<PlayerInput> {
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
        commands
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
        if self.engagement.is_none()
            && let Some((wanted, prefire)) = self.chasing
        {
            self.pressed_melee = false;
            let view = self.aim.step(
                INPUT_PERIOD.as_secs_f64(),
                wanted,
                prefire,
                false,
                &mut self.random,
            );
            input.yaw = view.yaw;
            input.pitch = view.pitch;
            if prefire {
                input.buttons |= FIRE;
            }
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

    fn commands_due(&mut self, now: SimTime) -> u64 {
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
        let start = SimTime::ZERO;
        let mut bot = BotPlayer::new("bot".into());
        assert_eq!(bot.inputs_at(start).len(), 0);
        bot.last_snapshot = Some(start);
        bot.own_entity_present = true;
        assert_eq!(bot.inputs_at(start).len(), 1);
        assert_eq!(bot.inputs_at(start + Duration::from_millis(4)).len(), 0);
        assert_eq!(bot.inputs_at(start + Duration::from_millis(40)).len(), 4);
        assert_eq!(bot.inputs_at(start + Duration::from_millis(48)).len(), 2);
        assert_eq!(bot.inputs_at(start + Duration::from_millis(300)).len(), 1);
        assert_eq!(bot.inputs_at(start + Duration::from_millis(501)).len(), 0);
    }

    #[test]
    fn turns_to_and_fires_at_an_enemy_in_sight_but_not_through_walls() {
        use crate::games::washoi::sight::tests::{cube, encode};
        use crate::games::washoi::{PlayerView, Vec3};
        let sight = Sight::parse(&encode(
            "test",
            &[cube([-8.0, -100.0, 200.0], [8.0, 200.0, 1000.0])],
            &[],
        ))
        .unwrap();
        let mut bot = BotPlayer::new("bot".into())
            .with_difficulty(Difficulty::Insane)
            .with_sight(Arc::new(sight));
        let mut world = Observation::default();
        // The foe stands level with the bot's fire origin, 45° to the right
        // of where the bot starts looking: inside its 55° view.
        for (id, at) in [
            ("bot", Vec3::default()),
            ("foe", Vec3::new(400.0, 34.0, 400.0)),
        ] {
            world.players.push(PlayerView {
                entity_id: id.into(),
                ..PlayerView::default()
            });
            world.body_or_insert(id).position = Some(at);
        }
        let start = SimTime::ZERO;
        let mut fired = 0;
        let mut last = Controls::default();
        for step in 0..150u64 {
            let now = start + INPUT_PERIOD * step as u32;
            world.tick = step as u32;
            bot.observe_at(world.clone(), now);
            bot.decide(now);
            for input in bot.inputs_at(now) {
                fired += usize::from(input.buttons & FIRE != 0);
                last = input;
            }
        }
        // Insane notices it within 4 ticks and turns to it well within
        // 1.2 s, firing all the while.
        assert!((last.yaw - std::f64::consts::FRAC_PI_4).abs() < 0.05);
        assert!(fired >= 140);
        // Behind the wall, fire stops.
        world.body_mut("foe").unwrap().position = Some(Vec3::new(0.0, 34.0, 500.0));
        let now = start + INPUT_PERIOD * 150;
        world.tick = 150;
        bot.observe_at(world.clone(), now);
        bot.decide(now);
        assert!(
            bot.inputs_at(now)
                .into_iter()
                .all(|input| input.buttons & FIRE == 0)
        );
    }

    #[test]
    fn chases_an_enemy_that_ducked_out_of_sight() {
        use crate::games::washoi::sight::tests::{cube, encode};
        use crate::games::washoi::{PlayerView, Vec3};
        // A floor, and a wall that hides x > 200 from the bot at the origin.
        let sight = Sight::parse(&encode(
            "test",
            &[
                cube([-500.0, -10.0, -500.0], [1000.0, 0.0, 500.0]),
                cube([200.0, 0.0, 40.0], [210.0, 200.0, 500.0]),
            ],
            &[],
        ))
        .unwrap();
        let navigation: WashoiNavigation = serde_json::from_str(
            r#"{
            "map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,24,0],[150,24,0],[300,24,0],[300,24,150]],
                "edges":[[0,1],[1,0],[1,2],[2,1],[2,3],[3,2]],"entries":[0],"goals":[0,3]
            }
        }"#,
        )
        .unwrap();
        let mut bot = BotPlayer::new("bot".into())
            .with_navigation(Arc::new(navigation))
            .with_sight(Arc::new(sight));
        let mut world = Observation::default();
        for (id, at) in [
            ("bot", Vec3::new(0.0, 24.0, 0.0)),
            ("foe", Vec3::new(300.0, 24.0, 0.0)),
        ] {
            world.players.push(PlayerView {
                entity_id: id.into(),
                ..PlayerView::default()
            });
            world.body_or_insert(id).position = Some(at);
        }
        let start = SimTime::ZERO;
        // Ahead along the walk, so in view; noticed after 18 ticks.
        world.tick = 100;
        bot.observe_at(world.clone(), start);
        bot.decide(start);
        world.tick = 120;
        bot.observe_at(world.clone(), start + Duration::from_millis(160));
        bot.decide(start + Duration::from_millis(160));
        assert!(bot.engagement.is_some());
        // Behind the wall now, so the bot walks toward where it was seen.
        world.body_mut("foe").unwrap().position = Some(Vec3::new(300.0, 24.0, 150.0));
        world.tick = 130;
        bot.observe_at(world.clone(), start + Duration::from_millis(240));
        bot.decide(start + Duration::from_millis(240));
        assert!(bot.engagement.is_none());
        let (view, prefire) = bot.chasing.expect("chasing");
        assert!((view.yaw - std::f64::consts::FRAC_PI_2).abs() < 1e-6);
        assert!(!prefire);
        assert_eq!(bot.planned_input.direction, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn one_decision_applies_a_burst_of_events_in_order_on_the_latest_view() {
        use crate::games::washoi::Vec3;
        let state = |dead: bool| SnapshotEvent::CombatState {
            entity_id: "bot".into(),
            health: if dead { 0 } else { 100 },
            armor: 0,
            dead,
        };
        let frame = |tick: u32, x: f64, events: Vec<SnapshotEvent>| {
            let mut world = Observation {
                tick,
                events,
                ..Observation::default()
            };
            world.body_or_insert("bot").position = Some(Vec3::new(x, 0.0, 0.0));
            world
        };
        let start = SimTime::ZERO;
        // Died, then came back, inside one burst: the bot ends alive.
        let mut bot = BotPlayer::new("bot".into());
        bot.observe_at(frame(1, 0.0, vec![state(true)]), start);
        bot.observe_at(frame(2, 10.0, vec![state(false)]), start);
        bot.observe_at(frame(3, 20.0, Vec::new()), start);
        assert_eq!(bot.pending.len(), 2);
        assert!(bot.respawn_at.is_none(), "observing alone decides nothing");
        bot.decide(start);
        assert!(bot.respawn_at.is_none() && bot.pending.is_empty());
        let latest = bot.latest.as_ref().unwrap();
        assert_eq!(latest.tick, 3);
        assert_eq!(
            latest.body("bot").unwrap().position,
            Some(Vec3::new(20.0, 0.0, 0.0))
        );
        assert!(latest.events.is_empty());
        // The other order ends dead and starts the respawn delay.
        let mut bot = BotPlayer::new("bot".into());
        bot.observe_at(frame(1, 0.0, vec![state(false)]), start);
        bot.observe_at(frame(2, 0.0, vec![state(true)]), start);
        bot.decide(start);
        assert_eq!(bot.respawn_at, Some(start + RESPAWN_DELAY));
        // A burst past the queue drops its oldest events, and says so.
        let mut bot = BotPlayer::new("bot".into());
        let many = (0..MAX_PENDING_EVENTS + 3).map(|_| state(false)).collect();
        bot.observe_at(frame(1, 0.0, many), start);
        assert_eq!(bot.pending.len(), MAX_PENDING_EVENTS);
        assert_eq!(bot.dropped_events(), 3);
    }

    #[test]
    fn a_server_order_outranks_cover_while_fighting() {
        use crate::games::washoi::{
            Vec3,
            sight::tests::{cube, encode},
        };
        // No floor anywhere, so strafing never finds room: only the walk and
        // the peek move the bot.
        let sight = Sight::parse(&encode("test", &[cube([1000.0; 3], [1001.0; 3])], &[])).unwrap();
        let fight = Engagement {
            view: View {
                yaw: 0.0,
                pitch: 0.0,
            },
            buttons: 0,
            melee: false,
            weapon_slot: 0,
            peek: Some(0),
            in_sight: true,
            target: Vec3::new(0.0, 24.0, 600.0),
            splash: false,
        };
        let world = Observation::default();
        let here = Vec3::new(0.0, 24.0, 0.0);
        for (ordered, expected) in [(true, [1.0, 0.0, 0.0]), (false, [0.0; 3])] {
            let mut bot = BotPlayer::new("bot".into());
            bot.ordered = ordered;
            bot.planned_input.direction = [1.0, 0.0, 0.0];
            bot.fight_moves(&world, fight, &sight, here, SimTime::ZERO);
            assert_eq!(bot.planned_input.direction, expected, "ordered: {ordered}");
        }
    }

    #[test]
    fn a_bot_planting_on_order_holds_still_but_a_patrolling_guard_strafes() {
        use crate::games::washoi::{
            Vec3,
            sight::tests::{cube, encode},
        };
        // Open floor all round, so strafing always finds room.
        let sight = Sight::parse(&encode(
            "test",
            &[cube([-1000.0, -10.0, -1000.0], [1000.0, 0.0, 1000.0])],
            &[],
        ))
        .unwrap();
        let fight = Engagement {
            view: View {
                yaw: 0.0,
                pitch: 0.0,
            },
            buttons: FIRE,
            melee: false,
            weapon_slot: 0,
            peek: None,
            in_sight: true,
            target: Vec3::new(0.0, 24.0, 600.0),
            splash: false,
        };
        // Hit this tick: a skilled bot would dodge-jump.
        let mut world = Observation::default();
        world.events.push(SnapshotEvent::Damage {
            entity_id: "bot".into(),
            attacker_id: "foe".into(),
            damage: 10,
            weapon_id: "machinegun".into(),
            at: crate::games::washoi::protocol::Point {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            hit_location: None,
            pain_tier: None,
        });
        let here = Vec3::new(0.0, 24.0, 0.0);
        // Planting: the order stands still, and so does the fight.
        let mut planter = BotPlayer::new("bot".into()).with_difficulty(Difficulty::Insane);
        planter.ordered = true;
        planter.planned_input.direction = [0.0; 3];
        for tick in 0..60 {
            world.tick = tick;
            planter.fight_moves(&world, fight, &sight, here, SimTime::ZERO);
            assert_eq!(planter.planned_input.direction, [0.0; 3]);
            assert_eq!(planter.planned_input.buttons & (JUMP | CROUCH), 0);
        }
        // A guard patrolling under an order keeps strafing off its walk.
        let mut guard = BotPlayer::new("bot".into()).with_difficulty(Difficulty::Insane);
        let mut strafed = false;
        for tick in 0..200 {
            world.tick = tick;
            guard.ordered = true;
            guard.planned_input.direction = [1.0, 0.0, 0.0];
            guard.fight_moves(&world, fight, &sight, here, SimTime::ZERO);
            strafed |= guard.planned_input.direction != [1.0, 0.0, 0.0];
        }
        assert!(strafed);
    }

    #[test]
    fn dead_bots_wait_for_a_fresh_snapshot_before_requesting_respawn() {
        let start = SimTime::ZERO;
        let mut bot = BotPlayer::new("bot".into());
        let mut world = Observation::default();
        world.body_or_insert("bot");
        world.events.push(SnapshotEvent::CombatState {
            entity_id: "bot".into(),
            health: 0,
            armor: 0,
            dead: true,
        });
        bot.observe_at(world.clone(), start);
        bot.decide(start);
        assert_eq!(bot.inputs_at(start).len(), 0);
        let due = start + RESPAWN_DELAY;
        assert!(!bot.request_respawn_at(due));
        bot.observe_at(world.clone(), due);
        bot.decide(due);
        assert!(bot.request_respawn_at(due));
        assert!(!bot.request_respawn_at(due));
        world.events[0] = SnapshotEvent::CombatState {
            entity_id: "bot".into(),
            health: 100,
            armor: 0,
            dead: false,
        };
        bot.observe_at(world.clone(), due);
        bot.decide(due);
        assert_eq!(bot.inputs_at(due).len(), 1);
    }
}

#[cfg(test)]
mod replay;

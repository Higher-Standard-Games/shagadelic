//! The player bot: a Washoi agent that plays as a networked player.

pub(in crate::games::washoi) mod callouts;
pub(in crate::games::washoi) mod chat;
pub(in crate::games::washoi) mod chat_lines;
pub(in crate::games::washoi) mod combat;
pub(in crate::games::washoi) mod look;
pub(in crate::games::washoi) mod movement;
pub(in crate::games::washoi) mod navigation;
pub(in crate::games::washoi) mod strategy;
pub mod trace;

use std::{collections::VecDeque, sync::Arc, time::Duration};

use crate::games::washoi::{
    Controls, Observation, SimTime,
    agents::{Agent, NetworkedPlayer},
    observation::INPUT_PERIOD,
};

use crate::games::washoi::agents::player::callouts::{Callouts, Kind as CallKind, Locations};
use crate::games::washoi::agents::player::chat::{Chat, Frag};
use crate::games::washoi::agents::player::combat::aim::{AimController, View};
use crate::games::washoi::agents::player::combat::lunge::{self, Lunge, Mark, Runner};
use crate::games::washoi::agents::player::combat::mood::{BotMood, Mistake};
use crate::games::washoi::agents::player::combat::safety;
use crate::games::washoi::agents::player::combat::{ALT_FIRE, Combat, Engagement, FIRE};
use crate::games::washoi::agents::player::look::{self as looking, Look};
use crate::games::washoi::agents::player::movement::chase::Chase;
use crate::games::washoi::agents::player::movement::escape::Escape;
use crate::games::washoi::agents::player::movement::highground::{self, HighGround};
use crate::games::washoi::agents::player::movement::strafe::{self, Body, CombatMovement};
use crate::games::washoi::agents::player::movement::territory::Territory;
use crate::games::washoi::agents::player::navigation::{
    RouteFollower, RouteObservation,
    hop::{HopChain, HopView},
    slide::{RouteSlide, SlideView},
    unstick::{Unstick, UnstickView},
};
use crate::games::washoi::agents::player::strategy::candidates::{Candidate, GoalTarget};
use crate::games::washoi::agents::player::strategy::considerations::{Me, Situation};
use crate::games::washoi::agents::player::strategy::goals::OwnVitals;
use crate::games::washoi::agents::player::strategy::intel::{Baseline, PeopleProfile};
use crate::games::washoi::agents::player::strategy::match_read::{MatchRead, Moment};
use crate::games::washoi::agents::player::strategy::opponents::{Opponents, Scene};
use crate::games::washoi::agents::player::strategy::orders::Orders;
pub use crate::games::washoi::agents::player::strategy::planner::HunchMode;
use crate::games::washoi::agents::player::strategy::planner::{KnownPickup, Planner};
use crate::games::washoi::agents::player::strategy::strategist::{Reading, Strategist};
use crate::games::washoi::agents::player::strategy::weights::MatchContext;
use crate::games::washoi::difficulty::AimSkill;
use crate::games::washoi::hosting::net::protocol::{PlayerInput, SnapshotEvent};
use crate::games::washoi::random::Random;
use crate::games::washoi::{
    Difficulty, LearnedRoutes, Sight, WashoiNavigation,
    agents::player::navigation::learned::Traversal,
};

// `InputButtons` in `engine/network/components/InputComponent.ts`.
const JUMP: u16 = 1 << 4;
const CROUCH: u16 = 1 << 5;
const SPRINT: u16 = 1 << 6;
/// `WILD_SHOT_OFFSET`: a wild shot opens fire this far to one side.
const WILD_SHOT_OFFSET: f64 = 5.0 * std::f64::consts::PI / 180.0;
/// `ITEM_GLANCE_RANGE`.
const ITEM_GLANCE_RANGE: f64 = 1200.0;
const MAX_BURST: u64 = 4;
const MAX_DEBT: u64 = 25;
const MAX_OBSERVATION_AGE: Duration = Duration::from_millis(500);
const RESPAWN_DELAY: Duration = Duration::from_millis(2500);
/// Events kept for the next decision. A burst beyond this drops the oldest,
/// counted in `dropped_events`.
const MAX_PENDING_EVENTS: usize = 4096;
/// How long the route follower chooses freely after it could not plan to
/// the arbiter's pickup.
const FREE_ROAM: Duration = Duration::from_secs(2);
/// `CONTEST_SENSE`: the match sense at which a bot contests the items an
/// opponent keeps taking.
const CONTEST_SENSE: f64 = 0.6;

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
    look: Look,
    mood: BotMood,
    /// Where the bot and its living teammates were at the last decision,
    /// and whether the trigger was down on the aim.
    position: Option<super::super::Vec3>,
    allies: Vec<super::super::Vec3>,
    shooting: bool,
    /// Whether the last decision had a fight, to see fights start and end.
    fighting: bool,
    unstick: Unstick,
    slide: RouteSlide,
    hop: HopChain,
    /// A ledge to fight from with a splash weapon, and the walk there.
    high_ground: HighGround,
    climb: Chase,
    /// What to go for, the match it is chosen in, and the server ticks the
    /// bot was hurt at lately.
    planner: Planner,
    /// A HUNCH model, and whether to record features, kept so a new
    /// planner gets them too.
    hunch: Option<(Arc<crate::hunch::Model>, HunchMode)>,
    recording: bool,
    /// Chat and team callouts, the map's location names they use, the
    /// lines waiting to be sent, the directive calls already made, and the
    /// last enemy sighting a teammate called.
    chat: Chat,
    callouts: Callouts,
    locations: Option<Arc<Locations>>,
    /// `intel/`: people's typical stacks, and the map's strategy payoffs,
    /// applied once the gametype is known.
    people: Option<Arc<PeopleProfile>>,
    baseline: Option<Arc<Baseline>>,
    priors_for: Option<String>,
    outgoing: Vec<String>,
    said_calls: Vec<String>,
    team_sighting: Option<(super::super::Vec3, u32)>,
    hunt_walk: Chase,
    /// The match as read so far, and the strategy that answers it.
    match_read: MatchRead,
    strategist: Strategist,
    /// The ground held in a Duel, and the walk there.
    territory: Territory,
    territory_walk: Chase,
    /// The way out of a fight a fading strategy leaves, and the walk there.
    escape: Escape,
    escape_walk: Chase,
    /// `BotOptions.trace`: a record of each decision.
    tracing: bool,
    trace: Option<trace::DecisionTrace>,
    /// The arbiter was consulted this decision.
    arbitrated: bool,
    lunge: Lunge,
    opponents: Opponents,
    match_context: Option<MatchContext>,
    recent_damage: Vec<u32>,
    /// Pickup walks as priced from the last graph node, and where.
    /// Pickups with a walk from where the bot last decided, and the walk
    /// between pickup pairs, which the map fixes.
    walkable: Vec<usize>,
    /// The follower chooses its own way until then, after failing to plan
    /// to the goal set.
    free_until: Option<SimTime>,
    /// `SHAGADELIC_GOAL_ARBITER=verbose` logs each new goal choice.
    verbose_goals: bool,
    last_goal: Option<GoalTarget>,
    pair_costs: std::collections::HashMap<(usize, usize), Option<f64>>,
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
        let chat = Chat::new(&entity_id, Random::for_bot(&format!("{entity_id}/chat")));
        let callouts = Callouts::new(Random::for_bot(&format!("{entity_id}/callouts")));
        let strategist = Strategist::new(
            Difficulty::default().match_sense(),
            Random::for_bot(&format!("{entity_id}/strategy")),
        );
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
            look: Look::default(),
            mood: BotMood::default(),
            position: None,
            allies: Vec::new(),
            shooting: false,
            fighting: false,
            unstick: Unstick::default(),
            planner: Planner::new(Difficulty::default().mistake_rate()),
            hunch: None,
            recording: false,
            chat,
            callouts,
            locations: None,
            people: None,
            baseline: None,
            priors_for: None,
            outgoing: Vec::new(),
            said_calls: Vec::new(),
            team_sighting: None,
            hunt_walk: Chase::default(),
            match_read: MatchRead::default(),
            strategist,
            territory: Territory::default(),
            territory_walk: Chase::default(),
            escape: Escape::default(),
            escape_walk: Chase::default(),
            tracing: false,
            trace: None,
            arbitrated: false,
            lunge: Lunge::default(),
            opponents: Opponents::default(),
            match_context: None,
            recent_damage: Vec::new(),
            walkable: Vec::new(),
            free_until: None,
            verbose_goals: std::env::var("SHAGADELIC_GOAL_ARBITER")
                .is_ok_and(|mode| mode == "verbose"),
            last_goal: None,
            pair_costs: std::collections::HashMap::new(),
            slide: RouteSlide::default(),
            hop: HopChain::default(),
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

    /// The name the bot plays under, which sets its chat personality.
    pub fn with_name(mut self, name: &str) -> Self {
        self.chat = Chat::new(name, Random::for_bot(&format!("{}/chat", self.entity_id)));
        self
    }

    /// How people's stacks grow with time alive, for estimating humans.
    pub fn with_people(mut self, people: Arc<PeopleProfile>) -> Self {
        self.people = Some(people);
        self
    }

    /// What strategies paid on this map in earlier matches.
    pub fn with_baseline(mut self, baseline: Arc<Baseline>) -> Self {
        self.baseline = Some(baseline);
        self
    }

    /// The map's location names, for team callouts.
    pub fn with_locations(mut self, locations: Arc<Locations>) -> Self {
        self.locations = Some(locations);
        self
    }

    /// Chat lines ready to send.
    pub fn chat_at(&mut self, _now: SimTime) -> Vec<String> {
        std::mem::take(&mut self.outgoing)
    }

    /// Let a HUNCH model of schema `washoi.goal.v0` score goal candidates,
    /// in shadow or deciding. The artifact is checked against the schema.
    pub fn with_hunch(mut self, model: Arc<crate::hunch::Model>, mode: HunchMode) -> Self {
        if model.validate(strategy::hunch_features::SCHEMA).is_ok()
            && model.width() == strategy::hunch_features::WIDTH
        {
            self.hunch = Some((model.clone(), mode));
            self.planner.set_hunch(model, mode);
        }
        self
    }

    /// Keep each decision's goal candidates with their HUNCH features
    /// (`washoi.goal.v0`), for `hunch extract`.
    pub fn with_hunch_recording(mut self) -> Self {
        self.recording = true;
        self.planner.record();
        self
    }

    /// The goal candidates and feature rows of the last decision, when the
    /// arbiter was consulted: IDs, then one row per ID.
    pub fn hunch_record(&self) -> Option<(Vec<&str>, &[f32])> {
        self.arbitrated.then(|| self.planner.recorded())
    }

    /// `washoi.goal.v0`: the schema name and feature names, in row order.
    pub fn hunch_schema() -> (&'static str, &'static [&'static str]) {
        (
            strategy::hunch_features::SCHEMA,
            &strategy::hunch_features::NAMES,
        )
    }

    /// Keep a `DecisionTrace` of each decision, for tools.
    pub fn with_trace(mut self, tracing: bool) -> Self {
        self.tracing = tracing;
        self
    }

    /// The record of the last decision, for a bot made `with_trace(true)`.
    pub fn last_trace(&self) -> Option<&trace::DecisionTrace> {
        self.trace.as_ref()
    }

    pub fn with_difficulty(mut self, difficulty: Difficulty) -> Self {
        self.difficulty = difficulty;
        self.planner = Planner::new(difficulty.mistake_rate());
        if let Some((model, mode)) = self.hunch.clone() {
            self.planner.set_hunch(model, mode);
        }
        if self.recording {
            self.planner.record();
        }
        self.strategist = Strategist::new(
            difficulty.match_sense(),
            Random::for_bot(&format!("{}/strategy", self.entity_id)),
        );
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
            || self.hop.active()
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
        self.arbitrated = false;
        self.combat
            .observe(&world.events, &self.entity_id, world.tick);
        self.exact_view = self.plan(world, now);
        self.read_opponents(world);
        self.read_match(world);
        self.talk(world, now);
        self.mood.update(
            &self.entity_id,
            &world.events,
            self.vitals.health,
            self.respawn_at.is_some(),
            self.opponents.armed_enemy(world.tick),
            world.tick,
        );
        // `aimDifficulty`: a tilted bot aims looser and reacts slower.
        let base = self.difficulty.aim();
        let (accuracy, reaction_ms) = self.mood.aim(base.accuracy, base.reaction_ms);
        self.aim.set_skill(AimSkill {
            accuracy,
            reaction_ms,
            ..base
        });
        let position = world
            .body(&self.entity_id)
            .and_then(|entity| entity.position);
        self.face_teleport_exit(position);
        // A strafe-jump chain gives way to a fight outside its takeoff
        // commitment, as TypeScript interrupts it on a change of state.
        let hop_yields = self.hop.active() && self.hop.interruptible(world.tick);
        self.engagement = match (&self.sight, position) {
            (Some(sight), Some(position))
                if (!self.exact_view || hop_yields)
                    && self.respawn_at.is_none()
                    && !self.traversal.active() =>
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
        if hop_yields && self.engagement.is_some() {
            self.hop.reset();
            self.exact_view = false;
        }
        self.apply_mood(world, position);
        self.position = position;
        self.allies = self.combat.allies(world, &self.entity_id);
        match (self.engagement, self.sight.clone(), position) {
            (Some(fight), Some(sight), Some(position)) => {
                // `RETREAT`: out of the enemy's sight first, then to health
                // on the route, the aim still on the enemy.
                if self.should_fade(world) {
                    self.fade(world, fight, &sight, position, now);
                } else {
                    self.escape.reset();
                    self.escape_walk.reset();
                    self.fight_moves(world, fight, &sight, position, now);
                }
                // A guard answers the fight from the ground it was sent to hold.
                self.planned_input = self.orders.leash(position, self.planned_input);
            }
            _ => {
                self.combat.disengage();
                self.moves.reset();
                if !self.exact_view && self.respawn_at.is_none() {
                    if self.combat.carry_charge(world.tick) {
                        self.planned_input.buttons |= ALT_FIRE;
                    }
                    self.planned_input.weapon_slot =
                        self.combat.ready_slot(world.tick, self.difficulty.aim());
                }
            }
        }
        self.blade_lunge(world, position);
        let was_chasing = self.chasing.take().is_some();
        // The `forget` mistake drops an enemy as soon as it is out of sight.
        let forgot = self
            .mood
            .mistake(world.tick)
            .is_some_and(|(mistake, _)| mistake == Mistake::Forget);
        if self.engagement.is_none()
            && !forgot
            && !self.exact_view
            && !self.ordered
            && self.respawn_at.is_none()
            && let (Some(navigation), Some(sight), Some(position)) =
                (self.navigation.clone(), self.sight.clone(), position)
            && self.strategist.stance().chase
            && let Some(lost) = self.combat.lost(world.tick)
            && !self.divert_to_item(world, position, lost, was_chasing)
        {
            self.chase_lost(world, now, &navigation, &sight, position, lost);
        } else {
            self.chase.reset();
        }
        self.hold_ground(world, position, now);
        self.look_around(world, position);
        self.turn_from_block(world, position, now);
        if self.tracing {
            self.trace = Some(self.record(world, position));
        }
    }

    /// `DecisionTrace` for this decision.
    fn record(
        &self,
        world: &Observation,
        position: Option<super::super::Vec3>,
    ) -> trace::DecisionTrace {
        let state = if self.respawn_at.is_some() {
            "dead"
        } else if self.lunge.active() {
            "lunge"
        } else if self.exact_view {
            "traversal"
        } else if self.engagement.is_some() {
            "fight"
        } else if self.chasing.is_some() {
            "chase"
        } else if self.ordered {
            "order"
        } else {
            "roam"
        };
        let (goal, candidates, gated) = if self.arbitrated {
            self.planner.last()
        } else {
            (None, &[][..], 0)
        };
        let velocity = world.body(&self.entity_id).and_then(|body| body.velocity);
        trace::DecisionTrace {
            tick: world.tick,
            state,
            orders: self
                .orders
                .candidates(position.unwrap_or_default())
                .map(|order| order.id)
                .collect(),
            goal: goal.map(str::to_owned),
            candidates: candidates.to_vec(),
            gated,
            explain: if self.arbitrated {
                self.planner.explain().to_owned()
            } else {
                String::new()
            },
            sequence: self.lunge.active().then_some("combat_slide_blade"),
            position: position.map(|at| [at.x, at.y, at.z]),
            speed: velocity.map_or(0.0, |v| v.linear.x.hypot(v.linear.z)),
            grounded: velocity.is_some_and(|v| v.grounded),
            crouching: velocity.is_some_and(|v| v.is_crouching),
            health: self.vitals.health,
            armor: self.vitals.armor,
            weapon: self.combat.weapon().to_owned(),
            strategy: self.strategist.explain(),
            hunch: if self.arbitrated {
                self.planner.hunch_pick().map(str::to_owned)
            } else {
                None
            },
        }
    }

    /// Whatever the state, a bot that asks to move and goes nowhere turns
    /// away from what holds it and thinks again (`navigation::unstick`). A
    /// recorded maneuver sets its own movement and is left alone.
    fn turn_from_block(
        &mut self,
        world: &Observation,
        position: Option<shagger_shared::net::game_state::Vec3>,
        now: SimTime,
    ) {
        let Some(position) = position
            .filter(|_| !self.exact_view && self.respawn_at.is_none() && !self.traversal.active())
        else {
            self.unstick.reset();
            return;
        };
        let own = &self.entity_id;
        let hit = world.events.iter().any(|event| {
            matches!(event, SnapshotEvent::Damage { entity_id, attacker_id, .. }
                if entity_id == own && attacker_id != own)
        });
        let view = UnstickView {
            now,
            position,
            velocity: world.body(own).and_then(|entity| entity.velocity),
            fighting: self.engagement.is_some() || self.chasing.is_some() || hit,
            enemy_near: self.engagement.is_some_and(|fight| {
                let target = fight.target;
                (target.x - position.x)
                    .hypot(target.z - position.z)
                    .hypot(target.y - position.y)
                    < 72.0
            }),
            sight: self.sight.as_deref(),
        };
        let (input, give_up) = self.unstick.filter(&view, self.planned_input);
        self.planned_input = input;
        if give_up && let Some(navigation) = &self.navigation {
            self.route.give_up_leg(navigation, now);
        }
    }

    /// `chase`: walk toward where the enemy should be, watch it, and fire
    /// into it within 200 units while the memory is above 0.7.
    /// `BotLook`: note which major item spots are in sight, and while
    /// roaming out of a fight, let the view glance into open space or at a
    /// stale spot. Movement keeps its heading.
    /// A teleporter sets the view to its exit yaw, as it does for a human,
    /// after TypeScript `BotClient._faceTeleportExit`. Without this the aim
    /// would turn there from the entry view after the reaction time, and a bot
    /// that walked into a teleporter mid-fight would come out looking back.
    fn face_teleport_exit(&mut self, position: Option<super::super::Vec3>) {
        let (Some(before), Some(after), Some(navigation)) =
            (self.position, position, self.navigation.as_deref())
        else {
            return;
        };
        if let Some(yaw) = navigation.teleport_exit_yaw(before, after) {
            let pitch = self
                .aim
                .current()
                .map_or(self.planned_input.pitch, |view| view.pitch);
            self.aim.reset(View { yaw, pitch });
        }
    }

    fn look_around(&mut self, world: &Observation, position: Option<super::super::Vec3>) {
        let (Some(sight), Some(navigation), Some(position)) =
            (self.sight.clone(), self.navigation.clone(), position)
        else {
            self.look.stop();
            return;
        };
        let spots: Vec<(usize, [f64; 3])> = navigation
            .pickups()
            .into_iter()
            .filter(|(pickup, _)| pickup.is_major())
            .map(|(pickup, at)| (pickup.id, at))
            .collect();
        let view_yaw = self
            .aim
            .current()
            .map_or(self.planned_input.yaw, |view| view.yaw);
        self.look
            .watch(world.tick, position, view_yaw, &spots, &sight);
        let [x, _, z] = self.planned_input.direction;
        let roaming = self.engagement.is_none()
            && self.chasing.is_none()
            && !self.exact_view
            && self.respawn_at.is_none()
            && (x != 0.0 || z != 0.0)
            && self.planned_input.buttons & (FIRE | ALT_FIRE) == 0;
        if !roaming {
            self.look.stop();
            return;
        }
        let travel = x.atan2(z);
        let glance = self.look.glance(
            world.tick,
            position,
            travel,
            view_yaw,
            &spots,
            &sight,
            &mut self.random,
        );
        // Sprinting, the glance keeps the forward key, and so the speed.
        self.planned_input.yaw = if self.planned_input.buttons & SPRINT != 0 {
            looking::keep_sprint(glance, travel)
        } else {
            glance
        };
    }

    /// `BotMood` around a fight: a new fight may roll a mistake, and an
    /// ended one clears all but forgetting. While a mistake lasts it bends
    /// the fight: a wild shot fires 5 degrees off to one side, the wrong
    /// weapon fights on with whatever is out, and an item glance looks at the
    /// nearest active pickup within 1,200 units and holds fire.
    fn apply_mood(&mut self, world: &Observation, position: Option<super::super::Vec3>) {
        let tick = world.tick;
        match (self.engagement.is_some(), self.fighting) {
            (true, false) => {
                self.mood
                    .start_fight(tick, self.difficulty.mistake_rate(), &mut self.random);
            }
            (false, true) => self.mood.end_fight(),
            _ => {}
        }
        if self.respawn_at.is_some() {
            self.mood.clear_mistake();
        }
        self.fighting = self.engagement.is_some();
        let Some(fight) = self.engagement.as_mut() else {
            return;
        };
        match self.mood.mistake(tick) {
            Some((Mistake::WildShot, began)) => {
                let side = if began % 2 == 0 { 1.0 } else { -1.0 };
                fight.view.yaw += side * WILD_SHOT_OFFSET;
            }
            Some((Mistake::WrongWeapon, _)) => fight.weapon_slot = 0,
            Some((Mistake::ItemGlance, _)) => {
                let item = position
                    .zip(self.navigation.as_ref())
                    .and_then(|(at, navigation)| {
                        navigation
                            .pickups()
                            .into_iter()
                            .filter(|(pickup, _)| world.pickup_active.get(pickup.id) == Some(&true))
                            .map(|(_, [x, _, z])| ((x - at.x).hypot(z - at.z), x - at.x, z - at.z))
                            .filter(|(distance, _, _)| *distance <= ITEM_GLANCE_RANGE)
                            .min_by(|a, b| a.0.total_cmp(&b.0))
                    });
                if let Some((_, dx, dz)) = item {
                    fight.view = View {
                        yaw: dx.atan2(dz),
                        pitch: 0.0,
                    };
                    fight.buttons = 0;
                }
            }
            _ => {}
        }
    }

    fn chase_lost(
        &mut self,
        world: &Observation,
        now: SimTime,
        navigation: &WashoiNavigation,
        sight: &Sight,
        position: shagger_shared::net::game_state::Vec3,
        lost: crate::games::washoi::agents::player::combat::Lost,
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
        if body.grounded {
            let walkable = |to: crate::games::washoi::Vec3| {
                crate::games::washoi::agents::player::navigation::local_walk(sight, position, to)
            };
            let floor_below = |from: [f64; 3], to: [f64; 3]| !sight.clear(from, to);
            self.planned_input.direction = highground::keep_upper_ground(
                self.planned_input.direction,
                position,
                fight.target,
                &walkable,
                &floor_below,
            );
        }
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
        let reachable = |node: crate::games::washoi::Vec3| {
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
                    if self.vitals.health.is_some_and(|before| *health < before) {
                        self.recent_damage.push(world.tick);
                    }
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
                SnapshotEvent::MatchState(state) => {
                    self.match_context = Some(MatchContext::read(state));
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
            self.hop.reset();
            self.slide.reset();
            self.aim.clear();
            self.combat.respawned();
            self.planner.reset();
            self.territory.reset();
            self.walkable.clear();
            self.route.set_goal(None);
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
        let position = own
            .and_then(|entity| entity.position)
            .filter(|_| self.respawn_at.is_none() && !self.route.traversing());
        // A pickup the follower cannot route to from here is given up on.
        // The follower then chooses freely for a while.
        if let Some(id) = self.route.goal_unplanned(now) {
            self.planner.fail(&format!("pickup:{id}"), world.tick);
            self.route.set_goal(None);
            self.free_until = Some(now + FREE_ROAM);
        }
        let free = self.free_until.is_some_and(|until| now < until);
        let goal = position.and_then(|position| self.choose_goal(world, now, position));
        let to_pickup = matches!(goal, Some(GoalTarget::Pickup(_)));
        // The follower walks the graph. From off it, or to a pickup it has
        // no walk to, it keeps its own choice until it is back on a walk.
        let walkable = |id: usize| !free && self.walkable.contains(&id);
        match goal {
            // Already on the way: setting the goal would drop the route.
            Some(GoalTarget::Pickup(id))
                if self
                    .navigation
                    .as_deref()
                    .is_some_and(|navigation| self.route.active_pickup(navigation) == Some(id)) => {
            }
            Some(GoalTarget::Pickup(id)) if walkable(id) => self.route.set_goal(Some(id)),
            Some(GoalTarget::Pickup(_)) => {}
            Some(GoalTarget::Directive(id)) => {
                self.orders.choose(&id);
                self.route.set_goal(None);
            }
            None => self.route.set_goal(None),
        }
        if !to_pickup
            && !self.route.traversing()
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
                    self.slide.reset();
                    self.hop.reset();
                    return true;
                }
                // A strafe-jump chain under way owns the controls
                // (`navigation::hop`).
                let hop_view = HopView {
                    tick: world.tick,
                    position,
                    velocity: own.and_then(|entity| entity.velocity),
                    lane: self
                        .route
                        .hop_lane(navigation, position, self.sight.as_deref()),
                    sight: self.sight.as_deref(),
                    skill: self.difficulty.strafe_jump_skill(),
                    view_yaw: self
                        .aim
                        .current()
                        .map_or(self.planned_input.yaw, |view| view.yaw),
                    fighting: self.engagement.is_some() || self.chasing.is_some(),
                };
                if self.hop.active()
                    && let Some(controls) = self.hop.steer(&hop_view, self.planned_input)
                {
                    self.slide.reset();
                    self.planned_input = controls;
                    return true;
                }
                // Fast down a straight, plain walk, crouch-slide
                // (`navigation::slide`). A fight's movement replaces it.
                let lane = self
                    .route
                    .slide_lane(navigation, position, self.sight.as_deref());
                let item = self.route.item_leg(navigation);
                self.planned_input = self.slide.filter(
                    &SlideView {
                        tick: world.tick,
                        position,
                        velocity: own.and_then(|entity| entity.velocity),
                        lane,
                        item,
                        pickup_active: &world.pickup_active,
                        allies: &self.allies,
                        strafe_jump_skill: self.difficulty.strafe_jump_skill(),
                        fighting: self.engagement.is_some() || self.chasing.is_some(),
                    },
                    self.planned_input,
                );
                // Then, as TypeScript tries them, a strafe-jump chain.
                if !self.slide.active()
                    && let Some(controls) = self.hop.steer(&hop_view, self.planned_input)
                {
                    self.planned_input = controls;
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

    /// `SlideBladeSequence`: running at the enemy fought, slide in with the
    /// blade. While it runs the lunge sets movement, view, and weapon.
    fn blade_lunge(&mut self, world: &Observation, position: Option<super::super::Vec3>) {
        let body = world.body(&self.entity_id).copied();
        let (Some(position), Some(velocity), Some(sight)) = (
            position,
            body.and_then(|body| body.velocity),
            self.sight.clone(),
        ) else {
            self.lunge.stop();
            return;
        };
        if self.respawn_at.is_some() || self.traversal.active() || self.exact_view {
            self.lunge.stop();
            return;
        }
        let view = self.aim.current().unwrap_or(View {
            yaw: self.planned_input.yaw,
            pitch: self.planned_input.pitch,
        });
        let me = Runner {
            position,
            velocity: velocity.linear,
            grounded: velocity.grounded,
            sliding: velocity.is_sliding,
            yaw: view.yaw,
            pitch: view.pitch,
        };
        let mark = |entity: &str| {
            if self.combat.is_dead(entity) {
                return None;
            }
            let body = world.body(entity)?;
            Some(Mark {
                position: body.position?,
                velocity: body.velocity.map(|v| v.linear).unwrap_or_default(),
            })
        };
        let (weapons, _) = self.combat.inventory();
        if !self.lunge.active()
            && self.engagement.is_some_and(|fight| fight.in_sight)
            && let Some(target) = self.combat.tracked()
            && let Some(at) = mark(target)
            && lunge::can_lunge(me, at, weapons, |from, to| {
                // `isWalkable`: open at knee and chest height.
                [16.0, 40.0].into_iter().all(|height| {
                    sight.clear(
                        [from.x, from.y + height, from.z],
                        [to.x, to.y + height, to.z],
                    )
                })
            })
        {
            let left = self.random.next() < 0.5;
            let target = target.to_owned();
            let weapon = self.combat.weapon().to_owned();
            self.lunge.start(world.tick, &target, &weapon, left);
        }
        let (weapons, _) = self.combat.inventory();
        if let Some(input) = self.lunge.step(
            world.tick,
            me,
            mark,
            weapons,
            self.combat.weapon(),
            self.difficulty.aim().prediction,
        ) {
            self.planned_input = input;
            self.exact_view = true;
            self.moves.reset();
        }
    }

    /// `RoamChoice`: a roaming bot takes the arbiter's best item, hunts the
    /// place a teammate just called enemies at, or in a mode that keeps a
    /// territory holds high ground, whichever is worth most; an item wins
    /// ties. Hunting is worth `(0.2 + 0.25 x aggression) x seekScale x
    /// freshness`, holding `0.5 x mapControl` when healthy and armed. The bot's
    /// own fresh sightings are the chase, which comes first.
    fn hold_ground(
        &mut self,
        world: &Observation,
        position: Option<super::super::Vec3>,
        now: SimTime,
    ) {
        const HOLD_DESIRE_PER_CONTROL: f64 = 0.5;
        const HUNT_DESIRE_BASE: f64 = 0.2;
        const HUNT_DESIRE_PER_AGGRESSION: f64 = 0.25;
        const TEAM_SIGHTING_TICKS: f64 = 6.0 * 125.0;
        const HOLD_HEALTH: u16 = 60;
        let roaming = self.engagement.is_none()
            && self.chasing.is_none()
            && !self.exact_view
            && !self.ordered
            && self.respawn_at.is_none()
            && !self.lunge.active();
        let (Some(position), Some(navigation), Some(sight), true) = (
            position,
            self.navigation.clone(),
            self.sight.clone(),
            roaming,
        ) else {
            self.territory_walk.reset();
            self.hunt_walk.reset();
            return;
        };
        let weights = self.weights(world);
        let item = self.planner.desire();
        let hunt = self.team_sighting.map_or(0.0, |(_, at)| {
            let fresh = 1.0 - f64::from(world.tick.saturating_sub(at)) / TEAM_SIGHTING_TICKS;
            (HUNT_DESIRE_BASE + HUNT_DESIRE_PER_AGGRESSION * weights.aggression)
                * self.strategist.stance().seek_scale
                * fresh.max(0.0)
        });
        let keeps = self
            .match_context
            .as_ref()
            .is_some_and(|context| context.keeps_territory());
        let hold = if keeps
            && self
                .vitals
                .health
                .is_some_and(|health| health >= HOLD_HEALTH)
            && self.combat.armed()
        {
            HOLD_DESIRE_PER_CONTROL * weights.map_control
        } else {
            0.0
        };
        let velocity = world.body(&self.entity_id).and_then(|body| body.velocity);
        if let Some((called, _)) = self.team_sighting
            && hunt > item
            && hunt >= hold
        {
            self.territory_walk.reset();
            if let Some(input) =
                self.hunt_walk
                    .steer(&navigation, &sight, position, velocity, called, now)
            {
                self.planned_input = PlayerInput {
                    weapon_slot: self.planned_input.weapon_slot,
                    ..input
                };
            }
            return;
        }
        self.hunt_walk.reset();
        if hold <= 0.0 || item >= hold {
            self.territory_walk.reset();
            return;
        }
        let point = self.territory.choose(
            world.tick,
            position,
            navigation.nodes(),
            self.combat.last_known(),
            &sight,
            None,
            |at| {
                navigation
                    .objective_walk(position, at, 64.0, None, Some(&sight))
                    .is_some()
            },
        );
        let Some(point) = point else {
            self.territory_walk.reset();
            return;
        };
        if let Some(input) =
            self.territory_walk
                .steer(&navigation, &sight, position, velocity, point, now)
        {
            self.planned_input = PlayerInput {
                weapon_slot: self.planned_input.weapon_slot,
                ..input
            };
        }
    }

    /// A distant sound does not pull a bot off an item it needs, from
    /// `StateMachineBrain` priority 2.5: a track only heard, more than 384
    /// units off, while not chasing yet, gives way to the item run
    /// (`_evaluateItemState`). Once chasing, it keeps chasing.
    fn divert_to_item(
        &self,
        world: &Observation,
        position: super::super::Vec3,
        lost: crate::games::washoi::agents::player::combat::Lost,
        was_chasing: bool,
    ) -> bool {
        const SOUND_DIVERT_DISTANCE: f64 = 384.0;
        let far = (lost.predicted.x - position.x).hypot(lost.predicted.z - position.z)
            > SOUND_DIVERT_DISTANCE;
        lost.heard && far && !was_chasing && self.should_seek_items(world)
    }

    /// `_shouldSeekItems` and the retreat: hurt or out of ammo, always;
    /// with 25 armor or more, no; else when the arbiter's best item is
    /// worth more than the hunt, by the mode's aggression. Only called
    /// with an enemy to hunt.
    fn should_seek_items(&self, world: &Observation) -> bool {
        const LOW_HEALTH_THRESHOLD: f64 = 40.0;
        const SEEK_DESIRE_BASE: f64 = 0.2;
        const SEEK_DESIRE_PER_AGGRESSION: f64 = 0.25;
        let health = f64::from(self.vitals.health.unwrap_or(100));
        let stance = self.strategist.stance();
        let retreat = self.mood.retreat_health(LOW_HEALTH_THRESHOLD) * stance.retreat_scale;
        if health < 50.0 || health < retreat {
            return self.planner.desire() > 0.0;
        }
        if self.combat.current_ammo().is_some_and(|ammo| ammo < 3.0) {
            return true;
        }
        if self.vitals.armor.unwrap_or(0) >= 25 && !stance.shop_armored {
            return false;
        }
        let aggression = self.weights(world).aggression;
        let worth =
            (SEEK_DESIRE_BASE + SEEK_DESIRE_PER_AGGRESSION * aggression) * stance.seek_scale;
        self.planner.desire() > 0.0 && self.planner.desire() >= worth
    }

    /// `_shouldFade`: a strategy that fades (Recover, Protect lead), 40 or
    /// more damage taken in this fight, and no stack advantage by the
    /// estimate. TypeScript then retreats out of sight and to health; here the
    /// bot keeps walking its route, the arbiter's goal, instead of strafing.
    fn should_fade(&self, world: &Observation) -> bool {
        const FADE_DAMAGE: f64 = 40.0;
        let Some(enemy) = self.combat.tracked() else {
            return false;
        };
        if !self.strategist.stance().fade
            || self
                .match_read
                .open_fight(enemy)
                .is_none_or(|fight| fight.taken < FADE_DAMAGE)
        {
            return false;
        }
        let own = f64::from(self.vitals.health.unwrap_or(100))
            + f64::from(self.vitals.armor.unwrap_or(0));
        self.combat
            .enemy_estimate(enemy, world.tick)
            .is_none_or(|(stack, uncertainty)| uncertainty > 0.5 || own <= stack)
    }

    /// `BotClient._callout` and `_chatter`: read teammates' calls, call what
    /// the bot sees, say each directive's team call once, and let chat
    /// react to the match.
    fn talk(&mut self, world: &Observation, now: SimTime) {
        const TEAM_SIGHTING_TICKS: u32 = 6 * 125;
        let tick = world.tick;
        let own_team = world
            .player(&self.entity_id)
            .map_or("", |player| player.team.as_str());
        let on_team = !matches!(own_team, "" | "ffa" | "spectator");
        for event in &world.events {
            if let SnapshotEvent::Chat {
                sender_id,
                team,
                text,
                ..
            } = event
                && on_team
                && text.starts_with("[TEAM]")
                && *sender_id != self.entity_id
                && team == own_team
                && let Some(call) = self.callouts.heard(text, now)
                && matches!(call.kind, CallKind::Enemies | CallKind::Carrier)
                && let Some(at) = self
                    .locations
                    .as_ref()
                    .and_then(|locations| locations.position_of(&call.place))
            {
                self.team_sighting = Some((at, tick));
            }
        }
        self.team_sighting = self
            .team_sighting
            .filter(|(_, at)| tick.saturating_sub(*at) < TEAM_SIGHTING_TICKS);
        let alive = self.respawn_at.is_none() && self.own_entity_present;
        let roles = self
            .match_context
            .as_ref()
            .is_some_and(|context| !context.team_roles.is_empty());
        if alive && let Some(locations) = self.locations.clone() {
            let seen: Vec<(&str, super::super::Vec3)> = self
                .engagement
                .filter(|fight| fight.in_sight)
                .and_then(|fight| Some((self.combat.tracked()?, fight.target)))
                .into_iter()
                .collect();
            let objective = self
                .match_context
                .as_ref()
                .and_then(|context| context.objective.as_ref());
            let line = self.callouts.observe(
                &callouts::View {
                    now,
                    team: (on_team && roles).then_some(own_team),
                    enemies: &seen,
                    objective,
                },
                &locations,
            );
            self.outgoing.extend(line);
        }
        // The team's call the gametype gave this bot: said once.
        for event in &world.events {
            let SnapshotEvent::AgentDirectives {
                entity_id,
                directives,
            } = event
            else {
                continue;
            };
            if *entity_id != self.entity_id {
                continue;
            }
            for directive in directives.as_array().unwrap_or_default() {
                let Some(call) = directive.get("call") else {
                    continue;
                };
                let (Some(id), Some(text)) = (
                    call.get("id").and_then(|id| id.as_str()),
                    call.get("text").and_then(|text| text.as_str()),
                ) else {
                    continue;
                };
                if self.said_calls.iter().any(|said| said == id) {
                    continue;
                }
                if self.said_calls.len() > 200 {
                    self.said_calls.clear();
                }
                self.said_calls.push(id.to_owned());
                self.outgoing.push(format!("/say_team {text}"));
            }
        }
        let frags: Vec<Frag> = world
            .events
            .iter()
            .filter_map(|event| match event {
                SnapshotEvent::Death {
                    entity_id,
                    killer_id,
                    kill_weapon,
                    weapon_id,
                    ..
                } if *killer_id == self.entity_id && *entity_id != self.entity_id => {
                    Some(Frag::Kill {
                        victim: entity_id.clone(),
                        weapon: kill_weapon.clone().unwrap_or_else(|| weapon_id.clone()),
                    })
                }
                SnapshotEvent::Death {
                    entity_id,
                    killer_id,
                    ..
                } if *entity_id == self.entity_id && *killer_id != self.entity_id => {
                    Some(Frag::Death {
                        killer: killer_id.clone(),
                    })
                }
                _ => None,
            })
            .collect();
        let names: Vec<(&str, &str)> = world
            .players
            .iter()
            .filter(|player| player.entity_id != self.entity_id && !player.name.is_empty())
            .map(|player| (player.entity_id.as_str(), player.name.as_str()))
            .collect();
        let (kill_streak, death_streak, tilted) = self.mood.streaks();
        self.chat.observe(&chat::View {
            now,
            phase: self
                .match_context
                .as_ref()
                .map_or(0, |context| context.phase),
            time_remaining: self
                .match_context
                .as_ref()
                .and_then(|context| context.time_remaining),
            names: &names,
            tilted,
            kill_streak,
            death_streak,
            frags: &frags,
        });
        self.outgoing.extend(self.chat.poll(now));
    }

    /// `BotRoam.retreat` while the enemy is in sight: walk to the escape
    /// point. Out of sight, or with nowhere hidden, the route's walk to the
    /// arbiter's goal stands.
    fn fade(
        &mut self,
        world: &Observation,
        fight: Engagement,
        sight: &Sight,
        position: super::super::Vec3,
        now: SimTime,
    ) {
        let Some(navigation) = self.navigation.clone() else {
            return;
        };
        if !fight.in_sight {
            self.escape.reset();
            return;
        }
        let Some(point) = self.escape.point(
            world.tick,
            position,
            fight.target,
            navigation.nodes(),
            sight,
        ) else {
            return;
        };
        let velocity = world.body(&self.entity_id).and_then(|body| body.velocity);
        if let Some(input) =
            self.escape_walk
                .steer(&navigation, sight, position, velocity, point, now)
        {
            self.planned_input.direction = input.direction;
            self.planned_input.buttons = (self.planned_input.buttons & (FIRE | ALT_FIRE))
                | (input.buttons & !(FIRE | ALT_FIRE));
        }
    }

    /// `MatchRead.update`, then `Strategist.update`.
    fn read_match(&mut self, world: &Observation) {
        // `_loadPriors`: the map's payoffs for the gametype, once known.
        if let (Some(baseline), Some(context)) = (&self.baseline, &self.match_context)
            && !context.gametype.is_empty()
            && self.priors_for.as_deref() != Some(context.gametype.as_str())
        {
            self.strategist
                .set_priors(baseline.priors(&context.gametype));
            self.priors_for = Some(context.gametype.clone());
        }
        if self.people.is_some() {
            let humans = world
                .players
                .iter()
                .filter(|player| {
                    player.entity_id != self.entity_id
                        && !player.name.is_empty()
                        && !chat::is_bot_name(&player.name)
                })
                .map(|player| player.entity_id.clone())
                .collect();
            self.combat.know_people(self.people.clone(), humans);
        }
        let tick = world.tick;
        let seen = self
            .engagement
            .filter(|fight| fight.in_sight)
            .and(self.combat.tracked());
        let combat = &self.combat;
        let stack = |entity: &str| combat.enemy_stack(entity, tick);
        self.match_read.update(
            world,
            &Moment {
                own: &self.entity_id,
                health: self.vitals.health,
                armor: self.vitals.armor,
                dead: self.respawn_at.is_some(),
                seen,
                enemy_stack: &stack,
                time_remaining: self
                    .match_context
                    .as_ref()
                    .and_then(|context| context.time_remaining),
            },
        );
        let opponents = &self.opponents;
        let takes = |seconds: f64| opponents.takes(tick, seconds);
        self.strategist.update(
            &self.match_read,
            &Reading {
                own_stack: f64::from(self.vitals.health.unwrap_or(100))
                    + f64::from(self.vitals.armor.unwrap_or(0)),
                enemy: self.combat.freshest_enemy(tick),
                others_takes: &takes,
                timed_item_in_reach: self.planner.timed(),
            },
        );
    }

    /// The gametype's weight profile for this bot's team, scaled by its
    /// strategy (`_weights`).
    fn weights(&self, world: &Observation) -> strategy::weights::Weights {
        let team = world
            .player(&self.entity_id)
            .map_or("", |player| player.team.as_str());
        let base = self
            .match_context
            .as_ref()
            .map_or(strategy::weights::DEFAULT, |context| context.weights(team));
        self.strategist.weights(base)
    }

    /// Credit the major items enemies were heard or seen taking.
    fn read_opponents(&mut self, world: &Observation) {
        let pickups = self
            .navigation
            .as_deref()
            .map(|navigation| navigation.pickups())
            .unwrap_or_default();
        let allies: Vec<&str> = self
            .combat
            .allies_by_id(world, &self.entity_id)
            .into_iter()
            .map(|(entity, _)| entity)
            .collect();
        let opponents = self.combat.scoreboard_enemies(world, &self.entity_id);
        let position = world.body(&self.entity_id).and_then(|body| body.position);
        let credited = self.opponents.observe(
            world,
            &Scene {
                own: &self.entity_id,
                position,
                allies: &allies,
                opponents: &opponents,
                pickups: &pickups,
                sight: self.sight.as_deref(),
            },
        );
        for (entity, id) in credited {
            if let Some((pickup, _)) = pickups.iter().find(|(pickup, _)| pickup.id == id) {
                self.combat.credit_pickup(&entity, *pickup, world.tick);
            }
        }
    }

    /// `ItemGoalPlanner.select`: the pickup or order to pursue. Until a
    /// `match_state` names the gametype, orders keep their own choice and
    /// only pickups are weighed, as the default profile gives orders no
    /// weight.
    fn choose_goal(
        &mut self,
        world: &Observation,
        now: SimTime,
        position: super::super::Vec3,
    ) -> Option<GoalTarget> {
        const DAMAGE_MEMORY_TICKS: u32 = 250;
        let tick = world.tick;
        self.recent_damage
            .retain(|hit| tick.saturating_sub(*hit) <= DAMAGE_MEMORY_TICKS);
        let navigation = self.navigation.as_deref();
        let weights = self.weights(world);
        let costs = navigation
            .map(|navigation| navigation.pickup_costs(position, self.sight.as_deref()))
            .unwrap_or_default();
        self.walkable = costs.iter().map(|(id, _)| *id).collect();
        let pickups: Vec<KnownPickup> = navigation
            .map(|navigation| {
                navigation
                    .pickups()
                    .into_iter()
                    .map(|(pickup, at)| {
                        let returns = self.route.pickup_returns_at(pickup.id);
                        let gone = self.route.pickup_unavailable(pickup.id, now);
                        let up = world.pickup_active.get(pickup.id).copied().unwrap_or(!gone);
                        KnownPickup {
                            candidate: Candidate::pickup(
                                pickup,
                                at,
                                costs
                                    .iter()
                                    .find(|(id, _)| *id == pickup.id)
                                    .map(|(_, cost)| *cost),
                            ),
                            up,
                            returns_tick: returns
                                .filter(|_| !up)
                                .map(|at| tick + ((at - now).as_secs_f64() * 125.0).round() as u32),
                            blocked: up && gone || self.route.blocked_toward(at, now),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut pickups = pickups;
        // With no walk attached from here, rank by the straight-line
        // estimate, as TypeScript does without a graph, rather than gating
        // every pickup as unreachable. The follower keeps its own choice.
        if costs.is_empty() {
            for known in &mut pickups {
                let [x, y, z] = known.candidate.position;
                known.candidate.route_cost = Some(strategy::considerations::estimate_route_cost(
                    position,
                    super::super::Vec3::new(x, y, z),
                ) as f32);
            }
        }
        let orders: Vec<Candidate> = if self.match_context.is_some() {
            self.orders.candidates(position).collect()
        } else {
            Vec::new()
        };
        if pickups.is_empty() && orders.is_empty() {
            return None;
        }
        self.arbitrated = true;
        let contested = self.opponents.contested();
        let (weapons, ammo) = self.combat.inventory();
        let allies = self.combat.allies_by_id(world, &self.entity_id);
        let me = Me {
            position,
            health: self.vitals.health.unwrap_or(100),
            armor: self.vitals.armor.unwrap_or(0),
            weapons,
            ammo,
        };
        let situation = Situation {
            tick,
            recent_damage: &self.recent_damage,
            known_enemies: self.combat.known_enemies(),
            scoreboard_enemies: self.combat.scoreboard_enemies(world, &self.entity_id).len(),
            allies: &allies,
            contested: (self.difficulty.match_sense() >= CONTEST_SENSE).then_some(&contested),
        };
        // Pair walks start at a pickup's node, so they need no sight trace,
        // and the map fixes them: work each out once.
        let pairs = std::cell::RefCell::new(&mut self.pair_costs);
        let between = |from: &strategy::considerations::Goal,
                       to: &strategy::considerations::Goal| {
            let (from_id, to_id) = (from.pickup?.id, to.pickup?.id);
            *pairs
                .borrow_mut()
                .entry((from_id, to_id))
                .or_insert_with(|| {
                    navigation?
                        .pickup_costs(from.position, None)
                        .into_iter()
                        .find(|(id, _)| *id == to_id)
                        .map(|(_, cost)| f64::from(cost))
                })
        };
        let chosen = self.planner.select(
            pickups,
            orders,
            me,
            situation,
            weights,
            &between,
            &mut self.random,
        );
        // `BOT_GOAL_ARBITER=verbose`: the decision line on each new choice.
        if self.verbose_goals && chosen != self.last_goal {
            eprintln!("[bot {}] {}", self.entity_id, self.planner.explain());
        }
        self.last_goal.clone_from(&chosen);
        chosen
    }

    /// The goal arbiter's last decision on one line, for diagnostics.
    pub fn goal_explain(&self) -> &str {
        self.planner.explain()
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
    /// One command with the trigger gated as `BotClient._aimed` gates it:
    /// fire waits for the view to reach the aim, and no attack goes out with
    /// an ally on the line of the view sent.
    fn next_command(&mut self) -> PlayerInput {
        let (mut input, aim) = self.aimed_command();
        let sent = View {
            yaw: input.yaw,
            pitch: input.pitch,
        };
        match aim {
            Some(aim) => {
                let (buttons, shooting) =
                    safety::hold_fire_off_aim(input.buttons, aim, sent, self.shooting);
                input.buttons = buttons;
                self.shooting = shooting;
            }
            None => self.shooting = false,
        }
        if let Some(position) = self.position {
            input.buttons = safety::spare_allies(input.buttons, position, sent, &self.allies);
        }
        input
    }

    /// The planned command with the view turned by the aim controller, and
    /// the aim the view was turning to while fighting or chasing.
    fn aimed_command(&mut self) -> (PlayerInput, Option<View>) {
        let mut input = self.planned_input;
        let planned = View {
            yaw: input.yaw,
            pitch: input.pitch,
        };
        if self.exact_view {
            self.aim.reset(planned);
            return (input, None);
        }
        if self.engagement.is_none()
            && let Some((wanted, prefire)) = self.chasing
        {
            self.pressed_melee = false;
            let [x, _, z] = input.direction;
            let moving = x != 0.0 || z != 0.0;
            let wanted = if prefire {
                wanted
            } else {
                // Running after it unseen, look ahead along the run and
                // keep the forward key; turn to the spot when firing into it.
                View {
                    yaw: if moving {
                        looking::keep_sprint(wanted.yaw, x.atan2(z))
                    } else {
                        wanted.yaw
                    },
                    pitch: (wanted.pitch + looking::CALM_PITCH).min(looking::CALM_MAX_PITCH),
                }
            };
            self.aim.set_brisk(moving && !prefire);
            let view = self.aim.step(
                INPUT_PERIOD.as_secs_f64(),
                wanted,
                prefire,
                !prefire,
                &mut self.random,
            );
            input.yaw = view.yaw;
            input.pitch = view.pitch;
            if prefire {
                input.buttons |= FIRE;
            }
            return (input, Some(wanted));
        }
        let Some(fight) = self.engagement else {
            self.pressed_melee = false;
            let calm = View {
                pitch: (planned.pitch + looking::CALM_PITCH).min(looking::CALM_MAX_PITCH),
                ..planned
            };
            // Running a route with nobody about, turn to the next leg briskly.
            self.aim.set_brisk(input.direction != [0.0; 3]);
            let view = self.aim.step(
                INPUT_PERIOD.as_secs_f64(),
                calm,
                false,
                true,
                &mut self.random,
            );
            input.yaw = view.yaw;
            input.pitch = view.pitch;
            return (input, None);
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
        (input, Some(fight.view))
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

impl Agent for BotPlayer {
    fn observe_at(&mut self, observation: Observation, now: SimTime) {
        BotPlayer::observe_at(self, observation, now);
    }

    fn decide(&mut self, now: SimTime) {
        BotPlayer::decide(self, now);
    }
}

impl NetworkedPlayer for BotPlayer {
    fn request_respawn_at(&mut self, now: SimTime) -> bool {
        BotPlayer::request_respawn_at(self, now)
    }

    fn inputs_at(&mut self, now: SimTime) -> Vec<Controls> {
        BotPlayer::inputs_at(self, now)
    }

    fn chat_at(&mut self, now: SimTime) -> Vec<String> {
        BotPlayer::chat_at(self, now)
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
        use crate::games::washoi::perception::sight::tests::{cube, encode};
        use crate::games::washoi::{PlayerView, Vec3};
        let sight = Sight::parse(&encode(
            "test",
            &[cube([-8.0, -100.0, 200.0], [8.0, 200.0, 1000.0])],
            &[],
        ))
        .unwrap();
        let mut bot = BotPlayer::new("bot".into())
            .with_difficulty(Difficulty::Insane)
            .with_sight(Arc::new(sight))
            .with_trace(true);
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
        let mut first_shot = None;
        let mut last = Controls::default();
        for step in 0..150u64 {
            let now = start + INPUT_PERIOD * step as u32;
            world.tick = step as u32;
            bot.observe_at(world.clone(), now);
            bot.decide(now);
            for input in bot.inputs_at(now) {
                if input.buttons & FIRE != 0 {
                    fired += 1;
                    first_shot.get_or_insert(input.yaw);
                }
                last = input;
            }
        }
        // Insane notices it within 4 ticks and turns to it well within
        // 1.2 s. It holds fire until its view is within 12 degrees of the
        // aim, then keeps firing.
        let aim = std::f64::consts::FRAC_PI_4;
        assert!((last.yaw - aim).abs() < 0.05);
        let first_shot = first_shot.expect("fired");
        assert!((first_shot - aim).abs() <= 12f64.to_radians() + 1e-9);
        assert!(fired >= 100, "fired {fired} times");
        // The trace records the fight, and serializes for tools.
        let trace = bot.last_trace().expect("traced");
        assert_eq!((trace.tick, trace.state), (149, "fight"));
        assert_eq!(trace.goal, None);
        assert!(
            serde_json::to_string(trace)
                .unwrap()
                .contains(r#""state":"fight""#)
        );
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
        use crate::games::washoi::perception::sight::tests::{cube, encode};
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
            perception::sight::tests::{cube, encode},
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
            perception::sight::tests::{cube, encode},
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
            at: crate::games::washoi::hosting::net::protocol::Point {
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

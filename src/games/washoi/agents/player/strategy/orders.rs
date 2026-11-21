//! Gametype orders, following SHAG's AgentDirective contract.

use std::time::Duration;

use crate::games::washoi::{Observation, SimTime};
use shagger_shared::{
    json::Value,
    net::game_state::{Vec3, Velocity},
};

use crate::games::washoi::{
    Sight, WashoiNavigation,
    agents::player::navigation::{ObjectiveRecovery, ObjectiveWalk},
    agents::player::strategy::candidates::{Candidate, CandidateKind, GoalTarget, Interrupt},
    hosting::net::protocol::{PlayerInput, SnapshotEvent},
};

const DEFAULT_RADIUS: f64 = 160.0;
const WAYPOINT_RADIUS: f64 = 48.0;
const HEIGHT_TOLERANCE: f64 = 40.0;
const ESCORT_REPLAN_DISTANCE: f64 = 48.0;
const RETRY: Duration = Duration::from_millis(350);
const STALL: Duration = Duration::from_millis(850);
const BLOCKED_FOR: Duration = Duration::from_secs(5);
const MIN_PROGRESS: f64 = 8.0;

// Guard patrol, from TypeScript `BotOrders.guardPatrol` and `guardPoint`.
const GUARD_POINT_REACHED: f64 = 56.0;
const GUARD_POINT_FOR: Duration = Duration::from_secs(4);
const GUARD_MIN_RADIUS: f64 = 96.0;
const GUARD_MAX_RADIUS: f64 = 420.0;
const GUARD_RINGS: [f64; 2] = [0.45, 0.85];
const GUARD_DIRECTIONS: usize = 10;
const GUARD_SPACING: f64 = 96.0;
const GUARD_EYE: f64 = 40.0;
// 90 ticks at Washoi's 125 Hz physics.
const GUARD_STRAFE_PERIOD: f64 = 0.72;
const HOLD_SWEEP: f64 = 0.3;
const HOLD_SWEEP_RATE: f64 = 0.9;
const EYE_HEIGHT: f64 = 32.0;
const FORWARD: u16 = 1;
const LEFT: u16 = 1 << 2;
const RIGHT: u16 = 1 << 3;
const JUMP: u16 = 1 << 4;
const CROUCH: u16 = 1 << 5;
const SPRINT: u16 = 1 << 6;
const FIRE: u16 = 1 << 8;
const ALT_FIRE: u16 = 1 << 9;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    MoveTo,
    /// `hold_area` and `defend`: guard the ground, and come back to it.
    Guard,
    Escort,
}

#[derive(Clone, Debug, PartialEq)]
struct Directive {
    id: String,
    kind: Kind,
    target: Target,
    radius: f64,
    priority: f64,
    expires: f64,
    immediate: bool,
    interrupt: Interrupt,
    /// `AgentDirective.urgency`, zero when absent.
    urgency: f64,
    watch: Option<Vec3>,
    via: Vec<Vec3>,
    /// How far a fight may draw a guard from its ground.
    leash: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
enum Target {
    Position(Vec3),
    Escort(String),
}

#[derive(Default)]
pub(in crate::games::washoi) struct Orders {
    directive: Option<Directive>,
    destination: Option<Vec3>,
    via: usize,
    walk: Option<(Vec3, ObjectiveWalk)>,
    blocked: Option<(ObjectiveRecovery, SimTime)>,
    retry_at: Option<SimTime>,
    progress: Option<Progress>,
    patrol: Option<Patrol>,
    random: u64,
    /// Every valid order addressed to this bot, with where its target is
    /// now, for the goal arbiter. The followed order is one of them.
    offered: Vec<(Directive, Option<Vec3>)>,
}

struct Patrol {
    target: Vec3,
    until: SimTime,
}

struct Progress {
    waypoint: usize,
    distance: f64,
    since: SimTime,
}

impl Orders {
    pub(in crate::games::washoi) fn observe(&mut self, world: &Observation, own_entity: &str) {
        if self
            .directive
            .as_ref()
            .is_some_and(|order| order.expires <= f64::from(world.tick))
        {
            self.clear_order();
        }
        let addressed = world.events.iter().rev().find_map(|event| match event {
            SnapshotEvent::AgentDirectives {
                entity_id,
                directives,
            } if entity_id == own_entity => Some(directives),
            _ => None,
        });
        self.offered
            .retain(|(order, _)| order.expires > f64::from(world.tick));
        if let Some(directives) = addressed {
            self.offered = directives
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|value| Directive::parse(value, world.tick))
                .map(|order| {
                    let at = order.target.position(world, own_entity);
                    (order, at)
                })
                .collect();
            let latest = self
                .offered
                .iter()
                .filter_map(|(order, at)| Some(((*at)?, order.clone())))
                .min_by(|(_, a), (_, b)| {
                    b.immediate
                        .cmp(&a.immediate)
                        .then_with(|| b.priority.total_cmp(&a.priority))
                        .then_with(|| a.id.cmp(&b.id))
                });
            let same_route = matches!((&self.directive, &latest), (Some(a), Some((_, b)))
                if a.id == b.id && a.target == b.target && a.radius == b.radius && a.via == b.via);
            if !same_route {
                self.clear_order();
            }
            if let Some((destination, order)) = latest {
                self.directive = Some(order);
                self.destination = Some(destination);
            }
        } else {
            for (order, at) in &mut self.offered {
                *at = order.target.position(world, own_entity);
            }
            self.destination = self
                .directive
                .as_ref()
                .and_then(|order| order.target.position(world, own_entity));
        }
    }

    /// The orders addressed to this bot whose targets can be found, as goal
    /// candidates. There is no precomputed walk to an arbitrary order
    /// target, so `route_cost` is the straight distance from `from`.
    pub(in crate::games::washoi) fn candidates(
        &self,
        from: Vec3,
    ) -> impl Iterator<Item = Candidate> + '_ {
        self.offered.iter().filter_map(move |(order, at)| {
            let at = (*at)?;
            Some(Candidate {
                id: order.id.clone(),
                target: GoalTarget::Directive(order.id.clone()),
                position: [at.x, at.y, at.z],
                route_cost: Some(
                    ((at.x - from.x).hypot(at.z - from.z).hypot(at.y - from.y)) as f32,
                ),
                kind: match order.kind {
                    Kind::MoveTo => CandidateKind::MoveTo,
                    Kind::Guard => CandidateKind::Guard,
                    Kind::Escort => CandidateKind::Escort,
                },
                priority: order.priority,
                urgency: order.urgency,
                interrupt: order.interrupt,
                escort: match &order.target {
                    Target::Escort(carrier) => Some(carrier.clone()),
                    Target::Position(_) => None,
                },
                pickup: None,
            })
        })
    }

    /// Follow the offered order with this ID, as the goal arbiter chose,
    /// instead of the one the orders would pick themselves. False when no
    /// such order is offered. Choosing the order already followed keeps its
    /// walk.
    pub(in crate::games::washoi) fn choose(&mut self, id: &str) -> bool {
        let Some((order, at)) = self.offered.iter().find(|(order, _)| order.id == id) else {
            return false;
        };
        if self
            .directive
            .as_ref()
            .is_some_and(|current| current == order)
        {
            return true;
        }
        let (order, at) = (order.clone(), *at);
        self.clear_order();
        self.directive = Some(order);
        self.destination = at;
        true
    }

    fn clear_order(&mut self) {
        *self = Self {
            blocked: self.blocked,
            random: self.random,
            offered: std::mem::take(&mut self.offered),
            ..Self::default()
        };
    }

    pub(in crate::games::washoi) fn input(
        &mut self,
        navigation: Option<&WashoiNavigation>,
        sight: Option<&Sight>,
        position: Vec3,
        velocity: Option<Velocity>,
        now: SimTime,
    ) -> Option<PlayerInput> {
        let directive = self.directive.as_ref()?;
        let Some(destination) = self.destination else {
            self.walk = None;
            self.progress = None;
            self.retry_at = None;
            return None;
        };
        while let Some(&waypoint) = directive.via.get(self.via) {
            if !near(position, waypoint, WAYPOINT_RADIUS) {
                break;
            }
            self.via += 1;
            self.walk = None;
            self.progress = None;
            self.retry_at = None;
        }
        let (target, radius) = directive
            .via
            .get(self.via)
            .copied()
            .map_or((destination, directive.radius), |point| {
                (point, WAYPOINT_RADIUS)
            });
        if self
            .walk
            .as_ref()
            .is_some_and(|(previous, _)| !near(target, *previous, ESCORT_REPLAN_DISTANCE))
        {
            self.walk = None;
            self.progress = None;
            self.retry_at = None;
        }
        if near(position, target, radius) {
            self.walk = None;
            self.progress = None;
            self.retry_at = None;
            // Planting and defusing use move_to and stay still on arrival.
            // Guards and escorts keep moving round the ground they hold.
            if self.via >= directive.via.len() && directive.kind != Kind::MoveTo {
                let (radius, watch) = (directive.radius, directive.watch);
                return Some(self.guard_patrol(sight, position, destination, radius, watch, now));
            }
            self.patrol = None;
            let watch = directive.watch.unwrap_or(target);
            return Some(PlayerInput {
                yaw: (watch.x - position.x).atan2(watch.z - position.z),
                ..PlayerInput::default()
            });
        }
        if self.walk.is_none() && self.retry_at.is_none_or(|at| now >= at) {
            let recovery = self.blocked.map(|(mut edge, until)| {
                if now >= until {
                    edge.avoid = None;
                }
                edge
            });
            self.walk = navigation
                .and_then(|map| map.objective_walk(position, target, radius, recovery, sight))
                .map(|walk| (target, walk));
            self.retry_at = Some(now + RETRY);
        }
        let (_, walk) = self.walk.as_mut()?;
        let input = walk.steer(position, velocity);
        if let Some((waypoint, distance)) = walk.progress(position) {
            if self.progress.as_ref().is_none_or(|previous| {
                previous.waypoint != waypoint || distance <= previous.distance - MIN_PROGRESS
            }) {
                self.progress = Some(Progress {
                    waypoint,
                    distance,
                    since: now,
                });
            }
            if self
                .progress
                .as_ref()
                .is_some_and(|previous| now - previous.since < STALL)
            {
                return Some(input);
            }
            if let Some(edge) = walk.blocked_edge() {
                self.blocked = Some((edge, now + BLOCKED_FOR));
            }
        }

        // An escort can also finish its path outside the moving target's
        // radius when it drifted less than the replan threshold.
        self.walk = None;
        self.progress = None;
        self.retry_at = Some(now + RETRY);
        None
    }
}

impl Orders {
    /// On the move round `center`, never standing: run between walkable
    /// points that see the way in, watching it with a slow sweep. With
    /// nowhere to run, strafe across it. TypeScript watches one of the graph
    /// ways into the spot when the order names none. Here the order's watch
    /// point, or else the walk direction, sets the view.
    fn guard_patrol(
        &mut self,
        sight: Option<&Sight>,
        position: Vec3,
        center: Vec3,
        radius: f64,
        watch: Option<Vec3>,
        now: SimTime,
    ) -> PlayerInput {
        let seconds = now.elapsed().as_secs_f64();
        let sweep = (seconds * HOLD_SWEEP_RATE).sin() * HOLD_SWEEP;
        let last = self.patrol.as_ref().map(|patrol| patrol.target);
        if self.patrol.as_ref().is_some_and(|patrol| {
            now >= patrol.until || horizontal(position, patrol.target) <= GUARD_POINT_REACHED
        }) {
            self.patrol = None;
        }
        if self.patrol.is_none() {
            self.patrol = sight
                .and_then(|sight| self.guard_point(sight, position, center, radius, watch, last))
                .map(|target| Patrol {
                    target,
                    until: now + GUARD_POINT_FOR,
                });
        }
        let look = |yaw: f64| {
            let pitch = watch.map_or(0.0, |watch| {
                let dy = watch.y - (position.y + EYE_HEIGHT);
                -dy.atan2(horizontal(position, watch))
            });
            (watch.map_or(yaw, |watch| heading(position, watch)), pitch)
        };
        if let Some(patrol) = &self.patrol {
            let heading = heading(position, patrol.target);
            let (yaw, pitch) = look(heading);
            return PlayerInput {
                direction: [heading.sin(), 0.0, heading.cos()],
                yaw: yaw + sweep,
                pitch,
                buttons: FORWARD | SPRINT,
                weapon_slot: 0,
            };
        }
        let (yaw, pitch) = look(heading(position, center));
        let turn = if ((seconds / GUARD_STRAFE_PERIOD) as u64).is_multiple_of(2) {
            1.0
        } else {
            -1.0
        };
        let room = |side: f64| {
            let (sin, cos) = (yaw + side * std::f64::consts::FRAC_PI_2).sin_cos();
            let to = Vec3::new(position.x + sin * 96.0, position.y, position.z + cos * 96.0);
            sight.is_none_or(|sight| {
                crate::games::washoi::agents::player::navigation::local_walk(sight, position, to)
            })
        };
        let side = if room(turn) || !room(-turn) {
            turn
        } else {
            -turn
        };
        let (sin, cos) = yaw.sin_cos();
        PlayerInput {
            // The view's right is (cos, 0, -sin) in world space.
            direction: [side * cos, 0.0, -side * sin],
            yaw: yaw + sweep,
            pitch,
            buttons: if side < 0.0 { LEFT } else { RIGHT },
            weapon_slot: 0,
        }
    }

    /// The next patrol point round `center`: walkable from here on this
    /// floor, away from the last, and seeing the way in when one can.
    fn guard_point(
        &mut self,
        sight: &Sight,
        position: Vec3,
        center: Vec3,
        radius: f64,
        look: Option<Vec3>,
        last: Option<Vec3>,
    ) -> Option<Vec3> {
        let r = radius.clamp(GUARD_MIN_RADIUS, GUARD_MAX_RADIUS);
        let offset = self.next_random() * std::f64::consts::TAU;
        let mut best: Option<(f64, Vec3)> = None;
        for ring in GUARD_RINGS {
            for k in 0..GUARD_DIRECTIONS {
                let angle = offset + k as f64 / GUARD_DIRECTIONS as f64 * std::f64::consts::TAU;
                let point = Vec3::new(
                    center.x + angle.sin() * r * ring,
                    position.y,
                    center.z + angle.cos() * r * ring,
                );
                if horizontal(point, position) < GUARD_SPACING
                    || last.is_some_and(|last| horizontal(point, last) < GUARD_SPACING)
                    || !crate::games::washoi::agents::player::navigation::local_walk(
                        sight, position, point,
                    )
                {
                    continue;
                }
                let sees = look.is_some_and(|look| {
                    sight.clear(
                        [point.x, point.y + GUARD_EYE, point.z],
                        [look.x, look.y + GUARD_EYE, look.z],
                    )
                });
                let score = if sees { 2.0 } else { 0.0 } + self.next_random();
                if best.is_none_or(|(top, _)| score > top) {
                    best = Some((score, point));
                }
            }
        }
        best.map(|(_, point)| point)
    }

    /// A guard whose fight drifted past its order's leash heads back while it
    /// keeps aiming, after TypeScript `BotOrders.applyLeash`.
    pub(in crate::games::washoi) fn leash(
        &self,
        position: Vec3,
        mut input: PlayerInput,
    ) -> PlayerInput {
        let Some(directive) = &self.directive else {
            return input;
        };
        let (Kind::Guard, Target::Position(center), Some(leash)) =
            (directive.kind, &directive.target, directive.leash)
        else {
            return input;
        };
        if horizontal(position, *center) <= leash {
            return input;
        }
        let back = heading(position, *center);
        input.direction = [back.sin(), 0.0, back.cos()];
        input.buttons = (input.buttons & (FIRE | ALT_FIRE | JUMP | CROUCH)) | FORWARD;
        input
    }

    /// A uniform draw in [0, 1) from a per-bot xorshift sequence.
    fn next_random(&mut self) -> f64 {
        if self.random == 0 {
            self.random = 0x9e37_79b9_7f4a_7c15;
        }
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        (self.random >> 11) as f64 / (1u64 << 53) as f64
    }
}

impl Directive {
    fn parse(value: &Value, tick: u32) -> Option<Self> {
        let id = value.get("id")?.as_str()?.trim();
        let kind = value.get("kind")?.as_str()?;
        let target = value.get("target")?;
        let priority = value.get("priority")?.as_f64()?;
        let expires = value.get("expiresAtTick")?.as_f64()?;
        let interrupt_name = value.get("interrupt")?.as_str()?;
        let interrupt = Interrupt::parse(interrupt_name)?;
        if id.is_empty()
            || id.len() > 128
            || !priority.is_finite()
            || !expires.is_finite()
            || expires <= f64::from(tick)
        {
            return None;
        }
        let kind_of = match kind {
            "move_to" => Kind::MoveTo,
            "hold_area" | "defend" => Kind::Guard,
            _ => Kind::Escort,
        };
        let destination = match (kind, target.get("type")?.as_str()?) {
            ("move_to" | "hold_area" | "defend", "position") => {
                Target::Position(point(target.get("position")?)?)
            }
            ("escort", "player") => {
                let entity_id = target.get("entityId")?.as_str()?;
                if entity_id.is_empty() || entity_id.len() > 128 {
                    return None;
                }
                Target::Escort(entity_id.to_owned())
            }
            _ => return None,
        };
        let mut via = Vec::new();
        if let Some(points) = value.get("via") {
            let points = points.as_array()?;
            if points.len() > 32 {
                return None;
            }
            for waypoint in points {
                via.push(point(waypoint)?);
            }
        }
        let radius = target
            .get("radius")
            .and_then(Value::as_f64)
            .filter(|radius| radius.is_finite() && *radius > 0.0)
            .map_or(DEFAULT_RADIUS, |radius| radius.clamp(4.0, 1024.0));
        let leash = value
            .get("leash")
            .and_then(Value::as_f64)
            .filter(|leash| leash.is_finite() && *leash > 0.0);
        Some(Self {
            id: id.to_owned(),
            kind: kind_of,
            target: destination,
            radius,
            priority,
            expires,
            immediate: interrupt == Interrupt::Never,
            interrupt,
            urgency: value
                .get("urgency")
                .and_then(Value::as_f64)
                .filter(|urgency| urgency.is_finite())
                .unwrap_or(0.0),
            watch: value.get("watch").and_then(point),
            via,
            leash,
        })
    }
}

impl Target {
    fn position(&self, world: &Observation, own_entity: &str) -> Option<Vec3> {
        let carrier = match self {
            Self::Position(position) => return Some(*position),
            Self::Escort(carrier) => carrier,
        };
        if carrier == own_entity {
            return None;
        }
        let team = &world
            .players
            .iter()
            .find(|p| p.entity_id == own_entity)?
            .team;
        if matches!(team.as_str(), "" | "spectator" | "ffa")
            || !world
                .players
                .iter()
                .any(|p| p.entity_id == *carrier && p.team == *team)
        {
            return None;
        }
        let dead = world.events.iter().rev().find_map(|event| match event {
            SnapshotEvent::CombatState {
                entity_id, dead, ..
            } if entity_id == carrier => Some(*dead),
            SnapshotEvent::Death { entity_id, .. } if entity_id == carrier => Some(true),
            _ => None,
        });
        if dead == Some(true) {
            return None;
        }
        world.body(carrier)?.position.filter(|p| finite(*p))
    }
}

fn point(value: &Value) -> Option<Vec3> {
    let x = value.get("x")?.as_f64()?;
    let y = value.get("y")?.as_f64()?;
    let z = value.get("z")?.as_f64()?;
    let point = Vec3::new(x, y, z);
    finite(point).then_some(point)
}

fn finite(point: Vec3) -> bool {
    [point.x, point.y, point.z].into_iter().all(f64::is_finite)
}

fn horizontal(a: Vec3, b: Vec3) -> f64 {
    (a.x - b.x).hypot(a.z - b.z)
}

fn heading(from: Vec3, to: Vec3) -> f64 {
    (to.x - from.x).atan2(to.z - from.z)
}

fn near(position: Vec3, target: Vec3, radius: f64) -> bool {
    (position.x - target.x).hypot(position.z - target.z) <= radius
        && (position.y - target.y).abs() <= HEIGHT_TOLERANCE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::BotPlayer;
    use shagger_shared::json;
    use std::sync::Arc;

    #[test]
    fn follows_an_addressed_lane_holds_the_destination_and_discards_expired_orders() {
        let navigation: WashoiNavigation = serde_json::from_str(
            r#"{
          "map":"snd","graph":{
            "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "nodes":[[0,0,0],[64,0,0],[64,0,64]],"edges":[[0,1],[1,2]],
            "entries":[0],"goals":[2]
          }
        }"#,
        )
        .unwrap();
        let directives = json::parse(r#"[
            {"id":"invalid","kind":"move_to","target":{"type":"player","entityId":"human"},
             "priority":1,"expiresAtTick":99,"interrupt":"never"},
            {"id":"hold","kind":"hold_area","target":{"type":"position","position":{"x":64,"y":0,"z":64},"radius":24},
             "priority":0.9,"expiresAtTick":50,"interrupt":"never",
             "via":[{"x":64,"y":0,"z":0}],"watch":{"x":64,"y":0,"z":0}}
        ]"#).unwrap();
        let mut state = Observation {
            tick: 1,
            events: vec![SnapshotEvent::AgentDirectives {
                entity_id: "other".into(),
                directives: directives.clone(),
            }],
            ..Observation::default()
        };
        let start = SimTime::ZERO;
        let mut orders = Orders::default();
        let origin = Vec3::default();
        orders.observe(&state, "bot");
        assert!(
            orders
                .input(Some(&navigation), None, origin, None, start)
                .is_none()
        );
        state.events.push(SnapshotEvent::AgentDirectives {
            entity_id: "bot".into(),
            directives,
        });
        orders.observe(&state, "bot");
        assert_eq!(
            orders
                .input(Some(&navigation), None, origin, None, start)
                .unwrap()
                .direction,
            [1.0, 0.0, 0.0]
        );
        let corner = Vec3::new(64.0, 0.0, 0.0);
        assert_eq!(
            orders
                .input(Some(&navigation), None, corner, None, start)
                .unwrap()
                .direction,
            [0.0, 0.0, 1.0]
        );
        let destination = Vec3::new(64.0, 0.0, 64.0);
        let hold = orders
            .input(Some(&navigation), None, destination, None, start)
            .unwrap();
        // Without sight there is no patrol point, so the guard strafes
        // across the way in, facing it.
        assert_eq!(hold.yaw, std::f64::consts::PI);
        assert_eq!(hold.buttons, RIGHT);
        let [x, _, z] = hold.direction;
        assert!((x.hypot(z) - 1.0).abs() < 1e-9);
        assert!((x * hold.yaw.sin() + z * hold.yaw.cos()).abs() < 1e-9);

        state.events.clear();
        state.tick = 50;
        orders.observe(&state, "bot");
        assert!(
            orders
                .input(Some(&navigation), None, destination, None, start)
                .is_none()
        );
    }

    #[test]
    fn bot_inputs_keep_steering_into_a_small_move_to_radius() {
        let navigation = serde_json::from_str(
            r#"{"map":"snd","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,0,0],[64,0,0]],"edges":[[0,1]],"entries":[0],"goals":[1]
            }}"#,
        )
        .unwrap();
        let mut bot = BotPlayer::new("bot".into()).with_navigation(Arc::new(navigation));
        let mut world = Observation {
            tick: 1,
            events: vec![SnapshotEvent::AgentDirectives {
                entity_id: "bot".into(),
                directives: json::parse(
                    r#"[{"id":"site","kind":"move_to",
                         "target":{"type":"position","position":{"x":64,"y":0,"z":0},"radius":4},
                         "priority":1,"expiresAtTick":50,"interrupt":"never"}]"#,
                )
                .unwrap(),
            }],
            ..Observation::default()
        };
        let start = SimTime::ZERO;
        for (step, x, direction) in [
            (0, 0.0, [1.0, 0.0, 0.0]),
            (1, 56.0, [1.0, 0.0, 0.0]),
            (2, 62.0, [0.0; 3]),
        ] {
            world.tick += 1;
            world.body_or_insert("bot").position = Some(Vec3::new(x, 0.0, 0.0));
            let now = start + Duration::from_millis(16 * step);
            bot.observe_at(world.clone(), now);
            bot.decide(now);
            let mut inputs = bot.inputs_at(now).into_iter();
            assert_eq!(inputs.next().unwrap().direction, direction);
            assert!(inputs.all(|input| input.direction == direction));
        }
    }

    #[test]
    fn offers_every_order_as_a_candidate_and_follows_the_one_chosen() {
        use crate::games::washoi::PlayerView;

        let mut world = Observation {
            tick: 1,
            players: ["bot", "carrier"]
                .map(|id| PlayerView {
                    entity_id: id.into(),
                    team: "red".into(),
                })
                .into(),
            events: vec![SnapshotEvent::AgentDirectives {
                entity_id: "bot".into(),
                directives: json::parse(
                    r#"[{"id":"plant","kind":"move_to","urgency":0.7,
                         "target":{"type":"position","position":{"x":300,"y":0,"z":400},"radius":96},
                         "priority":1,"expiresAtTick":50,"interrupt":"never"},
                        {"id":"cover","kind":"escort","target":{"type":"player","entityId":"carrier"},
                         "priority":0.5,"expiresAtTick":50,"interrupt":"enemy_visible"}]"#,
                )
                .unwrap(),
            }],
            ..Observation::default()
        };
        world.body_or_insert("carrier").position = Some(Vec3::new(0.0, 0.0, 30.0));
        let mut orders = Orders::default();
        orders.observe(&world, "bot");
        let candidates: Vec<_> = orders.candidates(Vec3::default()).collect();
        assert_eq!(candidates.len(), 2);
        let plant = &candidates[0];
        assert_eq!(plant.id, "plant");
        assert_eq!(plant.target, GoalTarget::Directive("plant".into()));
        assert_eq!(plant.kind, CandidateKind::MoveTo);
        assert_eq!((plant.priority, plant.urgency), (1.0, 0.7));
        assert_eq!(plant.interrupt, Interrupt::Never);
        assert_eq!(plant.route_cost, Some(500.0));
        let cover = &candidates[1];
        assert_eq!(cover.escort.as_deref(), Some("carrier"));
        assert_eq!(cover.position, [0.0, 0.0, 30.0]);
        assert_eq!(cover.urgency, 0.0);

        // Left to itself the bot follows the immediate order.
        assert_eq!(orders.directive.as_ref().unwrap().id, "plant");
        assert!(orders.choose("cover"));
        assert_eq!(orders.directive.as_ref().unwrap().id, "cover");
        assert_eq!(orders.destination, Some(Vec3::new(0.0, 0.0, 30.0)));
        assert!(!orders.choose("missing"));
        assert_eq!(orders.directive.as_ref().unwrap().id, "cover");
        // The escort target moves, and the offer follows it.
        world.events.clear();
        world.body_or_insert("carrier").position = Some(Vec3::new(0.0, 0.0, 90.0));
        orders.observe(&world, "bot");
        let cover = orders.candidates(Vec3::default()).nth(1).unwrap();
        assert_eq!(cover.position, [0.0, 0.0, 90.0]);
        // Expired orders are no longer offered.
        world.tick = 50;
        orders.observe(&world, "bot");
        assert_eq!(orders.candidates(Vec3::default()).count(), 0);
    }

    #[test]
    fn a_guard_patrols_its_ground_and_a_leash_brings_it_back_from_a_fight() {
        let orders_for = |kind: &str| {
            let mut orders = Orders::default();
            let directive = format!(
                r#"[{{"id":"site","kind":"{kind}","leash":300,
                     "target":{{"type":"position","position":{{"x":0,"y":0,"z":0}},"radius":200}},
                     "priority":1,"expiresAtTick":50,"interrupt":"never"}}]"#
            );
            orders.observe(
                &Observation {
                    tick: 1,
                    events: vec![SnapshotEvent::AgentDirectives {
                        entity_id: "bot".into(),
                        directives: json::parse(&directive).unwrap(),
                    }],
                    ..Observation::default()
                },
                "bot",
            );
            orders
        };
        let now = SimTime::ZERO;
        let inside = Vec3::new(50.0, 0.0, 0.0);
        let mut guard = orders_for("defend");
        let strafe = guard.input(None, None, inside, None, now).unwrap();
        assert_ne!(strafe.direction, [0.0; 3]);
        assert!(strafe.buttons & (LEFT | RIGHT) != 0);
        // Plants and defuses stand still on arrival.
        let mut planter = orders_for("move_to");
        let still = planter.input(None, None, inside, None, now).unwrap();
        assert_eq!(still.direction, [0.0; 3]);

        let fight = PlayerInput {
            direction: [0.0, 0.0, 1.0],
            yaw: 1.0,
            buttons: FIRE | ALT_FIRE | RIGHT | CROUCH,
            ..PlayerInput::default()
        };
        // Past the leash the walk back wins over any fight movement, as in
        // TypeScript, including a cover peek or a high-ground climb. Those keep
        // their own state and steer again once the guard is back inside.
        assert_eq!(guard.leash(Vec3::new(250.0, 0.0, 0.0), fight), fight);
        let back = guard.leash(Vec3::new(400.0, 0.0, 0.0), fight);
        assert_eq!(back.yaw, 1.0);
        assert_eq!(back.buttons, FIRE | ALT_FIRE | CROUCH | FORWARD);
        assert!((back.direction[0] + 1.0).abs() < 1e-9 && back.direction[2].abs() < 1e-9);
        assert_eq!(planter.leash(Vec3::new(400.0, 0.0, 0.0), fight), fight);
    }

    #[test]
    fn escorts_a_moving_teammate_and_drops_an_unavailable_carrier() {
        use crate::games::washoi::PlayerView;

        let mut world = Observation {
            tick: 1,
            events: vec![SnapshotEvent::AgentDirectives {
                entity_id: "bot".into(),
                directives: json::parse(
                    r#"[{"id":"snd_escort_carrier","kind":"escort",
                         "target":{"type":"player","entityId":"carrier"},
                         "priority":1,"expiresAtTick":50,"interrupt":"enemy_visible"}]"#,
                )
                .unwrap(),
            }],
            ..Observation::default()
        };
        for id in ["bot", "carrier"] {
            world.players.push(PlayerView {
                entity_id: id.into(),
                team: "soba".into(),
            });
        }
        world.body_or_insert("carrier").position = Some(Vec3::new(300.0, 0.0, 0.0));
        let navigation = serde_json::from_str(
            r#"{"map":"snd","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,0,0],[220,0,0],[0,0,220]],
                "edges":[[0,1],[0,2]],"entries":[0],"goals":[1,2]
            }}"#,
        )
        .unwrap();
        let mut orders = Orders::default();
        let origin = Vec3::default();
        let start = SimTime::ZERO;
        orders.observe(&world, "bot");
        assert_eq!(
            orders
                .input(Some(&navigation), None, origin, None, start)
                .unwrap()
                .direction,
            [1.0, 0.0, 0.0]
        );

        world.tick = 2;
        world.body_mut("carrier").unwrap().position = Some(Vec3::new(0.0, 0.0, 300.0));
        orders.observe(&world, "bot");
        assert_eq!(
            orders
                .input(Some(&navigation), None, origin, None, start)
                .unwrap()
                .direction,
            [0.0, 0.0, 1.0]
        );

        world.events.push(SnapshotEvent::CombatState {
            entity_id: "carrier".into(),
            health: 0,
            armor: 0,
            dead: true,
        });
        orders.observe(&world, "bot");
        assert!(
            orders
                .input(Some(&navigation), None, origin, None, start)
                .is_none()
        );
        world.events.pop();
        world.player_mut("carrier").unwrap().team = "udon".into();
        orders.observe(&world, "bot");
        assert!(
            orders
                .input(Some(&navigation), None, origin, None, start)
                .is_none()
        );
    }

    #[test]
    fn backtracks_to_a_known_node_and_avoids_a_stalled_edge() {
        let navigation = serde_json::from_str(
            r#"{"map":"snd","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,0,0],[200,0,0],[0,0,100]],
                "edges":[[0,1],[0,2],[2,1]],"entries":[0],"goals":[1]
            }}"#,
        )
        .unwrap();
        let world = Observation {
            tick: 1,
            events: vec![SnapshotEvent::AgentDirectives {
                entity_id: "bot".into(),
                directives: json::parse(
                    r#"[{"id":"site","kind":"move_to",
                         "target":{"type":"position","position":{"x":200,"y":0,"z":0},"radius":24},
                         "priority":1,"expiresAtTick":1000,"interrupt":"never"}]"#,
                )
                .unwrap(),
            }],
            ..Observation::default()
        };
        let start = SimTime::ZERO;
        let origin = Vec3::default();
        let blocked = Vec3::new(80.0, 0.0, 0.0);
        let mut orders = Orders::default();
        orders.observe(&world, "bot");
        for (ms, position, expected) in [
            (0, origin, Some([1.0, 0.0, 0.0])),
            (300, blocked, Some([1.0, 0.0, 0.0])),
            // Sideways motion away from the waypoint does not reset the stall timer.
            (800, Vec3::new(200.0, 0.0, 120.0), Some([0.0, 0.0, -1.0])),
            (1200, blocked, None),
            (1600, blocked, Some([-1.0, 0.0, 0.0])),
            (1850, origin, Some([0.0, 0.0, 1.0])),
        ] {
            let input = orders.input(
                Some(&navigation),
                None,
                position,
                None,
                start + Duration::from_millis(ms),
            );
            assert_eq!(input.map(|input| input.direction), expected);
        }
    }
}

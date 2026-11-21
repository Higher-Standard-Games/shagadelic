//! Gametype orders, following SHAG's AgentDirective contract.

use std::time::Duration;

use crate::games::washoi::SimTime;
use shagger_shared::{
    json::Value,
    net::game_state::{GameState, Vec3, Velocity},
};

use super::{
    Sight, WashoiNavigation,
    navigation::{ObjectiveRecovery, ObjectiveWalk},
    protocol::{PlayerInput, SnapshotEvent},
};

const DEFAULT_RADIUS: f64 = 160.0;
const WAYPOINT_RADIUS: f64 = 48.0;
const HEIGHT_TOLERANCE: f64 = 40.0;
const ESCORT_REPLAN_DISTANCE: f64 = 48.0;
const RETRY: Duration = Duration::from_millis(350);
const STALL: Duration = Duration::from_millis(850);
const BLOCKED_FOR: Duration = Duration::from_secs(5);
const MIN_PROGRESS: f64 = 8.0;

#[derive(Debug, PartialEq)]
struct Directive {
    id: String,
    target: Target,
    radius: f64,
    priority: f64,
    expires: f64,
    immediate: bool,
    watch: Option<Vec3>,
    via: Vec<Vec3>,
}

#[derive(Debug, PartialEq)]
enum Target {
    Position(Vec3),
    Escort(String),
}

#[derive(Default)]
pub(super) struct Orders {
    directive: Option<Directive>,
    destination: Option<Vec3>,
    via: usize,
    walk: Option<(Vec3, ObjectiveWalk)>,
    blocked: Option<(ObjectiveRecovery, SimTime)>,
    retry_at: Option<SimTime>,
    progress: Option<Progress>,
}

struct Progress {
    waypoint: usize,
    distance: f64,
    since: SimTime,
}

impl Orders {
    pub(super) fn observe(&mut self, world: &GameState<SnapshotEvent>, own_entity: &str) {
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
        if let Some(directives) = addressed {
            let latest = directives
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|value| Directive::parse(value, world.tick))
                .filter_map(|order| Some((order.target.position(world, own_entity)?, order)))
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
            self.destination = self
                .directive
                .as_ref()
                .and_then(|order| order.target.position(world, own_entity));
        }
    }

    fn clear_order(&mut self) {
        *self = Self {
            blocked: self.blocked,
            ..Self::default()
        };
    }

    pub(super) fn input(
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

impl Directive {
    fn parse(value: &Value, tick: u32) -> Option<Self> {
        let id = value.get("id")?.as_str()?.trim();
        let kind = value.get("kind")?.as_str()?;
        let target = value.get("target")?;
        let priority = value.get("priority")?.as_f64()?;
        let expires = value.get("expiresAtTick")?.as_f64()?;
        let interrupt = value.get("interrupt")?.as_str()?;
        if id.is_empty()
            || id.len() > 128
            || !priority.is_finite()
            || !expires.is_finite()
            || expires <= f64::from(tick)
            || !matches!(
                interrupt,
                "never" | "danger" | "enemy_visible" | "higher_priority"
            )
        {
            return None;
        }
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
        Some(Self {
            id: id.to_owned(),
            target: destination,
            radius,
            priority,
            expires,
            immediate: interrupt == "never",
            watch: value.get("watch").and_then(point),
            via,
        })
    }
}

impl Target {
    fn position(&self, world: &GameState<SnapshotEvent>, own_entity: &str) -> Option<Vec3> {
        let carrier = match self {
            Self::Position(position) => return Some(*position),
            Self::Escort(carrier) => carrier,
        };
        if carrier == own_entity {
            return None;
        }
        let team = &world
            .players
            .values()
            .find(|p| p.entity_id == own_entity)?
            .team;
        if matches!(team.as_str(), "" | "spectator" | "ffa")
            || !world
                .players
                .values()
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
        world.entities.get(carrier)?.position.filter(|p| finite(*p))
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
        let mut state = GameState {
            tick: 1,
            events: vec![SnapshotEvent::AgentDirectives {
                entity_id: "other".into(),
                directives: directives.clone(),
            }],
            ..GameState::default()
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
        assert_eq!(hold.direction, [0.0; 3]);
        assert_eq!(hold.yaw, std::f64::consts::PI);

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
    fn bot_inputs_keep_steering_into_a_small_hold_area() {
        let navigation = serde_json::from_str(
            r#"{"map":"snd","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,0,0],[64,0,0]],"edges":[[0,1]],"entries":[0],"goals":[1]
            }}"#,
        )
        .unwrap();
        let mut bot = BotPlayer::new("bot".into()).with_navigation(Arc::new(navigation));
        let mut world = GameState {
            tick: 1,
            events: vec![SnapshotEvent::AgentDirectives {
                entity_id: "bot".into(),
                directives: json::parse(
                    r#"[{"id":"site","kind":"hold_area",
                         "target":{"type":"position","position":{"x":64,"y":0,"z":0},"radius":4},
                         "priority":1,"expiresAtTick":50,"interrupt":"never"}]"#,
                )
                .unwrap(),
            }],
            ..GameState::default()
        };
        let start = SimTime::ZERO;
        for (step, x, direction) in [
            (0, 0.0, [1.0, 0.0, 0.0]),
            (1, 56.0, [1.0, 0.0, 0.0]),
            (2, 62.0, [0.0; 3]),
        ] {
            world.tick += 1;
            world
                .entities
                .get_or_insert_with("bot", Default::default)
                .position = Some(Vec3::new(x, 0.0, 0.0));
            let now = start + Duration::from_millis(16 * step);
            bot.observe(&world, now);
            let mut inputs = bot.inputs(now);
            assert_eq!(inputs.next().unwrap().direction, direction);
            assert!(inputs.all(|input| input.direction == direction));
        }
    }

    #[test]
    fn escorts_a_moving_teammate_and_drops_an_unavailable_carrier() {
        use shagger_shared::net::game_state::PlayerState;

        let mut world = GameState {
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
            ..GameState::default()
        };
        for id in ["bot", "carrier"] {
            world.players.insert(
                id,
                PlayerState {
                    entity_id: id.into(),
                    team: "soba".into(),
                    ..PlayerState::default()
                },
            );
        }
        world
            .entities
            .get_or_insert_with("carrier", Default::default)
            .position = Some(Vec3::new(300.0, 0.0, 0.0));
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
        world.entities.get_mut("carrier").unwrap().position = Some(Vec3::new(0.0, 0.0, 300.0));
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
        world.players.get_mut("carrier").unwrap().team = "udon".into();
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
        let world = GameState {
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
            ..GameState::default()
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

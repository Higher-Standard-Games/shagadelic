//! Positional gametype orders, following SHAG's AgentDirective contract.

use std::time::Duration;

use shagger_shared::{
    json::Value,
    net::game_state::{GameState, Vec3, Velocity},
};
use tokio::time::Instant;

use super::{
    WashoiNavigation,
    navigation::ObjectiveWalk,
    protocol::{PlayerInput, SnapshotEvent},
};

const DEFAULT_RADIUS: f64 = 160.0;
const WAYPOINT_RADIUS: f64 = 48.0;
const HEIGHT_TOLERANCE: f64 = 40.0;
const RETRY: Duration = Duration::from_millis(350);
const STALL: Duration = Duration::from_millis(850);

#[derive(Debug, PartialEq)]
struct Directive {
    id: String,
    target: Vec3,
    radius: f64,
    priority: f64,
    expires: f64,
    immediate: bool,
    watch: Option<Vec3>,
    via: Vec<Vec3>,
}

#[derive(Default)]
pub(super) struct Orders {
    directive: Option<Directive>,
    via: usize,
    walk: Option<ObjectiveWalk>,
    retry_at: Option<Instant>,
    progress: Option<(Vec3, Instant)>,
}

impl Orders {
    pub(super) fn observe(&mut self, world: &GameState<SnapshotEvent>, own_entity: &str) {
        if self
            .directive
            .as_ref()
            .is_some_and(|order| order.expires <= f64::from(world.tick))
        {
            *self = Self::default();
        }
        for event in &world.events {
            let SnapshotEvent::AgentDirectives {
                entity_id,
                directives,
            } = event
            else {
                continue;
            };
            if entity_id != own_entity {
                continue;
            }
            let latest = directives
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|value| Directive::parse(value, world.tick))
                .min_by(|a, b| {
                    b.immediate
                        .cmp(&a.immediate)
                        .then_with(|| b.priority.total_cmp(&a.priority))
                        .then_with(|| a.id.cmp(&b.id))
                });
            let same_route = matches!((&self.directive, &latest), (Some(a), Some(b))
                if a.id == b.id && a.target == b.target && a.radius == b.radius && a.via == b.via);
            if !same_route {
                *self = Self::default();
            }
            self.directive = latest;
        }
    }

    pub(super) fn input(
        &mut self,
        navigation: Option<&WashoiNavigation>,
        position: Vec3,
        velocity: Option<Velocity>,
        now: Instant,
    ) -> Option<PlayerInput> {
        let directive = self.directive.as_ref()?;
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
            .map_or((directive.target, directive.radius), |point| {
                (point, WAYPOINT_RADIUS)
            });
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
        if let Some((previous, at)) = self.progress {
            if !near(position, previous, 8.0) {
                self.progress = Some((position, now));
            } else if now - at >= STALL {
                self.walk = None;
                self.progress = None;
                self.retry_at = Some(now + RETRY);
            }
        }
        if self.walk.is_none() && self.retry_at.is_none_or(|at| now >= at) {
            self.walk = navigation.and_then(|map| map.objective_walk(position, target, radius));
            self.progress = self.walk.as_ref().map(|_| (position, now));
            self.retry_at = Some(now + RETRY);
        }
        self.walk
            .as_mut()
            .map(|walk| walk.steer(position, velocity))
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
            || !matches!(kind, "move_to" | "hold_area" | "defend")
            || target.get("type")?.as_str()? != "position"
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
        let position = point(target.get("position")?)?;
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
            target: position,
            radius,
            priority,
            expires,
            immediate: interrupt == "never",
            watch: value.get("watch").and_then(point),
            via,
        })
    }
}

fn point(value: &Value) -> Option<Vec3> {
    let x = value.get("x")?.as_f64()?;
    let y = value.get("y")?.as_f64()?;
    let z = value.get("z")?.as_f64()?;
    [x, y, z]
        .into_iter()
        .all(f64::is_finite)
        .then(|| Vec3::new(x, y, z))
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
        let start = Instant::now();
        let mut orders = Orders::default();
        let origin = Vec3::default();
        orders.observe(&state, "bot");
        assert!(
            orders
                .input(Some(&navigation), origin, None, start)
                .is_none()
        );
        state.events.push(SnapshotEvent::AgentDirectives {
            entity_id: "bot".into(),
            directives,
        });
        orders.observe(&state, "bot");
        assert_eq!(
            orders
                .input(Some(&navigation), origin, None, start)
                .unwrap()
                .direction,
            [1.0, 0.0, 0.0]
        );
        let corner = Vec3::new(64.0, 0.0, 0.0);
        assert_eq!(
            orders
                .input(Some(&navigation), corner, None, start)
                .unwrap()
                .direction,
            [0.0, 0.0, 1.0]
        );
        let destination = Vec3::new(64.0, 0.0, 64.0);
        let hold = orders
            .input(Some(&navigation), destination, None, start)
            .unwrap();
        assert_eq!(hold.direction, [0.0; 3]);
        assert_eq!(hold.yaw, std::f64::consts::PI);

        state.events.clear();
        state.tick = 50;
        orders.observe(&state, "bot");
        assert!(
            orders
                .input(Some(&navigation), destination, None, start)
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
        let start = Instant::now();
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
}

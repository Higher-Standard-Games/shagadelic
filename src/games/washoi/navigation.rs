mod follower;
mod graph;

use std::{
    io::{self, Read},
    path::Path,
};

use serde::Deserialize;
use shagger_shared::net::game_state::{Vec3, Velocity};

use super::{goals::PickupGoal, protocol::PlayerInput};

pub(super) use follower::{RouteFollower, RouteObservation};

const ENTRY_RADIUS: f64 = 48.0;
const MAX_FILE: u64 = 4 * 1024 * 1024;
const ARRIVAL_RADIUS: f64 = 16.0;
const MAX_HEIGHT_DELTA: f64 = 40.0;
const ANCHOR_RADIUS: f64 = 128.0;
// Washoi `shared/server/simulation/physics/constants.ts`.
const GRAVITY: f64 = 800.0;
const RUN_SPEED: f64 = 320.0;
// Approach a ledge slowly enough to step off it rather than sail past the
// landing. Most cached falls land 32 units out and 100 to 230 units down.
const LEDGE_SPEED: f64 = 100.0;
const LEDGE_BRAKE_RADIUS: f64 = 96.0;
const MIN_STEP_SPEED: f64 = 30.0;

pub(super) struct ObjectiveWalk {
    route: Route,
    edges: Vec<ObjectiveEdge>,
    next: usize,
    arrival: f64,
}

struct ObjectiveEdge {
    nodes: (usize, usize),
    end: usize,
}

#[derive(Clone, Copy)]
pub(super) struct ObjectiveRecovery {
    pub(super) anchor: Option<usize>,
    pub(super) avoid: Option<(usize, usize)>,
}

impl ObjectiveWalk {
    pub(super) fn steer(&mut self, position: Vec3, velocity: Option<Velocity>) -> PlayerInput {
        WashoiNavigation::steer_with_arrival(
            &self.route,
            &mut self.next,
            position,
            velocity,
            self.arrival,
        )
    }

    pub(super) fn blocked_edge(&self) -> Option<ObjectiveRecovery> {
        let (previous, edge) = self
            .edges
            .iter()
            .scan(0, |previous, edge| {
                let start = *previous;
                *previous = edge.end;
                Some((start, edge))
            })
            .find(|&(start, edge)| start < self.next && self.next <= edge.end)?;
        // Backtracking directly to the start is safe only on the first
        // straight leg of this edge, before any corners.
        let anchor = (self.next == previous + 1).then_some(edge.nodes.0);
        Some(ObjectiveRecovery {
            anchor,
            avoid: Some(edge.nodes),
        })
    }

    pub(super) fn progress(&self, position: Vec3) -> Option<(usize, f64)> {
        let waypoint = *self.route.waypoints.get(self.next)?;
        let distance = horizontal_distance(position, waypoint).hypot(position.y - waypoint[1]);
        Some((self.next, distance))
    }
}

/// Authored walking routes. Coordinates are Washoi world-space XYZ.
#[derive(Deserialize)]
#[serde(try_from = "RouteFile")]
pub struct WashoiNavigation {
    map: String,
    routes: Vec<Route>,
    planner: Option<graph::Planner>,
}

#[derive(Deserialize)]
struct RouteFile {
    map: String,
    #[serde(default)]
    routes: Vec<Route>,
    graph: Option<graph::WalkGraph>,
}

#[derive(Deserialize)]
struct Route {
    entry: [f64; 3],
    waypoints: Vec<[f64; 3]>,
    learned_route: Option<String>,
    #[serde(default)]
    resumed_from: Option<String>,
    #[serde(default)]
    pickup: Option<PickupGoal>,
}

impl TryFrom<RouteFile> for WashoiNavigation {
    type Error = &'static str;

    fn try_from(file: RouteFile) -> Result<Self, Self::Error> {
        let (routes, planner) = match file.graph {
            Some(graph) if file.routes.is_empty() => {
                let (routes, planner) = graph.routes()?;
                (routes, Some(planner))
            }
            Some(_) => return Err("configure authored routes or a graph"),
            None => (file.routes, None),
        };
        if file.map.trim().is_empty() || routes.is_empty() {
            return Err("navigation needs a map and at least one route");
        }
        for route in &routes {
            if route.waypoints.is_empty()
                || !std::iter::once(&route.entry)
                    .chain(&route.waypoints)
                    .flatten()
                    .all(|value| value.is_finite())
            {
                return Err("navigation routes need finite coordinates and at least one waypoint");
            }
        }
        Ok(Self {
            map: file.map,
            routes,
            planner,
        })
    }
}

impl WashoiNavigation {
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_FILE + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "navigation file exceeds size limit",
            ));
        }
        serde_json::from_slice(&bytes).map_err(io::Error::from)
    }

    pub fn map(&self) -> &str {
        &self.map
    }

    pub(super) fn objective_walk(
        &self,
        position: Vec3,
        target: Vec3,
        radius: f64,
        recovery: Option<ObjectiveRecovery>,
    ) -> Option<ObjectiveWalk> {
        let arrival = ARRIVAL_RADIUS.min(radius / 2.0);
        let (route, edges) =
            self.planner
                .as_ref()?
                .plan_to(position, target, radius - arrival, recovery)?;
        Some(ObjectiveWalk {
            route,
            edges,
            next: 0,
            arrival,
        })
    }

    pub(super) fn has_learned_links(&self) -> bool {
        self.routes
            .iter()
            .any(|route| route.learned_route.is_some())
    }

    pub fn validate_learned_links(
        &self,
        library: Option<&super::LearnedRoutes>,
    ) -> Result<(), &'static str> {
        for route in &self.routes {
            if let Some(id) = &route.learned_route
                && !library.is_some_and(|library| {
                    let [x, y, z] = *route.waypoints.last().expect("validated route");
                    library.matches_takeoff(id, Vec3::new(x, y, z))
                })
            {
                return Err("learned route link must end near a known takeoff");
            }
            if let Some(id) = &route.resumed_from
                && !library.is_some_and(|library| {
                    let [x, y, z] = route.entry;
                    library.matches_landing(id, Vec3::new(x, y, z))
                })
            {
                return Err("learned continuation must start near a known landing");
            }
        }
        Ok(())
    }

    /// Plan from the nearest graph node when no precomputed route attaches.
    /// Authored route files have no graph to plan over.
    fn plan(
        &self,
        position: Vec3,
        anchor: Option<[f64; 3]>,
        accept: impl Fn(&Route) -> bool,
        score: impl Fn(&Route) -> f64,
    ) -> Option<Route> {
        self.planner.as_ref()?.plan(position, anchor, accept, score)
    }

    fn route_at(&self, position: Vec3, allowed: impl Fn(&Route) -> bool) -> Option<usize> {
        self.select_route(position, allowed, |_| 0.0)
    }

    fn select_route(
        &self,
        position: Vec3,
        allowed: impl Fn(&Route) -> bool,
        score: impl Fn(&Route) -> f64,
    ) -> Option<usize> {
        self.routes
            .iter()
            .enumerate()
            .filter(|(_, route)| allowed(route))
            .filter(|(_, route)| (route.entry[1] - position.y).abs() <= MAX_HEIGHT_DELTA)
            .filter_map(|(index, route)| {
                let distance = horizontal_distance(position, route.entry);
                (distance <= ENTRY_RADIUS).then(|| (index, distance, score(route)))
            })
            .min_by(
                |(a_index, a_distance, a_score), (b_index, b_distance, b_score)| {
                    a_distance
                        .total_cmp(b_distance)
                        .then_with(|| {
                            self.routes[*b_index]
                                .learned_route
                                .is_some()
                                .cmp(&self.routes[*a_index].learned_route.is_some())
                        })
                        .then_with(|| b_score.total_cmp(a_score))
                },
            )
            .map(|(index, _, _)| index)
    }

    fn steer(
        route: &Route,
        next: &mut usize,
        position: Vec3,
        velocity: Option<Velocity>,
    ) -> PlayerInput {
        Self::steer_with_arrival(route, next, position, velocity, ARRIVAL_RADIUS)
    }

    fn steer_with_arrival(
        route: &Route,
        next: &mut usize,
        position: Vec3,
        velocity: Option<Velocity>,
        arrival: f64,
    ) -> PlayerInput {
        let waypoints = &route.waypoints;
        while let Some(&waypoint) = waypoints.get(*next) {
            // Steer toward a waypoint at another height, as on a ramp or
            // over a ledge, but count it reached only on its own level.
            // A bot that passes over or under a waypoint hasn't reached it.
            let distance = horizontal_distance(position, waypoint);
            let drop = position.y - waypoint[1];
            let threshold = if *next + 1 == waypoints.len() {
                arrival
            } else {
                ARRIVAL_RADIUS
            };
            let (x, z) = if distance > threshold {
                (
                    (waypoint[0] - position.x) / distance,
                    (waypoint[2] - position.z) / distance,
                )
            } else if drop > MAX_HEIGHT_DELTA {
                // Over the landing but still on the ledge: keep stepping off
                // along the leg.
                let from = if *next == 0 {
                    route.entry
                } else {
                    waypoints[*next - 1]
                };
                let (x, z) = (waypoint[0] - from[0], waypoint[2] - from[2]);
                let length = x.hypot(z);
                if length == 0.0 {
                    break;
                }
                (x / length, z / length)
            } else if -drop > MAX_HEIGHT_DELTA {
                break;
            } else {
                *next += 1;
                continue;
            };
            let yaw = x.atan2(z);
            let cap = if drop > MAX_HEIGHT_DELTA {
                // Leave the ledge at the speed that lands on the waypoint.
                let fall = (2.0 * drop / GRAVITY).sqrt();
                Some((distance / fall).max(MIN_STEP_SPEED))
            } else {
                waypoints
                    .get(*next + 1)
                    .filter(|after| waypoint[1] - after[1] > MAX_HEIGHT_DELTA)
                    .and_then(|_| (distance <= LEDGE_BRAKE_RADIUS).then_some(LEDGE_SPEED))
            };
            // In the air, keep steering toward the landing.
            if let Some(cap) = cap.filter(|&cap| cap < RUN_SPEED)
                && let Some(velocity) = velocity.filter(|velocity| velocity.grounded)
                && velocity.linear.x.hypot(velocity.linear.z) > cap
            {
                // Coast: Washoi's ground friction brakes a player without input.
                return PlayerInput {
                    yaw,
                    ..PlayerInput::default()
                };
            }
            return PlayerInput {
                direction: [x, 0.0, z],
                yaw,
                ..PlayerInput::default()
            };
        }
        PlayerInput::default()
    }
}

fn horizontal_distance(position: Vec3, target: [f64; 3]) -> f64 {
    (target[0] - position.x).hypot(target[2] - position.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baked_graph_uses_the_detour_instead_of_the_blocked_shortcut() {
        let baked = include_str!("../../../tests/fixtures/washoi_dm2_walk_graph.json");
        let routes: WashoiNavigation = serde_json::from_str(baked).unwrap();
        let route = routes
            .route_at(Vec3::new(-184.0, 192.0, 288.0), |_| true)
            .unwrap();
        assert_eq!(
            routes.routes[route].waypoints,
            [[-232.0, 192.0, 192.0], [-88.0, 192.0, 312.0],]
        );
        let config: serde_json::Value = serde_json::from_str(baked).unwrap();
        let mut invalid = config.clone();
        invalid["graph"]["edges"][0] = serde_json::json!([0, 999]);
        assert!(serde_json::from_value::<WashoiNavigation>(invalid).is_err());
        let mut disconnected = config;
        disconnected["graph"]["edges"] = serde_json::json!([]);
        assert!(serde_json::from_value::<WashoiNavigation>(disconnected).is_err());
    }

    #[test]
    fn the_cached_washoi_dm2_graph_plans_from_its_spawns() {
        let baked = include_str!("../../../tests/fixtures/washoi_dm2_cache_graph.json");
        // The navigation object travels inside one 64 KiB control record.
        assert!(baked.len() < 48 * 1024);
        let graph: WashoiNavigation = serde_json::from_str(baked).unwrap();
        let raw: serde_json::Value = serde_json::from_str(baked).unwrap();
        let nodes: Vec<[f64; 3]> = serde_json::from_value(raw["graph"]["nodes"].clone()).unwrap();
        let entries: Vec<usize> = serde_json::from_value(raw["graph"]["entries"].clone()).unwrap();
        let edges = raw["graph"]["edges"].as_array().unwrap();
        let stuck: Vec<_> = entries
            .iter()
            .copied()
            .filter(|&entry| {
                let [x, y, z] = nodes[entry];
                graph
                    .plan(Vec3::new(x, y, z), None, |_| true, |_| 0.0)
                    .is_none()
            })
            .collect();
        // Only the Red Armor's ledge has no walk out. It is left by a jump.
        assert_eq!(stuck, [3]);
        assert!(!edges.iter().any(|edge| edge[0] == 3));
    }

    #[test]
    fn brakes_before_a_ledge_and_steps_off_toward_the_landing() {
        let routes: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","routes":[
                {"entry":[0,200,0],"waypoints":[[100,200,0],[132,0,0]]}
            ]}"#,
        )
        .unwrap();
        let route = &routes.routes[0];
        let moving = |speed: f64, grounded| Velocity {
            linear: Vec3::new(speed, 0.0, 0.0),
            grounded,
            is_crouching: false,
            is_sliding: false,
        };
        let steer = |next: &mut usize, x, y, velocity| {
            WashoiNavigation::steer(route, next, Vec3::new(x, y, 0.0), Some(velocity)).direction
        };
        let mut next = 0;
        // Far from the ledge, run. Near it, coast down to ledge speed.
        assert_eq!(
            steer(&mut next, 0.0, 200.0, moving(320.0, true)),
            [1.0, 0.0, 0.0]
        );
        assert_eq!(steer(&mut next, 20.0, 200.0, moving(320.0, true)), [0.0; 3]);
        assert_eq!(
            steer(&mut next, 20.0, 200.0, moving(90.0, true)),
            [1.0, 0.0, 0.0]
        );
        // 32 units out and 200 down takes about 0.7 s, so leave at under 46 u/s.
        assert_eq!(steer(&mut next, 100.0, 200.0, moving(90.0, true)), [0.0; 3]);
        assert_eq!(next, 1);
        assert_eq!(
            steer(&mut next, 100.0, 200.0, moving(40.0, true)),
            [1.0, 0.0, 0.0]
        );
        assert_eq!(
            steer(&mut next, 110.0, 150.0, moving(90.0, false)),
            [1.0, 0.0, 0.0]
        );
        // Over the landing but still on the ledge, keep stepping off.
        assert_eq!(
            steer(&mut next, 130.0, 200.0, moving(10.0, true)),
            [1.0, 0.0, 0.0]
        );
        assert_eq!(next, 1);
    }

    #[test]
    fn walks_only_to_goals_the_bot_can_walk_back_from() {
        // Node 2 is a pocket: walkable into, but its exit is a drop the
        // cache importer leaves out. Its pickup would score higher.
        let graph: WashoiNavigation = serde_json::from_str(
            r#"{
            "map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,0,0],[100,0,0],[0,0,100]],
                "edges":[[0,1],[1,0],[0,2]],"entries":[0],"goals":[1,2],
                "pickup_goals":[[2,{"id":0,"kind":"powerup"}]]
            }
        }"#,
        )
        .unwrap();
        let pocket = [0.0, 0.0, 100.0];
        assert!(
            graph
                .routes
                .iter()
                .all(|route| !route.waypoints.contains(&pocket))
        );
        let planned = graph
            .plan(
                Vec3::new(0.0, 0.0, 0.0),
                None,
                |_| true,
                |route| f64::from(u8::from(route.pickup.is_some())),
            )
            .unwrap();
        assert_eq!(planned.waypoints, [[100.0, 0.0, 0.0]]);
        // A bot left in the dead end has no walk out until it dies.
        let inside = graph.plan(Vec3::new(0.0, 0.0, 100.0), None, |_| true, |_| 0.0);
        assert!(inside.is_none());

        // Dropping one way from a small area into a larger one is fine.
        let ledge: WashoiNavigation = serde_json::from_str(
            r#"{
            "map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,200,0],[40,0,0],[200,0,0],[200,0,200]],
                "edges":[[0,1],[1,2],[2,1],[2,3],[3,2]],"entries":[0],"goals":[2,3]
            }
        }"#,
        )
        .unwrap();
        let down = ledge
            .plan(Vec3::new(0.0, 200.0, 0.0), None, |_| true, |_| 0.0)
            .unwrap();
        assert_eq!(down.waypoints, [[40.0, 0.0, 0.0], [200.0, 0.0, 0.0]]);
    }

    #[test]
    fn selects_a_nearby_route_on_the_same_floor_and_advances_waypoints() {
        let routes: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","routes":[
                {"entry":[0,100,0],"waypoints":[[40,100,0]]},
                {"entry":[0,0,0],"waypoints":[[40,0,0],[40,0,40]]}
            ]}"#,
        )
        .unwrap();
        let route = routes.route_at(Vec3::new(0.0, 0.0, 0.0), |_| true).unwrap();
        assert_eq!(route, 1);
        assert_eq!(routes.route_at(Vec3::new(100.0, 0.0, 0.0), |_| true), None);
        let mut next = 0;
        assert_eq!(
            WashoiNavigation::steer(
                &routes.routes[route],
                &mut next,
                Vec3::new(0.0, 0.0, 0.0),
                None,
            )
            .direction,
            [1.0, 0.0, 0.0]
        );
        assert_eq!(
            WashoiNavigation::steer(
                &routes.routes[route],
                &mut next,
                Vec3::new(40.0, 0.0, 0.0),
                None,
            )
            .direction,
            [0.0, 0.0, 1.0]
        );
        assert_eq!(
            WashoiNavigation::steer(
                &routes.routes[route],
                &mut next,
                Vec3::new(40.0, 0.0, 40.0),
                None,
            )
            .direction,
            [0.0; 3]
        );
        assert_eq!(next, 2);
    }
}

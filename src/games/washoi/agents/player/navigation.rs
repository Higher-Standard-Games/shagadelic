mod follower;
mod graph;
pub(in crate::games::washoi) mod learned;
mod traversal;
pub(in crate::games::washoi) mod unstick;

use std::{
    io::{self, Read},
    path::Path,
};

use serde::Deserialize;
use shagger_shared::net::game_state::{Vec3, Velocity};

use crate::games::washoi::{
    Sight, agents::player::strategy::goals::PickupGoal, hosting::net::protocol::PlayerInput,
};

pub(in crate::games::washoi) use follower::{RouteFollower, RouteObservation};

const ENTRY_RADIUS: f64 = 48.0;
const MAX_FILE: u64 = 4 * 1024 * 1024;
const ARRIVAL_RADIUS: f64 = 16.0;
const MAX_HEIGHT_DELTA: f64 = 40.0;
const ANCHOR_RADIUS: f64 = 128.0;
// Sparse cache graphs leave open floor more than 128 units from any node, as on
// `washoi_snd1`, where a fight can strand a bot. The walk is sampled every 16
// units, so a longer straight connector stays as safe as a short one.
const LOCAL_WALK_RADIUS: f64 = 256.0;
const SPRINT_LEG_DISTANCE: f64 = 128.0;
const SPRINT_HEIGHT_DELTA: f64 = 16.0;
// Washoi engine/network/components/InputComponent.ts.
const SPRINT: u16 = 1 << 6;
// Washoi `shared/server/simulation/physics/constants.ts`.
const GRAVITY: f64 = 800.0;
const RUN_SPEED: f64 = 320.0;
// Approach a ledge slowly enough to step off it rather than sail past the
// landing. Most cached falls land 32 units out and 100 to 230 units down.
const LEDGE_SPEED: f64 = 100.0;
const LEDGE_BRAKE_RADIUS: f64 = 96.0;
const MIN_STEP_SPEED: f64 = 30.0;

pub(in crate::games::washoi) struct ObjectiveWalk {
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
pub(in crate::games::washoi) struct ObjectiveRecovery {
    pub(in crate::games::washoi) anchor: Option<usize>,
    pub(in crate::games::washoi) avoid: Option<(usize, usize)>,
}

impl ObjectiveWalk {
    pub(in crate::games::washoi) fn steer(
        &mut self,
        position: Vec3,
        velocity: Option<Velocity>,
    ) -> PlayerInput {
        let mut input = WashoiNavigation::steer_with_arrival(
            &self.route,
            &mut self.next,
            position,
            velocity,
            self.arrival,
        );
        if input.direction != [0.0; 3]
            && velocity.is_some_and(|velocity| velocity.grounded)
            && let Some(&waypoint) = self.route.waypoints.get(self.next)
            && (position.y - waypoint[1]).abs() <= SPRINT_HEIGHT_DELTA
            && horizontal_distance(position, waypoint) >= SPRINT_LEG_DISTANCE
        {
            input.buttons |= SPRINT;
        }
        input
    }

    pub(in crate::games::washoi) fn blocked_edge(&self) -> Option<ObjectiveRecovery> {
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

    pub(in crate::games::washoi) fn progress(&self, position: Vec3) -> Option<(usize, f64)> {
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
    #[serde(skip)]
    maneuvers: Vec<(usize, traversal::Maneuver)>,
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

    /// The walk graph's node positions, standing origins, in graph index
    /// order and stable for the life of this navigation. Empty for
    /// authored routes.
    /// Every map pickup the navigation knows, with where to stand for it
    /// (a standing origin), from the graph's goals and authored routes, once
    /// per pickup ID. Empty without pickup goals.
    pub(in crate::games::washoi) fn pickups(&self) -> Vec<(PickupGoal, [f64; 3])> {
        let mut pickups: Vec<(PickupGoal, [f64; 3])> = Vec::new();
        let graph = self.planner.iter().flat_map(|planner| planner.pickups());
        let authored = self
            .routes
            .iter()
            .filter_map(|route| Some((route.pickup?, *route.waypoints.last()?)));
        for (pickup, at) in graph.chain(authored) {
            if !pickups.iter().any(|(known, _)| known.id == pickup.id) {
                pickups.push((pickup, at));
            }
        }
        pickups
    }

    pub(in crate::games::washoi) fn nodes(&self) -> &[[f64; 3]] {
        self.planner.as_ref().map_or(&[], |planner| planner.nodes())
    }

    pub(in crate::games::washoi) fn objective_walk(
        &self,
        position: Vec3,
        target: Vec3,
        radius: f64,
        recovery: Option<ObjectiveRecovery>,
        sight: Option<&Sight>,
    ) -> Option<ObjectiveWalk> {
        let arrival = ARRIVAL_RADIUS.min(radius / 2.0);
        let (route, edges) = self.planner.as_ref()?.plan_to(
            position,
            target,
            radius - arrival,
            recovery,
            |from, to| sight.is_some_and(|map| local_walk(map, from, to)),
        )?;
        Some(ObjectiveWalk {
            route,
            edges,
            next: 0,
            arrival,
        })
    }

    pub(in crate::games::washoi) fn has_learned_links(&self) -> bool {
        self.routes
            .iter()
            .any(|route| route.learned_route.is_some())
    }

    pub fn validate_learned_links(
        &self,
        library: Option<&crate::games::washoi::LearnedRoutes>,
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
        sight: Option<&Sight>,
        accept: impl Fn(&Route) -> bool,
        score: impl Fn(&Route) -> f64,
    ) -> Option<Route> {
        self.planner
            .as_ref()?
            .plan(position, anchor, accept, score, |from, to| {
                sight.is_some_and(|map| local_walk(map, from, to))
            })
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
            if route.maneuver_at(*next).is_some() {
                break;
            }
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

impl Route {
    fn maneuver_at(&self, next: usize) -> Option<traversal::Maneuver> {
        let index = self
            .maneuvers
            .binary_search_by_key(&next, |&(at, _)| at)
            .ok()?;
        Some(self.maneuvers[index].1)
    }
}

/// A short connector on the present floor. The sight geometry supplies
/// clearance and support samples, while the game server runs player physics.
pub(in crate::games::washoi) fn local_walk(sight: &Sight, from: Vec3, to: Vec3) -> bool {
    let (dx, dz) = (to.x - from.x, to.z - from.z);
    let distance = dx.hypot(dz);
    if !(1.0..=LOCAL_WALK_RADIUS).contains(&distance) || (from.y - to.y).abs() > MAX_HEIGHT_DELTA {
        return false;
    }
    let (sx, sz) = (dz / distance, -dx / distance);
    let steps = (distance / 16.0).ceil() as u32;
    // Sample the horizontal extent and the low and high planes of
    // SHAG's BotVision.WALK_SWEEP_BOUNDS.
    for side in [-15.0, 0.0, 15.0] {
        let from_x = from.x + sx * side;
        let from_z = from.z + sz * side;
        for height in [-6.0, 17.0, 40.0] {
            let y = from.y + height;
            if !sight.clear([from_x, y, from_z], [from_x + dx, y, from_z + dz]) {
                return false;
            }
        }
        for step in 0..=steps {
            let t = f64::from(step) / f64::from(steps);
            let x = from_x + dx * t;
            let z = from_z + dz * t;
            if sight.clear([x, from.y - 6.0, z], [x, from.y - 40.0, z]) {
                return false;
            }
        }
    }
    true
}

fn horizontal_distance(position: Vec3, target: [f64; 3]) -> f64 {
    (target[0] - position.x).hypot(target[2] - position.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_objective_walk_starts_partway_up_a_cornered_stair_edge() {
        // Node 1 sits 128 units above node 0, and the baked walk climbs
        // through two corners. Halfway up, the bot is more than the entry
        // radius from either node and far above node 0, as live S&D
        // carriers were after a fight on `washoi_snd1`'s east stairs.
        let navigation: WashoiNavigation = serde_json::from_str(
            r#"{"map":"stairs","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,0,0],[256,128,0],[400,128,0]],
                "edges":[[0,1,[[96,48,0],[160,80,0]]],[1,2],[2,1],[1,0]],
                "entries":[0],"goals":[2]
            }}"#,
        )
        .unwrap();
        let halfway = Vec3::new(128.0, 64.0, 20.0);
        let walk = navigation
            .objective_walk(halfway, Vec3::new(400.0, 128.0, 0.0), 48.0, None, None)
            .expect("the bot finishes the stair walk and plans on");
        assert_eq!(
            walk.route.waypoints[..2],
            [[160.0, 80.0, 0.0], [256.0, 128.0, 0.0]]
        );
        assert_eq!(walk.route.entry, [160.0, 80.0, 0.0]);
        // Off every walk, it still has nowhere to start.
        assert!(
            navigation
                .objective_walk(
                    Vec3::new(128.0, 64.0, 200.0),
                    Vec3::new(400.0, 128.0, 0.0),
                    48.0,
                    None,
                    None
                )
                .is_none()
        );
    }

    #[test]
    fn objective_walk_releases_sprint_before_a_ledge() {
        let mut walk = ObjectiveWalk {
            route: Route {
                entry: [0.0; 3],
                waypoints: vec![[240.0, 0.0, 0.0], [320.0, -100.0, 0.0]],
                maneuvers: Vec::new(),
                learned_route: None,
                resumed_from: None,
                pickup: None,
            },
            edges: Vec::new(),
            next: 0,
            arrival: ARRIVAL_RADIUS,
        };
        let grounded = Some(Velocity {
            grounded: true,
            ..Velocity::default()
        });
        let start = Vec3::default();
        assert_ne!(walk.steer(start, grounded).buttons & SPRINT, 0);
        assert_eq!(
            walk.steer(Vec3::new(135.0, 0.0, 0.0), grounded).buttons & SPRINT,
            0
        );
        assert_eq!(
            walk.steer(Vec3::new(240.0, 0.0, 0.0), grounded).buttons & SPRINT,
            0
        );
    }

    #[test]
    fn joins_sparse_graphs_to_objectives_with_checked_local_connections() {
        let map: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,0,0],[64,0,0],[0,0,30]],
                "edges":[[0,1],[0,2]],"entries":[0],"goals":[1]
            }}"#,
        )
        .unwrap();
        let planner = map.planner.unwrap();
        let start = Vec3::new(-64.0, 0.0, 0.0);
        let target = Vec3::new(124.0, 0.0, 0.0);
        assert!(
            planner
                .plan_to(start, target, 12.0, None, |_, _| false)
                .is_none()
        );

        let (route, _) = planner
            .plan_to(start, target, 12.0, None, |a, b| {
                // The closer graph node can also reach the target, but its
                // connector makes the complete route longer.
                (a.x, b.x) == (-64.0, 0.0) || (a.x, b.x) == (64.0, 112.0) || a.z == 30.0
            })
            .unwrap();
        assert_eq!(
            route.waypoints,
            [[0.0, 0.0, 0.0], [64.0, 0.0, 0.0], [112.0, 0.0, 0.0]]
        );
    }

    #[test]
    fn baked_graph_uses_the_detour_instead_of_the_blocked_shortcut() {
        let baked = include_str!("../../../../../tests/fixtures/washoi_dm2_walk_graph.json");
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
        let baked = include_str!("../../../../../tests/fixtures/washoi_dm2_cache_graph.json");
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
                    .plan(Vec3::new(x, y, z), None, None, |_| true, |_| 0.0)
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
                None,
                |_| true,
                |route| f64::from(u8::from(route.pickup.is_some())),
            )
            .unwrap();
        assert_eq!(planned.waypoints, [[100.0, 0.0, 0.0]]);
        // A bot left in the dead end has no walk out until it dies.
        let inside = graph.plan(Vec3::new(0.0, 0.0, 100.0), None, None, |_| true, |_| 0.0);
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
            .plan(Vec3::new(0.0, 200.0, 0.0), None, None, |_| true, |_| 0.0)
            .unwrap();
        assert_eq!(down.waypoints, [[40.0, 0.0, 0.0], [200.0, 0.0, 0.0]]);
    }

    #[test]
    fn walks_back_to_a_reached_corner_after_leaving_the_graph() {
        // The walk from 0 to 1 turns at two corners. Live bots that last
        // reached a corner and drifted past the entry radius stood still.
        let graph: WashoiNavigation = serde_json::from_str(
            r#"{
            "map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,0,0],[400,0,400]],
                "edges":[[0,1,[[200,0,0],[400,0,200]]],[1,0]],"entries":[0],"goals":[0,1]
            }
        }"#,
        )
        .unwrap();
        let off = Vec3::new(260.0, 0.0, 60.0);
        let back = graph
            .plan(off, Some([200.0, 0.0, 0.0]), None, |_| true, |_| 0.0)
            .unwrap();
        assert_eq!(
            back.waypoints,
            [
                [200.0, 0.0, 0.0],
                [400.0, 0.0, 200.0],
                [400.0, 0.0, 400.0],
                [0.0, 0.0, 0.0]
            ]
        );
        assert_eq!(back.entry, [200.0, 0.0, 0.0]);
        // A node anchor still leads straight back to it, and an unknown point not at all.
        let node = graph
            .plan(
                Vec3::new(60.0, 0.0, 30.0),
                Some([0.0; 3]),
                None,
                |_| true,
                |_| 0.0,
            )
            .unwrap();
        assert_eq!(node.waypoints[0], [0.0; 3]);
        assert!(
            graph
                .plan(off, Some([1.0; 3]), None, |_| true, |_| 0.0)
                .is_none()
        );
    }

    #[test]
    fn walks_straight_back_to_an_open_node_when_the_anchor_walk_fails() {
        use crate::games::washoi::perception::sight::tests::{cube, encode};
        // Off the graph between two nodes. A wall stands between the bot and
        // node 0; node 1 is farther but in the open. The floor spans both.
        let graph: WashoiNavigation = serde_json::from_str(
            r#"{
            "map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,24,0],[180,24,0],[180,24,300]],
                "edges":[[0,1],[1,0],[1,2],[2,1]],"entries":[0],"goals":[0,2]
            }
        }"#,
        )
        .unwrap();
        let sight = Sight::parse(&encode(
            "test",
            &[
                cube([-200.0, -10.0, -200.0], [400.0, 0.0, 400.0]),
                cube([40.0, 0.0, -50.0], [50.0, 100.0, 50.0]),
            ],
            &[],
        ))
        .unwrap();
        let off = Vec3::new(100.0, 24.0, 0.0);
        let plan = |anchor, sight, accept: &dyn Fn(&Route) -> bool| {
            graph.plan(off, anchor, sight, accept, |_| 0.0)
        };
        // Without sight there is no way back, as before.
        assert!(plan(None, None, &|_| true).is_none());
        let open = plan(None, Some(&sight), &|_| true).unwrap();
        assert_eq!(open.entry, [180.0, 24.0, 0.0]);
        // An anchor comes first, unless its walk was blocked.
        let anchored = plan(Some([0.0, 24.0, 0.0]), Some(&sight), &|_| true).unwrap();
        assert_eq!(anchored.entry, [0.0, 24.0, 0.0]);
        let blocked = |route: &Route| route.entry != [0.0, 24.0, 0.0];
        let around = plan(Some([0.0, 24.0, 0.0]), Some(&sight), &blocked).unwrap();
        assert_eq!(around.entry, [180.0, 24.0, 0.0]);
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

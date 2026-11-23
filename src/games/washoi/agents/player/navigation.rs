pub(in crate::games::washoi) mod clearance;
pub(in crate::games::washoi) mod flying_start;
mod follower;
mod graph;
pub(in crate::games::washoi) mod hop;
pub(in crate::games::washoi) mod learned;
pub(in crate::games::washoi) mod ledge_slide;
pub(in crate::games::washoi) mod liquid;
pub(in crate::games::washoi) mod rollout;
pub(in crate::games::washoi) mod slide;
mod traversal;
pub(in crate::games::washoi) mod tricks;
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
pub(in crate::games::washoi) const LOCAL_WALK_RADIUS: f64 = 256.0;
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
        sprint_on_long_legs(&mut input, &self.route, self.next, position, velocity);
        jump_up_to_waypoint(&mut input, &self.route, self.next, position, velocity);
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
    /// The SHA-256 of the BSP a walk graph was baked from.
    bsp_sha256: Option<String>,
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
        let bsp_sha256 = file
            .graph
            .as_ref()
            .map(|graph| graph.bsp_sha256().to_ascii_lowercase());
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
            bsp_sha256,
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

    /// The SHA-256 of the BSP the walk graph was baked from, in lowercase
    /// hex. None for authored routes.
    pub fn bsp_sha256(&self) -> Option<&str> {
        self.bsp_sha256.as_deref()
    }

    /// The walk graph's node positions, standing origins, in graph index
    /// order and stable for the life of this navigation. Empty for
    /// authored routes.
    /// Each known pickup's ID and standing origin, for tools.
    pub fn pickup_spots(&self) -> Vec<(usize, [f64; 3])> {
        self.pickups()
            .into_iter()
            .map(|(pickup, at)| (pickup.id, at))
            .collect()
    }

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

    /// The walk from `position` to each pickup the bot can reach, in world
    /// units, by pickup ID. It attaches to the graph as roaming does: at a
    /// node, partway along a walk, by a straight walk to a node up to 256
    /// units away, or toward a node in clear view. A pickup missing from the
    /// list has no walk from here.
    pub(in crate::games::washoi) fn pickup_costs(
        &self,
        position: Vec3,
        sight: Option<&Sight>,
    ) -> Vec<(usize, f32)> {
        self.planner.as_ref().map_or_else(Vec::new, |planner| {
            planner.pickup_costs(
                position,
                |from, to| sight.is_some_and(|map| local_walk(map, from, to)),
                |from, to| sight.is_some_and(|map| open_line(map, from, to)),
            )
        })
    }

    /// The view yaw a baked teleporter set when the bot's own position
    /// jumped from `before` to `after` through it.
    pub(in crate::games::washoi) fn teleport_exit_yaw(
        &self,
        before: Vec3,
        after: Vec3,
    ) -> Option<f64> {
        self.planner.as_ref()?.teleport_exit_yaw(before, after)
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
            |from, to| sight.is_some_and(|map| open_line(map, from, to)),
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
        self.planner.as_ref()?.plan(
            position,
            anchor,
            accept,
            score,
            |from, to| sight.is_some_and(|map| local_walk(map, from, to)),
            |from, to| sight.is_some_and(|map| open_line(map, from, to)),
        )
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
                // `followRouteLine`: on a level run of the route, make for
                // the point a little way along it, so a corner turns the
                // walk over the run into it, as a player rounds one.
                match line_ahead(route, *next, position) {
                    Some((passed, _)) if passed > 0 => {
                        *next += passed;
                        continue;
                    }
                    Some((_, ahead))
                        if (ahead[0] - position.x).hypot(ahead[2] - position.z) > 1e-6 =>
                    {
                        let length = (ahead[0] - position.x).hypot(ahead[2] - position.z);
                        (
                            (ahead[0] - position.x) / length,
                            (ahead[2] - position.z) / length,
                        )
                    }
                    _ => (
                        (waypoint[0] - position.x) / distance,
                        (waypoint[2] - position.z) / distance,
                    ),
                }
            } else if drop > MAX_HEIGHT_DELTA {
                // Over the landing but still on the ledge: keep stepping off
                // along the leg. A route that starts at the landing, as one
                // planned from a ledge over its first node does, has no leg
                // in: step off toward the waypoint after it. Without that,
                // an S&D carrier stood above a node for 36 seconds.
                let from = if *next == 0 {
                    route.entry
                } else {
                    waypoints[*next - 1]
                };
                let (mut x, mut z) = (waypoint[0] - from[0], waypoint[2] - from[2]);
                if x.hypot(z) == 0.0
                    && let Some(after) = waypoints.get(*next + 1)
                {
                    (x, z) = (after[0] - waypoint[0], after[2] - waypoint[2]);
                }
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

/// How far along the route line a walk makes for, `ROUTE_LINE_AHEAD` in
/// TypeScript's `BotNavigator`.
const ROUTE_LINE_AHEAD: f64 = 64.0;
/// Waypoints of the level run looked along at most.
const ROUTE_LINE_POINTS: usize = 6;
/// A bot this near its route's first leg is on it.
const ROUTE_LINE_JOIN: f64 = 48.0;

/// Horizontal distance from `at` to the segment from `a` to `b`, and the
/// fraction along it of the nearest point.
fn nearest_on_segment(at: Vec3, a: [f64; 3], b: [f64; 3]) -> (f64, f64) {
    let (dx, dz) = (b[0] - a[0], b[2] - a[2]);
    let length_sq = dx * dx + dz * dz;
    let t = if length_sq < 1.0 {
        1.0
    } else {
        (((at.x - a[0]) * dx + (at.z - a[2]) * dz) / length_sq).clamp(0.0, 1.0)
    };
    ((at.x - (a[0] + dx * t)).hypot(at.z - (a[2] + dz * t)), t)
}

fn segment_distance(at: Vec3, a: [f64; 3], b: [f64; 3]) -> f64 {
    nearest_on_segment(at, a, b).0
}

/// `RouteLine.pointAlong` over the route's level run from the leg into
/// waypoint `next`: the run's points, from the waypoint before (the bot,
/// on the first leg), while each
/// is on the level of the last and reached by walking, no maneuver. Returns
/// how many of its waypoints the bot has passed, its nearest point lying
/// on a later stretch, and the point `ROUTE_LINE_AHEAD` along the run past
/// its nearest point. None off the run's level, where ledges and climbs
/// steer for the waypoint itself.
fn line_ahead(route: &Route, next: usize, position: Vec3) -> Option<(usize, [f64; 3])> {
    let waypoint = *route.waypoints.get(next)?;
    if route.maneuver_at(next).is_some() || (position.y - waypoint[1]).abs() > MAX_HEIGHT_DELTA {
        return None;
    }
    // The first leg runs from the route's entry when the bot is on it, and
    // otherwise from where the bot is, as TypeScript joins a line from the
    // bot: an entry need not be where the bot stands.
    let start = match next {
        0 if segment_distance(position, route.entry, waypoint) > ROUTE_LINE_JOIN => {
            [position.x, position.y, position.z]
        }
        0 => route.entry,
        _ => route.waypoints[next - 1],
    };
    if (start[1] - waypoint[1]).abs() > MAX_HEIGHT_DELTA {
        return None;
    }
    let mut points = vec![start, waypoint];
    for index in next + 1..route.waypoints.len().min(next + ROUTE_LINE_POINTS) {
        let point = route.waypoints[index];
        if route.maneuver_at(index).is_some()
            || (point[1] - points[points.len() - 1][1]).abs() > MAX_HEIGHT_DELTA
        {
            break;
        }
        points.push(point);
    }
    // Nearest point on the first stretches: the line is followed forward.
    let mut best = (0, 0.0, f64::INFINITY);
    for stretch in 0..(points.len() - 1).min(3) {
        let (distance, t) = nearest_on_segment(position, points[stretch], points[stretch + 1]);
        if distance < best.2 {
            best = (stretch, t, distance);
        }
    }
    let (stretch, t, _) = best;
    let lerp = |a: [f64; 3], b: [f64; 3], t: f64| {
        [
            a[0] + (b[0] - a[0]) * t,
            a[1] + (b[1] - a[1]) * t,
            a[2] + (b[2] - a[2]) * t,
        ]
    };
    let mut left = ROUTE_LINE_AHEAD;
    let mut a = lerp(points[stretch], points[stretch + 1], t);
    for &b in &points[stretch + 1..] {
        let length = (b[0] - a[0]).hypot(b[2] - a[2]);
        if length >= left {
            return Some((stretch, lerp(a, b, left / length)));
        }
        left -= length;
        a = b;
    }
    Some((stretch, points[points.len() - 1]))
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

/// Sprint while walking, as TypeScript bots sprint whenever they move
/// (`StateMachineBrain._moveToward`). Washoi gives 400 u/s only to sprint with
/// forward alone, and 320 otherwise. Within 128 units of a drop, at the next
/// waypoint or the leg after it, the bot runs instead, so it doesn't overshoot
/// the ledge. Live, sprint was missing from 20% of the Rust bot's moving
/// commands while its keys were forward alone, on short or sloping legs.
fn sprint_on_long_legs(
    input: &mut PlayerInput,
    route: &Route,
    next: usize,
    position: Vec3,
    velocity: Option<Velocity>,
) {
    let Some(&waypoint) = route.waypoints.get(next) else {
        return;
    };
    let drops = |from: f64, to: [f64; 3]| from - to[1] > SPRINT_HEIGHT_DELTA;
    let near_ledge = horizontal_distance(position, waypoint) < SPRINT_LEG_DISTANCE
        && (drops(position.y, waypoint)
            || route
                .waypoints
                .get(next + 1)
                .is_some_and(|&after| drops(waypoint[1], after)));
    if input.direction != [0.0; 3]
        && velocity.is_some_and(|velocity| velocity.grounded)
        && route.maneuver_at(next).is_none()
        && !near_ledge
    {
        input.buttons |= SPRINT;
    }
}

// Washoi `shared/server/simulation/physics/constants.ts` STEP_HEIGHT is 18:
// a rise past this is no step.
const JUMP_UP_RISE: f64 = 20.0;
// A standing jump clears about 45, and the landing steps up 18 more.
const JUMP_UP_MAX: f64 = 64.0;
const JUMP_UP_RADIUS: f64 = 96.0;
// Held at the lip: a bot running up stairs is faster than this.
const JUMP_UP_SPEED: f64 = 160.0;
const JUMP: u16 = 1 << 4;

/// Jump onto a waypoint above step height that the walk is held under, as
/// TypeScript `BotNavigator` jumps a "jump" step when grounded within 180 of
/// it. The bake keeps unvalidated jump links out of the graph, but a walk
/// link round corners can still climb a lip: on snd1 every way west from
/// site 1 meets the platform 39 above the floor, and walks stood at its
/// face. Only up: never on a drop, and not under a recorded maneuver.
fn jump_up_to_waypoint(
    input: &mut PlayerInput,
    route: &Route,
    next: usize,
    position: Vec3,
    velocity: Option<Velocity>,
) {
    let Some(&waypoint) = route.waypoints.get(next) else {
        return;
    };
    let rise = waypoint[1] - position.y;
    let held = velocity.is_some_and(|velocity| {
        velocity.grounded && velocity.linear.x.hypot(velocity.linear.z) < JUMP_UP_SPEED
    });
    if input.direction != [0.0; 3]
        && held
        && route.maneuver_at(next).is_none()
        && rise > JUMP_UP_RISE
        && rise <= JUMP_UP_MAX
        && horizontal_distance(position, waypoint) <= JUMP_UP_RADIUS
    {
        input.buttons |= JUMP;
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

/// Nothing static in the way of a straight walk toward `to` at knee and
/// chest height, held at the bot's own height, with ground under every 32
/// units of it no lower than 64 below the lower end. A walk that steps off a
/// ledge toward a lower point passes, and one over a pit does not.
pub(in crate::games::washoi) fn open_line(sight: &Sight, from: Vec3, to: Vec3) -> bool {
    let clear = [17.0, 40.0].into_iter().all(|height| {
        sight.clear(
            [from.x, from.y + height, from.z],
            [to.x, from.y + height, to.z],
        )
    });
    let (dx, dz) = (to.x - from.x, to.z - from.z);
    let steps = (dx.hypot(dz) / 32.0).ceil().max(1.0) as u32;
    let lowest = from.y.min(to.y) - 64.0;
    clear
        && (1..=steps).all(|step| {
            let t = f64::from(step) / f64::from(steps);
            let (x, z) = (from.x + dx * t, from.z + dz * t);
            !sight.clear([x, from.y - 6.0, z], [x, lowest, z])
        })
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
                "edges":[[0,1,[[96,24,0],[160,56,0]]],[1,2],[2,1],[1,0]],
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
    fn an_objective_walk_partway_along_a_two_way_link_heads_for_the_target_end() {
        // A ramp from A at the top to B at the bottom, its way back up baked
        // 40 units to one side, with D past A and C past B. Partway down,
        // the bot stands on both edges. It walks back up toward D and down
        // toward C instead of finishing whichever edge is nearer first.
        let navigation: WashoiNavigation = serde_json::from_str(
            r#"{"map":"ramp","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,100,0],[400,0,0],[800,0,0],[-400,100,0]],
                "edges":[[0,1,[[100,90,0]]],[1,0,[[400,0,40],[200,50,40]]],
                         [1,2],[2,1],[0,3],[3,0]],
                "entries":[2],"goals":[2,3]
            }}"#,
        )
        .unwrap();
        let partway = Vec3::new(200.0, 70.0, 0.0);
        let up = navigation
            .objective_walk(partway, Vec3::new(-400.0, 100.0, 0.0), 48.0, None, None)
            .unwrap();
        assert_eq!(up.route.waypoints[0], [0.0, 100.0, 0.0]);
        assert!(up.route.waypoints.iter().all(|point| point[0] < 400.0));
        let down = navigation
            .objective_walk(partway, Vec3::new(800.0, 0.0, 0.0), 48.0, None, None)
            .unwrap();
        assert_eq!(down.route.waypoints[0], [400.0, 0.0, 0.0]);
        assert!(down.route.waypoints.iter().all(|point| point[0] > 0.0));
    }

    /// A level corner is rounded: 40 units before it the walk already
    /// bends toward the next leg, past it the corner counts as passed, and
    /// a leg up a step steers for its waypoint.
    #[test]
    fn a_walk_rounds_level_corners_along_the_route_line() {
        let route = Route {
            entry: [0.0; 3],
            waypoints: vec![[200.0, 0.0, 0.0], [200.0, 0.0, 200.0], [200.0, 80.0, 400.0]],
            maneuvers: Vec::new(),
            learned_route: None,
            resumed_from: None,
            pickup: None,
        };
        let (passed, ahead) = line_ahead(&route, 0, Vec3::new(160.0, 0.0, 0.0)).unwrap();
        assert_eq!(passed, 0);
        assert!(
            (ahead[0] - 200.0).abs() < 1e-9 && (ahead[2] - 24.0).abs() < 1e-9,
            "{ahead:?}"
        );
        let mut next = 0;
        let input = WashoiNavigation::steer_with_arrival(
            &route,
            &mut next,
            Vec3::new(160.0, 0.0, 0.0),
            None,
            ARRIVAL_RADIUS,
        );
        assert_eq!(next, 0);
        assert!(input.direction[2] > 0.3, "{:?}", input.direction);
        // Cut inside the corner, nearest the second leg: the corner is passed.
        let mut next = 0;
        WashoiNavigation::steer_with_arrival(
            &route,
            &mut next,
            Vec3::new(190.0, 0.0, 30.0),
            None,
            ARRIVAL_RADIUS,
        );
        assert_eq!(next, 1);
        // The leg up to the step's top is not on the line.
        let (_, ahead) = line_ahead(&route, 1, Vec3::new(200.0, 0.0, 180.0)).unwrap();
        assert_eq!(ahead, [200.0, 0.0, 200.0]);
        assert_eq!(line_ahead(&route, 2, Vec3::new(200.0, 0.0, 210.0)), None);
    }

    #[test]
    fn sprints_everywhere_but_the_approach_to_a_drop() {
        let route = Route {
            entry: [0.0; 3],
            // Level, a short level leg, a climb, then a drop of 100.
            waypoints: vec![
                [300.0, 0.0, 0.0],
                [360.0, 0.0, 0.0],
                [360.0, 80.0, 300.0],
                [360.0, -20.0, 600.0],
            ],
            maneuvers: Vec::new(),
            learned_route: None,
            resumed_from: None,
            pickup: None,
        };
        let grounded = Some(Velocity {
            grounded: true,
            ..Velocity::default()
        });
        let sprints = |next: usize, at: [f64; 3], velocity: Option<Velocity>| {
            let mut input = PlayerInput {
                direction: [1.0, 0.0, 0.0],
                ..PlayerInput::default()
            };
            sprint_on_long_legs(
                &mut input,
                &route,
                next,
                Vec3::new(at[0], at[1], at[2]),
                velocity,
            );
            input.buttons & SPRINT != 0
        };
        assert!(sprints(0, [0.0, 0.0, 0.0], grounded));
        assert!(sprints(1, [320.0, 0.0, 0.0], grounded));
        assert!(sprints(2, [360.0, 0.0, 100.0], grounded));
        // Within 128 of the climb's top, whose next leg drops: run.
        assert!(!sprints(2, [360.0, 60.0, 250.0], grounded));
        assert!(sprints(2, [360.0, 30.0, 150.0], grounded));
        // In the air, no sprint.
        assert!(!sprints(0, [0.0, 0.0, 0.0], None));
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
    fn walk_corners_are_lifted_to_standing_height_and_a_lip_is_jumped() {
        // snd1's link from node 66 on the platform down to node 16 on the
        // floor, as baked: nodes at standing height, corners on the floor.
        let map: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[1584,64,48],[1736,-5,64]],
                "edges":[[0,1,[[1648,9.3,48],[1680,-30.7,48]]],[1,0,[[1662.6,9.3,9.4]]]],
                "entries":[0,1],"goals":[0,1]
            }}"#,
        )
        .unwrap();
        let down = map
            .objective_walk(
                Vec3::new(1584.0, 64.0, 48.0),
                Vec3::new(1736.0, -5.0, 64.0),
                16.0,
                None,
                None,
            )
            .unwrap();
        // The corner at the foot is on node 16's level, not 24 below it.
        let foot = down
            .route
            .waypoints
            .iter()
            .find(|p| p[0] == 1680.0)
            .unwrap();
        assert!((foot[1] - -6.7).abs() < 1e-9);
        let up = map
            .objective_walk(
                Vec3::new(1736.0, -5.0, 64.0),
                Vec3::new(1584.0, 64.0, 48.0),
                16.0,
                None,
                None,
            )
            .unwrap();
        let lip = up
            .route
            .waypoints
            .iter()
            .position(|p| p[0] == 1662.6)
            .unwrap();
        assert!((up.route.waypoints[lip][1] - 33.3).abs() < 1e-9);

        let held = |speed: f64| {
            Some(Velocity {
                linear: Vec3::new(speed, 0.0, 0.0),
                grounded: true,
                ..Velocity::default()
            })
        };
        let jumps = |next: usize, at: Vec3, velocity: Option<Velocity>| {
            let mut input = PlayerInput {
                direction: [0.0, 0.0, -1.0],
                ..PlayerInput::default()
            };
            jump_up_to_waypoint(&mut input, &up.route, next, at, velocity);
            input.buttons & JUMP != 0
        };
        // Held under the lip, 39 below it: jump.
        let under = Vec3::new(1664.0, -6.0, 48.0);
        assert!(jumps(lip, under, held(0.0)));
        // Running up to it, or in the air, or far off: no jump yet.
        assert!(!jumps(lip, under, held(300.0)));
        assert!(!jumps(lip, under, None));
        assert!(!jumps(lip, Vec3::new(1664.0, -6.0, 200.0), held(0.0)));
        // A step, or a rise no jump clears: no jump.
        assert!(!jumps(lip, Vec3::new(1664.0, 20.0, 48.0), held(0.0)));
        assert!(!jumps(lip, Vec3::new(1664.0, -40.0, 48.0), held(0.0)));
        // Down a ledge, never.
        let foot_at = down
            .route
            .waypoints
            .iter()
            .position(|p| p[0] == 1680.0)
            .unwrap();
        let mut input = PlayerInput {
            direction: [1.0, 0.0, 0.0],
            ..PlayerInput::default()
        };
        jump_up_to_waypoint(
            &mut input,
            &down.route,
            foot_at,
            Vec3::new(1650.0, 33.0, 48.0),
            held(0.0),
        );
        assert_eq!(input.buttons & JUMP, 0);
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
                .plan_to(start, target, 12.0, None, |_, _| false, |_, _| false)
                .is_none()
        );

        let (route, _) = planner
            .plan_to(
                start,
                target,
                12.0,
                None,
                |a, b| {
                    // The closer graph node can also reach the target, but its
                    // connector makes the complete route longer.
                    (a.x, b.x) == (-64.0, 0.0) || (a.x, b.x) == (64.0, 112.0) || a.z == 30.0
                },
                |_, _| false,
            )
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

        // Planned from a ledge right over its first node, the route has no
        // leg in to step off along: it steps off toward the next waypoint.
        let from_above: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","routes":[
                {"entry":[0,0,0],"waypoints":[[0,0,0],[0,0,-400]]}
            ]}"#,
        )
        .unwrap();
        let mut next = 0;
        let input = WashoiNavigation::steer(
            &from_above.routes[0],
            &mut next,
            Vec3::new(4.0, 86.0, 0.0),
            Some(moving(0.0, true)),
        );
        assert_eq!((input.direction, next), ([0.0, 0.0, -1.0], 0));
    }

    #[test]
    fn a_roaming_bot_walks_to_a_node_up_to_256_units_away() {
        // A `washoi_dm2` spawn sits 195 to 234 units from every node at its
        // height. Roaming attached only within 128 units, so live bots
        // spawned there stood still for the rest of the match.
        let graph: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,0,0],[100,0,0]],"edges":[[0,1],[1,0]],"entries":[0],"goals":[1]
            }}"#,
        )
        .unwrap();
        let planner = graph.planner.as_ref().unwrap();
        let plan = |x: f64| {
            planner.plan(
                Vec3::new(x, 0.0, 0.0),
                None,
                |_| true,
                |_| 0.0,
                |_, _| true,
                |_, _| false,
            )
        };
        assert_eq!(
            plan(-200.0).unwrap().waypoints,
            [[0.0; 3], [100.0, 0.0, 0.0]]
        );
        assert!(plan(-300.0).is_none());
    }

    #[test]
    fn prices_the_walk_to_every_pickup_from_where_the_bot_attaches() {
        let graph: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,0,0],[100,0,0],[200,0,0]],
                "edges":[[0,1],[1,0],[1,2],[2,1]],"entries":[0],"goals":[1,2],
                "pickup_goals":[[2,{"id":5,"kind":"powerup"}]]
            }}"#,
        )
        .unwrap();
        let planner = graph.planner.as_ref().unwrap();
        let costs =
            |x: f64, z: f64| planner.pickup_costs(Vec3::new(x, 0.0, z), |_, _| true, |_, _| false);
        // Beside node 0: its walk plus the step to it.
        let [(id, cost)] = costs(10.0, 0.0)[..] else {
            panic!("one pickup");
        };
        assert_eq!(id, 5);
        assert!((cost - 210.0).abs() < 1e-3);
        // Between nodes, partway along the walk from 1 to 2: the rest of it.
        let [(_, cost)] = costs(150.0, 20.0)[..] else {
            panic!("one pickup");
        };
        assert!((cost - 50.0f32.hypot(20.0)).abs() < 1e-3);
        // Off the graph but within a straight walk of node 0.
        let [(_, cost)] = costs(-200.0, 0.0)[..] else {
            panic!("one pickup");
        };
        assert!((cost - 400.0).abs() < 1e-3);
        assert!(costs(10.0, 900.0).is_empty());
        // Farther than any straight walk, a node in clear view still prices.
        let far = planner.pickup_costs(Vec3::new(-600.0, 0.0, 0.0), |_, _| false, |_, _| true);
        assert_eq!(far.len(), 1);
        assert!((far[0].1 - 800.0).abs() < 1e-3);
    }

    #[test]
    fn a_jump_out_of_a_teleporter_reports_its_exit_yaw() {
        let graph: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,24,0],[80,24,160],[1000,24,500]],
                "edges":[[0,1],[1,0],[2,0]],"entries":[0],"goals":[1,2],
                "maneuvers":[[1,2,{"kind":"teleporter","min":[72,0,152],"max":[88,56,168],
                    "exit":[1000,24,500],"yaw":1.5}]]
            }}"#,
        )
        .unwrap();
        let entry = Vec3::new(80.0, 24.0, 160.0);
        let teleported = Vec3::new(1040.0, 24.0, 520.0);
        assert_eq!(graph.teleport_exit_yaw(entry, teleported), Some(1.5));
        // An ordinary step, or a long move that lands away from any exit.
        assert_eq!(
            graph.teleport_exit_yaw(entry, Vec3::new(160.0, 24.0, 160.0)),
            None
        );
        assert_eq!(
            graph.teleport_exit_yaw(entry, Vec3::new(600.0, 24.0, 500.0)),
            None
        );
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
                "edges":[[0,1,[[200,-24,0],[400,-24,200]]],[1,0]],"entries":[0],"goals":[0,1]
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
        // A node anchor still leads straight back to it, and an unknown
        // point away from every walk not at all.
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
                .plan(
                    Vec3::new(260.0, 0.0, 160.0),
                    Some([1.0; 3]),
                    None,
                    |_| true,
                    |_| 0.0
                )
                .is_none()
        );
    }

    #[test]
    fn an_open_line_may_drop_off_a_ledge_but_not_into_a_pit() {
        use crate::games::washoi::perception::sight::tests::{cube, encode};
        // A ledge at height 100 from x = -100 to 0, floor at 0 from 0 to 300,
        // and a pit from 300 to 400 before more floor.
        let sight = Sight::parse(&encode(
            "test",
            &[
                cube([-100.0, 90.0, -50.0], [0.0, 100.0, 50.0]),
                cube([0.0, -10.0, -50.0], [300.0, 0.0, 50.0]),
                cube([400.0, -10.0, -50.0], [600.0, 0.0, 50.0]),
            ],
            &[],
        ))
        .unwrap();
        // Standing origins sit 24 above the floor.
        let ledge = Vec3::new(-50.0, 124.0, 0.0);
        assert!(open_line(&sight, ledge, Vec3::new(250.0, 24.0, 0.0)));
        assert!(!open_line(&sight, ledge, Vec3::new(500.0, 24.0, 0.0)));
        assert!(!open_line(
            &sight,
            Vec3::new(100.0, 24.0, 0.0),
            Vec3::new(500.0, 24.0, 0.0)
        ));
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
        // Without sight, the bot on the baked walk from 0 to 1 finishes it.
        assert_eq!(
            plan(None, None, &|_| true).unwrap().entry,
            [180.0, 24.0, 0.0]
        );
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
            // 64 units along the line: round the corner, to (40, 0, 24).
            [40.0 / 40f64.hypot(24.0), 0.0, 24.0 / 40f64.hypot(24.0)]
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

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap},
};

use serde::Deserialize;

use shagger_shared::net::game_state::Vec3;

use super::{
    ANCHOR_RADIUS, ARRIVAL_RADIUS, ENTRY_RADIUS, MAX_HEIGHT_DELTA, ObjectiveEdge,
    ObjectiveRecovery, PickupGoal, Route, horizontal_distance,
};

const MAX_NODES: usize = 1024;
const MAX_ENTRIES: usize = 64;
const MAX_EDGES: usize = 32768;
const ROUTES_PER_ENTRY: usize = 3;
const MAX_CORNERS: usize = 32;

/// Intermediate points of a walk edge, from the TypeScript nav cache `via`.
type Corners = BTreeMap<(usize, usize), Vec<[f64; 3]>>;

/// `[from, to]`, or `[from, to, corners]` for a walk that turns corners.
#[derive(Deserialize)]
#[serde(untagged)]
enum Edge {
    Straight([usize; 2]),
    Cornered(usize, usize, Vec<[f64; 3]>),
}

#[derive(Deserialize)]
pub(super) struct WalkGraph {
    bsp_sha256: String,
    nodes: Vec<[f64; 3]>,
    edges: Vec<Edge>,
    entries: Vec<usize>,
    goals: Vec<usize>,
    #[serde(default)]
    learned_goals: Vec<(usize, String)>,
    #[serde(default)]
    learned_landings: Vec<(usize, String)>,
    #[serde(default)]
    pickup_goals: Vec<(usize, PickupGoal)>,
}

/// Shortest walks to every ordinary goal from every node, computed once and
/// shared by all bots. This lets a bot replan from any graph node instead of
/// only from the entries with precomputed routes.
pub(super) struct Planner {
    nodes: Vec<[f64; 3]>,
    neighbors: Vec<Vec<usize>>,
    corners: Corners,
    goals: Vec<PlannedGoal>,
    areas: Areas,
}

struct PlannedGoal {
    node: usize,
    pickup: Option<PickupGoal>,
    /// The next node toward this goal from each node, if it is reachable.
    toward: Vec<Option<u16>>,
    distance: Vec<f32>,
}

struct Search {
    queue: BinaryHeap<Visit>,
    distances: Vec<f64>,
    predecessor: Vec<Option<usize>>,
}

impl Search {
    fn new(count: usize, start: usize) -> Self {
        let mut distances = vec![f64::INFINITY; count];
        distances[start] = 0.0;
        Self {
            queue: BinaryHeap::from([Visit {
                node: start,
                cost: 0.0,
            }]),
            distances,
            predecessor: vec![None; count],
        }
    }

    fn next(&mut self) -> Option<usize> {
        while let Some(visit) = self.queue.pop() {
            if visit.cost == self.distances[visit.node] {
                return Some(visit.node);
            }
        }
        None
    }

    fn extend(&mut self, from: usize, to: usize, travel: f64) {
        let cost = self.distances[from] + travel;
        if cost < self.distances[to] {
            self.distances[to] = cost;
            self.predecessor[to] = Some(from);
            self.queue.push(Visit { node: to, cost });
        }
    }
}

struct Visit {
    node: usize,
    cost: f64,
}

impl Ord for Visit {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .total_cmp(&self.cost)
            .then_with(|| other.node.cmp(&self.node))
    }
}

impl PartialOrd for Visit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Visit {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Visit {}

impl Planner {
    fn nearest_entry(&self, position: Vec3) -> Option<usize> {
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(node, &point)| {
                let distance = horizontal_distance(position, point);
                (distance <= ENTRY_RADIUS && (point[1] - position.y).abs() <= MAX_HEIGHT_DELTA)
                    .then_some((node, distance))
            })
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(node, _)| node)
    }

    pub(super) fn plan_to(
        &self,
        position: Vec3,
        target: Vec3,
        radius: f64,
        recovery: Option<ObjectiveRecovery>,
        local_walk: impl Fn(Vec3, Vec3) -> bool,
    ) -> Option<(Route, Vec<ObjectiveEdge>)> {
        let start = self
            .nearest_entry(position)
            .or_else(|| {
                let node = recovery?.anchor?;
                let point = *self.nodes.get(node)?;
                (horizontal_distance(position, point) <= ANCHOR_RADIUS
                    && (point[1] - position.y).abs() <= MAX_HEIGHT_DELTA)
                    .then_some(node)
            })
            .or_else(|| {
                self.nodes
                    .iter()
                    .enumerate()
                    .filter(|&(_, &[x, y, z])| local_walk(position, Vec3::new(x, y, z)))
                    .min_by(|(_, a), (_, b)| {
                        horizontal_distance(position, **a)
                            .total_cmp(&horizontal_distance(position, **b))
                    })
                    .map(|(node, _)| node)
            })?;
        let mut search = Search::new(self.nodes.len(), start);
        let mut finish: Option<(usize, f64, Option<[f64; 3]>)> = None;
        while let Some(node) = search.next() {
            let travelled = search.distances[node];
            if finish.is_some_and(|(_, total, _)| travelled >= total) {
                break;
            }
            let [x, y, z] = self.nodes[node];
            let distance = (x - target.x).hypot(z - target.z);
            if (y - target.y).abs() <= MAX_HEIGHT_DELTA {
                if distance <= radius {
                    finish = Some((node, travelled, None));
                    break;
                }
                // Connect to the inside edge of the arrival area. Include
                // this leg in the cost so a shorter graph path cannot win
                // with a longer overall walk.
                let tail = (distance - radius).max(1.0);
                let total = travelled + tail;
                if finish.is_none_or(|(_, best, _)| total < best) {
                    let t = tail / distance;
                    let end = Vec3::new(x + (target.x - x) * t, y, z + (target.z - z) * t);
                    if local_walk(Vec3::new(x, y, z), end) {
                        finish = Some((node, total, Some([end.x, end.y, end.z])));
                    }
                }
            }
            for &next in &self.neighbors[node] {
                if recovery.and_then(|edge| edge.avoid) == Some((node, next)) {
                    continue;
                }
                search.extend(
                    node,
                    next,
                    edge_cost(&self.nodes, &self.corners, node, next),
                );
            }
        }
        let (node, _, tail) = finish?;
        let mut path = vec![node];
        let mut current = node;
        while let Some(previous) = search.predecessor[current] {
            path.push(previous);
            current = previous;
        }
        path.reverse();
        let mut waypoints = vec![self.nodes[start]];
        let mut edges = Vec::with_capacity(path.len() - 1);
        for pair in path.windows(2) {
            walk_edge(&mut waypoints, &self.nodes, &self.corners, pair[0], pair[1]);
            edges.push(ObjectiveEdge {
                nodes: (pair[0], pair[1]),
                end: waypoints.len() - 1,
            });
        }
        waypoints.extend(tail);
        Some((
            Route {
                entry: self.nodes[start],
                waypoints,
                learned_route: None,
                resumed_from: None,
                pickup: None,
            },
            edges,
        ))
    }

    /// The best accepted walk from the node nearest `position` to a goal
    /// the bot isn't already standing on. Ties go to the shorter walk.
    ///
    /// Off the graph, the bot can start from `anchor`, the last node it
    /// reached, and walks back to it first. It got here from there.
    /// Where a bot that left the graph walks back to: the last waypoint it
    /// reached. That is a node, or a corner of the walk between two nodes,
    /// in which case the bot finishes that walk to its end node.
    fn resume_at(&self, anchor: [f64; 3]) -> Option<(usize, Vec<[f64; 3]>)> {
        if let Some(node) = self.nodes.iter().position(|&node| node == anchor) {
            return Some((node, vec![anchor]));
        }
        self.corners.iter().find_map(|(&(_, to), corners)| {
            let at = corners.iter().position(|&corner| corner == anchor)?;
            let mut lead = corners[at..].to_vec();
            lead.push(self.nodes[to]);
            Some((to, lead))
        })
    }

    pub(super) fn plan(
        &self,
        position: Vec3,
        anchor: Option<[f64; 3]>,
        accept: impl Fn(&Route) -> bool,
        score: impl Fn(&Route) -> f64,
    ) -> Option<Route> {
        let here = |node: [f64; 3], radius: f64| {
            horizontal_distance(position, node) <= radius
                && (node[1] - position.y).abs() <= MAX_HEIGHT_DELTA
        };
        let (start, lead) = match self.nearest_entry(position) {
            Some(node) => (node, Vec::new()),
            None => self.resume_at(anchor.filter(|&anchor| here(anchor, ANCHOR_RADIUS))?)?,
        };
        let mut best: Option<(f64, f32, Route)> = None;
        for goal in &self.goals {
            let distance = goal.distance[start];
            if goal.node == start
                || !self.areas.allows(start, goal.node)
                || !distance.is_finite()
                || here(self.nodes[goal.node], ARRIVAL_RADIUS)
            {
                continue;
            }
            let mut waypoints = lead.clone();
            let mut node = start;
            while let Some(next) = goal.toward[node] {
                let next = usize::from(next);
                walk_edge(&mut waypoints, &self.nodes, &self.corners, node, next);
                node = next;
            }
            let route = Route {
                entry: lead.first().copied().unwrap_or(self.nodes[start]),
                waypoints,
                learned_route: None,
                resumed_from: None,
                pickup: goal.pickup,
            };
            if !accept(&route) {
                continue;
            }
            let value = score(&route);
            if best.as_ref().is_none_or(|(best_value, best_distance, _)| {
                value > *best_value || (value == *best_value && distance < *best_distance)
            }) {
                best = Some((value, distance, route));
            }
        }
        best.map(|(_, _, route)| route)
    }
}

impl WalkGraph {
    pub(super) fn routes(self) -> Result<(Vec<Route>, Planner), &'static str> {
        let count = self.nodes.len();
        if !(2..=MAX_NODES).contains(&count)
            || !(1..=MAX_ENTRIES).contains(&self.entries.len())
            || self.goals.is_empty()
            || self.goals.len() > count
            || self.edges.len() > MAX_EDGES
            || self.bsp_sha256.len() != 64
            || !self.bsp_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !self.nodes.iter().flatten().all(|value| value.is_finite())
            || self
                .entries
                .iter()
                .chain(&self.goals)
                .any(|&node| node >= count)
        {
            return Err("invalid navigation graph nodes, anchors or BSP hash");
        }
        if self.learned_landings.len() > MAX_ENTRIES
            || self.learned_landings.iter().any(|(node, id)| {
                *node >= count || !self.learned_goals.iter().any(|(_, goal)| goal == id)
            })
        {
            return Err("invalid learned landing anchor");
        }
        let mut neighbors = vec![Vec::new(); count];
        let mut pickups = vec![None; count];
        for (goal, pickup) in &self.pickup_goals {
            if !self.goals.contains(goal) || pickups[*goal].replace(*pickup).is_some() {
                return Err("invalid or duplicate pickup goal");
            }
        }
        let mut learned = vec![None; count];
        for (goal, id) in &self.learned_goals {
            if !self.goals.contains(goal) || id.trim().is_empty() || learned[*goal].is_some() {
                return Err("invalid or duplicate learned goal");
            }
            learned[*goal] = Some(id.as_str());
        }
        let mut corners = Corners::new();
        for edge in &self.edges {
            let (from, to, via) = match edge {
                Edge::Straight([from, to]) => (*from, *to, None),
                Edge::Cornered(from, to, via) => (*from, *to, Some(via)),
            };
            if from >= count
                || to >= count
                || from == to
                || via.is_some_and(|via| {
                    via.len() > MAX_CORNERS || !via.iter().flatten().all(|value| value.is_finite())
                })
            {
                return Err("invalid navigation graph edge");
            }
            neighbors[from].push(to);
            if let Some(via) = via.filter(|via| !via.is_empty()) {
                // The first duplicate edge decides its corners.
                corners.entry((from, to)).or_insert_with(|| via.clone());
            }
        }
        for adjacent in &mut neighbors {
            adjacent.sort_unstable();
            adjacent.dedup();
        }

        let mut reverse = vec![Vec::new(); count];
        for (from, adjacent) in neighbors.iter().enumerate() {
            for &to in adjacent {
                reverse[to].push(from);
            }
        }
        let areas = Areas::new(components(&neighbors, &reverse), &self.goals, &learned);
        let routes: Vec<_> = self
            .entries
            .iter()
            .map(|&entry| (entry, None))
            .chain(
                self.learned_landings
                    .iter()
                    .map(|(entry, id)| (*entry, Some(id.as_str()))),
            )
            .flat_map(|(entry, resumed_from)| {
                let walk = Walk {
                    neighbors: &neighbors,
                    corners: &corners,
                    areas: &areas,
                    learned: &learned,
                    pickups: &pickups,
                };
                self.routes_from(entry, walk, resumed_from)
            })
            .collect();
        if routes.is_empty() {
            return Err("navigation graph has no reachable goals");
        }
        let mut goals: Vec<_> = self
            .goals
            .iter()
            .copied()
            .filter(|&goal| learned[goal].is_none())
            .collect();
        goals.sort_unstable();
        goals.dedup();
        let goals = goals
            .into_iter()
            .map(|node| self.walks_to(node, &reverse, &corners, pickups[node]))
            .collect();
        Ok((
            routes,
            Planner {
                nodes: self.nodes,
                neighbors,
                corners,
                goals,
                areas,
            },
        ))
    }

    /// Dijkstra over reversed edges, so each node learns its next hop toward `goal`.
    fn walks_to(
        &self,
        goal: usize,
        reverse: &[Vec<usize>],
        corners: &Corners,
        pickup: Option<PickupGoal>,
    ) -> PlannedGoal {
        let mut search = Search::new(self.nodes.len(), goal);
        while let Some(node) = search.next() {
            for &previous in &reverse[node] {
                search.extend(
                    node,
                    previous,
                    edge_cost(&self.nodes, corners, previous, node),
                );
            }
        }
        PlannedGoal {
            node: goal,
            pickup,
            // MAX_NODES keeps node indices within u16.
            toward: search
                .predecessor
                .into_iter()
                .map(|node| node.map(|node| node as u16))
                .collect(),
            distance: search
                .distances
                .into_iter()
                .map(|value| value as f32)
                .collect(),
        }
    }

    fn routes_from(&self, entry: usize, walk: Walk<'_>, resumed_from: Option<&str>) -> Vec<Route> {
        let Walk {
            neighbors,
            corners,
            areas,
            learned,
            pickups,
        } = walk;
        let mut routes = Vec::new();
        let mut search = Search::new(self.nodes.len(), entry);
        while let Some(node) = search.next() {
            let is_learned = learned[node].is_some();
            let category_full = if is_learned {
                resumed_from.is_some()
                    || routes
                        .iter()
                        .any(|route: &Route| route.learned_route.is_some())
            } else {
                routes
                    .iter()
                    .filter(|route: &&Route| route.learned_route.is_none())
                    .count()
                    >= ROUTES_PER_ENTRY
            };
            // Learned takeoffs leave by their recording instead.
            let allowed = is_learned || areas.allows(entry, node);
            if node != entry && self.goals.contains(&node) && allowed && !category_full {
                let mut path = vec![node];
                while let Some(previous) =
                    search.predecessor[*path.last().expect("path has a node")]
                {
                    path.push(previous);
                }
                path.reverse();
                let mut waypoints = Vec::new();
                for pair in path.windows(2) {
                    walk_edge(&mut waypoints, &self.nodes, corners, pair[0], pair[1]);
                }
                routes.push(Route {
                    entry: self.nodes[entry],
                    waypoints,
                    learned_route: learned[node].map(str::to_owned),
                    resumed_from: resumed_from.map(str::to_owned),
                    pickup: pickups[node],
                });
                if routes.len() == ROUTES_PER_ENTRY + usize::from(resumed_from.is_none()) {
                    break;
                }
            }
            for &next in &neighbors[node] {
                search.extend(node, next, edge_cost(&self.nodes, corners, node, next));
            }
        }
        routes
    }
}

#[derive(Clone, Copy)]
struct Walk<'a> {
    neighbors: &'a [Vec<usize>],
    corners: &'a Corners,
    areas: &'a Areas,
    learned: &'a [Option<&'a str>],
    pickups: &'a [Option<PickupGoal>],
}

/// Walkable areas: nodes that can walk to each other.
struct Areas {
    component: Vec<usize>,
    goals: Vec<usize>,
}

impl Areas {
    fn new(component: Vec<usize>, goals: &[usize], learned: &[Option<&str>]) -> Self {
        let mut counts = vec![0; component.len()];
        let mut ordinary: Vec<_> = goals
            .iter()
            .filter(|&&goal| learned[goal].is_none())
            .collect();
        ordinary.sort_unstable();
        ordinary.dedup();
        for &goal in ordinary {
            counts[component[goal]] += 1;
        }
        Self {
            component,
            goals: counts,
        }
    }

    /// A walk stays within the bot's area, or leaves it one way for an
    /// area with more goals. That keeps a bot out of one-way pockets and
    /// lets it drop out of a small area into the main one.
    fn allows(&self, from: usize, to: usize) -> bool {
        let (from, to) = (self.component[from], self.component[to]);
        from == to || self.goals[to] > self.goals[from]
    }
}

/// Strongly connected components, by Kosaraju's algorithm. The cache
/// importer keeps only straight walk edges, so a bot can walk into a pocket
/// whose exits are jumps or drops. Two nodes share a component when each
/// can walk to the other.
fn components(neighbors: &[Vec<usize>], reverse: &[Vec<usize>]) -> Vec<usize> {
    let count = neighbors.len();
    let mut order = Vec::with_capacity(count);
    let mut visited = vec![false; count];
    for root in 0..count {
        if visited[root] {
            continue;
        }
        visited[root] = true;
        let mut stack = vec![(root, 0)];
        while let Some((node, edge)) = stack.last_mut() {
            if let Some(&next) = neighbors[*node].get(*edge) {
                *edge += 1;
                if !visited[next] {
                    visited[next] = true;
                    stack.push((next, 0));
                }
            } else {
                order.push(*node);
                stack.pop();
            }
        }
    }
    let mut component = vec![usize::MAX; count];
    for (id, &root) in order.iter().rev().enumerate() {
        if component[root] != usize::MAX {
            continue;
        }
        component[root] = id;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            for &previous in &reverse[node] {
                if component[previous] == usize::MAX {
                    component[previous] = id;
                    stack.push(previous);
                }
            }
        }
    }
    component
}

/// Append the walk from `from` to `to`, through its corners.
fn walk_edge(
    waypoints: &mut Vec<[f64; 3]>,
    nodes: &[[f64; 3]],
    corners: &Corners,
    from: usize,
    to: usize,
) {
    waypoints.extend(corners.get(&(from, to)).into_iter().flatten().copied());
    waypoints.push(nodes[to]);
}

fn edge_cost(nodes: &[[f64; 3]], corners: &Corners, from: usize, to: usize) -> f64 {
    let (last, travel) = corners
        .get(&(from, to))
        .into_iter()
        .flatten()
        .fold((nodes[from], 0.0), |(previous, distance), &point| {
            (point, distance + edge_length(previous, point))
        });
    travel + edge_length(last, nodes[to])
}

fn edge_length(from: [f64; 3], to: [f64; 3]) -> f64 {
    (to[0] - from[0])
        .hypot(to[2] - from[2])
        .hypot(to[1] - from[1])
}

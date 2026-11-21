use serde::Deserialize;

use shagger_shared::net::game_state::Vec3;

use super::{
    ANCHOR_RADIUS, ARRIVAL_RADIUS, ENTRY_RADIUS, MAX_HEIGHT_DELTA, PickupGoal, Route,
    horizontal_distance,
};

const MAX_NODES: usize = 256;
const MAX_ENTRIES: usize = 32;
const ROUTES_PER_ENTRY: usize = 3;

#[derive(Deserialize)]
pub(super) struct WalkGraph {
    bsp_sha256: String,
    nodes: Vec<[f64; 3]>,
    edges: Vec<[usize; 2]>,
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
    goals: Vec<PlannedGoal>,
    component: Vec<usize>,
}

struct PlannedGoal {
    node: usize,
    pickup: Option<PickupGoal>,
    /// The next node toward this goal from each node, if it is reachable.
    toward: Vec<Option<u16>>,
    distance: Vec<f32>,
}

impl Planner {
    /// The best accepted walk from the node nearest `position` to a goal
    /// the bot isn't already standing on. Ties go to the shorter walk.
    ///
    /// Off the graph, the bot can start from `anchor`, the last node it
    /// reached, and walks back to it first. It got here from there.
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
        let nearby = (0..self.nodes.len())
            .filter(|&node| here(self.nodes[node], ENTRY_RADIUS))
            .min_by(|&a, &b| {
                horizontal_distance(position, self.nodes[a])
                    .total_cmp(&horizontal_distance(position, self.nodes[b]))
            });
        let (start, back) = match nearby {
            Some(node) => (node, false),
            None => {
                let anchor = anchor.filter(|&anchor| here(anchor, ANCHOR_RADIUS))?;
                let node = self.nodes.iter().position(|&node| node == anchor)?;
                (node, true)
            }
        };
        let mut best: Option<(f64, f32, Route)> = None;
        for goal in &self.goals {
            let distance = goal.distance[start];
            if goal.node == start
                || self.component[goal.node] != self.component[start]
                || !distance.is_finite()
                || here(self.nodes[goal.node], ARRIVAL_RADIUS)
            {
                continue;
            }
            let mut waypoints = Vec::new();
            if back {
                waypoints.push(self.nodes[start]);
            }
            let mut node = start;
            while let Some(next) = goal.toward[node] {
                node = usize::from(next);
                waypoints.push(self.nodes[node]);
            }
            let route = Route {
                entry: self.nodes[start],
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
        for [from, to] in self.edges.iter().copied() {
            if from >= count || to >= count || from == to {
                return Err("invalid navigation graph edge");
            }
            neighbors[from].push(to);
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
        let component = components(&neighbors, &reverse);
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
                    component: &component,
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
            .map(|node| self.walks_to(node, &reverse, pickups[node]))
            .collect();
        Ok((
            routes,
            Planner {
                nodes: self.nodes,
                goals,
                component,
            },
        ))
    }

    /// Dijkstra over reversed edges, so each node learns its next hop toward `goal`.
    fn walks_to(
        &self,
        goal: usize,
        reverse: &[Vec<usize>],
        pickup: Option<PickupGoal>,
    ) -> PlannedGoal {
        let count = self.nodes.len();
        let mut distance = vec![f64::INFINITY; count];
        let mut toward = vec![None; count];
        let mut settled = vec![false; count];
        distance[goal] = 0.0;
        while let Some(node) = (0..count)
            .filter(|&node| !settled[node] && distance[node].is_finite())
            .min_by(|&a, &b| distance[a].total_cmp(&distance[b]))
        {
            settled[node] = true;
            for &previous in &reverse[node] {
                let cost = distance[node] + edge_length(self.nodes[previous], self.nodes[node]);
                if cost < distance[previous] {
                    distance[previous] = cost;
                    // MAX_NODES keeps node indices within u16.
                    toward[previous] = Some(node as u16);
                }
            }
        }
        PlannedGoal {
            node: goal,
            pickup,
            toward,
            distance: distance.into_iter().map(|value| value as f32).collect(),
        }
    }

    fn routes_from(&self, entry: usize, walk: Walk<'_>, resumed_from: Option<&str>) -> Vec<Route> {
        let Walk {
            neighbors,
            component,
            learned,
            pickups,
        } = walk;
        let mut routes = Vec::new();
        let mut distances = vec![f64::INFINITY; self.nodes.len()];
        let mut predecessor: Vec<Option<usize>> = vec![None; self.nodes.len()];
        let mut settled = vec![false; self.nodes.len()];
        distances[entry] = 0.0;
        while let Some(node) = (0..self.nodes.len())
            .filter(|&node| !settled[node] && distances[node].is_finite())
            .min_by(|&a, &b| distances[a].total_cmp(&distances[b]))
        {
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
            // A walk that can't return could trap the bot. Learned takeoffs
            // leave by their recording instead.
            let returns = is_learned || component[node] == component[entry];
            if node != entry && self.goals.contains(&node) && returns && !category_full {
                let mut waypoints = vec![self.nodes[node]];
                let mut previous = predecessor[node].expect("reachable goal has predecessor");
                while previous != entry {
                    waypoints.push(self.nodes[previous]);
                    previous = predecessor[previous].expect("reachable node has predecessor");
                }
                waypoints.reverse();
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
            settled[node] = true;
            for &next in &neighbors[node] {
                let cost = distances[node] + edge_length(self.nodes[node], self.nodes[next]);
                if cost < distances[next] {
                    distances[next] = cost;
                    predecessor[next] = Some(node);
                }
            }
        }
        routes
    }
}

#[derive(Clone, Copy)]
struct Walk<'a> {
    neighbors: &'a [Vec<usize>],
    component: &'a [usize],
    learned: &'a [Option<&'a str>],
    pickups: &'a [Option<PickupGoal>],
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

fn edge_length(from: [f64; 3], to: [f64; 3]) -> f64 {
    (to[0] - from[0])
        .hypot(to[2] - from[2])
        .hypot(to[1] - from[1])
}

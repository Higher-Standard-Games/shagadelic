use serde::Deserialize;

use super::Route;

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
}

impl WalkGraph {
    pub(super) fn routes(self) -> Result<Vec<Route>, &'static str> {
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
                self.routes_from(entry, &neighbors, &learned, resumed_from)
            })
            .collect();
        if routes.is_empty() {
            return Err("navigation graph has no reachable goals");
        }
        Ok(routes)
    }

    fn routes_from(
        &self,
        entry: usize,
        neighbors: &[Vec<usize>],
        learned: &[Option<&str>],
        resumed_from: Option<&str>,
    ) -> Vec<Route> {
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
            if node != entry && self.goals.contains(&node) && !category_full {
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

fn edge_length(from: [f64; 3], to: [f64; 3]) -> f64 {
    (to[0] - from[0])
        .hypot(to[2] - from[2])
        .hypot(to[1] - from[1])
}

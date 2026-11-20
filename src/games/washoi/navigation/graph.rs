use serde::Deserialize;

use super::Route;

const MAX_NODES: usize = 256;
const MAX_ENTRIES: usize = 32;

#[derive(Deserialize)]
pub(super) struct WalkGraph {
    bsp_sha256: String,
    nodes: Vec<[f64; 3]>,
    edges: Vec<[usize; 2]>,
    entries: Vec<usize>,
    goals: Vec<usize>,
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
        let mut neighbors = vec![Vec::new(); count];
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
            .filter_map(|&entry| self.route_from(entry, &neighbors))
            .collect();
        if routes.is_empty() {
            return Err("navigation graph has no reachable goals");
        }
        Ok(routes)
    }

    fn route_from(&self, entry: usize, neighbors: &[Vec<usize>]) -> Option<Route> {
        let mut distances = vec![f64::INFINITY; self.nodes.len()];
        let mut predecessor = vec![None; self.nodes.len()];
        let mut settled = vec![false; self.nodes.len()];
        distances[entry] = 0.0;
        loop {
            let node = (0..self.nodes.len())
                .filter(|&node| !settled[node] && distances[node].is_finite())
                .min_by(|&a, &b| distances[a].total_cmp(&distances[b]))?;
            if node != entry && self.goals.contains(&node) {
                let mut waypoints = vec![self.nodes[node]];
                let mut previous = predecessor[node]?;
                while previous != entry {
                    waypoints.push(self.nodes[previous]);
                    previous = predecessor[previous]?;
                }
                waypoints.reverse();
                return Some(Route {
                    entry: self.nodes[entry],
                    waypoints,
                });
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
    }
}

fn edge_length(from: [f64; 3], to: [f64; 3]) -> f64 {
    (to[0] - from[0])
        .hypot(to[2] - from[2])
        .hypot(to[1] - from[1])
}

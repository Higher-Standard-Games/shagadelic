use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap},
};

use crate::games::washoi::agents::player::navigation::traversal::Maneuver;
use serde::Deserialize;

use shagger_shared::net::game_state::Vec3;

use crate::games::washoi::agents::player::navigation::{
    ANCHOR_RADIUS, ARRIVAL_RADIUS, ENTRY_RADIUS, LOCAL_WALK_RADIUS, MAX_HEIGHT_DELTA,
    ObjectiveEdge, ObjectiveRecovery, PickupGoal, Route, horizontal_distance,
};

/// Nodes tried in turn when recovery walks straight back to the graph.
const MAX_LOCAL_STARTS: usize = 4;
const MAX_NODES: usize = 1024;
const MAX_ENTRIES: usize = 64;
const MAX_EDGES: usize = 32768;
const ROUTES_PER_ENTRY: usize = 3;
const MAX_CORNERS: usize = 32;
/// How far a bot off the graph looks for a node in clear view.
const FAR_RADIUS: f64 = 1024.0;

/// The bake lifts floor and item nodes by SHAG's `STANDING_LIFT` to the
/// standing origin (`BotNavGraph.walkOrigin`), but writes an edge's `via`
/// as the cache has it, on the floor. Corners are lifted by this on load so
/// a walk compares the bot's origin with points at the same height.
const STANDING_LIFT: f64 = 24.0;

/// Intermediate points of a walk edge, from the TypeScript nav cache `via`,
/// at standing height.
type Corners = BTreeMap<(usize, usize), Vec<[f64; 3]>>;
type Maneuvers = BTreeMap<(usize, usize), Maneuver>;
/// Learned links, takeoff to landing: the recording's ID and the link's cost.
type LearnedLinks = BTreeMap<(usize, usize), (String, f64)>;
/// A learned link's cost is its recording's ticks as distance at this speed,
/// TypeScript's `LEARNED_COST_SPEED` (`BotNavGraph.addLearnedRoutes`).
const LEARNED_COST_SPEED: f64 = 320.0;
/// Washoi's tick rate, to turn a recording's ticks into seconds.
const TICK_RATE: f64 = 125.0;
/// A walk edge a bot stands on: how far off it, the edge, and the rest of
/// its walk from there.
type EdgeMatch = (f64, (usize, usize), Vec<[f64; 3]>);

/// `[from, to]`, or `[from, to, corners]` for a walk that turns corners.
#[derive(Deserialize)]
#[serde(untagged)]
enum Edge {
    Straight([usize; 2]),
    Cornered(usize, usize, Vec<[f64; 3]>),
}

#[derive(Deserialize)]
pub(in crate::games::washoi) struct WalkGraph {
    bsp_sha256: String,
    nodes: Vec<[f64; 3]>,
    edges: Vec<Edge>,
    #[serde(default)]
    maneuvers: Vec<(usize, usize, Maneuver)>,
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
pub(in crate::games::washoi) struct Planner {
    nodes: Vec<[f64; 3]>,
    neighbors: Vec<Vec<usize>>,
    corners: Corners,
    maneuvers: Maneuvers,
    /// Every teleporter maneuver's trigger box, min and max.
    teleporters: Vec<([f64; 3], [f64; 3])>,
    goals: Vec<PlannedGoal>,
    areas: Areas,
    /// Each learned takeoff with the landing of the same recording, from
    /// the bake's `learned_goals` and `learned_landings`.
    learned_pairs: Vec<(usize, usize, String)>,
    /// The pairs whose recording the bot has, once linked
    /// (`Planner::link_learned`). Empty: the walk graph alone.
    links: LearnedLinks,
    /// Once linked, the walk graph's own goals and areas, for a bot that
    /// can't take off now (`Planner::walks`).
    walk: Option<WalkOnly>,
}

/// The walk graph's own goals and areas, kept once learned links join them.
struct WalkOnly {
    goals: Vec<PlannedGoal>,
    areas: Areas,
    /// Each node whose walkable goals are all dead ends: goals that walk to
    /// no other goal. washoi_dm2's Red Armor ledge is one, the Red Armor its
    /// one walkable goal, left by a recording alone. A node that walks to no
    /// goal at all is one too, so a stranded bot keeps the links out.
    pocket: Vec<bool>,
}

impl WalkOnly {
    fn new(goals: Vec<PlannedGoal>, areas: Areas, count: usize) -> Self {
        let (all, walkable) = (&goals, &areas);
        let reach = |start: usize| {
            all.iter().enumerate().filter(move |(_, goal)| {
                goal.node != start
                    && goal.distance[start].is_finite()
                    && walkable.allows(start, goal.node)
            })
        };
        let dead_end: Vec<bool> = goals
            .iter()
            .map(|goal| reach(goal.node).next().is_none())
            .collect();
        let pocket = (0..count)
            .map(|node| reach(node).all(|(index, _)| dead_end[index]))
            .collect();
        Self {
            goals,
            areas,
            pocket,
        }
    }
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

    /// Another place the search may start, `cost` away.
    fn seed(&mut self, node: usize, cost: f64) {
        if cost < self.distances[node] {
            self.distances[node] = cost;
            self.queue.push(Visit { node, cost });
        }
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
    /// Each node's area and each area's ordinary goal count, for tests
    /// comparing the areas across a link.
    #[cfg(test)]
    pub(super) fn area_counts(&self) -> (Vec<usize>, Vec<usize>) {
        (self.areas.component.clone(), self.areas.goals.clone())
    }

    /// Link each learned takeoff to its recording's landing, for the
    /// recordings `ticks` knows, as TypeScript's `BotNavGraph.addLearnedRoutes`
    /// adds a learned route as a `jump` edge costing its ticks as distance at
    /// `LEARNED_COST_SPEED`. Goals past a recording, such as washoi_dm2's Red
    /// Armor, become reachable to goal choice and planning, and their area
    /// joins the area the takeoff is in. A walk planned across a link ends
    /// at the takeoff with the recording to play (`best_route`). Without a
    /// recording, a pair stays unlinked and the walks are the graph's alone.
    pub(in crate::games::washoi) fn link_learned(&mut self, ticks: impl Fn(&str) -> Option<usize>) {
        // Start again from the walk graph's own goals and areas.
        if let Some(walk) = self.walk.take() {
            self.goals = walk.goals;
            self.areas = walk.areas;
        }
        self.links = self
            .learned_pairs
            .iter()
            .filter_map(|(takeoff, landing, id)| {
                let ticks = ticks(id)?;
                let cost = ticks as f64 / TICK_RATE * LEARNED_COST_SPEED;
                Some(((*takeoff, *landing), (id.clone(), cost)))
            })
            .collect();
        // No recording to link: keep the walk graph's own goals and areas.
        if self.links.is_empty() {
            return;
        }
        let count = self.nodes.len();
        let mut forward = self.neighbors.clone();
        for &(takeoff, landing) in self.links.keys() {
            if !forward[takeoff].contains(&landing) {
                forward[takeoff].push(landing);
            }
        }
        let mut reverse = vec![Vec::new(); count];
        for (from, adjacent) in forward.iter().enumerate() {
            for &to in adjacent {
                reverse[to].push(from);
            }
        }
        // The planned goals are the ordinary ones: learned takeoffs were left
        // out of them at the bake (`routes`), so they count as Areas::new
        // counts them there.
        let goals: Vec<usize> = self.goals.iter().map(|goal| goal.node).collect();
        let areas = Areas::new(components(&forward, &reverse), &goals, &vec![None; count]);
        let linked = self
            .goals
            .iter()
            .map(|goal| {
                walks_to(
                    &self.nodes,
                    goal.node,
                    &reverse,
                    &self.corners,
                    &self.maneuvers,
                    &self.links,
                    goal.pickup,
                )
            })
            .collect();
        let walk_goals = std::mem::replace(&mut self.goals, linked);
        let walk_areas = std::mem::replace(&mut self.areas, areas);
        self.walk = Some(WalkOnly::new(walk_goals, walk_areas, count));
    }

    /// The goals and areas a bot plans with from `start`. One that can't take
    /// off now plans over the walk graph alone, the learned links masked,
    /// so it never plans a walk that ends at a takeoff it won't take. In a
    /// pocket (`WalkOnly::pocket`), whose way out is a recording, the links
    /// stay, or a bot there would have no plan once it has what the pocket
    /// holds.
    fn walks(&self, start: usize, takeoffs: bool) -> (&[PlannedGoal], &Areas) {
        match &self.walk {
            Some(walk) if !takeoffs && !walk.pocket[start] => (&walk.goals, &walk.areas),
            _ => (&self.goals, &self.areas),
        }
    }

    /// Node positions, standing origins, in graph index order. The order is
    /// stable for the life of the planner.
    /// Every pickup goal and its node's standing origin.
    pub(in crate::games::washoi) fn pickups(
        &self,
    ) -> impl Iterator<Item = (PickupGoal, [f64; 3])> + '_ {
        self.goals
            .iter()
            .filter_map(|goal| Some((goal.pickup?, self.nodes[goal.node])))
    }

    /// The precomputed walk to every reachable pickup, by pickup ID, plus
    /// the way onto the graph. Attaching once serves every pickup.
    pub(in crate::games::washoi) fn pickup_costs(
        &self,
        position: Vec3,
        takeoffs: bool,
        local_walk: impl Fn(Vec3, Vec3) -> bool,
        open_line: impl Fn(Vec3, Vec3) -> bool,
    ) -> Vec<(usize, f32)> {
        let costs = |(start, lead): (usize, f64)| -> Vec<(usize, f32)> {
            let (goals, areas) = self.walks(start, takeoffs);
            goals
                .iter()
                .filter_map(|goal| {
                    let pickup = goal.pickup?;
                    let distance = goal.distance[start];
                    (distance.is_finite() && areas.allows(start, goal.node))
                        .then_some((pickup.id, distance + lead as f32))
                })
                .collect()
        };
        // A start with no pickup in reach, such as a ledge pocket's node, is
        // passed over for the next way onto the graph, as roaming does.
        self.starts(position, &local_walk, &open_line)
            .map(costs)
            .find(|costs| !costs.is_empty())
            .unwrap_or_default()
    }

    /// The ways onto the graph in the order roaming tries them, each with the
    /// walk to it: a node within the entry radius, the end of a walk the bot
    /// stands on, the nearest nodes within a straight local walk, then nodes
    /// in clear view. Each is found only when the one before is passed over.
    fn starts<'a>(
        &'a self,
        position: Vec3,
        local_walk: &'a impl Fn(Vec3, Vec3) -> bool,
        open_line: &'a impl Fn(Vec3, Vec3) -> bool,
    ) -> impl Iterator<Item = (usize, f64)> + 'a {
        let entry = self
            .nearest_entry(position)
            .map(|node| (node, horizontal_distance(position, self.nodes[node])));
        let edge = std::iter::once_with(move || {
            self.on_edge(position).map(|(node, lead)| {
                let (_, length) = lead.iter().fold(
                    ([position.x, position.y, position.z], 0.0),
                    |(from, total), &to| (to, total + edge_length(from, to)),
                );
                (node, length)
            })
        })
        .flatten();
        let local = std::iter::once_with(move || {
            let mut near: Vec<_> = (0..self.nodes.len())
                .map(|node| (horizontal_distance(position, self.nodes[node]), node))
                .filter(|&(distance, node)| {
                    distance <= LOCAL_WALK_RADIUS
                        && (self.nodes[node][1] - position.y).abs() <= MAX_HEIGHT_DELTA
                })
                .collect();
            near.sort_by(|a, b| a.0.total_cmp(&b.0));
            near.into_iter().filter(move |&(_, node)| {
                let [x, y, z] = self.nodes[node];
                local_walk(position, Vec3::new(x, y, z))
            })
        })
        .flatten()
        .take(MAX_LOCAL_STARTS)
        .map(|(distance, node)| (node, distance));
        let view = std::iter::once_with(move || self.in_view(position, open_line))
            .flatten()
            .take(MAX_LOCAL_STARTS)
            .map(move |node| (node, horizontal_distance(position, self.nodes[node])));
        entry.into_iter().chain(edge).chain(local).chain(view)
    }

    /// Every baked teleporter's trigger box, min and max.
    pub(in crate::games::washoi) fn teleporters(&self) -> &[([f64; 3], [f64; 3])] {
        &self.teleporters
    }

    /// The view yaw a teleporter set, when the bot moved from `before` to
    /// `after` by going through one, after TypeScript `teleportExitYaw`. One
    /// snapshot of movement never covers 150 units, and the bot may have
    /// moved 96 on from the exit by the time a snapshot shows it.
    pub(in crate::games::washoi) fn teleport_exit_yaw(
        &self,
        before: Vec3,
        after: Vec3,
    ) -> Option<f64> {
        let step = (after.x - before.x)
            .hypot(after.z - before.z)
            .hypot(after.y - before.y);
        if step < 150.0 {
            return None;
        }
        self.maneuvers
            .values()
            .filter_map(|maneuver| maneuver.teleport_exit())
            .map(|(exit, yaw)| {
                let distance = (exit[0] - after.x)
                    .hypot(exit[2] - after.z)
                    .hypot(exit[1] - after.y);
                (distance, yaw)
            })
            .filter(|&(distance, _)| distance <= 96.0)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, yaw)| yaw)
    }

    pub(in crate::games::washoi) fn nodes(&self) -> &[[f64; 3]] {
        &self.nodes
    }

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

    pub(in crate::games::washoi) fn plan_to(
        &self,
        position: Vec3,
        target: Vec3,
        radius: f64,
        recovery: Option<ObjectiveRecovery>,
        local_walk: impl Fn(Vec3, Vec3) -> bool,
        open_line: impl Fn(Vec3, Vec3) -> bool,
    ) -> Option<(Route, Vec<ObjectiveEdge>)> {
        // Partway along a walk, every edge the bot stands on is a start: a
        // two-way link is two edges at the same spot, and only the search
        // knows which end the target is past.
        let mut leads: Vec<(usize, Vec<[f64; 3]>, f64)> = Vec::new();
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
                let from = [position.x, position.y, position.z];
                for (node, rest) in self.on_edges(position) {
                    let (_, cost) = rest.iter().fold((from, 0.0), |(from, total), &to| {
                        (to, total + edge_length(from, to))
                    });
                    match leads.iter_mut().find(|(seen, _, _)| *seen == node) {
                        Some(lead) if cost < lead.2 => *lead = (node, rest, cost),
                        Some(_) => {}
                        None => leads.push((node, rest, cost)),
                    }
                }
                leads.first().map(|&(node, _, _)| node)
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
            })
            .or_else(|| self.in_view(position, &open_line).next())?;
        let mut search = Search::new(self.nodes.len(), start);
        if let Some(&(_, _, cost)) = leads.first() {
            search.distances[start] = cost;
            search.queue = BinaryHeap::from([Visit { node: start, cost }]);
        }
        for &(node, _, cost) in leads.iter().skip(1) {
            search.seed(node, cost);
        }
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
                if self.maneuvers.contains_key(&(node, next)) {
                    continue;
                }
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
        let mut waypoints = leads
            .into_iter()
            .find(|(node, _, _)| *node == path[0])
            .map_or_else(|| vec![self.nodes[path[0]]], |(_, lead, _)| lead);
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
                entry: waypoints[0],
                waypoints,
                maneuvers: Vec::new(),
                learned_route: None,
                resumed_from: None,
                pickup: None,
            },
            edges,
        ))
    }

    /// A walk edge the bot stands on, partway along: within the entry radius
    /// of the edge's path, at that point's height. Stairs and ramps baked
    /// with corners leave long stretches far from either node, where a
    /// fight or a stall can leave the bot. It finishes that walk to the
    /// edge's end node, and the returned lead is the rest of the walk,
    /// ending at that node. Maneuver edges are not walks and never match.
    fn on_edge(&self, position: Vec3) -> Option<(usize, Vec<[f64; 3]>)> {
        self.on_edges(position).into_iter().next()
    }

    /// Every walk edge the bot stands on as `on_edge` finds one, nearest
    /// first: those within `ARRIVAL_RADIUS` of the nearest, and the reverse
    /// of each. Both directions of a two-way link match the same spot, the
    /// reverse up to the entry radius off where its corners were baked
    /// apart. Nearer is no guide to which way the walk goes: choosing one
    /// sent a bot partway down a drop back up the ledge to its top, and one
    /// beside a corner 600 units away and back.
    fn on_edges(&self, position: Vec3) -> Vec<(usize, Vec<[f64; 3]>)> {
        let mut found: Vec<EdgeMatch> = Vec::new();
        for (from, adjacent) in self.neighbors.iter().enumerate() {
            for &to in adjacent {
                if self.maneuvers.contains_key(&(from, to)) {
                    continue;
                }
                let mut points = vec![self.nodes[from]];
                points.extend(self.corners.get(&(from, to)).into_iter().flatten().copied());
                points.push(self.nodes[to]);
                let mut best: Option<EdgeMatch> = None;
                for (index, pair) in points.windows(2).enumerate() {
                    let ([ax, ay, az], [bx, by, bz]) = (pair[0], pair[1]);
                    let (dx, dz) = (bx - ax, bz - az);
                    let length = dx * dx + dz * dz;
                    let t = if length > 0.0 {
                        (((position.x - ax) * dx + (position.z - az) * dz) / length).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let distance = (ax + dx * t - position.x).hypot(az + dz * t - position.z);
                    let height = ay + (by - ay) * t;
                    if distance <= ENTRY_RADIUS
                        && (height - position.y).abs() <= MAX_HEIGHT_DELTA
                        && best.as_ref().is_none_or(|(top, _, _)| distance < *top)
                    {
                        best = Some((distance, (from, to), points[index + 1..].to_vec()));
                    }
                }
                found.extend(best);
            }
        }
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        let nearest = found.first().map_or(0.0, |&(distance, _, _)| distance);
        let close: Vec<_> = found
            .iter()
            .filter(|&&(distance, _, _)| distance <= nearest + ARRIVAL_RADIUS)
            .map(|&(_, edge, _)| edge)
            .collect();
        found
            .into_iter()
            .filter(|&(_, (from, to), _)| {
                close.contains(&(from, to)) || close.contains(&(to, from))
            })
            .map(|(_, (_, to), lead)| (to, lead))
            .collect()
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

    /// The best accepted walk to a goal. Within the entry radius of a node,
    /// it starts there. Otherwise the bot walks back to its anchor, or,
    /// when that walk is blocked or missing, to one of the nearest nodes
    /// `local_walk` finds a straight walk to.
    ///
    /// A bot that can't take off now plans with the learned links masked,
    /// but for a pocket whose way out is one (`Planner::walks`).
    #[allow(clippy::too_many_arguments)]
    pub(in crate::games::washoi) fn plan(
        &self,
        position: Vec3,
        anchor: Option<[f64; 3]>,
        takeoffs: bool,
        accept: impl Fn(&Route) -> bool,
        score: impl Fn(&Route) -> f64,
        local_walk: impl Fn(Vec3, Vec3) -> bool,
        open_line: impl Fn(Vec3, Vec3) -> bool,
    ) -> Option<Route> {
        let here = |node: [f64; 3], radius: f64| {
            horizontal_distance(position, node) <= radius
                && (node[1] - position.y).abs() <= MAX_HEIGHT_DELTA
        };
        // A node with no route out, as on a ledge in a pocket, is no start:
        // try the other ways onto the graph.
        if let Some(node) = self.nearest_entry(position)
            && let Some(route) =
                self.best_route(position, node, Vec::new(), takeoffs, &accept, &score)
        {
            return Some(route);
        }
        let resumed = anchor
            .filter(|&anchor| here(anchor, ANCHOR_RADIUS))
            .and_then(|anchor| self.resume_at(anchor));
        if let Some((start, lead)) = resumed
            && let Some(route) = self.best_route(position, start, lead, takeoffs, &accept, &score)
        {
            return Some(route);
        }
        // Partway along a walk, finish it, as an objective walk does.
        if let Some((start, lead)) = self.on_edge(position)
            && let Some(route) = self.best_route(position, start, lead, takeoffs, &accept, &score)
        {
            return Some(route);
        }
        // Spawns and ledges on sparse cache graphs can sit farther than the
        // anchor radius from any node at their height.
        let mut near: Vec<_> = (0..self.nodes.len())
            .filter(|&node| here(self.nodes[node], LOCAL_WALK_RADIUS))
            .map(|node| (horizontal_distance(position, self.nodes[node]), node))
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        near.into_iter()
            .map(|(_, node)| node)
            .filter(|&node| {
                let [x, y, z] = self.nodes[node];
                local_walk(position, Vec3::new(x, y, z))
            })
            .take(MAX_LOCAL_STARTS)
            .find_map(|node| {
                self.best_route(
                    position,
                    node,
                    vec![self.nodes[node]],
                    takeoffs,
                    &accept,
                    &score,
                )
            })
            .or_else(|| {
                self.in_view(position, &open_line)
                    .take(MAX_LOCAL_STARTS)
                    .find_map(|node| {
                        self.best_route(
                            position,
                            node,
                            vec![self.nodes[node]],
                            takeoffs,
                            &accept,
                            &score,
                        )
                    })
            })
    }

    /// The last way onto the graph: nodes in clear view, nearest first,
    /// within `FAR_RADIUS` and no more than a step above. A lower node is
    /// fine, because walking straight at it drops off whatever ledge the
    /// bot is on. Wide open floors on cache graphs leave bots 300 to 600
    /// units from the nearest node, which is what TypeScript's recovery walk
    /// handles. Without this, a bot there stood still for the match.
    fn in_view<'a>(
        &'a self,
        position: Vec3,
        open_line: &'a impl Fn(Vec3, Vec3) -> bool,
    ) -> impl Iterator<Item = usize> + 'a {
        let mut near: Vec<_> = (0..self.nodes.len())
            .filter_map(|node| {
                let point = self.nodes[node];
                let distance = horizontal_distance(position, point);
                (distance <= FAR_RADIUS && point[1] - position.y <= MAX_HEIGHT_DELTA)
                    .then_some((distance, node))
            })
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        near.into_iter().map(|(_, node)| node).filter(move |&node| {
            let [x, y, z] = self.nodes[node];
            open_line(position, Vec3::new(x, y, z))
        })
    }

    fn best_route(
        &self,
        position: Vec3,
        start: usize,
        lead: Vec<[f64; 3]>,
        takeoffs: bool,
        accept: &impl Fn(&Route) -> bool,
        score: &impl Fn(&Route) -> f64,
    ) -> Option<Route> {
        let here = |node: [f64; 3], radius: f64| {
            horizontal_distance(position, node) <= radius
                && (node[1] - position.y).abs() <= MAX_HEIGHT_DELTA
        };
        let mut best: Option<(f64, f32, Route)> = None;
        let (goals, areas) = self.walks(start, takeoffs);
        for goal in goals {
            let distance = goal.distance[start];
            if goal.node == start
                || !areas.allows(start, goal.node)
                || !distance.is_finite()
                || here(self.nodes[goal.node], ARRIVAL_RADIUS)
            {
                continue;
            }
            let mut waypoints = lead.clone();
            let mut maneuvers = Vec::new();
            let mut learned_route = None;
            let mut node = start;
            while let Some(next) = goal.toward[node] {
                let next = usize::from(next);
                // The walk ends at a learned takeoff; the recording flies the
                // rest, and the landing's continuation walks on to the goal.
                if let Some((id, _)) = self.links.get(&(node, next)) {
                    if waypoints.is_empty() {
                        waypoints.push(self.nodes[node]);
                    }
                    learned_route = Some(id.clone());
                    break;
                }
                if let Some(&maneuver) = self.maneuvers.get(&(node, next)) {
                    maneuvers.push((waypoints.len(), maneuver));
                }
                walk_edge(&mut waypoints, &self.nodes, &self.corners, node, next);
                node = next;
            }
            let route = Route {
                entry: lead.first().copied().unwrap_or(self.nodes[start]),
                waypoints,
                maneuvers,
                learned_route,
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
    pub(in crate::games::washoi) fn bsp_sha256(&self) -> &str {
        &self.bsp_sha256
    }

    pub(in crate::games::washoi) fn routes(self) -> Result<(Vec<Route>, Planner), &'static str> {
        let count = self.nodes.len();
        if !(2..=MAX_NODES).contains(&count)
            || !(1..=MAX_ENTRIES).contains(&self.entries.len())
            || self.goals.is_empty()
            || self.goals.len() > count
            || self.edges.len().saturating_add(self.maneuvers.len()) > MAX_EDGES
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
                corners.entry((from, to)).or_insert_with(|| {
                    via.iter()
                        .map(|&[x, y, z]| [x, y + STANDING_LIFT, z])
                        .collect()
                });
            }
        }
        let mut maneuvers = Maneuvers::new();
        for &(from, to, maneuver) in &self.maneuvers {
            if from >= count
                || to >= count
                || from == to
                || !maneuver.valid()
                || neighbors[from].contains(&to)
                || maneuvers.insert((from, to), maneuver).is_some()
            {
                return Err("invalid or duplicate navigation maneuver");
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
                    maneuvers: &maneuvers,
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
        let links = LearnedLinks::new();
        let goals = goals
            .into_iter()
            .map(|node| {
                walks_to(
                    &self.nodes,
                    node,
                    &reverse,
                    &corners,
                    &maneuvers,
                    &links,
                    pickups[node],
                )
            })
            .collect();
        let learned_pairs = self
            .learned_goals
            .iter()
            .filter_map(|(takeoff, id)| {
                let (landing, _) = self
                    .learned_landings
                    .iter()
                    .find(|(_, other)| other == id)?;
                (takeoff != landing).then(|| (*takeoff, *landing, id.clone()))
            })
            .collect();
        let teleporters = maneuvers
            .values()
            .filter_map(|maneuver| maneuver.teleporter_box())
            .collect();
        Ok((
            routes,
            Planner {
                nodes: self.nodes,
                neighbors,
                corners,
                maneuvers,
                teleporters,
                goals,
                areas,
                learned_pairs,
                links,
                walk: None,
            },
        ))
    }

    fn routes_from(&self, entry: usize, walk: Walk<'_>, resumed_from: Option<&str>) -> Vec<Route> {
        let Walk {
            neighbors,
            corners,
            maneuvers,
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
                let mut actions = Vec::new();
                for pair in path.windows(2) {
                    if let Some(&maneuver) = maneuvers.get(&(pair[0], pair[1])) {
                        actions.push((waypoints.len(), maneuver));
                    }
                    walk_edge(&mut waypoints, &self.nodes, corners, pair[0], pair[1]);
                }
                routes.push(Route {
                    entry: self.nodes[entry],
                    waypoints,
                    maneuvers: actions,
                    learned_route: learned[node].map(str::to_owned),
                    resumed_from: resumed_from.map(str::to_owned),
                    pickup: pickups[node],
                });
                if routes.len() == ROUTES_PER_ENTRY + usize::from(resumed_from.is_none()) {
                    break;
                }
            }
            for &next in &neighbors[node] {
                search.extend(
                    node,
                    next,
                    travel_cost(&self.nodes, corners, maneuvers, node, next),
                );
            }
        }
        routes
    }
}

#[derive(Clone, Copy)]
struct Walk<'a> {
    neighbors: &'a [Vec<usize>],
    corners: &'a Corners,
    maneuvers: &'a Maneuvers,
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

/// Dijkstra over reversed edges, so each node learns its next hop toward
/// `goal`. A learned link costs its recording's time (`LearnedLinks`).
fn walks_to(
    nodes: &[[f64; 3]],
    goal: usize,
    reverse: &[Vec<usize>],
    corners: &Corners,
    maneuvers: &Maneuvers,
    links: &LearnedLinks,
    pickup: Option<PickupGoal>,
) -> PlannedGoal {
    let mut search = Search::new(nodes.len(), goal);
    while let Some(node) = search.next() {
        for &previous in &reverse[node] {
            let cost = links.get(&(previous, node)).map_or_else(
                || travel_cost(nodes, corners, maneuvers, previous, node),
                |&(_, cost)| cost,
            );
            search.extend(node, previous, cost);
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

fn travel_cost(
    nodes: &[[f64; 3]],
    corners: &Corners,
    maneuvers: &Maneuvers,
    from: usize,
    to: usize,
) -> f64 {
    match maneuvers.get(&(from, to)) {
        Some(Maneuver::Teleporter { .. }) => 96.0,
        Some(_) => edge_cost(nodes, corners, from, to) + 96.0,
        None => edge_cost(nodes, corners, from, to),
    }
}

fn edge_length(from: [f64; 3], to: [f64; 3]) -> f64 {
    (to[0] - from[0])
        .hypot(to[2] - from[2])
        .hypot(to[1] - from[1])
}

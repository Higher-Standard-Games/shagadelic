//! The line a bot runs along through a plain walking stretch of its route,
//! after TypeScript `src/games/fps/bot/RouteLine.ts` and the held line of
//! `BotNavigator.followRouteLine` and `routeLineFor`.
//!
//! Running waypoint to waypoint takes the inside of every corner and the
//! side of every room the waypoints happen to sit at. A line is built once
//! per stretch: pull the string past each waypoint the standing hull can run
//! straight past with floor under it, then move each corner off the walls
//! near it toward `LINE_CLEARANCE`, and add a point off the wall where a
//! stretch still passes closer than `LINE_TIGHT`. Every change keeps both
//! stretches next to it walkable. The walk holds the line while it is on it
//! and makes for the point `ROUTE_LINE_AHEAD` along it.
//!
//! Deviations: a stretch ends at a maneuver or where consecutive waypoints
//! differ in height by more than a step's worth (`MAX_HEIGHT_DELTA`), as
//! Rust routes carry drops and climbs as plain waypoints where TypeScript
//! has steps of their own (`lineEndStep`). Lines are kept per walk rather
//! than per graph (`sharedRouteLine`); a stretch's line comes out the same.

use std::collections::HashMap;

use super::{MAX_HEIGHT_DELTA, Route};
use crate::games::washoi::Sight;
use shagger_shared::net::game_state::Vec3;

/// `LINE_CLEARANCE`: clearance the line keeps from walls where the room
/// allows, from the hull's side.
const LINE_CLEARANCE: f64 = 48.0;
/// `LINE_TIGHT`: a stretch passing closer than this gets a point moved off.
const LINE_TIGHT: f64 = 24.0;
/// `PUSH_STEP`: moves off a wall are tried in steps of this.
const PUSH_STEP: f64 = 16.0;
/// `SAMPLE_STEP`: stretches are sampled this often for their clearance.
const SAMPLE_STEP: f64 = 32.0;
/// `PASSES` of corner moves and added points.
const PASSES: usize = 2;
/// `BotNavigator` ROUTE_LINE_OFF: a bot farther off its line than this
/// leaves it, and joins a stretch's line only this near it.
const ROUTE_LINE_OFF: f64 = 128.0;

/// `BotNavGraph.wallClearance`: rays, hull half-width, and ray height.
const CLEARANCE_RAYS: usize = 16;
const HULL_HALF: f64 = 16.0;
const CLEARANCE_LIFT: f64 = 16.0;
/// `BotVision.isWalkable`: floor sample spacing and depth, the most the
/// floor may rise at once (Washoi STEP_HEIGHT), and the recheck spacing
/// where it rises more.
const FLOOR_SAMPLE_STEP: f64 = 64.0;
const FLOOR_SAMPLE_DEPTH: f64 = 64.0;
const WALK_STEP_UP: f64 = 18.0;
const WALK_RISE_STEP: f64 = 8.0;

/// What a line is built against, as TypeScript's `RouteWorld`.
pub(super) trait RouteWorld {
    /// A clear straight run for the standing hull with floor under it,
    /// origin to origin.
    fn walkable(&self, from: [f64; 3], to: [f64; 3]) -> bool;
    /// Gap from the hull to the nearest wall around an origin, and the way
    /// away from walls closer than `want`.
    fn clearance(&self, at: [f64; 3], want: f64) -> (f64, [f64; 2]);
}

/// The map's static collision as a `RouteWorld`.
pub(super) struct SightWorld<'a>(pub(super) &'a Sight);

impl RouteWorld for SightWorld<'_> {
    /// `BotVision.isWalkable`: the hull swept origin to origin, at its sides
    /// and centre, at the low and high planes of WALK_SWEEP_BOUNDS and
    /// between, then floor within 64 under every 64 units between the ends,
    /// rising at most a step at a time.
    fn walkable(&self, from: [f64; 3], to: [f64; 3]) -> bool {
        let (dx, dz) = (to[0] - from[0], to[2] - from[2]);
        let length = dx.hypot(dz);
        if length < 1e-6 {
            return true;
        }
        let (sx, sz) = (dz / length, -dx / length);
        for side in [-15.0, 0.0, 15.0] {
            for height in [-6.0, 17.0, 40.0] {
                let a = [from[0] + sx * side, from[1] + height, from[2] + sz * side];
                let b = [to[0] + sx * side, to[1] + height, to[2] + sz * side];
                if !self.0.clear(a, b) {
                    return false;
                }
            }
        }
        let samples = (length / FLOOR_SAMPLE_STEP).floor().max(1.0) as u32;
        let mut previous = self.floor_under(from, to, 0.0);
        for i in 1..=samples {
            let t = f64::from(i) / f64::from(samples);
            let Some(floor) = self.floor_under(from, to, t) else {
                if i < samples {
                    return false;
                }
                break;
            };
            if previous.is_some_and(|previous| floor - previous > WALK_STEP_UP)
                && !self.climbs_in_steps(from, to, f64::from(i - 1) / f64::from(samples), t)
            {
                return false;
            }
            previous = Some(floor);
        }
        true
    }

    /// `BotNavGraph.wallClearance`.
    fn clearance(&self, at: [f64; 3], want: f64) -> (f64, [f64; 2]) {
        let reach = want + HULL_HALF;
        let at = [at[0], at[1] + CLEARANCE_LIFT, at[2]];
        let (mut nearest, mut away_x, mut away_z) = (reach, 0.0, 0.0);
        for i in 0..CLEARANCE_RAYS {
            let angle = i as f64 / CLEARANCE_RAYS as f64 * std::f64::consts::TAU;
            let (dx, dz) = angle.sin_cos();
            let distance = self
                .0
                .reach(at, [at[0] + dx * reach, at[1], at[2] + dz * reach])
                * reach;
            nearest = nearest.min(distance);
            away_x -= dx * (reach - distance);
            away_z -= dz * (reach - distance);
        }
        let length = away_x.hypot(away_z);
        let away = if length > 1e-6 {
            [away_x / length, away_z / length]
        } else {
            [0.0, 0.0]
        };
        ((nearest - HULL_HALF).max(0.0), away)
    }
}

impl SightWorld<'_> {
    /// `BotVision._floorUnder`: the floor height within 64 under the point
    /// `t` of the way, or None.
    fn floor_under(&self, from: [f64; 3], to: [f64; 3], t: f64) -> Option<f64> {
        let at = lerp(from, to, t);
        let fraction = self.0.reach(at, [at[0], at[1] - FLOOR_SAMPLE_DEPTH, at[2]]);
        (fraction < 1.0).then(|| at[1] - fraction * FLOOR_SAMPLE_DEPTH)
    }

    /// `BotVision._climbsInSteps`.
    fn climbs_in_steps(&self, from: [f64; 3], to: [f64; 3], t0: f64, t1: f64) -> bool {
        let length = (to[0] - from[0]).hypot(to[2] - from[2]) * (t1 - t0);
        let steps = (length / WALK_RISE_STEP).ceil().max(1.0) as u32;
        let Some(mut previous) = self.floor_under(from, to, t0) else {
            return false;
        };
        for i in 1..=steps {
            match self.floor_under(from, to, t0 + (t1 - t0) * f64::from(i) / f64::from(steps)) {
                Some(floor) if floor - previous <= WALK_STEP_UP => previous = floor,
                _ => return false,
            }
        }
        true
    }
}

/// A built line: standing origins in order.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RouteLine {
    pub(super) points: Vec<[f64; 3]>,
    /// The waypoint the stretch ends before: a maneuver, a change of floor,
    /// or the route's end.
    pub(super) end: usize,
}

/// The waypoints from `from` a line can cover from a start at height
/// `start_y`: up to a maneuver or a change of floor (`lineEndStep`).
fn stretch_end(route: &Route, from: usize, start_y: f64) -> usize {
    let mut previous = start_y;
    let mut end = from;
    while let Some(point) = route.waypoints.get(end) {
        if route.maneuver_at(end).is_some() || (point[1] - previous).abs() > MAX_HEIGHT_DELTA {
            break;
        }
        previous = point[1];
        end += 1;
    }
    end
}

/// `buildRouteLine` from `start` through the waypoints from `from` to the
/// stretch's end, or None where the stretch is empty.
fn build(
    start: [f64; 3],
    route: &Route,
    from: usize,
    world: &impl RouteWorld,
) -> Option<RouteLine> {
    let end = stretch_end(route, from, start[1]);
    if end == from {
        return None;
    }
    let mut points = vec![start];
    points.extend_from_slice(&route.waypoints[from..end]);
    let mut points = pull_string(&points, world);
    for _ in 0..PASSES {
        points = move_corners_off_walls(&points, world);
        points = add_points_off_walls(&points, world);
    }
    Some(RouteLine { points, end })
}

/// Keep only the points the hull cannot run straight past.
fn pull_string(raw: &[[f64; 3]], world: &impl RouteWorld) -> Vec<[f64; 3]> {
    let mut kept = vec![raw[0]];
    let mut from = 0;
    while from < raw.len() - 1 {
        let next = (from + 2..raw.len())
            .rev()
            .find(|&far| world.walkable(raw[from], raw[far]))
            .unwrap_or(from + 1);
        kept.push(raw[next]);
        from = next;
    }
    kept
}

/// A point moved off the walls near it, as far as the stretches either side
/// stay walkable.
fn off_walls(
    point: [f64; 3],
    before: [f64; 3],
    after: [f64; 3],
    world: &impl RouteWorld,
) -> [f64; 3] {
    let (gap, away) = world.clearance(point, LINE_CLEARANCE);
    if gap >= LINE_CLEARANCE || away == [0.0, 0.0] {
        return point;
    }
    let (mut best, mut best_gap) = (point, gap);
    let mut push = PUSH_STEP;
    while push <= LINE_CLEARANCE - gap + 1e-6 {
        let moved = [
            point[0] + away[0] * push,
            point[1],
            point[2] + away[1] * push,
        ];
        if !world.walkable(before, moved) || !world.walkable(moved, after) {
            break;
        }
        let gap = world.clearance(moved, LINE_CLEARANCE).0;
        if gap > best_gap {
            best = moved;
            best_gap = gap;
        }
        push += PUSH_STEP;
    }
    best
}

fn move_corners_off_walls(points: &[[f64; 3]], world: &impl RouteWorld) -> Vec<[f64; 3]> {
    let mut moved = points.to_vec();
    for i in 1..moved.len().saturating_sub(1) {
        moved[i] = off_walls(moved[i], moved[i - 1], moved[i + 1], world);
    }
    moved
}

/// Where a stretch passes closer than `LINE_TIGHT` to a wall, add a point
/// there moved off it.
fn add_points_off_walls(points: &[[f64; 3]], world: &impl RouteWorld) -> Vec<[f64; 3]> {
    let mut out = vec![points[0]];
    for &b in &points[1..] {
        let a = out[out.len() - 1];
        let samples = ((b[0] - a[0]).hypot(b[2] - a[2]) / SAMPLE_STEP).floor() as u32;
        let mut tightest = None;
        let mut tightest_gap = LINE_TIGHT;
        for s in 1..samples {
            let at = lerp(a, b, f64::from(s) / f64::from(samples));
            let gap = world.clearance(at, LINE_CLEARANCE).0;
            if gap < tightest_gap {
                tightest_gap = gap;
                tightest = Some(at);
            }
        }
        if let Some(tightest) = tightest {
            let moved = off_walls(tightest, a, b, world);
            if moved != tightest {
                out.push(moved);
            }
        }
        out.push(b);
    }
    out
}

/// `RouteLine.pointAlong`: the point `ahead` units along `points` past the
/// point on the line nearest `at`, searched on stretch `from` and the two
/// after it, and the stretch that nearest point lies on.
pub(super) fn point_along(
    points: &[[f64; 3]],
    at: Vec3,
    ahead: f64,
    from: usize,
) -> ([f64; 3], usize) {
    if points.len() < 2 {
        return (points[0], 0);
    }
    let first = from.min(points.len() - 2);
    let (mut stretch, mut best_t, mut best) = (first, 0.0, f64::INFINITY);
    for i in first..(points.len() - 1).min(first + 3) {
        let (distance, t) = nearest(at, points[i], points[i + 1]);
        if distance < best {
            (stretch, best_t, best) = (i, t, distance);
        }
    }
    let mut left = ahead;
    let mut a = lerp(points[stretch], points[stretch + 1], best_t);
    for &b in &points[stretch + 1..] {
        let length = (b[0] - a[0]).hypot(b[2] - a[2]);
        if length >= left {
            let share = if length < 1e-6 { 1.0 } else { left / length };
            return (lerp(a, b, share), stretch);
        }
        left -= length;
        a = b;
    }
    (points[points.len() - 1], stretch)
}

/// `nearestOnLine`: the stretch of `points` nearest `at` over the whole
/// line, and how far off it `at` is, horizontally.
fn nearest_on_line(points: &[[f64; 3]], at: Vec3) -> (usize, f64) {
    (0..points.len().saturating_sub(1))
        .map(|i| (i, nearest(at, points[i], points[i + 1]).0))
        .fold((0, f64::INFINITY), |best, (i, distance)| {
            if distance < best.1 {
                (i, distance)
            } else {
                best
            }
        })
}

/// Horizontal distance from `at` to the segment, and the fraction along it
/// of the nearest point.
fn nearest(at: Vec3, a: [f64; 3], b: [f64; 3]) -> (f64, f64) {
    let (dx, dz) = (b[0] - a[0], b[2] - a[2]);
    let length_sq = dx * dx + dz * dz;
    let t = if length_sq < 1.0 {
        1.0
    } else {
        (((at.x - a[0]) * dx + (at.z - a[2]) * dz) / length_sq).clamp(0.0, 1.0)
    };
    ((at.x - (a[0] + dx * t)).hypot(at.z - (a[2] + dz * t)), t)
}

fn lerp(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Where the walk's line came from this decision, for traces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LineSource {
    /// The line held from an earlier decision.
    Held,
    /// The stretch's own line, joined at its nearest point.
    Shared,
    /// A straight run from the bot onto the stretch's line.
    RunOnto,
    /// A line built from where the bot stands.
    Own,
}

impl LineSource {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Shared => "shared",
            Self::RunOnto => "run_onto",
            Self::Own => "own",
        }
    }
}

/// A walk's held line and the stretch lines built for it
/// (`BotNavigator.followRouteLine` and `routeLineFor`).
#[derive(Default)]
pub(super) struct HeldLine {
    held: Option<Held>,
    /// Each stretch's line from its waypoint before, by its first waypoint.
    stretches: HashMap<usize, Option<RouteLine>>,
    /// For traces: where this decision's line came from, and the lines
    /// built since the last record.
    pub(super) source: Option<&'static str>,
    pub(super) builds: u32,
}

struct Held {
    line: RouteLine,
    /// The waypoint the line was taken at.
    from: usize,
    stretch: usize,
}

impl HeldLine {
    /// The line to walk from `position` with waypoint `next` ahead: the
    /// held one while the walk is on it, else a new one, as
    /// `followRouteLine` keeps its line. Then where to make for, `ahead`
    /// along it. None where no stretch of plain walking starts at `next`.
    pub(super) fn walk(
        &mut self,
        route: &Route,
        next: usize,
        position: Vec3,
        ahead: f64,
        world: &impl RouteWorld,
    ) -> Option<[f64; 3]> {
        if let Some(held) = &self.held {
            let near = point_along(&held.line.points, position, 0.0, held.stretch).0;
            if next < held.from
                || next >= held.line.end
                || (near[0] - position.x).hypot(near[2] - position.z) > ROUTE_LINE_OFF
            {
                self.held = None;
            }
        }
        let source = if self.held.is_some() {
            LineSource::Held
        } else {
            let (held, source) = self.line_for(route, next, position, world)?;
            self.held = Some(held);
            source
        };
        self.source = Some(source.name());
        let held = self.held.as_mut().expect("held above");
        let (point, stretch) = point_along(&held.line.points, position, ahead, held.stretch);
        held.stretch = stretch;
        Some(point)
    }

    /// The held line while waypoint `next` is on it, and the stretch the
    /// walk is on.
    pub(super) fn line(&self, next: usize) -> Option<(&[[f64; 3]], usize)> {
        self.held
            .as_ref()
            .filter(|held| held.from <= next && next < held.line.end)
            .map(|held| (held.line.points.as_slice(), held.stretch))
    }

    /// Drop the held line, as when the walk leaves the route's plain
    /// walking; the stretch lines stay built.
    #[cfg(test)]
    fn release(&mut self) {
        self.held = None;
    }

    /// `routeLineFor`: the stretch's line joined where it passes nearest,
    /// a straight run onto it, or a line from where the bot stands.
    fn line_for(
        &mut self,
        route: &Route,
        next: usize,
        position: Vec3,
        world: &impl RouteWorld,
    ) -> Option<(Held, LineSource)> {
        let here = [position.x, position.y, position.z];
        if !self.stretches.contains_key(&next) {
            let start = if next == 0 {
                route.entry
            } else {
                route.waypoints[next - 1]
            };
            let line = build(start, route, next, world);
            self.builds += 1;
            self.stretches.insert(next, line);
        }
        if let Some(shared) = self.stretches[&next].clone() {
            let (join, distance) = nearest_on_line(&shared.points, position);
            if distance <= ROUTE_LINE_OFF {
                let held = Held {
                    line: shared,
                    from: next,
                    stretch: join,
                };
                return Some((held, LineSource::Shared));
            }
            if let Some(&onto) = shared.points.get(join + 1)
                && world.walkable(here, onto)
            {
                let mut points = vec![here];
                points.extend_from_slice(&shared.points[join + 1..]);
                let line = RouteLine {
                    points,
                    end: shared.end,
                };
                let held = Held {
                    line,
                    from: next,
                    stretch: 0,
                };
                return Some((held, LineSource::RunOnto));
            }
        }
        let line = build(here, route, next, world)?;
        self.builds += 1;
        let held = Held {
            line,
            from: next,
            stretch: 0,
        };
        Some((held, LineSource::Own))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A floor at y 0 from x -1000 to 1000 and z -1000 to 1000, with
    /// optional walls as boxes the hull can't cross, and a gap in the floor.
    struct Toy {
        walls: Vec<([f64; 2], [f64; 2])>,
        gap: Option<([f64; 2], [f64; 2])>,
    }

    fn inside(p: [f64; 2], b: &([f64; 2], [f64; 2])) -> bool {
        p[0] >= b.0[0] && p[0] <= b.1[0] && p[1] >= b.0[1] && p[1] <= b.1[1]
    }

    impl RouteWorld for Toy {
        fn walkable(&self, from: [f64; 3], to: [f64; 3]) -> bool {
            let length = (to[0] - from[0]).hypot(to[2] - from[2]);
            let steps = (length / 4.0).ceil().max(1.0) as u32;
            (0..=steps).all(|i| {
                let p = lerp(from, to, f64::from(i) / f64::from(steps));
                let blocked = self.walls.iter().any(|wall| {
                    let grown = (
                        [wall.0[0] - 15.0, wall.0[1] - 15.0],
                        [wall.1[0] + 15.0, wall.1[1] + 15.0],
                    );
                    inside([p[0], p[2]], &grown)
                });
                let fallen = i > 0
                    && i < steps
                    && self
                        .gap
                        .as_ref()
                        .is_some_and(|gap| inside([p[0], p[2]], gap));
                !blocked && !fallen
            })
        }

        fn clearance(&self, at: [f64; 3], want: f64) -> (f64, [f64; 2]) {
            let mut nearest = want + HULL_HALF;
            let (mut ax, mut az) = (0.0, 0.0);
            for wall in &self.walls {
                let cx = at[0].clamp(wall.0[0], wall.1[0]);
                let cz = at[2].clamp(wall.0[1], wall.1[1]);
                let (dx, dz) = (at[0] - cx, at[2] - cz);
                let distance = dx.hypot(dz);
                if distance < want + HULL_HALF && distance > 1e-6 {
                    ax += dx / distance * (want + HULL_HALF - distance);
                    az += dz / distance * (want + HULL_HALF - distance);
                }
                nearest = nearest.min(distance);
            }
            let length = ax.hypot(az);
            let away = if length > 1e-6 {
                [ax / length, az / length]
            } else {
                [0.0, 0.0]
            };
            ((nearest - HULL_HALF).max(0.0), away)
        }
    }

    fn route(waypoints: &[[f64; 3]]) -> Route {
        Route {
            entry: [0.0, 0.0, 0.0],
            waypoints: waypoints.to_vec(),
            maneuvers: Vec::new(),
            learned_route: None,
            resumed_from: None,
            pickup: None,
        }
    }

    #[test]
    fn pulls_the_string_past_a_waypoint_with_a_clear_run() {
        // A dogleg of waypoints across an open room: the line runs straight
        // to the end.
        let open = Toy {
            walls: Vec::new(),
            gap: None,
        };
        let route = route(&[[100.0, 0.0, 100.0], [0.0, 0.0, 300.0], [0.0, 0.0, 500.0]]);
        let line = build([0.0; 3], &route, 0, &open).unwrap();
        assert_eq!(line.points, vec![[0.0; 3], [0.0, 0.0, 500.0]]);
        assert_eq!(line.end, 3);
    }

    #[test]
    fn refuses_a_shortcut_across_a_gap_in_the_floor() {
        // The waypoints skirt a pit; the straight run over it has no floor.
        let pit = Toy {
            walls: Vec::new(),
            gap: Some(([-100.0, 150.0], [100.0, 350.0])),
        };
        let route = route(&[[200.0, 0.0, 100.0], [200.0, 0.0, 400.0], [0.0, 0.0, 500.0]]);
        let line = build([0.0; 3], &route, 0, &pit).unwrap();
        assert!(line.points.contains(&[200.0, 0.0, 400.0]), "{line:?}");
        assert!(!pit.walkable([0.0; 3], [0.0, 0.0, 500.0]));
    }

    #[test]
    fn the_maps_floor_refuses_a_run_over_a_gap_and_a_step_too_high() {
        use crate::games::washoi::perception::sight::tests::{cube, encode};
        let world = |solids: &[Vec<[f32; 4]>]| Sight::parse(&encode("gap", solids, &[])).unwrap();
        // Origins stand 24 over the floor at y 0.
        let (from, to) = ([0.0, 24.0, 0.0], [0.0, 24.0, 600.0]);
        let floor = world(&[cube([-100.0, -10.0, -100.0], [100.0, 0.0, 700.0])]);
        assert!(SightWorld(&floor).walkable(from, to));
        // A 100-wide gap across the run: floor sampled every 64 finds it.
        let gapped = world(&[
            cube([-100.0, -10.0, -100.0], [100.0, 0.0, 250.0]),
            cube([-100.0, -10.0, 350.0], [100.0, 0.0, 700.0]),
        ]);
        assert!(!SightWorld(&gapped).walkable(from, to));
        // A ledge 30 up, more than a step, halfway: refused; 12 up is a step.
        for (rise, walkable) in [(30.0, false), (12.0, true)] {
            let ledge = world(&[
                cube([-100.0, -10.0, -100.0], [100.0, 0.0, 300.0]),
                cube([-100.0, -10.0, 300.0], [100.0, rise, 700.0]),
            ]);
            let end = [0.0, 24.0 + f64::from(rise), 600.0];
            assert_eq!(SightWorld(&ledge).walkable(from, end), walkable, "{rise}");
        }
    }

    #[test]
    fn moves_a_corner_off_the_wall_it_hugs() {
        // Round the corner of a wall block: the corner waypoint sits 12 off
        // the wall's corner; the line moves it out toward 48 of clearance,
        // in steps of 16 that fit in what it lacks.
        let block = Toy {
            walls: vec![([-500.0, 200.0], [80.0, 1000.0])],
            gap: None,
        };
        let corner = [100.0, 0.0, 180.0];
        let route = route(&[corner, [140.0, 0.0, 600.0]]);
        let line = build([-300.0, 0.0, 100.0], &route, 0, &block).unwrap();
        let moved = line.points[1];
        let gap = |p: [f64; 3]| block.clearance(p, LINE_CLEARANCE).0;
        assert!(gap(moved) > gap(corner), "{line:?}");
        assert_eq!(line.points.last(), Some(&[140.0, 0.0, 600.0]));
    }

    #[test]
    fn adds_a_point_off_a_wall_a_stretch_runs_along() {
        // A long run with a pillar beside its middle, 10 off the line.
        let pillar = Toy {
            walls: vec![([26.0, 380.0], [60.0, 420.0])],
            gap: None,
        };
        let route = route(&[[0.0, 0.0, 800.0]]);
        let line = build([0.0; 3], &route, 0, &pillar).unwrap();
        assert!(line.points.len() > 2, "{line:?}");
        assert!(
            line.points[1][0] < 0.0,
            "pushed away from the pillar: {line:?}"
        );
    }

    #[test]
    fn a_stretch_ends_at_a_change_of_floor() {
        let route = route(&[[0.0, 0.0, 200.0], [0.0, 0.0, 400.0], [0.0, -120.0, 500.0]]);
        assert_eq!(stretch_end(&route, 0, 0.0), 2);
        assert_eq!(stretch_end(&route, 2, 0.0), 2);
    }

    #[test]
    fn holds_its_line_rejoins_near_it_and_builds_its_own_far_off() {
        let open = Toy {
            walls: Vec::new(),
            gap: None,
        };
        let route = route(&[[0.0, 0.0, 400.0], [400.0, 0.0, 400.0]]);
        let mut line = HeldLine::default();
        // On the stretch's own line from the entry: shared, then held.
        let ahead = line
            .walk(&route, 0, Vec3::new(10.0, 0.0, 100.0), 64.0, &open)
            .unwrap();
        assert_eq!(line.source, Some("shared"));
        assert!(ahead[0] > 10.0 && ahead[2] > 100.0, "{ahead:?}");
        line.walk(&route, 1, Vec3::new(40.0, 0.0, 140.0), 64.0, &open);
        assert_eq!(line.source, Some("held"));
        assert_eq!(line.builds, 1);
        // Pushed 300 off it: the stretch's line is too far, and a straight
        // run onto its next point is open.
        line.release();
        line.walk(&route, 1, Vec3::new(-300.0, 0.0, 100.0), 64.0, &open);
        assert_eq!(line.source, Some("run_onto"));
        // Past the stretch's end there is no line.
        assert!(
            line.walk(&route, 2, Vec3::new(400.0, 0.0, 400.0), 64.0, &open)
                .is_none()
        );
    }

    #[test]
    fn point_along_rounds_a_corner_and_follows_the_line_forward() {
        let points = [[0.0, 0.0, 0.0], [0.0, 0.0, 300.0], [400.0, 0.0, 300.0]];
        let (ahead, stretch) = point_along(&points, Vec3::new(10.0, 0.0, 280.0), 64.0, 0);
        assert_eq!(stretch, 0);
        assert!(
            ahead[0] > 0.0 && (ahead[2] - 300.0).abs() < 1e-9,
            "{ahead:?}"
        );
        // Stretch 1 on: the first stretch is not searched again.
        let (_, stretch) = point_along(&points, Vec3::new(0.0, 0.0, 100.0), 64.0, 1);
        assert_eq!(stretch, 1);
    }
}

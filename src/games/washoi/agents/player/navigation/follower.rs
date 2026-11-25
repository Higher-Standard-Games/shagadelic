use std::collections::BTreeMap;
use std::time::Duration;

use crate::games::washoi::SimTime;
use shagger_shared::net::game_state::{Vec3, Velocity};

use crate::games::washoi::agents::player::navigation::traversal::{Outcome, Traversal};
use crate::games::washoi::agents::player::navigation::{
    PlayerInput, Route, WashoiNavigation, horizontal_distance, jump_up_to_waypoint,
    look_along_route, sprint_on_long_legs,
};
use crate::games::washoi::{Difficulty, Sight, agents::player::strategy::goals::OwnVitals};

const STALL_TIMEOUT: Duration = Duration::from_millis(800);
const RETRY_DELAY: Duration = Duration::from_secs(5);
const MIN_PROGRESS: f64 = 8.0;
const DISPLACEMENT: f64 = 128.0;
const MAX_BLOCKS: usize = 16;
const LOCAL_PICKUP_RADIUS: f64 = 24.0;
// Washoi takes a beneficial item on the first tick a player overlaps it. An
// item that stays after this wait is one the bot can't take, such as full
// ammo or a new weapon that needs USE to swap. A bot at a plain goal also
// waits this long before choosing its next one.
const ARRIVAL_DWELL: Duration = Duration::from_millis(400);
const DECLINED_PICKUP: Duration = Duration::from_secs(15);
// Planning visits every goal, so a bot with nowhere to go retries slowly.
const REPLAN_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy)]
pub(in crate::games::washoi) struct RouteObservation<'a> {
    pub(in crate::games::washoi) now: SimTime,
    pub(in crate::games::washoi) vitals: OwnVitals,
    pub(in crate::games::washoi) velocity: Option<Velocity>,
    pub(in crate::games::washoi) difficulty: Difficulty,
    /// The pickups the bot believes are up (`perception::items`).
    pub(in crate::games::washoi) pickup_up: &'a [bool],
    pub(in crate::games::washoi) allow_takeoff: bool,
    pub(in crate::games::washoi) landed_id: Option<&'a str>,
    /// The map's static collision, for walks back onto the graph.
    pub(in crate::games::washoi) sight: Option<&'a Sight>,
    /// Fight movement crosses the walk, so slow progress is no stall.
    pub(in crate::games::washoi) fighting: bool,
    /// In TypeScript's FIGHT state, which follows no path, so no maneuver
    /// starts (`StateMachineBrain.ts`, `_fight.fight`).
    pub(in crate::games::washoi) hold_maneuvers: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct Leg {
    from: [f64; 3],
    to: [f64; 3],
}

struct Progress {
    next: usize,
    distance: f64,
    updated: SimTime,
}

enum RouteSlot {
    Shared(usize),
    Planned(Box<Route>),
}

impl RouteSlot {
    fn get<'a>(&'a self, navigation: &'a WashoiNavigation) -> &'a Route {
        match self {
            Self::Shared(index) => &navigation.routes[*index],
            Self::Planned(route) => route,
        }
    }
}

struct ActiveRoute {
    route: RouteSlot,
    next: usize,
    progress: Option<Progress>,
    arrived: Option<SimTime>,
    traversal: Option<Traversal>,
    /// The walk's held line (`route_line`).
    line: super::route_line::HeldLine,
}

#[derive(Default)]
pub(in crate::games::washoi) struct RouteFollower {
    active: Option<ActiveRoute>,
    last_position: Option<Vec3>,
    blocked: Vec<(Leg, SimTime)>,
    /// Pickups waited out at the spot and not taken, until the hold ends.
    declined: BTreeMap<usize, SimTime>,
    /// Learned routes whose takeoff failed, by recording ID, until the retry
    /// delay is out, as a failed traversal blocks its leg.
    blocked_takeoffs: Vec<(String, SimTime)>,
    /// The pickup the goal arbiter chose, when it chose one.
    goal: Option<usize>,
    replan_at: Option<SimTime>,
    /// The last waypoint the bot reached. Momentum can carry a bot past the
    /// end of a route and out of every node's attach radius.
    anchor: Option<[f64; 3]>,
    exact: bool,
}

impl RouteFollower {
    /// Drop the active route, such as on death or a learned traversal, and
    /// keep what the bot learned about blocked legs and missing pickups.
    /// Both memories expire on their own.
    pub(in crate::games::washoi) fn detach(&mut self) {
        self.active = None;
        self.last_position = None;
        self.replan_at = None;
        self.anchor = None;
        self.exact = false;
    }

    /// Keep off the learned route `id` for the retry delay, after its
    /// recording failed, as a failed traversal blocks its leg.
    pub(in crate::games::washoi) fn block_takeoff(&mut self, id: &str, now: SimTime) {
        if self.blocked_takeoffs.len() == MAX_BLOCKS {
            self.blocked_takeoffs.remove(0);
        }
        self.blocked_takeoffs
            .push((id.to_owned(), now + RETRY_DELAY));
    }

    /// Give up the leg under way, as when the bot is blocked on it, so the
    /// next plan goes another way for a while.
    pub(in crate::games::washoi) fn give_up_leg(
        &mut self,
        navigation: &WashoiNavigation,
        now: SimTime,
    ) {
        if let Some(active) = self.active.take() {
            let route = active.route.get(navigation);
            if active.next < route.waypoints.len() {
                if self.blocked.len() == MAX_BLOCKS {
                    self.blocked.remove(0);
                }
                self.blocked
                    .push((route.leg(active.next), now + RETRY_DELAY));
            }
        }
        self.last_position = None;
        self.exact = false;
    }

    /// Whether the bot waited at this pickup and couldn't take it, such as
    /// full ammo, and still holds off it.
    pub(in crate::games::washoi) fn pickup_declined(&self, id: usize, now: SimTime) -> bool {
        self.declined.get(&id).is_some_and(|until| now < *until)
    }

    /// Whether a leg into `point` failed recently, so walks that end there
    /// should wait.
    pub(in crate::games::washoi) fn blocked_toward(&self, point: [f64; 3], now: SimTime) -> bool {
        self.blocked
            .iter()
            .any(|(leg, until)| leg.to == point && now < *until)
    }

    /// The pickup the route under way leads to, if any.
    pub(in crate::games::washoi) fn active_pickup(
        &self,
        navigation: &WashoiNavigation,
    ) -> Option<usize> {
        let active = self.active.as_ref()?;
        Some(active.route.get(navigation).pickup?.id)
    }

    /// The goal set has no route from here: the last plan for it failed and
    /// the follower waits to plan again.
    pub(in crate::games::washoi) fn goal_unplanned(&self, now: SimTime) -> Option<usize> {
        self.goal
            .filter(|_| self.active.is_none() && self.replan_at.is_some_and(|at| now < at))
    }

    /// Walk only to this pickup, as the goal arbiter chose, or choose freely
    /// with None. A route under way to anything else is dropped.
    pub(in crate::games::washoi) fn set_goal(&mut self, pickup: Option<usize>) {
        if self.goal != pickup {
            self.goal = pickup;
            if pickup.is_some() {
                self.active = None;
            }
        }
    }

    /// The pickup at the end of the route under way, once its last leg has
    /// begun, for an item slide.
    pub(in crate::games::washoi) fn item_leg(
        &self,
        navigation: &WashoiNavigation,
    ) -> Option<crate::games::washoi::agents::player::navigation::slide::ItemLeg> {
        let active = self.active.as_ref()?;
        if self.exact || active.traversal.is_some() {
            return None;
        }
        let route = active.route.get(navigation);
        // A walk to a learned takeoff ends short of its pickup.
        let pickup = route.pickup.filter(|_| route.learned_route.is_none())?;
        let last = route.waypoints.len().checked_sub(1)?;
        (active.next == last && route.maneuver_at(last).is_none()).then(|| {
            crate::games::washoi::agents::player::navigation::slide::ItemLeg {
                pickup: pickup.id,
                position: route.waypoints[last],
            }
        })
    }

    /// The heading of the route ahead when it is a lane to slide down,
    /// after TypeScript `BotTraversal.laneOpen`: the next 520 units of the
    /// walk stay within 64 units of the line to the next waypoint, widening
    /// past 320, with no maneuver or change of floor on the way, and a
    /// straight 320-unit run along it is walkable.
    pub(in crate::games::washoi) fn slide_lane(
        &self,
        navigation: &WashoiNavigation,
        position: Vec3,
        sight: Option<&Sight>,
    ) -> Option<f64> {
        self.lane(navigation, position, sight, 320.0, 520.0, 64.0)
    }

    /// An open stretch to strafe-jump down, after TypeScript
    /// `BotTraversal.routeStrafeLane` (`super::hop_lane_along`). The last
    /// walk to a pickup needs 800 units left, so the bot can stop at the
    /// item. Without one, why not, for traces.
    pub(in crate::games::washoi) fn hop_lane(
        &self,
        navigation: &WashoiNavigation,
        position: Vec3,
        sight: Option<&Sight>,
    ) -> Result<crate::games::washoi::agents::player::navigation::hop::HopLane, &'static str> {
        const ITEM_MIN_REMAINING: f64 = 800.0;
        let active = self.active.as_ref().ok_or("no_route")?;
        if self.exact {
            return Err("exact");
        }
        if active.traversal.is_some() {
            return Err("traversal");
        }
        let route = active.route.get(navigation);
        let last = route.waypoints.len().checked_sub(1).ok_or("no_route")?;
        if route.pickup.is_some()
            && active.next == last
            && horizontal_distance(position, route.waypoints[last]) < ITEM_MIN_REMAINING
        {
            return Err("item_near");
        }
        super::hop_lane_along(route, active.next, position, sight.ok_or("no_sight")?)
    }

    /// `BotTraversal.laneOpen` from the next waypoint (`Route::open_lane`).
    fn lane(
        &self,
        navigation: &WashoiNavigation,
        position: Vec3,
        sight: Option<&Sight>,
        run: f64,
        reach: f64,
        width: f64,
    ) -> Option<f64> {
        let sight = sight?;
        let active = self.active.as_ref()?;
        if self.exact || active.traversal.is_some() {
            return None;
        }
        active
            .route
            .get(navigation)
            .open_lane(active.next, position, sight, run, reach, width)
    }

    /// The waypoint the walk makes for, and whether its leg leads into a
    /// jump pad or teleporter, for steering (TypeScript
    /// `BotSteering.target` and `routeUsesTrigger`). None on a maneuver.
    pub(in crate::games::washoi) fn walk_target(
        &self,
        navigation: &WashoiNavigation,
    ) -> Option<(Vec3, bool)> {
        let active = self.active.as_ref()?;
        if self.exact || active.traversal.is_some() {
            return None;
        }
        let route = active.route.get(navigation);
        let point = route.waypoints.get(active.next)?;
        let trigger = |at: usize| {
            route
                .maneuver_at(at)
                .is_some_and(super::traversal::Maneuver::uses_trigger)
        };
        Some((
            Vec3::new(point[0], point[1], point[2]),
            trigger(active.next) || trigger(active.next + 1),
        ))
    }

    /// A failed takeoff is still cooling down.
    pub(in crate::games::washoi) fn takeoff_blocked(&self, now: SimTime) -> bool {
        self.blocked_takeoffs.iter().any(|(_, until)| now < *until)
    }

    /// A route is under way.
    pub(in crate::games::washoi) fn routed(&self) -> bool {
        self.active.is_some()
    }

    pub(in crate::games::washoi) fn traversing(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|route| route.traversal.is_some())
    }

    pub(in crate::games::washoi) fn exact(&self) -> bool {
        self.exact
    }

    /// A maneuver under way that has not yet taken off, so a fight may
    /// still interrupt it.
    pub(in crate::games::washoi) fn approaching_maneuver(&self) -> bool {
        self.active
            .as_ref()
            .and_then(|route| route.traversal.as_ref())
            .is_some_and(|traversal| !traversal.committed())
    }

    /// For traces: where this decision's walk line came from, and the lines
    /// built since the last record.
    pub(in crate::games::washoi) fn line_trace(&self) -> (Option<&'static str>, u32) {
        self.active
            .as_ref()
            .map_or((None, 0), |active| (active.line.source, active.line.builds))
    }

    pub(in crate::games::washoi) fn clear_line_trace(&mut self) {
        if let Some(active) = &mut self.active {
            active.line.source = None;
            active.line.builds = 0;
        }
    }

    /// The landing of a drop the walk is about to take, after TypeScript
    /// `tryStartLedgeSlideJump`: the leg the bot walks drops more than a
    /// step, or the one after it does. A maneuver leg is not a walk.
    pub(in crate::games::washoi) fn ledge_leg(
        &self,
        navigation: &WashoiNavigation,
        position: Vec3,
    ) -> Option<Vec3> {
        let active = self.active.as_ref()?;
        if self.exact || active.traversal.is_some() {
            return None;
        }
        active
            .route
            .get(navigation)
            .drop_ahead(active.next, position)
    }

    pub(in crate::games::washoi) fn takeoff<'a>(
        &'a self,
        navigation: &'a WashoiNavigation,
    ) -> Option<&'a str> {
        let active = self.active.as_ref()?;
        let route = active.route.get(navigation);
        if self.exact || route.maneuver_at(active.next).is_some() {
            return None;
        }
        (active.next >= route.waypoints.len() - 1)
            .then_some(route.learned_route.as_deref())
            .flatten()
    }

    pub(in crate::games::washoi) fn steer(
        &mut self,
        navigation: &WashoiNavigation,
        position: Vec3,
        observation: RouteObservation<'_>,
    ) -> PlayerInput {
        let RouteObservation {
            now,
            vitals,
            velocity,
            difficulty,
            pickup_up,
            allow_takeoff,
            landed_id,
            sight,
            fighting,
            hold_maneuvers,
        } = observation;
        self.exact = false;
        let traversing = self.traversing();
        self.blocked.retain(|(_, until)| now < *until);
        self.blocked_takeoffs.retain(|(_, until)| now < *until);
        self.declined.retain(|_, until| now < *until);
        let previous = self.last_position.replace(position);
        let observed_exit = previous.is_some_and(|before| {
            self.active.as_ref().is_some_and(|active| {
                active
                    .route
                    .get(navigation)
                    .maneuver_at(active.next + 1)
                    .is_some_and(|action| action.observed_teleport(before, position))
            })
        });
        if !traversing
            && !observed_exit
            && previous.is_some_and(|previous| {
                (position.x - previous.x)
                    .hypot(position.z - previous.z)
                    .hypot(position.y - previous.y)
                    > DISPLACEMENT
            })
        {
            self.active = None;
            self.anchor = None;
        }
        if self.active.is_none() {
            let goal = self.goal;
            let unblocked = |route: &Route| {
                let chosen =
                    goal.is_none_or(|id| route.pickup.is_some_and(|pickup| pickup.id == id));
                let available = route.pickup.is_none_or(|pickup| {
                    goal == Some(pickup.id)
                        || pickup.benefits(vitals)
                            && !self.declined.contains_key(&pickup.id)
                            && pickup_up.get(pickup.id) != Some(&false)
                });
                let takeoff_open = route.learned_route.as_deref().is_none_or(|id| {
                    !self
                        .blocked_takeoffs
                        .iter()
                        .any(|(blocked, _)| blocked == id)
                });
                chosen
                    && available
                    && takeoff_open
                    && (0..route.waypoints.len())
                        .all(|next| !self.blocked.iter().any(|(leg, _)| *leg == route.leg(next)))
            };
            let continued = landed_id.and_then(|id| {
                navigation.route_at(position, |route| {
                    route.resumed_from.as_deref() == Some(id) && unblocked(route)
                })
            });
            let score = |route: &Route| {
                route.pickup.map_or(0.0, |pickup| {
                    let travel = route
                        .waypoints
                        .iter()
                        .fold((route.entry, 0.0), |(from, total), &to| {
                            let distance = (to[0] - from[0]).hypot(to[2] - from[2]);
                            (to, total + distance)
                        })
                        .1;
                    pickup.value(vitals, difficulty) / (1.0 + travel / 512.0)
                })
            };
            let shared = continued.or_else(|| {
                navigation.select_route(
                    position,
                    |route| {
                        route.resumed_from.is_none()
                            && (allow_takeoff || route.learned_route.is_none())
                            && unblocked(route)
                    },
                    score,
                )
            });
            let route = match shared {
                Some(index) => RouteSlot::Shared(index),
                None => {
                    if self.replan_at.is_some_and(|at| now < at) {
                        return PlayerInput::default();
                    }
                    // A bot that can't take off now plans with the learned
                    // links masked, but for a pocket whose way out is one
                    // (`Planner::walks`).
                    let planned = navigation.plan(
                        position,
                        self.anchor,
                        sight,
                        allow_takeoff && self.blocked_takeoffs.is_empty(),
                        unblocked,
                        score,
                    );
                    let Some(route) = planned else {
                        self.replan_at = Some(now + REPLAN_DELAY);
                        return PlayerInput::default();
                    };
                    RouteSlot::Planned(Box::new(route))
                }
            };
            self.replan_at = None;
            self.active = Some(ActiveRoute {
                route,
                next: 0,
                progress: None,
                arrived: None,
                traversal: None,
                line: super::route_line::HeldLine::default(),
            });
        }

        let ActiveRoute {
            route,
            next,
            progress,
            arrived,
            traversal,
            line,
        } = self.active.as_mut().expect("route selected");
        let route = route.get(navigation);
        let reached = *next;
        if observed_exit
            || route.maneuver_at(*next + 1).is_some_and(|action| {
                if traversal.is_some() {
                    action.enter_before_waypoint(position) && action.pad_launch(position, velocity)
                } else {
                    action.enter_before_waypoint(position) || action.pad_launch(position, velocity)
                }
            })
        {
            *next += 1;
            *traversal = None;
        }
        let mut input = WashoiNavigation::steer_on_line(
            route,
            next,
            position,
            velocity,
            super::ARRIVAL_RADIUS,
            sight.map(|sight| (&mut *line, sight)),
        );
        if sight.is_none() {
            line.source = Some("no_sight");
        }
        // Roaming walks sprint as objective walks do. Live, the Rust bot
        // topped out near 330 u/s while the TypeScript bot ran at 380 to 430.
        sprint_on_long_legs(&mut input, route, *next, position, velocity);
        jump_up_to_waypoint(&mut input, route, *next, position, velocity);
        look_along_route(&mut input, route, *next, position, line.line(*next));
        if *next > reached {
            self.anchor = Some(route.waypoints[*next - 1]);
        }
        if let Some(maneuver) = route.maneuver_at(*next) {
            if hold_maneuvers && traversal.is_none() {
                return PlayerInput {
                    yaw: input.yaw,
                    ..PlayerInput::default()
                };
            }
            self.exact = true;
            let state = traversal.get_or_insert_with(|| Traversal::new(maneuver, now));
            return match state.update(position, velocity, now) {
                Outcome::Running(input) => input,
                Outcome::Arrived(yaw) => {
                    self.anchor = Some(route.waypoints[*next]);
                    *next += 1;
                    *traversal = None;
                    *progress = None;
                    PlayerInput {
                        yaw: yaw.unwrap_or(input.yaw),
                        ..PlayerInput::default()
                    }
                }
                Outcome::Failed => {
                    if self.blocked.len() == MAX_BLOCKS {
                        self.blocked.remove(0);
                    }
                    self.blocked.push((route.leg(*next), now + RETRY_DELAY));
                    self.active = None;
                    PlayerInput::default()
                }
            };
        }
        // A walk to a learned takeoff ends out of sight of its pickup, and
        // one waited out declines it.
        let at_pickup = route.pickup.filter(|_| route.learned_route.is_none());
        let Some(&goal) = route.waypoints.get(*next) else {
            let Some(&target) = route.waypoints.last() else {
                return input;
            };
            if horizontal_distance(position, target) > LOCAL_PICKUP_RADIUS
                || (target[1] - position.y).abs() > LOCAL_PICKUP_RADIUS
            {
                return input;
            }
            // Taken, or found gone, as the bot believes it: its perception
            // dates the take and expects it back.
            let taken = at_pickup.filter(|pickup| pickup_up.get(pickup.id) == Some(&false));
            let waited = now - *arrived.get_or_insert(now) >= ARRIVAL_DWELL;
            if taken.is_none()
                && waited
                && let Some(pickup) = route.pickup
            {
                self.declined.insert(pickup.id, now + DECLINED_PICKUP);
            }
            if taken.is_some() || waited {
                self.active = None;
            }
            return input;
        };
        let distance = horizontal_distance(position, goal);
        let progress = progress.get_or_insert(Progress {
            next: *next,
            distance,
            updated: now,
        });
        if fighting || *next != progress.next || distance + MIN_PROGRESS <= progress.distance {
            *progress = Progress {
                next: *next,
                distance,
                updated: now,
            };
        }
        if now - progress.updated >= STALL_TIMEOUT {
            if self.blocked.len() == MAX_BLOCKS {
                self.blocked.remove(0);
            }
            self.blocked.push((route.leg(*next), now + RETRY_DELAY));
            self.active = None;
            // Leave a neutral update between abandoning one segment and
            // driving toward a new anchor.
            return PlayerInput::default();
        }
        input
    }
}

impl Route {
    fn leg(&self, next: usize) -> Leg {
        let from = if next == 0 {
            self.entry
        } else {
            self.waypoints[next - 1]
        };
        Leg {
            from,
            to: self.waypoints[next],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_jump_pad_and_teleport_then_continues_the_route() {
        let graph: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,24,0],[80,24,0],[80,100,160],[1000,24,500],[1064,24,500]],
                "edges":[[3,4],[4,0]], "entries":[0],"goals":[4],
                "maneuvers":[
                    [0,1,{"kind":"jump","takeoff":[0,24,0],"landing":[80,24,0],
                          "yaw":1.5707963267948966,"speed":0,"radius":48,"ticks":150}],
                    [1,2,{"kind":"jumppad","min":[70,0,-8],"max":[90,16,8],"landing":[80,100,160]}],
                    [2,3,{"kind":"teleporter","min":[72,76,152],"max":[88,132,168],
                          "exit":[1000,24,500],"yaw":1.5}]
                ]
            }}"#,
        )
        .unwrap();
        let mut follower = RouteFollower::default();
        let start = SimTime::ZERO;
        let mut steer = |ms, p: [f64; 3], grounded, vertical| {
            let now = start + Duration::from_millis(ms);
            let mut observation = observe(now, false, None);
            observation.velocity = Some(Velocity {
                grounded,
                linear: Vec3::new(0.0, vertical, 0.0),
                ..Velocity::default()
            });
            let input = follower.steer(&graph, Vec3::new(p[0], p[1], p[2]), observation);
            (input, follower.exact())
        };
        let (settle, exact) = steer(0, [0.0, 44.0, 0.0], false, -100.0);
        assert!(exact);
        assert_eq!(settle.buttons, 0);
        let (jump, exact) = steer(104, [0.0, 24.0, 0.0], true, 0.0);
        assert!(exact);
        assert_eq!(jump.buttons & (1 << 4), 1 << 4);
        let (airborne, exact) = steer(120, [12.0, 48.0, 0.0], false, 260.0);
        assert!(exact);
        assert_eq!(airborne.buttons & (1 << 4), 0);
        steer(600, [80.0, 24.0, 0.0], true, 0.0);
        assert!(steer(616, [80.0, 24.0, 0.0], true, 0.0).1);
        assert!(steer(632, [80.0, 50.0, 0.0], false, 500.0).1);
        assert!(steer(1000, [80.0, 100.0, 160.0], true, 0.0).1);
        assert!(steer(1016, [80.0, 100.0, 160.0], true, 0.0).1);
        let (exit, exact) = steer(1032, [1000.0, 24.0, 500.0], true, 0.0);
        assert!(exact);
        assert_eq!(exit.yaw, 1.5);
        let (walk, exact) = steer(1048, [1000.0, 24.0, 500.0], true, 0.0);
        assert!(!exact);
        assert_eq!(walk.direction, [1.0, 0.0, 0.0]);
    }

    /// A bot in a fight starts no maneuver. One started is still an
    /// approach a fight may interrupt until its jump goes out.
    #[test]
    fn holds_a_maneuver_in_a_fight_and_commits_it_at_the_jump() {
        let graph: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,24,0],[80,24,0]],
                "edges":[], "entries":[0],"goals":[1],
                "maneuvers":[
                    [0,1,{"kind":"jump","takeoff":[0,24,0],"landing":[80,24,0],
                          "yaw":1.5707963267948966,"speed":0,"radius":48,"ticks":150}]
                ]
            }}"#,
        )
        .unwrap();
        let mut follower = RouteFollower::default();
        let mut steer = |ms, p: [f64; 3], hold| {
            let mut observation = observe(SimTime::ZERO + Duration::from_millis(ms), false, None);
            observation.velocity = Some(Velocity {
                grounded: true,
                ..Velocity::default()
            });
            observation.hold_maneuvers = hold;
            let input = follower.steer(&graph, Vec3::new(p[0], p[1], p[2]), observation);
            (
                input.buttons,
                follower.exact(),
                follower.approaching_maneuver(),
            )
        };
        assert_eq!(steer(0, [0.0, 24.0, 0.0], true), (0, false, false));
        let (_, exact, approaching) = steer(16, [0.0, 24.0, 40.0], false);
        assert!(exact && approaching);
        let (buttons, exact, approaching) = steer(32, [0.0, 24.0, 0.0], false);
        assert_eq!(buttons & (1 << 4), 1 << 4);
        assert!(exact && !approaching);
        // Under way, a fight no longer holds it.
        assert!(steer(48, [4.0, 40.0, 0.0], true).1);
    }

    fn observe(now: SimTime, allow_takeoff: bool, landed_id: Option<&str>) -> RouteObservation<'_> {
        RouteObservation {
            now,
            vitals: OwnVitals::default(),
            velocity: None,
            difficulty: Difficulty::default(),
            pickup_up: &[],
            allow_takeoff,
            landed_id,
            sight: None,
            fighting: false,
            hold_maneuvers: false,
        }
    }

    #[test]
    fn low_health_prefers_recovery_and_skips_pickups_believed_gone() {
        let nav: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","routes":[
            {"entry":[0,0,0],"waypoints":[[40,0,0]],"pickup":{"id":0,"kind":"health"}},
            {"entry":[0,0,0],"waypoints":[[0,0,40]],"pickup":{"id":1,"kind":"armor"}},
            {"entry":[40,0,0],"waypoints":[[40,0,40]],"pickup":{"id":1,"kind":"armor"}}
        ]}"#,
        )
        .unwrap();
        let now = SimTime::ZERO;
        let origin = Vec3::default();
        let health = Vec3::new(40.0, 0.0, 0.0);
        let hurt = RouteObservation {
            vitals: OwnVitals {
                health: Some(10),
                armor: Some(50),
            },
            pickup_up: &[true, true],
            ..observe(now, false, None)
        };
        let mut follower = RouteFollower::default();
        assert_eq!(
            follower.steer(&nav, origin, hurt).direction,
            [1.0, 0.0, 0.0]
        );
        // Believed taken on arrival: the walk ends there, and the next one
        // goes for the armor.
        let gone = RouteObservation {
            pickup_up: &[false, true],
            ..hurt
        };
        assert_eq!(follower.steer(&nav, health, gone).direction, [0.0; 3]);
        assert_eq!(
            follower.steer(&nav, health, gone).direction,
            [0.0, 0.0, 1.0]
        );
        // A death or learned traversal detaches the route. The item is still
        // believed gone.
        follower.detach();
        assert_eq!(
            follower.steer(&nav, origin, gone).direction,
            [0.0, 0.0, 1.0]
        );

        let mut healthy = RouteFollower::default();
        let observation = RouteObservation {
            vitals: OwnVitals {
                health: Some(100),
                armor: Some(0),
            },
            ..hurt
        };
        assert_eq!(
            healthy.steer(&nav, origin, observation).direction,
            [0.0, 0.0, 1.0]
        );
    }

    #[test]
    fn walks_only_to_the_pickup_the_arbiter_chose_and_reports_its_memory() {
        let nav: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","routes":[
            {"entry":[0,0,0],"waypoints":[[40,0,0]],"pickup":{"id":0,"kind":"health"}},
            {"entry":[0,0,0],"waypoints":[[0,0,40]],"pickup":{"id":1,"kind":"armor"}}
        ]}"#,
        )
        .unwrap();
        let now = SimTime::ZERO;
        let origin = Vec3::default();
        let hurt = RouteObservation {
            vitals: OwnVitals {
                health: Some(10),
                armor: Some(100),
            },
            pickup_up: &[true, true],
            ..observe(now, false, None)
        };
        let mut follower = RouteFollower::default();
        // Left to itself, a hurt bot goes for the health.
        assert_eq!(
            follower.steer(&nav, origin, hurt).direction,
            [1.0, 0.0, 0.0]
        );
        // The arbiter chose the armor, even at full armor: the route
        // under way is dropped and only the armor is walked to.
        follower.set_goal(Some(1));
        assert_eq!(
            follower.steer(&nav, origin, hurt).direction,
            [0.0, 0.0, 1.0]
        );
        follower.set_goal(None);
        assert!(!follower.pickup_declined(0, now));
        assert!(!follower.blocked_toward([40.0, 0.0, 0.0], now));

        follower.declined.insert(0, now + Duration::from_secs(5));
        assert!(follower.pickup_declined(0, now));
        assert!(!follower.pickup_declined(0, now + Duration::from_secs(5)));
        follower.blocked.push((
            Leg {
                from: [0.0; 3],
                to: [40.0, 0.0, 0.0],
            },
            now + RETRY_DELAY,
        ));
        assert!(follower.blocked_toward([40.0, 0.0, 0.0], now));
        assert!(!follower.blocked_toward([40.0, 0.0, 0.0], now + RETRY_DELAY));
    }

    #[test]
    fn roaming_walks_sprint() {
        let nav: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","routes":[{"entry":[0,0,0],"waypoints":[[400,0,0],[400,100,400]]}]}"#,
        )
        .unwrap();
        let grounded = RouteObservation {
            velocity: Some(Velocity {
                grounded: true,
                ..Velocity::default()
            }),
            ..observe(SimTime::ZERO, false, None)
        };
        const SPRINT: u16 = 1 << 6;
        let mut follower = RouteFollower::default();
        let far = follower.steer(&nav, Vec3::default(), grounded);
        assert_ne!(far.buttons & SPRINT, 0);
    }

    #[test]
    fn leaves_a_pickup_that_stays_after_arrival_and_skips_full_health() {
        let nav: WashoiNavigation = serde_json::from_str(
            r#"{"map":"test","routes":[
            {"entry":[0,0,0],"waypoints":[[40,0,0]],"pickup":{"id":0,"kind":"health","item":"large"}},
            {"entry":[0,0,0],"waypoints":[[0,0,40]],"pickup":{"id":1,"kind":"weapon"}},
            {"entry":[0,0,40],"waypoints":[[40,0,40]],"pickup":{"id":0,"kind":"health","item":"large"}},
            {"entry":[0,0,40],"waypoints":[[0,0,80]]}
        ]}"#,
        )
        .unwrap();
        let now = SimTime::ZERO;
        let observation = |now| RouteObservation {
            vitals: OwnVitals {
                health: Some(100),
                armor: Some(0),
            },
            pickup_up: &[true, true],
            ..observe(now, false, None)
        };
        let mut follower = RouteFollower::default();
        // The health scores higher but full health can't take it.
        assert_eq!(
            follower
                .steer(&nav, Vec3::default(), observation(now))
                .direction,
            [0.0, 0.0, 1.0]
        );
        let weapon = Vec3::new(0.0, 0.0, 40.0);
        assert_eq!(
            follower.steer(&nav, weapon, observation(now)).direction,
            [0.0; 3]
        );
        let waited = now + ARRIVAL_DWELL;
        assert_eq!(
            follower.steer(&nav, weapon, observation(waited)).direction,
            [0.0; 3]
        );
        assert_eq!(
            follower.steer(&nav, weapon, observation(waited)).direction,
            [0.0, 0.0, 1.0]
        );
    }

    #[test]
    fn graph_continuation_requires_the_matching_landing() {
        let graph: WashoiNavigation = serde_json::from_str(
            r#"{
            "map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,0,0],[0,0,40],[60,0,0],[200,0,200],[160,0,200]],
                "edges":[[0,1],[1,0],[0,2],[2,0],[3,4],[4,3]],"entries":[0],"goals":[1,2,4],
                "learned_goals":[[2,"jump"]],"learned_landings":[[3,"jump"]]
            }
        }"#,
        )
        .unwrap();
        let now = SimTime::ZERO;
        let mut follower = RouteFollower::default();
        let start = Vec3::new(0.0, 0.0, 0.0);
        assert_eq!(
            follower
                .steer(&graph, start, observe(now, true, None))
                .direction,
            [1.0, 0.0, 0.0]
        );
        assert_eq!(follower.takeoff(&graph), Some("jump"));
        follower = RouteFollower::default();
        assert_eq!(
            follower
                .steer(&graph, start, observe(now, false, None))
                .direction,
            [0.0, 0.0, 1.0]
        );
        // A failed recording keeps the follower off its takeoff for the
        // retry delay, then offers it again.
        follower = RouteFollower::default();
        follower.block_takeoff("jump", now);
        assert_eq!(
            follower
                .steer(&graph, start, observe(now, true, None))
                .direction,
            [0.0, 0.0, 1.0]
        );
        follower = RouteFollower {
            blocked_takeoffs: follower.blocked_takeoffs,
            ..RouteFollower::default()
        };
        let later = now + RETRY_DELAY;
        assert_eq!(
            follower
                .steer(&graph, start, observe(later, true, None))
                .direction,
            [1.0, 0.0, 0.0]
        );
        let landing = Vec3::new(200.0, 0.0, 200.0);
        let slot = |follower: &RouteFollower| {
            matches!(
                follower.active.as_ref().map(|active| &active.route),
                Some(RouteSlot::Shared(_))
            )
        };
        // Another recording's landing doesn't take the baked continuation,
        // but the bot still plans a walk from the nearest graph node.
        assert_eq!(
            follower
                .steer(&graph, landing, observe(now, false, Some("other")))
                .direction,
            [-1.0, 0.0, 0.0]
        );
        assert!(!slot(&follower));
        follower.detach();
        assert_eq!(
            follower
                .steer(&graph, landing, observe(now, false, Some("jump")))
                .direction,
            [-1.0, 0.0, 0.0]
        );
        assert!(slot(&follower));
    }

    #[test]
    fn replans_from_any_graph_node_and_moves_on_from_a_plain_goal() {
        let graph: WashoiNavigation = serde_json::from_str(
            r#"{
            "map":"test","graph":{
                "bsp_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "nodes":[[0,0,0],[200,0,0],[200,0,200],[0,0,200]],
                "edges":[[0,1],[1,2],[2,3],[3,0]],"entries":[0],"goals":[2,3]
            }
        }"#,
        )
        .unwrap();
        let now = SimTime::ZERO;
        let mut follower = RouteFollower::default();
        // Node 1 isn't an entry, so only the planner can start here. The
        // nearer goal wins when neither holds a pickup.
        let side = Vec3::new(200.0, 0.0, 0.0);
        assert_eq!(
            follower
                .steer(&graph, side, observe(now, false, None))
                .direction,
            [0.0, 0.0, 1.0]
        );
        let goal = Vec3::new(200.0, 0.0, 200.0);
        let at_goal = |now| observe(now, false, None);
        let halfway = Vec3::new(200.0, 0.0, 100.0);
        assert_eq!(
            follower.steer(&graph, halfway, at_goal(now)).direction,
            [0.0, 0.0, 1.0]
        );
        assert_eq!(
            follower.steer(&graph, goal, at_goal(now)).direction,
            [0.0; 3]
        );
        let waited = now + ARRIVAL_DWELL;
        assert_eq!(
            follower.steer(&graph, goal, at_goal(waited)).direction,
            [0.0; 3]
        );
        assert_eq!(
            follower.steer(&graph, goal, at_goal(waited)).direction,
            [-1.0, 0.0, 0.0]
        );

        // Away from every node, the bot waits before planning again.
        let mut lost = RouteFollower::default();
        let nowhere = Vec3::new(100.0, 0.0, 100.0);
        assert_eq!(
            lost.steer(&graph, nowhere, at_goal(now)).direction,
            [0.0; 3]
        );
        assert!(lost.replan_at.is_some_and(|at| at > now));
    }

    #[test]
    fn switches_after_a_stall_and_reattaches_after_displacement() {
        let nav = serde_json::from_str(
            r#"{"map":"test","routes":[
            {"entry":[0,0,0],"waypoints":[[60,0,0]]},
            {"entry":[0,0,0],"waypoints":[[0,0,60]]},
            {"entry":[200,0,0],"waypoints":[[140,0,0]]}
        ]}"#,
        )
        .unwrap();
        let mut follower = RouteFollower::default();
        let start = SimTime::ZERO;
        let anchor = Vec3::new(0.0, 0.0, 0.0);
        assert_eq!(
            follower
                .steer(&nav, anchor, observe(start, true, None))
                .direction,
            [1.0, 0.0, 0.0]
        );
        let stalled = start + STALL_TIMEOUT;
        assert_eq!(
            follower
                .steer(&nav, anchor, observe(stalled, true, None))
                .direction,
            [0.0; 3]
        );
        assert_eq!(
            follower
                .steer(&nav, anchor, observe(stalled, true, None))
                .direction,
            [0.0, 0.0, 1.0]
        );
        let displaced = Vec3::new(200.0, 0.0, 0.0);
        assert_eq!(
            follower
                .steer(&nav, displaced, observe(stalled, true, None))
                .direction,
            [-1.0, 0.0, 0.0]
        );
    }
}

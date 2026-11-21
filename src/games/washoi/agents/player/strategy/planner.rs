//! Chooses what to go for, from TypeScript `bot/goals/ItemGoalPlanner.ts`
//! with `ItemGoalSource.ts`, `DirectiveGoalSource.ts`, and
//! `orders/BotOrders.ts` `selectMandatoryGoal`.
//!
//! Candidates are the map's pickups the bot believes up, a major item due
//! back soon when the mode times items, and the orders addressed to the
//! bot. They pass the gates, then the arbiter scores them. An order that
//! must not be interrupted is taken without arbitration.

use std::fmt::Write as _;

use crate::games::washoi::agents::player::trace::{TRACED_CANDIDATES, TracedCandidate};

use crate::games::washoi::{
    Vec3,
    agents::player::strategy::{
        arbiter::{Arbiter, Choice, Gate, Trace, mistake_probability},
        candidates::{Candidate, CandidateKind, GoalTarget, Interrupt},
        considerations::{self, Context, Goal, Me, Order, Situation},
        goals::PickupKind,
        items,
        weights::Weights,
    },
    random::Random,
};

/// `MAX_ITEM_CANDIDATES`: the nearest pickups scored besides major items.
const MAX_ITEM_CANDIDATES: usize = 12;
/// `PICKUP_SEEK_RANGE`: other than major items, pickups beyond this
/// straight-line distance are not candidates.
const PICKUP_SEEK_RANGE: f64 = 1500.0;
/// `TIMING_LEAD_SECONDS`: at item timing 1, a bot sets off for a major item
/// that returns within this long of its arrival.
const TIMING_LEAD_SECONDS: f64 = 10.0;
/// `TIMING_TRAVEL_SPEED`: run speed in units per second.
const TIMING_TRAVEL_SPEED: f64 = 320.0;
const TICK_RATE: f64 = 125.0;
/// `TOUR_DETOUR`: a stop costing this much extra running is worth nothing.
const TOUR_DETOUR: f64 = 400.0;
/// `TOUR_MIN_VALUE`: a stop must be worth at least this.
const TOUR_MIN_VALUE: f64 = 0.1;
/// `TOUR_HOLD_TICKS`: a stop chosen holds for 3 seconds.
const TOUR_HOLD_TICKS: u32 = 375;
/// `PICKUP_FAILURE_COOLDOWN_TICKS`: a pickup given up on is left for 5 s.
const FAILURE_COOLDOWN_TICKS: u32 = 625;
/// `ITEM_CIRCLE_RADIUS` and `ITEM_CIRCLE_TICKS`: a bot this close to an item
/// for a second without taking it is circling it.
const CIRCLE_RADIUS: f64 = 64.0;
const CIRCLE_TICKS: u32 = 125;
/// A pursuit whose route has not shortened by this much in 4 s is stuck,
/// the brain's `stuck` recovery, which puts the item on cooldown.
const PROGRESS_STEP: f64 = 64.0;
const PROGRESS_TICKS: u32 = 500;

/// A pickup the bot knows of, with what it knows about it now.
#[derive(Clone, Debug)]
pub(in crate::games::washoi) struct KnownPickup {
    pub(in crate::games::washoi) candidate: Candidate,
    /// Up by the server's mask, or believed up without one.
    pub(in crate::games::washoi) up: bool,
    /// For one down, the server tick it is due back, when known.
    pub(in crate::games::washoi) returns_tick: Option<u32>,
    /// The follower found it gone, or its last leg is blocked for now.
    pub(in crate::games::washoi) blocked: bool,
}

pub(in crate::games::washoi) struct Planner {
    arbiter: Arbiter,
    explain: String,
    /// The arbiter's score for the last goal chosen, zero for none.
    desire: f64,
    /// The last decision for its trace: the goal chosen, the best
    /// candidates past the gates, and how many a gate ruled out.
    last_goal: Option<String>,
    last_candidates: Vec<TracedCandidate>,
    last_gated: usize,
    /// The pickup pursued, and how it is going.
    pursuit: Option<Pursuit>,
    /// Goals given up on, and the server tick they may be tried again.
    failed: Vec<(String, u32)>,
    /// The item goal, the stop on the way, and until when the stop holds.
    stopover: Option<(String, String, u32)>,
}

struct Pursuit {
    goal: String,
    /// The shortest route left so far, and when it last shortened.
    best: f64,
    since: u32,
    /// Since when the bot has been beside the item without taking it.
    circling: Option<u32>,
}

impl Planner {
    pub(in crate::games::washoi) fn new(mistake_rate: f64) -> Self {
        Self {
            arbiter: Arbiter::new(&considerations::ALL, mistake_probability(mistake_rate)),
            explain: String::new(),
            desire: 0.0,
            last_goal: None,
            last_candidates: Vec::new(),
            last_gated: 0,
            pursuit: None,
            failed: Vec::new(),
            stopover: None,
        }
    }

    /// The last decision as `DecisionTrace` records it: the goal, the best
    /// candidates past the gates, and the number gated.
    pub(in crate::games::washoi) fn last(&self) -> (Option<&str>, &[TracedCandidate], usize) {
        (
            self.last_goal.as_deref(),
            &self.last_candidates,
            self.last_gated,
        )
    }

    /// What the last goal chosen is worth, in the arbiter's currency.
    pub(in crate::games::washoi) fn desire(&self) -> f64 {
        self.desire
    }

    /// The last decision on one line.
    pub(in crate::games::washoi) fn explain(&self) -> &str {
        &self.explain
    }

    pub(in crate::games::washoi) fn reset(&mut self) {
        self.arbiter.reset();
        self.pursuit = None;
        self.stopover = None;
        self.explain.clear();
    }

    /// The goal to pursue, or None when nothing passes its gates.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::games::washoi) fn select(
        &mut self,
        pickups: Vec<KnownPickup>,
        orders: Vec<Candidate>,
        me: Me,
        situation: Situation,
        weights: Weights,
        between: &dyn Fn(&Goal, &Goal) -> Option<f64>,
        random: &mut Random,
    ) -> Option<GoalTarget> {
        if weights.objective_push > 0.0
            && let Some(order) = mandatory(&orders)
        {
            self.explain = format!("goal={} mandatory", order.id);
            self.last_goal = Some(order.id.clone());
            self.last_candidates.clear();
            self.last_gated = 0;
            return Some(order.target.clone());
        }
        let tick = situation.tick;
        self.failed.retain(|(_, until)| tick < *until);
        let mut blocked: Vec<String> = self.failed.iter().map(|(id, _)| id.clone()).collect();
        let mut goals: Vec<Goal> = Vec::new();
        let mut costs: Vec<Option<f64>> = Vec::new();
        let mut targets: Vec<GoalTarget> = Vec::new();
        for (pickup, cost, target, blocked_now) in collect(pickups, me.position, weights, tick) {
            if blocked_now {
                blocked.push(pickup.id.clone());
            }
            goals.push(pickup);
            costs.push(cost);
            targets.push(target);
        }
        for order in orders {
            goals.push(Goal {
                id: order.id.clone(),
                position: vec3(order.position),
                pickup: None,
                order: Some(Order {
                    priority: order.priority,
                    urgency: order.urgency,
                    escort: order.escort.clone(),
                }),
                respawn_tick: None,
            });
            costs.push(order.route_cost.map(f64::from));
            targets.push(order.target);
        }

        let blocked_gate =
            |context: &Context| blocked.contains(&context.goal.id).then_some("blocked");
        let gates: [Gate; 4] = [&blocked_gate, &no_gain, &unreachable, &not_yet];
        let context = |index: usize| Context {
            goal: &goals[index],
            me,
            situation,
            route_cost: costs[index],
            weights,
        };
        self.desire = 0.0;
        let Choice {
            goal,
            explain,
            traces,
            desire,
        } = self.arbiter.select(
            &goals,
            |goal| {
                let index = goals
                    .iter()
                    .position(|g| std::ptr::eq(g, goal))
                    .unwrap_or(0);
                Context {
                    goal,
                    me,
                    situation,
                    route_cost: costs[index],
                    weights,
                }
            },
            tick,
            &gates,
            random,
        );
        self.explain = explain;
        let mut passed: Vec<&Trace> = traces.iter().filter(|t| t.gated.is_none()).collect();
        passed.sort_by(|a, b| b.desire.total_cmp(&a.desire));
        self.last_candidates = passed
            .into_iter()
            .take(TRACED_CANDIDATES)
            .map(|trace| TracedCandidate {
                id: trace.goal.clone(),
                desire: trace.desire,
                via: trace.via,
            })
            .collect();
        self.last_gated = traces.iter().filter(|t| t.gated.is_some()).count();
        self.last_goal = goal.map(|index| goals[index].id.clone());
        let Some(index) = goal else {
            self.stopover = None;
            return None;
        };
        self.desire = desire;
        let stop = self.stopover(index, &goals, &costs, &traces, &context, between, tick);
        if let Some(stop) = stop {
            let _ = write!(self.explain, " via={}", goals[stop].id);
        }
        let index = stop.unwrap_or(index);
        let pursued = &goals[index];
        if pursued.pickup.is_some() && pursued.respawn_tick.is_none() {
            let left = costs[index].unwrap_or_else(|| {
                considerations::estimate_route_cost(me.position, pursued.position)
            });
            let beside = (pursued.position.x - me.position.x)
                .hypot(pursued.position.z - me.position.z)
                <= CIRCLE_RADIUS;
            if self.pursue(&pursued.id, left, beside, tick) {
                let id = pursued.id.clone();
                self.fail(&id, tick);
                let _ = write!(self.explain, " [gave up on {}]", pursued.id);
                return None;
            }
        } else {
            self.pursuit = None;
        }
        Some(targets[index].clone())
    }

    /// `chooseStopover`: an item worth taking on the way to an item goal,
    /// valued by `ItemValue` and discounted by the detour, the tour of an
    /// arena player. A stop holds for 3 seconds while it stays usable.
    #[allow(clippy::too_many_arguments)]
    fn stopover<'a>(
        &mut self,
        goal: usize,
        goals: &'a [Goal],
        costs: &[Option<f64>],
        traces: &[Trace],
        context: &dyn Fn(usize) -> Context<'a>,
        between: &dyn Fn(&Goal, &Goal) -> Option<f64>,
        tick: u32,
    ) -> Option<usize> {
        let tour = |index: usize| {
            let goal = &goals[index];
            goal.pickup.is_some() && goal.respawn_tick.is_none() && goal.order.is_none()
        };
        let Some(goal_cost) = costs[goal].filter(|_| tour(goal)) else {
            self.stopover = None;
            return None;
        };
        let usable = |index: usize| traces[index].gated.is_none() && costs[index].is_some();
        if let Some((held_goal, held_stop, until)) = &self.stopover
            && *held_goal == goals[goal].id
            && tick < *until
            && let Some(stop) = goals.iter().position(|g| g.id == *held_stop)
            && usable(stop)
        {
            return Some(stop);
        }
        let mut best = None;
        let mut best_score = 0.0;
        for stop in 0..goals.len() {
            if stop == goal || !tour(stop) || !usable(stop) {
                continue;
            }
            let value = considerations::item_value(&context(stop));
            if value < TOUR_MIN_VALUE {
                continue;
            }
            let (Some(to_stop), Some(onward)) = (costs[stop], between(&goals[stop], &goals[goal]))
            else {
                continue;
            };
            let detour = to_stop + onward - goal_cost;
            let score = value * items::clamp01(1.0 - detour / TOUR_DETOUR);
            if score > best_score {
                best_score = score;
                best = Some(stop);
            }
        }
        self.stopover = best.map(|stop| {
            (
                goals[goal].id.clone(),
                goals[stop].id.clone(),
                tick + TOUR_HOLD_TICKS,
            )
        });
        best
    }

    /// Give up on a goal for the failure cooldown, as when no route to it
    /// can be planned.
    pub(in crate::games::washoi) fn fail(&mut self, goal: &str, tick: u32) {
        self.failed
            .push((goal.to_owned(), tick + FAILURE_COOLDOWN_TICKS));
        self.arbiter.reset();
        self.pursuit = None;
    }

    /// `_giveUpOnCircledItem` and the stuck recovery: true when the pursuit
    /// of this pickup has failed, circled for a second or stalled for four.
    fn pursue(&mut self, goal: &str, left: f64, beside: bool, tick: u32) -> bool {
        let pursuit = match &mut self.pursuit {
            Some(pursuit) if pursuit.goal == goal => pursuit,
            _ => self.pursuit.insert(Pursuit {
                goal: goal.to_owned(),
                best: left,
                since: tick,
                circling: None,
            }),
        };
        if left <= pursuit.best - PROGRESS_STEP {
            pursuit.best = left;
            pursuit.since = tick;
        }
        pursuit.circling = beside.then(|| pursuit.circling.unwrap_or(tick));
        pursuit
            .circling
            .is_some_and(|since| tick.saturating_sub(since) > CIRCLE_TICKS)
            || tick.saturating_sub(pursuit.since) > PROGRESS_TICKS
    }
}

/// `selectMandatoryGoal`: the highest-priority order that may never be
/// interrupted.
fn mandatory(orders: &[Candidate]) -> Option<&Candidate> {
    orders
        .iter()
        .filter(|order| order.interrupt == Interrupt::Never && order.priority.is_finite())
        .min_by(|a, b| {
            b.priority
                .total_cmp(&a.priority)
                .then_with(|| a.id.cmp(&b.id))
        })
}

/// `collectItemGoals`: pickups up, or major items due back when the mode
/// times items, the nearest twelve within seek range plus every major item.
fn collect(
    pickups: Vec<KnownPickup>,
    from: Vec3,
    weights: Weights,
    tick: u32,
) -> Vec<(Goal, Option<f64>, GoalTarget, bool)> {
    let mut majors = Vec::new();
    let mut others = Vec::new();
    for known in pickups {
        let Some(pickup) = known.candidate.pickup else {
            continue;
        };
        if known.candidate.kind != CandidateKind::Pickup {
            continue;
        }
        let major = pickup.is_major();
        let respawn_tick = if known.up {
            None
        } else {
            match known.returns_tick {
                Some(back) if major && weights.item_timing > 0.0 && back > tick => Some(back),
                _ => continue,
            }
        };
        let position = vec3(known.candidate.position);
        let distance = straight(from, position);
        if distance > PICKUP_SEEK_RANGE && !major {
            continue;
        }
        let goal = Goal {
            id: known.candidate.id.clone(),
            position,
            pickup: Some(pickup),
            order: None,
            respawn_tick,
        };
        let entry = (
            distance,
            (
                goal,
                known.candidate.route_cost.map(f64::from),
                known.candidate.target,
                known.blocked,
            ),
        );
        if major {
            majors.push(entry);
        } else {
            others.push(entry);
        }
    }
    others.sort_by(|a, b| a.0.total_cmp(&b.0));
    others.truncate(MAX_ITEM_CANDIDATES);
    majors
        .into_iter()
        .chain(others)
        .map(|(_, entry)| entry)
        .collect()
}

/// The server leaves health and armor that would add nothing.
fn no_gain(context: &Context) -> Option<&'static str> {
    let pickup = context.goal.pickup?;
    let me = context.me;
    let gains = match pickup.kind {
        PickupKind::Health => items::health_after(me.health, pickup.item.0) > me.health,
        PickupKind::Armor => items::armor_after(me.armor, pickup.item.0) > me.armor,
        _ => true,
    };
    (!gains).then_some("no_gain")
}

/// No known route, except for an order, which must still be attempted.
fn unreachable(context: &Context) -> Option<&'static str> {
    (context.route_cost.is_none() && context.goal.order.is_none()).then_some("unreachable")
}

/// A timed item is a goal only once the run there ends within the mode's
/// lead of its return.
fn not_yet(context: &Context) -> Option<&'static str> {
    let (back, cost) = (context.goal.respawn_tick?, context.route_cost?);
    let left = f64::from(back.saturating_sub(context.situation.tick)) / TICK_RATE;
    let travel = cost / TIMING_TRAVEL_SPEED;
    (left - travel > TIMING_LEAD_SECONDS * context.weights.item_timing).then_some("not_yet")
}

fn vec3([x, y, z]: [f64; 3]) -> Vec3 {
    Vec3::new(x, y, z)
}

fn straight(a: Vec3, b: Vec3) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::agents::player::strategy::{
        considerations::tests::me, goals::PickupGoal, weights,
    };

    fn known(json: &str, at: [f64; 3], cost: Option<f32>, up: bool) -> KnownPickup {
        let pickup: PickupGoal = serde_json::from_str(json).unwrap();
        KnownPickup {
            candidate: Candidate::pickup(pickup, at, cost),
            up,
            returns_tick: None,
            blocked: false,
        }
    }

    fn straight_between(from: &Goal, to: &Goal) -> Option<f64> {
        Some(considerations::estimate_route_cost(from.position, to.position) / DETOUR)
    }

    /// The estimate without its detour factor: straight lines in tests.
    const DETOUR: f64 = considerations::DETOUR_FACTOR;

    fn situation() -> Situation<'static> {
        Situation {
            tick: 1000,
            scoreboard_enemies: 1,
            ..Situation::default()
        }
    }

    #[test]
    fn takes_the_best_reachable_item_and_leaves_what_it_cannot_use() {
        let mut planner = Planner::new(0.0);
        let mut random = Random::new(1);
        let pickups = vec![
            known(
                r#"{"id":0,"kind":"health","item":"small"}"#,
                [100.0, 0.0, 0.0],
                Some(100.0),
                true,
            ),
            known(
                r#"{"id":1,"kind":"armor","item":"ra"}"#,
                [900.0, 0.0, 0.0],
                Some(1200.0),
                true,
            ),
            known(
                r#"{"id":2,"kind":"health","item":"mega"}"#,
                [500.0, 0.0, 0.0],
                None,
                true,
            ),
            known(
                r#"{"id":3,"kind":"powerup","item":"quad"}"#,
                [300.0, 0.0, 0.0],
                Some(300.0),
                false,
            ),
        ];
        let choice = planner.select(
            pickups,
            Vec::new(),
            me(100, 0),
            situation(),
            weights::DEFAULT,
            &straight_between,
            &mut random,
        );
        // The quad is down with no known return, the Mega has no route.
        assert_eq!(choice, Some(GoalTarget::Pickup(1)), "{}", planner.explain());
        assert!(
            planner.explain().contains("gated=1[unreachable…]"),
            "{}",
            planner.explain()
        );
    }

    #[test]
    fn takes_an_item_on_the_way_when_the_detour_is_small() {
        let mut planner = Planner::new(0.0);
        let mut random = Random::new(1);
        let pickups = || {
            vec![
                known(
                    r#"{"id":1,"kind":"armor","item":"ra"}"#,
                    [1000.0, 0.0, 0.0],
                    Some(1000.0),
                    true,
                ),
                // Beside the way: 50 units off the line at the halfway mark.
                known(
                    r#"{"id":2,"kind":"health","item":"large"}"#,
                    [500.0, 0.0, 50.0],
                    Some(502.5),
                    true,
                ),
                // Behind the bot: a long detour.
                known(
                    r#"{"id":3,"kind":"health","item":"large"}"#,
                    [-500.0, 0.0, 0.0],
                    Some(500.0),
                    true,
                ),
            ]
        };
        let choice = planner.select(
            pickups(),
            Vec::new(),
            me(40, 0),
            situation(),
            weights::DEFAULT,
            &straight_between,
            &mut random,
        );
        assert_eq!(choice, Some(GoalTarget::Pickup(2)), "{}", planner.explain());
        assert!(
            planner.explain().contains("goal=pickup:1 "),
            "{}",
            planner.explain()
        );
        assert!(
            planner.explain().contains(" via=pickup:2"),
            "{}",
            planner.explain()
        );
    }

    #[test]
    fn times_a_major_item_only_when_the_run_ends_near_its_return() {
        let mut planner = Planner::new(0.0);
        let mut random = Random::new(1);
        let duel = weights::MatchContext {
            gametype: "Duel".into(),
            ..Default::default()
        }
        .weights("");
        let timed = |back: u32| {
            let mut red = known(
                r#"{"id":1,"kind":"armor","item":"ra"}"#,
                [900.0, 0.0, 0.0],
                Some(960.0),
                false,
            );
            red.returns_tick = Some(back);
            vec![red]
        };
        // 3 s of running against 20 s left: too early.
        let early = planner.select(
            timed(1000 + 20 * 125),
            Vec::new(),
            me(100, 0),
            situation(),
            duel,
            &straight_between,
            &mut random,
        );
        assert_eq!(early, None, "{}", planner.explain());
        let soon = planner.select(
            timed(1000 + 10 * 125),
            Vec::new(),
            me(100, 0),
            situation(),
            duel,
            &straight_between,
            &mut random,
        );
        assert_eq!(soon, Some(GoalTarget::Pickup(1)), "{}", planner.explain());
        // Deathmatch doesn't time items without a profile that does.
        let none = planner.select(
            timed(1000 + 10 * 125),
            Vec::new(),
            me(100, 0),
            situation(),
            weights::DEFAULT,
            &straight_between,
            &mut random,
        );
        assert_eq!(none, None);
    }

    #[test]
    fn an_order_outranks_items_where_the_mode_pushes_objectives() {
        let mut planner = Planner::new(0.0);
        let mut random = Random::new(1);
        let snd = weights::MatchContext {
            gametype: "Search & Destroy".into(),
            ..Default::default()
        }
        .weights("");
        let order = |interrupt| Candidate {
            id: "plant".into(),
            target: GoalTarget::Directive("plant".into()),
            position: [2000.0, 0.0, 0.0],
            route_cost: Some(2000.0),
            kind: CandidateKind::MoveTo,
            priority: 0.9,
            urgency: 0.0,
            interrupt,
            escort: None,
            pickup: None,
        };
        let pickups = || {
            vec![known(
                r#"{"id":1,"kind":"armor","item":"ra"}"#,
                [300.0, 0.0, 0.0],
                Some(300.0),
                true,
            )]
        };
        let choice = planner.select(
            pickups(),
            vec![order(Interrupt::HigherPriority)],
            me(100, 0),
            situation(),
            snd,
            &straight_between,
            &mut random,
        );
        assert_eq!(
            choice,
            Some(GoalTarget::Directive("plant".into())),
            "{}",
            planner.explain()
        );
        let choice = planner.select(
            pickups(),
            vec![order(Interrupt::Never)],
            me(100, 0),
            situation(),
            snd,
            &straight_between,
            &mut random,
        );
        assert_eq!(choice, Some(GoalTarget::Directive("plant".into())));
        assert_eq!(planner.explain(), "goal=plant mandatory");
        // A pursuit that makes no progress is given up for a while.
        let stuck = || {
            vec![known(
                r#"{"id":1,"kind":"armor","item":"ra"}"#,
                [300.0, 0.0, 0.0],
                Some(300.0),
                true,
            )]
        };
        let at = |tick| Situation {
            tick,
            ..situation()
        };
        let dm = weights::DEFAULT;
        assert!(
            planner
                .select(
                    stuck(),
                    Vec::new(),
                    me(100, 0),
                    at(2000),
                    dm,
                    &straight_between,
                    &mut random
                )
                .is_some()
        );
        assert!(
            planner
                .select(
                    stuck(),
                    Vec::new(),
                    me(100, 0),
                    at(2400),
                    dm,
                    &straight_between,
                    &mut random
                )
                .is_some()
        );
        assert_eq!(
            planner.select(
                stuck(),
                Vec::new(),
                me(100, 0),
                at(2600),
                dm,
                &straight_between,
                &mut random
            ),
            None
        );
        assert!(
            planner.explain().ends_with("[gave up on pickup:1]"),
            "{}",
            planner.explain()
        );
        assert_eq!(
            planner.select(
                stuck(),
                Vec::new(),
                me(100, 0),
                at(3000),
                dm,
                &straight_between,
                &mut random
            ),
            None
        );
        assert!(
            planner
                .select(
                    stuck(),
                    Vec::new(),
                    me(100, 0),
                    at(3300),
                    dm,
                    &straight_between,
                    &mut random
                )
                .is_some()
        );
        // Blocked: the follower found it gone.
        let mut gone = pickups();
        gone[0].blocked = true;
        assert_eq!(
            planner.select(
                gone,
                Vec::new(),
                me(100, 0),
                situation(),
                snd,
                &straight_between,
                &mut random
            ),
            None
        );
    }
}

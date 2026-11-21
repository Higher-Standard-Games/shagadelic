//! Scores candidate goals and picks one, from TypeScript
//! `bot/goals/GoalArbiter.ts`.
//!
//! desire = gate × attraction × discount, with attraction the best weighted
//! attractor plus a small share of the mean of the others, and discount the
//! product of the discounts. The best reason drives the choice, as a sum
//! would let a goal mediocre at everything beat one that matters. Gates are
//! hard: "no route" and "slightly unattractive" must differ.
//!
//! A goal chosen anew is committed to for 1.5 seconds and gets a small bonus
//! while committed. Re-picking the same goal does not extend the window.
//! Only on a genuine switch does a difficulty's mistake rate sometimes take
//! the runner-up, held for its window, so the bot looks like it misjudged
//! rather than malfunctioned.

use std::fmt::Write as _;

use crate::games::washoi::{
    agents::player::strategy::considerations::{Consideration, Context, Goal, Role},
    random::Random,
};

/// `SECONDARY_WEIGHT`: the other attractors' share against the winning one.
pub(in crate::games::washoi) const SECONDARY_WEIGHT: f64 = 0.15;
/// `HYSTERESIS_BONUS` for the committed goal inside its window.
pub(in crate::games::washoi) const HYSTERESIS_BONUS: f64 = 0.1;
/// `COMMITMENT_TICKS`: 1.5 seconds at 125 Hz, longer than a typical
/// movement sequence.
pub(in crate::games::washoi) const COMMITMENT_TICKS: u32 = 188;
/// `MISTAKE_RATE_FULL`: the mistake rate that takes the runner-up on nearly
/// every switch, so the presets give about 1, 6, 19, and 50 percent.
const MISTAKE_RATE_FULL: f64 = 16.0;

/// `mistakeProbability`: a difficulty's mistake rate as a chance per switch.
pub(in crate::games::washoi) fn mistake_probability(rate: f64) -> f64 {
    if rate.is_finite() && rate > 0.0 {
        (rate / MISTAKE_RATE_FULL).min(1.0)
    } else {
        0.0
    }
}

/// A gate: a hard precondition, the reason a candidate is rejected or None.
pub(in crate::games::washoi) type Gate<'a> = &'a dyn Fn(&Context) -> Option<&'static str>;

/// `GoalScoreTrace`: one candidate's scores, kept for decision traces.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::games::washoi) struct Trace {
    pub(in crate::games::washoi) goal: String,
    /// The gate that rejected the candidate, if any.
    pub(in crate::games::washoi) gated: Option<&'static str>,
    /// Raw consideration outputs, by ID.
    pub(in crate::games::washoi) features: Vec<(&'static str, f64)>,
    /// Weighted attractor contributions, by ID.
    pub(in crate::games::washoi) weighted: Vec<(&'static str, f64)>,
    pub(in crate::games::washoi) attraction: f64,
    pub(in crate::games::washoi) discount: f64,
    pub(in crate::games::washoi) desire: f64,
    /// The attractor that won the maximum.
    pub(in crate::games::washoi) via: Option<&'static str>,
}

impl Trace {
    pub(in crate::games::washoi) fn feature(&self, id: &str) -> Option<f64> {
        self.features
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, value)| *value)
    }
}

/// `ArbitrationResult`: the index of the goal chosen among the candidates.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::games::washoi) struct Choice {
    pub(in crate::games::washoi) goal: Option<usize>,
    pub(in crate::games::washoi) desire: f64,
    /// One line for the bot's diagnostics.
    pub(in crate::games::washoi) explain: String,
    pub(in crate::games::washoi) traces: Vec<Trace>,
}

pub(in crate::games::washoi) struct Arbiter {
    considerations: &'static [Consideration],
    mistake_chance: f64,
    committed: Option<String>,
    commitment_until: u32,
    /// The committed goal is a runner-up taken by mistake.
    mistaken: bool,
}

impl Arbiter {
    pub(in crate::games::washoi) fn new(
        considerations: &'static [Consideration],
        mistake_chance: f64,
    ) -> Self {
        Self {
            considerations,
            mistake_chance,
            committed: None,
            commitment_until: 0,
            mistaken: false,
        }
    }

    /// The goal the bot is committed to.
    pub(in crate::games::washoi) fn committed(&self) -> Option<&str> {
        self.committed.as_deref()
    }

    /// Drop the commitment: on death, completion, or failure.
    pub(in crate::games::washoi) fn reset(&mut self) {
        self.committed = None;
        self.commitment_until = 0;
        self.mistaken = false;
    }

    /// Score every candidate and choose. The caller makes each context, so
    /// the arbiter never knows how a route cost is found.
    pub(in crate::games::washoi) fn select<'a>(
        &mut self,
        candidates: &'a [Goal],
        context: impl Fn(&'a Goal) -> Context<'a>,
        tick: u32,
        gates: &[Gate],
        random: &mut Random,
    ) -> Choice {
        let traces: Vec<Trace> = candidates
            .iter()
            .map(|goal| self.score(goal, &context(goal), tick, gates))
            .collect();
        let mut best: Option<usize> = None;
        let mut best_desire = 0.0;
        for (index, trace) in traces.iter().enumerate() {
            if trace.gated.is_none() && trace.desire > best_desire {
                best_desire = trace.desire;
                best = Some(index);
            }
        }

        let mut mistaken = false;
        let held = (self.mistaken && tick < self.commitment_until)
            .then(|| {
                traces.iter().position(|trace| {
                    Some(trace.goal.as_str()) == self.committed.as_deref() && trace.gated.is_none()
                })
            })
            .flatten();
        if let Some(held) = held {
            best = Some(held);
            best_desire = traces[held].desire;
            mistaken = true;
        } else if let Some(winner) = best
            && Some(candidates[winner].id.as_str()) != self.committed.as_deref()
            && self.mistake_chance > 0.0
            && random.next() < self.mistake_chance
            && let Some(runner_up) = runner_up(&traces, winner)
        {
            best = Some(runner_up);
            best_desire = traces[runner_up].desire;
            mistaken = true;
        }

        match best {
            Some(index) if Some(candidates[index].id.as_str()) != self.committed.as_deref() => {
                self.committed = Some(candidates[index].id.clone());
                self.commitment_until = tick + COMMITMENT_TICKS;
            }
            Some(_) => {}
            None => self.reset(),
        }
        self.mistaken = mistaken;

        let mut explain = explain(best, &traces);
        if mistaken {
            explain.push_str(" [mistake: took runner-up]");
        }
        Choice {
            goal: best,
            desire: best_desire,
            explain,
            traces,
        }
    }

    fn score(&self, goal: &Goal, context: &Context, tick: u32, gates: &[Gate]) -> Trace {
        let mut trace = Trace {
            goal: goal.id.clone(),
            gated: None,
            features: Vec::with_capacity(self.considerations.len()),
            weighted: Vec::new(),
            attraction: 0.0,
            discount: 0.0,
            desire: 0.0,
            via: None,
        };
        if let Some(reason) = gates.iter().find_map(|gate| gate(context)) {
            trace.gated = Some(reason);
            return trace;
        }
        let (mut top, mut top_id) = (0.0, None);
        let (mut secondary_sum, mut secondary_count) = (0.0, 0u32);
        let mut discount = 1.0;
        for consideration in self.considerations {
            let raw = (consideration.score)(context);
            trace.features.push((consideration.id, raw));
            if consideration.role == Role::Discount {
                discount *= raw;
                continue;
            }
            let weight = consideration
                .weight
                .map_or(1.0, |weight| weight(&context.weights));
            let value = raw * weight;
            trace.weighted.push((consideration.id, value));
            if value > top {
                // The previous leader demotes to a secondary reason.
                if top_id.is_some() {
                    secondary_sum += top;
                    secondary_count += 1;
                }
                top = value;
                top_id = Some(consideration.id);
            } else {
                secondary_sum += value;
                secondary_count += 1;
            }
        }
        let secondary_mean = if secondary_count > 0 {
            secondary_sum / f64::from(secondary_count)
        } else {
            0.0
        };
        trace.attraction = top + SECONDARY_WEIGHT * secondary_mean;
        trace.discount = discount;
        trace.desire = trace.attraction * discount;
        if Some(goal.id.as_str()) == self.committed.as_deref() && tick < self.commitment_until {
            trace.desire += HYSTERESIS_BONUS;
        }
        trace.via = top_id;
        trace
    }
}

/// The best ungated candidate other than the winner.
fn runner_up(traces: &[Trace], winner: usize) -> Option<usize> {
    traces
        .iter()
        .enumerate()
        .filter(|(index, trace)| *index != winner && trace.gated.is_none())
        .max_by(|(_, a), (_, b)| a.desire.total_cmp(&b.desire))
        .map(|(index, _)| index)
}

/// `explain`: the decision on one line.
fn explain(best: Option<usize>, traces: &[Trace]) -> String {
    let gated: Vec<&Trace> = traces.iter().filter(|t| t.gated.is_some()).collect();
    let Some(best) = best else {
        return format!(
            "goal=none candidates={} gated={}",
            traces.len(),
            gated.len()
        );
    };
    let trace = &traces[best];
    let via = trace.via.unwrap_or("?");
    let mut line = format!(
        "goal={} desire={:.2} via={via}({:.2}) route={:.2} danger={:.2}",
        trace.goal,
        trace.desire,
        trace.feature(via).unwrap_or(0.0),
        trace.feature("route_cost").unwrap_or(1.0),
        trace.feature("danger").unwrap_or(1.0),
    );
    if let Some(next) = runner_up(traces, best) {
        let _ = write!(
            line,
            " next={}({:.2})",
            traces[next].goal, traces[next].desire
        );
    }
    if let Some(first) = gated.first() {
        let _ = write!(
            line,
            " gated={}[{}…]",
            gated.len(),
            first.gated.unwrap_or_default()
        );
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::agents::player::strategy::{
        considerations::{
            self, Me, Situation,
            tests::{me, pickup},
        },
        goals::PickupKind,
        weights,
    };

    fn context<'a>(goal: &'a Goal, me: Me<'a>, cost: Option<f64>) -> Context<'a> {
        Context {
            goal,
            me,
            situation: Situation {
                scoreboard_enemies: 1,
                ..Situation::default()
            },
            route_cost: cost,
            weights: weights::DEFAULT,
        }
    }

    #[test]
    fn the_best_reason_wins_and_distance_only_discounts() {
        let goals = [
            pickup(0, PickupKind::Health, "small"),
            pickup(1, PickupKind::Health, "mega"),
            pickup(2, PickupKind::Armor, "ra"),
        ];
        let mut arbiter = Arbiter::new(&considerations::ALL, 0.0);
        let mut random = Random::new(1);
        // The bubble is right here and the Mega across the room.
        let costs = [0.0, 1500.0, 9000.0];
        let choice = arbiter.select(
            &goals,
            |goal| {
                let index = goals.iter().position(|g| g.id == goal.id).unwrap();
                context(goal, me(40, 100), Some(costs[index]))
            },
            10,
            &[],
            &mut random,
        );
        assert_eq!(choice.goal, Some(1), "{}", choice.explain);
        assert!(
            choice.explain.starts_with("goal=pickup:1 "),
            "{}",
            choice.explain
        );
        assert!(
            choice.explain.contains("via=item_value"),
            "{}",
            choice.explain
        );
        // Full armor: the Red Armor is gated, not merely unattractive.
        let no_gain: Gate = &|context: &Context| {
            let pickup = context.goal.pickup?;
            (pickup.kind == PickupKind::Armor && context.me.armor >= 100).then_some("no_gain")
        };
        let choice = arbiter.select(
            &goals,
            |goal| context(goal, me(40, 100), Some(0.0)),
            20,
            &[no_gain],
            &mut random,
        );
        assert_eq!(choice.traces[2].gated, Some("no_gain"));
        assert!(
            choice.explain.contains("gated=1[no_gain…]"),
            "{}",
            choice.explain
        );
    }

    #[test]
    fn commitment_holds_a_close_choice_for_its_window_only() {
        let goals = [
            pickup(0, PickupKind::Armor, "ya"),
            pickup(1, PickupKind::Armor, "ya"),
        ];
        let mut arbiter = Arbiter::new(&considerations::ALL, 0.0);
        let mut random = Random::new(1);
        let pick = |arbiter: &mut Arbiter, random: &mut Random, tick, first: f64| {
            arbiter
                .select(
                    &goals,
                    |goal| {
                        let cost = if goal.id == "pickup:0" { first } else { 1000.0 };
                        context(goal, me(100, 0), Some(cost))
                    },
                    tick,
                    &[],
                    random,
                )
                .goal
        };
        assert_eq!(pick(&mut arbiter, &mut random, 0, 900.0), Some(0));
        // Slightly farther now, but the bonus holds it inside the window.
        assert_eq!(pick(&mut arbiter, &mut random, 100, 1100.0), Some(0));
        // Re-picking did not extend the window: past it, the closer one wins.
        assert_eq!(pick(&mut arbiter, &mut random, 190, 1100.0), Some(1));
        assert_eq!(arbiter.committed(), Some("pickup:1"));
        arbiter.reset();
        assert_eq!(arbiter.committed(), None);
    }

    #[test]
    fn a_mistake_takes_the_runner_up_on_a_switch_and_holds_it() {
        let goals = [
            pickup(0, PickupKind::Armor, "ra"),
            pickup(1, PickupKind::Armor, "ga"),
        ];
        let mut arbiter = Arbiter::new(&considerations::ALL, 1.0);
        let mut random = Random::new(7);
        let choose = |arbiter: &mut Arbiter, random: &mut Random, tick| {
            arbiter.select(
                &goals,
                |goal| context(goal, me(100, 0), Some(0.0)),
                tick,
                &[],
                random,
            )
        };
        let first = choose(&mut arbiter, &mut random, 0);
        assert_eq!(first.goal, Some(1));
        assert!(first.explain.ends_with("[mistake: took runner-up]"));
        assert_eq!(choose(&mut arbiter, &mut random, 100).goal, Some(1));
        assert_eq!(mistake_probability(8.0), 0.5);
        assert_eq!(mistake_probability(0.0), 0.0);
    }
}

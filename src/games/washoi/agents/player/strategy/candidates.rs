//! What the goal arbiter chooses between, after TypeScript `goals/BotGoal.ts`
//! as `ItemGoalSource.ts` and `DirectiveGoalSource.ts` build it: the map's
//! pickups and the server orders addressed to this bot, with the facts the
//! considerations read. The arbiter picks one, and the route follower or the
//! orders walk to it.

use crate::games::washoi::agents::player::strategy::goals::PickupGoal;

/// What a chosen goal makes the bot do.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::games::washoi) enum GoalTarget {
    /// Walk to this pickup, by its stable pickup ID.
    Pickup(usize),
    /// Follow this server order, by its directive ID.
    Directive(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::games::washoi) enum CandidateKind {
    Pickup,
    /// `move_to`: plant, defuse, or reach a spot, and stand there.
    MoveTo,
    /// `hold_area` and `defend`.
    Guard,
    Escort,
}

/// A directive's `interrupt`, what may take the bot off it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::games::washoi) enum Interrupt {
    Never,
    Danger,
    EnemyVisible,
    HigherPriority,
}

impl Interrupt {
    pub(in crate::games::washoi) fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "never" => Self::Never,
            "danger" => Self::Danger,
            "enemy_visible" => Self::EnemyVisible,
            "higher_priority" => Self::HigherPriority,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub(in crate::games::washoi) struct Candidate {
    /// Stable across decisions: `pickup:<id>` or the directive's ID. The
    /// commitment window and hysteresis key on it.
    pub(in crate::games::washoi) id: String,
    pub(in crate::games::washoi) target: GoalTarget,
    /// Where the goal is: a pickup's node, a standing origin, or the
    /// order's resolved target.
    pub(in crate::games::washoi) position: [f64; 3],
    /// Walk distance in world units from the bot's graph node, or None when
    /// no walk is known from here.
    pub(in crate::games::washoi) route_cost: Option<f32>,
    pub(in crate::games::washoi) kind: CandidateKind,
    /// A directive's priority. Zero for pickups.
    pub(in crate::games::washoi) priority: f64,
    /// A directive's urgency, `AgentDirective.urgency`, zero when absent.
    pub(in crate::games::washoi) urgency: f64,
    pub(in crate::games::washoi) interrupt: Interrupt,
    /// An escort order's allied carrier.
    pub(in crate::games::washoi) escort: Option<String>,
    pub(in crate::games::washoi) pickup: Option<PickupGoal>,
}

impl Candidate {
    pub(in crate::games::washoi) fn pickup(
        pickup: PickupGoal,
        position: [f64; 3],
        route_cost: Option<f32>,
    ) -> Self {
        Self {
            id: format!("pickup:{}", pickup.id),
            target: GoalTarget::Pickup(pickup.id),
            position,
            route_cost,
            kind: CandidateKind::Pickup,
            priority: 0.0,
            urgency: 0.0,
            interrupt: Interrupt::HigherPriority,
            escort: None,
            pickup: Some(pickup),
        }
    }
}

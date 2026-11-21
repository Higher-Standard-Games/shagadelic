//! One record per decision, from TypeScript `bot/DecisionTrace.ts`: what the
//! bot was doing and why, for tools that explain behavior from outside and
//! for the training data HUNCH needs. Built only for a bot made with
//! `with_trace(true)`. An untraced bot pays nothing.

use serde::Serialize;

/// `TRACED_CANDIDATES`: candidates kept per record.
pub const TRACED_CANDIDATES: usize = 3;

/// A candidate goal as the arbiter scored it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TracedCandidate {
    pub id: String,
    pub desire: f64,
    /// The strongest attractor, for one that passed the gates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<&'static str>,
}

/// `DecisionTrace`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DecisionTrace {
    pub tick: u32,
    /// What the decision did, in place of the TypeScript `BotState`:
    /// `dead`, `traversal`, `fight`, `lunge`, `chase`, `order`, or `roam`.
    pub state: &'static str,
    /// IDs of the gametype orders offered.
    pub orders: Vec<String>,
    /// The goal the arbiter chose, or None when it chose nothing.
    pub goal: Option<String>,
    /// The best candidates past the gates, best first.
    pub candidates: Vec<TracedCandidate>,
    /// Candidates a gate ruled out.
    pub gated: usize,
    /// The arbiter's one-line explanation.
    pub explain: String,
    /// The movement sequence under way, if any.
    pub sequence: Option<&'static str>,
    pub position: Option<[f64; 3]>,
    pub speed: f64,
    pub grounded: bool,
    pub crouching: bool,
    pub health: Option<u16>,
    pub armor: Option<u16>,
    pub weapon: String,
}

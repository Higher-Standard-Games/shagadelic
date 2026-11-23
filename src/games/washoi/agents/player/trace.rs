//! One record per decision, from TypeScript `bot/DecisionTrace.ts`: what the
//! bot was doing and why, for tools that explain behavior from outside and
//! for the training data HUNCH needs. Built only for a bot made with
//! `with_trace(true)`. An untraced bot pays nothing.

use serde::Serialize;

/// `TRACED_CANDIDATES`: candidates kept per record.
pub const TRACED_CANDIDATES: usize = 3;

/// A goal candidate as a HUNCH model scored it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HunchScore {
    pub id: String,
    /// Not finite when a row was, and then the rule's goal stands.
    pub score: f32,
}

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
    /// The goal pursued, or None when nothing was chosen: the arbiter's, or
    /// HUNCH's when a model decides.
    pub goal: Option<String>,
    /// The arbiter's own goal, when a HUNCH model scores goals.
    pub rule: Option<String>,
    /// HUNCH's score for each eligible candidate, in row order, when a
    /// model scores goals.
    pub hunch_scores: Vec<HunchScore>,
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
    /// The strategy and the diagnosis it answers.
    pub strategy: String,
    /// The candidate a HUNCH model preferred, when one scores goals.
    pub hunch: Option<String>,
    /// The digest of the HUNCH goal model bound to this bot, if any.
    pub hunch_model: Option<String>,
    /// Wall-clock nanoseconds HUNCH took to build its rows and score them,
    /// when a model scores goals.
    pub hunch_nanos: Option<u64>,
    /// Wall-clock microseconds the whole decision took, up to this record.
    /// Traces only: decisions run on simulation time.
    pub decide_micros: u64,
    /// Which part of the bot set the movement: `route`, `order`,
    /// `traversal`, `strafe`, `order_strafe`, `peek`, `climb`, `dodge`,
    /// `fade`, `escape`, `chase`, `hunt`, `hold`, `lunge`, `unstick`,
    /// `pad_ride`, `climb_jump`, or `wall_jump`.
    pub mover: &'static str,
    /// An enemy in sight this decision.
    pub engaged: bool,
    /// Fire or alt-fire went out in a command since the previous decision.
    /// The aim gate adds and holds fire as commands are paced out, after
    /// the decision plans them, so the planned input alone never shows it.
    pub firing: bool,
    /// Clearance turned the heading round a wall, ledge, or trigger
    /// (`navigation::clearance`), as TypeScript `BotSteering.steered`.
    pub steered: bool,
}

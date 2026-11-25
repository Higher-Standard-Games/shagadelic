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
    /// Of the commands sent since the previous decision, the share that
    /// held sprint, and the share whose keys Washoi gives run speed to:
    /// sprint, forward alone, no fire or crouch.
    #[serde(default)]
    pub sprint: f32,
    #[serde(default)]
    pub run_keys: f32,
    /// Over the sprinting commands that moved, the mean degrees the sent
    /// view was off the move, and the share off past 45 degrees.
    #[serde(default)]
    pub sprint_off: f32,
    #[serde(default)]
    pub sprint_wide: f32,
    /// Degrees the decision's planned view is off its planned move.
    #[serde(default)]
    pub planned_off: f32,
    /// Clearance turned the heading round a wall, ledge, or trigger
    /// (`navigation::clearance`), as TypeScript `BotSteering.steered`.
    pub steered: bool,
    /// Where the followed gametype order points, while one is followed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_target: Option<[f64; 3]>,
    /// The strafe-jump chain since the last record (`navigation::hop`): the
    /// first gate that refused a start (`cooldown`, `skill`, `fighting`,
    /// `airborne`, `sliding`, `no_wish`, `keys`, `slow`, `no_lane`,
    /// `heading`, `ally`), a start (`start_straight`), a
    /// landing (`land_straight`, `land_no_lane`, `land_short`),
    /// or an end (`end_long`, `end_takeoff`, `end_ground`, `end_slow`,
    /// `end_off_line`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hop: Option<&'static str>,
    /// Why the route's tricks had no hop lane: `no_route`, `exact`,
    /// `traversal`, `item_near`, `end_near`, `no_sight`, or `lane_closed`
    /// when no straight lane is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hop_lane: Option<&'static str>,
    /// Horizontal speed at the landing `hop` records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hop_landing_speed: Option<f32>,
    /// A running chain held back a slide: on the ground at slide speed with
    /// a slide lane open.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub slide_held: bool,
    /// Where the walk's line came from this decision (`navigation::route_line`):
    /// the line held from before (`held`), a stretch's line joined
    /// (`shared`), a run onto one (`run_onto`), one built from where the bot
    /// stands (`own`), or none, walking the route's waypoints: no stretch
    /// of plain walking ahead (`none`), or no map geometry (`no_sight`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub walk_line: Option<&'static str>,
    /// Walk lines built since the last record.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub line_builds: u32,
    /// Input timing, for how long a decided input takes to reach the
    /// server's simulation: commands sent on this connection by the
    /// decision, the sequence the snapshot says the server had run, commands
    /// sent since the last record, and milliseconds from the snapshot's
    /// arrival to the decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<InputTiming>,
    /// This decision dropped a learned route's or maneuver's approach
    /// for a fight or a fade.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approach_yielded: bool,
    /// Believed against true, for diagnosis only: major pickups the bot
    /// believes up that the server has down, and down that it has up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items_wrong_up: Vec<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items_wrong_down: Vec<usize>,
    /// Major pickups back since the last decision that the bot had a return
    /// time for: `[id, believed minus real return in ticks]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub item_return_errors: Vec<[i64; 2]>,
}

/// See `DecisionTrace::inputs`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct InputTiming {
    pub sent: u64,
    pub acked: u32,
    pub since: u32,
    pub age_ms: f32,
}

fn is_zero(count: &u32) -> bool {
    *count == 0
}

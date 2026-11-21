//! The kinds of Washoi AI. Every kind takes `Observation`s and decides on
//! the caller's clock. How its actions reach the game is up to its hosting:
//! `NetworkedPlayer` is the contract for agents played as networked clients.

use crate::games::washoi::{Controls, Observation, SimTime};

pub(in crate::games::washoi) mod player;

/// A Washoi agent of any kind. `observe_at` only records an observation and
/// its events; `decide` does the work, so a scheduler can budget it.
pub trait Agent: Send {
    fn observe_at(&mut self, observation: Observation, now: SimTime);
    fn decide(&mut self, now: SimTime);
}

/// An agent that plays as a networked Washoi player: it may ask the server
/// for a respawn, and it produces one `Controls` per logical input tick.
pub trait NetworkedPlayer: Agent {
    /// Whether to send a respawn request now. Asking starts its delay.
    fn request_respawn_at(&mut self, now: SimTime) -> bool;
    /// The controls due by `now`, paced at `INPUT_PERIOD`.
    fn inputs_at(&mut self, now: SimTime) -> Vec<Controls>;
}

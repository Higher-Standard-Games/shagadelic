//! Networked players: one WebSocket client per bot. The session drives each
//! brain only through `agents::NetworkedPlayer`. `connection` is the generic
//! Washoi connection, and `player` is the player's wire codec over it.

mod connection;
mod events;
mod player;
pub mod protocol;
pub mod session;
mod snapshot;

pub use events::{Point, SnapshotEvent, WashoiEvents};
pub use snapshot::{ReceivedSnapshot, SnapshotReceiver};

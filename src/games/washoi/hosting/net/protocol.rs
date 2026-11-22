//! The Washoi wire protocol as one public surface: the generic connection's
//! snapshots, events, and errors, and the player codec's admission,
//! observation mapping, and inputs.

pub use super::connection::AdmissionError;
pub(crate) use super::player::InputBatch;
pub use super::player::{
    Admission, AdmissionEvent, Connection, PlayerInput, connect, entity_keys, observe,
};
pub use super::{Point, ReceivedSnapshot, SnapshotEvent, SnapshotReceiver, WashoiEvents};

//! Washoi wire protocol integration.

mod admission;
mod connection;
mod events;
mod keys;
mod observation;
mod snapshot;

pub use admission::{Admission, AdmissionEvent};
pub(crate) use connection::InputBatch;
pub use connection::{AdmissionError, Connection, PlayerInput, connect};
pub use events::{Point, SnapshotEvent, WashoiEvents};
pub use observation::observe;
pub use snapshot::{ReceivedSnapshot, SnapshotReceiver};

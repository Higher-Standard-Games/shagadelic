//! Washoi wire protocol integration.

mod admission;
mod connection;

pub use admission::{Admission, AdmissionEvent};
pub use connection::{AdmissionError, Connection, connect};

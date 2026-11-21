//! Caller-driven AI session runtime for embedded games and standalone hosts.

pub mod hunch;
pub mod runtime;

#[cfg(feature = "washoi")]
pub mod games;

//! Networked players: one WebSocket client per bot. The session drives each
//! brain only through `agents::NetworkedPlayer`.

pub mod protocol;
pub mod session;

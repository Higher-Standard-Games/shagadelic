//! How a bot moves in a fight: strafing and dodges, cover, high ground, and
//! the chase after an enemy out of sight, and the ground a duelist holds.

pub(in crate::games::washoi) mod chase;
pub(in crate::games::washoi) mod cover;
pub(in crate::games::washoi) mod escape;
pub(in crate::games::washoi) mod highground;
pub(in crate::games::washoi) mod strafe;
pub(in crate::games::washoi) mod territory;

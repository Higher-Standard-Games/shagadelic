//! What a Washoi bot receives and returns, independent of the wire. The
//! adapter builds an `Observation` from each reconstructed snapshot for one
//! bot and turns `Controls` into paced Washoi commands. Time is the caller's
//! `SimTime`, so a replay or an embedded host can drive the brain without a
//! clock or a socket.
//!
//! Positions and velocities use SHAGGER's plain math types. Events are the
//! Washoi event catalogue in snapshot order.
//!
//! The brain may receive several observations before it decides. It keeps
//! every event, tagged with its snapshot tick, in order, but only the latest
//! bodies, players, and pickup mask. Perception, the noticing delay, and
//! chase memory therefore see the latest positions only. Velocities come
//! from the snapshot, never from differences between positions.

use std::{
    ops::{Add, Sub},
    time::Duration,
};

pub use shagger_shared::net::game_state::{Vec3, Velocity};

use super::protocol::SnapshotEvent;

/// Simulation time since the session started. It never goes backward.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SimTime(Duration);

impl SimTime {
    pub const ZERO: Self = Self(Duration::ZERO);

    pub const fn from_elapsed(elapsed: Duration) -> Self {
        Self(elapsed)
    }

    pub const fn elapsed(self) -> Duration {
        self.0
    }
}

impl Add<Duration> for SimTime {
    type Output = Self;

    fn add(self, duration: Duration) -> Self {
        Self(self.0 + duration)
    }
}

impl Sub for SimTime {
    type Output = Duration;

    /// The time between two instants, zero when `earlier` is later.
    fn sub(self, earlier: Self) -> Duration {
        self.0.saturating_sub(earlier.0)
    }
}

/// One player in the scoreboard: the entity that plays it, and its team. The
/// team is empty, `"ffa"`, or `"spectator"` outside team play.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerView {
    pub entity_id: String,
    pub team: String,
}

/// An entity's physical state, when the snapshot carries it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Body {
    pub position: Option<Vec3>,
    pub velocity: Option<Velocity>,
}

/// One bot's view of one server snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Observation {
    /// The server tick the snapshot describes.
    pub tick: u32,
    pub players: Vec<PlayerView>,
    /// Every entity with a body, in snapshot order: players, projectiles
    /// (`proj_` IDs), and anything else the server sends.
    pub bodies: Vec<(String, Body)>,
    /// Active state of every map pickup, indexed by the stable pickup ID of
    /// the map's pickup table (`PickupTable.ts` BSP order), the IDs the baked
    /// navigation's pickup goals use. Empty when the snapshot has no mask.
    pub pickup_active: Vec<bool>,
    /// The snapshot's one-tick events, in wire order.
    pub events: Vec<SnapshotEvent>,
}

impl Observation {
    pub fn body(&self, entity_id: &str) -> Option<&Body> {
        self.bodies
            .iter()
            .find(|(id, _)| id == entity_id)
            .map(|(_, body)| body)
    }

    pub fn body_mut(&mut self, entity_id: &str) -> Option<&mut Body> {
        self.bodies
            .iter_mut()
            .find(|(id, _)| id == entity_id)
            .map(|(_, body)| body)
    }

    /// The body of `entity_id`, added at the end when missing.
    pub fn body_or_insert(&mut self, entity_id: &str) -> &mut Body {
        let index = match self.bodies.iter().position(|(id, _)| id == entity_id) {
            Some(index) => index,
            None => {
                self.bodies.push((entity_id.to_owned(), Body::default()));
                self.bodies.len() - 1
            }
        };
        &mut self.bodies[index].1
    }

    pub fn player(&self, entity_id: &str) -> Option<&PlayerView> {
        self.players
            .iter()
            .find(|player| player.entity_id == entity_id)
    }

    pub fn player_mut(&mut self, entity_id: &str) -> Option<&mut PlayerView> {
        self.players
            .iter_mut()
            .find(|player| player.entity_id == entity_id)
    }
}

/// What the bot does for one logical input tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Controls {
    /// The wish direction in world space, normalized or zero. The adapter may
    /// snap it to held movement keys unless `exact` is set.
    pub direction: [f64; 3],
    pub yaw: f64,
    pub pitch: f64,
    /// Washoi `InputButtons` bits.
    pub buttons: u16,
    /// A 1-based weapon slot to switch to, or 0 to keep the current weapon.
    pub weapon_slot: u8,
    /// A recorded or precise maneuver: send the direction and buttons as they
    /// are, in order, one per tick. The encoder also releases any movement
    /// keys it held, so they don't leak into the next ordinary command.
    pub exact: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulation_time_orders_and_measures_without_going_negative() {
        let later = SimTime::ZERO + Duration::from_millis(40);
        assert!(later > SimTime::ZERO);
        assert_eq!(later - SimTime::ZERO, Duration::from_millis(40));
        assert_eq!(SimTime::ZERO - later, Duration::ZERO);
        assert_eq!(later.elapsed(), Duration::from_millis(40));
    }
}

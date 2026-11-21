//! Washoi bot sequences from `src/games/fps/bot/sequences/`
//! `ValidatedJumpSequence.ts`, `TraverseJumppadSequence.ts`, and
//! `TraverseTeleporterSequence.ts`. The server supplies movement outcomes.

use std::time::Duration;

use crate::games::washoi::SimTime;
use serde::Deserialize;
use shagger_shared::net::game_state::{Vec3, Velocity};

use super::PlayerInput;

const FORWARD: u16 = 1;
const JUMP: u16 = 1 << 4;
const SPRINT: u16 = 1 << 6;
const TICK_MS: u64 = 8;

#[derive(Clone, Copy, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Maneuver {
    Jump {
        takeoff: [f64; 3],
        landing: [f64; 3],
        yaw: f64,
        speed: f64,
        radius: f64,
        ticks: u16,
    },
    Jumppad {
        min: [f64; 3],
        max: [f64; 3],
        landing: [f64; 3],
    },
    Teleporter {
        min: [f64; 3],
        max: [f64; 3],
        exit: [f64; 3],
        yaw: f64,
    },
}

impl Maneuver {
    pub(super) fn enter_before_waypoint(self, position: Vec3) -> bool {
        match self {
            Self::Jumppad { min, max, .. } | Self::Teleporter { min, max, .. } => {
                near_trigger(position, min, max, 0.0)
            }
            Self::Jump { .. } => false,
        }
    }

    pub(super) fn observed_teleport(self, before: Vec3, after: Vec3) -> bool {
        matches!(self, Self::Teleporter { min, max, exit, .. }
            if near_trigger(before, min, max, 24.0) && near(after, exit, 72.0, 72.0))
    }

    pub(super) fn pad_launch(self, position: Vec3, velocity: Option<Velocity>) -> bool {
        matches!(self, Self::Jumppad { min, max, .. }
            if near_trigger(position, min, max, 64.0)
                && velocity.is_some_and(|v| !v.grounded && v.linear.y >= 80.0))
    }

    pub(super) fn valid(self) -> bool {
        let point = |p: [f64; 3]| p.into_iter().all(f64::is_finite);
        let bounds = |min: [f64; 3], max: [f64; 3]| {
            point(min) && point(max) && min.into_iter().zip(max).all(|(a, b)| a <= b)
        };
        match self {
            Self::Jump {
                takeoff,
                landing,
                yaw,
                speed,
                radius,
                ticks,
            } => {
                point(takeoff)
                    && point(landing)
                    && yaw.is_finite()
                    && (0.0..=400.0).contains(&speed)
                    && (1.0..=128.0).contains(&radius)
                    && (1..=220).contains(&ticks)
            }
            Self::Jumppad { min, max, landing } => bounds(min, max) && point(landing),
            Self::Teleporter {
                min,
                max,
                exit,
                yaw,
            } => bounds(min, max) && point(exit) && yaw.is_finite(),
        }
    }
}

pub(super) enum Outcome {
    Running(PlayerInput),
    Arrived(Option<f64>),
    Failed,
}

pub(super) struct Traversal {
    maneuver: Maneuver,
    started: SimTime,
    activated: Option<SimTime>,
    airborne: bool,
}

impl Traversal {
    pub(super) fn new(maneuver: Maneuver, now: SimTime) -> Self {
        Self {
            maneuver,
            started: now,
            activated: None,
            airborne: false,
        }
    }

    pub(super) fn update(
        &mut self,
        position: Vec3,
        velocity: Option<Velocity>,
        now: SimTime,
    ) -> Outcome {
        if now - self.started >= ticks(260) {
            return Outcome::Failed;
        }
        let Some(velocity) = velocity else {
            return Outcome::Running(PlayerInput::default());
        };
        match self.maneuver {
            Maneuver::Jump {
                takeoff,
                landing,
                yaw,
                speed,
                radius,
                ticks: flight_ticks,
            } => {
                if let Some(activated) = self.activated {
                    let elapsed = now - activated;
                    if self.airborne && velocity.grounded {
                        return if near(position, landing, radius, 72.0) {
                            Outcome::Arrived(None)
                        } else {
                            Outcome::Failed
                        };
                    }
                    if elapsed >= ticks(u64::from(flight_ticks))
                        || (!self.airborne && velocity.grounded && elapsed >= ticks(12))
                    {
                        return Outcome::Failed;
                    }
                    self.airborne |= !velocity.grounded;
                    return forward(yaw, if self.airborne { 0 } else { JUMP });
                }
                if !velocity.grounded {
                    // A newly spawned bot can still be dropping onto the
                    // takeoff. Wait for support before pressing jump.
                    return Outcome::Running(PlayerInput {
                        yaw,
                        ..PlayerInput::default()
                    });
                }
                if !near(position, takeoff, 32.0, 40.0) {
                    return forward(bearing(position, takeoff, yaw), 0);
                }
                let along = velocity.linear.x * yaw.sin() + velocity.linear.z * yaw.cos();
                if along < speed * 0.9 {
                    return forward(yaw, 0);
                }
                self.activated = Some(now);
                forward(yaw, JUMP)
            }
            Maneuver::Jumppad { min, max, landing } => {
                if self.airborne {
                    if velocity.grounded {
                        return if near(position, landing, 96.0, 96.0) {
                            Outcome::Arrived(None)
                        } else {
                            Outcome::Failed
                        };
                    }
                    return forward(bearing(position, landing, 0.0), 0);
                }
                if near_trigger(position, min, max, 64.0)
                    && !velocity.grounded
                    && velocity.linear.y >= 80.0
                {
                    self.airborne = true;
                    return forward(bearing(position, landing, 0.0), 0);
                }
                self.approach(position, min, max, now)
            }
            Maneuver::Teleporter {
                min,
                max,
                exit,
                yaw,
            } => {
                if near(position, exit, 72.0, 72.0) {
                    return Outcome::Arrived(Some(yaw));
                }
                self.approach(position, min, max, now)
            }
        }
    }

    fn approach(&mut self, position: Vec3, min: [f64; 3], max: [f64; 3], now: SimTime) -> Outcome {
        if near_trigger(position, min, max, 0.0) {
            self.activated.get_or_insert(now);
        }
        if self.activated.is_some_and(|at| now - at >= ticks(40)) {
            return Outcome::Failed;
        }
        let center = std::array::from_fn(|axis| (min[axis] + max[axis]) / 2.0);
        forward(bearing(position, center, 0.0), 0)
    }
}

fn ticks(count: u64) -> Duration {
    Duration::from_millis(count * TICK_MS)
}

fn forward(yaw: f64, buttons: u16) -> Outcome {
    Outcome::Running(PlayerInput {
        direction: [yaw.sin(), 0.0, yaw.cos()],
        yaw,
        buttons: FORWARD | SPRINT | buttons,
        ..PlayerInput::default()
    })
}

fn bearing(from: Vec3, to: [f64; 3], fallback: f64) -> f64 {
    let (x, z) = (to[0] - from.x, to[2] - from.z);
    if x.hypot(z) < 1.0 {
        fallback
    } else {
        x.atan2(z)
    }
}

fn near(position: Vec3, target: [f64; 3], horizontal: f64, vertical: f64) -> bool {
    (target[0] - position.x).hypot(target[2] - position.z) <= horizontal
        && (target[1] - position.y).abs() <= vertical
}

fn near_trigger(p: Vec3, min: [f64; 3], max: [f64; 3], extra: f64) -> bool {
    p.x + 16.0 + extra >= min[0]
        && p.x - 16.0 - extra <= max[0]
        && p.y + 32.0 + extra >= min[1]
        && p.y - 24.0 - extra <= max[1]
        && p.z + 16.0 + extra >= min[2]
        && p.z - 16.0 - extra <= max[2]
}

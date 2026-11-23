//! Washoi's own movement, stepped ahead of the bot: the rollouts TypeScript
//! runs through `MovementProfile.simulate` to prove a move before making it
//! (`LedgeSlideJump.ts`, `sequences/FlyingStart.ts`, `BotVisionTracer.ts`).
//! The simulation is the one the server runs, so a move that lands in a
//! rollout lands in the game, given the same collision world.

use shagger_shared::{
    bsp::trigger_table::TriggerVolume,
    collision::collision_world::CollisionWorld,
    net::game_state::Vec3,
    network::systems::movement::movement_profile::{MovementInput, ViewAngles},
    server::simulation::types::Vec3 as SimVec3,
};
use washoi_movement::simulation::movement_profile::{
    DEFAULT_FPS_MOVEMENT_CONFIG, MovementProfile, MovementProfileState,
};

use crate::games::washoi::hosting::net::protocol::PlayerInput;

/// One server tick at 125 Hz.
pub(in crate::games::washoi) const TICK_SECONDS: f64 = 1.0 / 125.0;

fn sim(point: Vec3) -> SimVec3 {
    SimVec3 {
        x: point.x,
        y: point.y,
        z: point.z,
    }
}

fn game(point: SimVec3) -> Vec3 {
    Vec3::new(point.x, point.y, point.z)
}

/// A body stepped tick by tick from a known state.
pub(in crate::games::washoi) struct Rollout {
    profile: MovementProfile,
    pub(in crate::games::washoi) position: Vec3,
    pub(in crate::games::washoi) velocity: Vec3,
    tick: u32,
}

/// What one tick left the body doing.
#[derive(Clone, Copy, Debug)]
pub(in crate::games::washoi) struct Step {
    pub(in crate::games::washoi) grounded: bool,
    pub(in crate::games::washoi) sliding: bool,
}

impl Rollout {
    /// From `position` and `velocity`, as TypeScript builds a rollout's
    /// profile: wall jumps and ledge grabs off, so the plan depends only on
    /// the moves it scripts.
    pub(in crate::games::washoi) fn new(position: Vec3, velocity: Vec3, grounded: bool) -> Self {
        let mut config = DEFAULT_FPS_MOVEMENT_CONFIG;
        config.wall_jump_enabled = false;
        config.ledge_grab_enabled = false;
        let mut profile = MovementProfile::new(config);
        profile.reset_physics_state(grounded);
        Self {
            profile,
            position,
            velocity,
            tick: 0,
        }
    }

    /// From `position` and `velocity` with Washoi's whole movement, wall
    /// jumps and ledge grabs included, and the map's jump pads and
    /// teleporters, as TypeScript's `FlyingStart` builds its profile: a
    /// learned route's trick may use any of them.
    pub(in crate::games::washoi) fn full(
        position: Vec3,
        velocity: Vec3,
        grounded: bool,
        triggers: &[TriggerVolume],
    ) -> Self {
        let mut profile = MovementProfile::new(DEFAULT_FPS_MOVEMENT_CONFIG);
        profile.set_trigger_volumes(triggers);
        profile.reset_physics_state(grounded);
        Self {
            profile,
            position,
            velocity,
            tick: 0,
        }
    }

    /// From `position` and `velocity` with the game's own profile, wall jumps
    /// and ledge grabs on and no trigger volumes, as `BotVisionTracer.rollout`
    /// builds it to prove a climb or a wall jump. Its first tick is tick 1.
    pub(in crate::games::washoi) fn with_tricks(
        position: Vec3,
        velocity: Vec3,
        grounded: bool,
    ) -> Self {
        let mut profile = MovementProfile::new(DEFAULT_FPS_MOVEMENT_CONFIG);
        profile.reset_physics_state(grounded);
        Self {
            profile,
            position,
            velocity,
            tick: 1,
        }
    }

    /// What the movement carries between ticks besides position and velocity.
    pub(in crate::games::washoi) fn state(&self) -> MovementProfileState {
        self.profile.save_state()
    }

    /// One tick of `input`, as the server applies it.
    pub(in crate::games::washoi) fn step(
        &mut self,
        input: &PlayerInput,
        world: &mut dyn CollisionWorld,
    ) -> Step {
        let [x, y, z] = input.direction;
        let command = MovementInput {
            direction: SimVec3 { x, y, z },
            view_angles: ViewAngles {
                pitch: input.pitch,
                yaw: input.yaw,
            },
            buttons: u32::from(input.buttons),
            sequence: Some(f64::from(self.tick)),
            sim_step: Some(f64::from(self.tick)),
        };
        let result = self.profile.simulate(
            sim(self.position),
            sim(self.velocity),
            Some(&command),
            TICK_SECONDS,
            Some(world),
        );
        self.tick += 1;
        self.position = game(result.position);
        self.velocity = game(result.velocity);
        Step {
            grounded: result.grounded,
            sliding: result.is_sliding,
        }
    }
}

/// Axis-aligned boxes as a collision world, for tests: solid boxes that stop
/// the player hull, and slime or lava boxes it passes through. It slides
/// along what it hits but never steps up, so test floors are flat.
#[cfg(test)]
pub(in crate::games::washoi) mod box_world {
    use shagger_shared::{
        bsp::{brush_model_state::BrushModelState, lift_system::LiftSystem},
        collision::{
            collision_world::{
                SweepBackend, SweepBackendTraceOptions, SweepMove, SweepTrace, contents,
            },
            shared_collision_world::SharedCollisionWorld,
        },
        physics::player_physics_constants::{PLAYER_HEIGHT, PLAYER_MINS_Z},
        server::simulation::types::Vec3,
    };

    type Box3 = ([f64; 3], [f64; 3]);

    #[derive(Default)]
    pub(in crate::games::washoi) struct Boxes {
        pub(in crate::games::washoi) solids: Vec<Box3>,
        /// With their shared contents, `contents::LAVA` or `contents::SLIME`.
        pub(in crate::games::washoi) liquids: Vec<(Box3, u32)>,
    }

    pub(in crate::games::washoi) fn world(boxes: Boxes) -> SharedCollisionWorld<Boxes> {
        SharedCollisionWorld::new(boxes)
    }

    fn at(point: Vec3) -> [f64; 3] {
        [point.x, point.y, point.z]
    }

    fn back(point: [f64; 3]) -> Vec3 {
        Vec3::new(point[0], point[1], point[2])
    }

    /// The hull's extent below and above its origin, as the sweep backend
    /// scales the player hull to a height.
    fn hull(options: SweepBackendTraceOptions) -> ([f64; 3], [f64; 3]) {
        let radius = options.radius;
        let below = options.height * (-PLAYER_MINS_Z / PLAYER_HEIGHT);
        (
            [-radius, -below, -radius],
            [radius, options.height - below, radius],
        )
    }

    impl Boxes {
        /// The first solid the hull meets from `start` to `end`: the fraction
        /// of the way it gets, backed off a little as Q3 traces are, and the
        /// face normal. A hull that starts inside a solid gets nowhere.
        fn sweep(
            &self,
            start: [f64; 3],
            end: [f64; 3],
            options: SweepBackendTraceOptions,
        ) -> (f64, Option<[f64; 3]>) {
            let (mins, maxs) = hull(options);
            let delta: [f64; 3] = std::array::from_fn(|axis| end[axis] - start[axis]);
            let length = delta.iter().map(|d| d * d).sum::<f64>().sqrt();
            let mut best = (1.0, None);
            for (low, high) in &self.solids {
                let (mut enter, mut exit, mut normal) = (f64::NEG_INFINITY, f64::INFINITY, None);
                let mut missed = false;
                for axis in 0..3 {
                    let (from, to) = (low[axis] - maxs[axis], high[axis] - mins[axis]);
                    if delta[axis] == 0.0 {
                        if start[axis] <= from || start[axis] >= to {
                            missed = true;
                        }
                        continue;
                    }
                    let (mut near, mut far) = (
                        (from - start[axis]) / delta[axis],
                        (to - start[axis]) / delta[axis],
                    );
                    if near > far {
                        std::mem::swap(&mut near, &mut far);
                    }
                    if near > enter {
                        enter = near;
                        let mut face = [0.0; 3];
                        face[axis] = -delta[axis].signum();
                        normal = Some(face);
                    }
                    exit = exit.min(far);
                }
                if missed || enter >= exit || enter > 1.0 || exit <= 0.0 {
                    continue;
                }
                let fraction = if enter < 0.0 {
                    0.0
                } else {
                    (enter - 0.03125 / length.max(1e-9)).max(0.0)
                };
                if fraction < best.0 {
                    best = (fraction, normal);
                }
            }
            best
        }
    }

    impl SweepBackend for Boxes {
        fn lifts(&self) -> Option<&LiftSystem> {
            None
        }

        fn brush_models(&self) -> Option<&BrushModelState> {
            None
        }

        fn trace(
            &mut self,
            start: Vec3,
            end: Vec3,
            options: SweepBackendTraceOptions,
        ) -> SweepTrace {
            let (start, end) = (at(start), at(end));
            let (fraction, normal) = self.sweep(start, end, options);
            SweepTrace {
                fraction,
                end_pos: Some(back(std::array::from_fn(|axis| {
                    start[axis] + (end[axis] - start[axis]) * fraction
                }))),
                normal: normal.map(back),
                contents: (fraction < 1.0).then_some(contents::SOLID),
            }
        }

        fn step_slide_move(
            &mut self,
            start: Vec3,
            movement: Vec3,
            options: SweepBackendTraceOptions,
        ) -> SweepMove {
            let mut position = at(start);
            let mut remaining = at(movement);
            let mut moved = remaining;
            let mut hit_count = 0;
            for _ in 0..4 {
                let end = std::array::from_fn(|axis| position[axis] + remaining[axis]);
                let (fraction, normal) = self.sweep(position, end, options);
                position = std::array::from_fn(|axis| position[axis] + remaining[axis] * fraction);
                let Some(normal) = normal.filter(|_| fraction < 1.0) else {
                    break;
                };
                hit_count += 1;
                let clip = |vector: [f64; 3]| {
                    let into: f64 = (0..3).map(|axis| vector[axis] * normal[axis]).sum();
                    std::array::from_fn(|axis| vector[axis] - normal[axis] * into.min(0.0))
                };
                remaining = clip(std::array::from_fn(|axis| {
                    remaining[axis] * (1.0 - fraction)
                }));
                moved = clip(moved);
            }
            SweepMove {
                end_pos: back(position),
                velocity: back(moved),
                hit_count,
            }
        }

        fn point_contents(&mut self, point: Vec3) -> u32 {
            let point = at(point);
            let inside = |(low, high): &Box3| {
                (0..3).all(|axis| low[axis] <= point[axis] && point[axis] <= high[axis])
            };
            let solid = if self.solids.iter().any(inside) {
                contents::SOLID
            } else {
                0
            };
            self.liquids
                .iter()
                .filter(|(bounds, _)| inside(bounds))
                .fold(solid, |all, (_, liquid)| all | liquid)
        }

        fn is_in_ladder(&mut self, _min: Vec3, _max: Vec3) -> Option<bool> {
            Some(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{box_world::*, *};

    const FORWARD: u16 = 1;
    const SPRINT: u16 = 1 << 6;

    /// The floor's top is at y = 0, so a standing origin is at y = 24.
    pub(in crate::games::washoi) fn floor(to_z: f64) -> Boxes {
        Boxes {
            solids: vec![([-512.0, -16.0, -512.0], [512.0, 0.0, to_z])],
            ..Boxes::default()
        }
    }

    #[test]
    fn a_rollout_runs_on_a_floor_and_falls_off_its_end() {
        let mut world = world(floor(256.0));
        let mut body = Rollout::new(Vec3::new(0.0, 24.0, 0.0), Vec3::new(0.0, 0.0, 320.0), true);
        let run = PlayerInput {
            direction: [0.0, 0.0, 1.0],
            buttons: FORWARD | SPRINT,
            ..PlayerInput::default()
        };
        let mut grounded_at_edge = false;
        let mut fell = false;
        for _ in 0..125 {
            let step = body.step(&run, &mut world);
            if body.position.z < 230.0 {
                grounded_at_edge = step.grounded && (body.position.y - 24.0).abs() < 0.5;
            }
            fell |= body.position.y < -20.0;
        }
        assert!(grounded_at_edge, "runs on the floor at standing height");
        assert!(fell, "and drops off its end: {:?}", body.position);
        // Sprinting forward alone reaches the 400 u/s sprint speed.
        let mut run_up = Rollout::new(Vec3::new(0.0, 24.0, -400.0), Vec3::default(), true);
        let mut world = world_long();
        for _ in 0..125 {
            run_up.step(&run, &mut world);
        }
        assert!(
            (run_up.velocity.z - 400.0).abs() < 1.0,
            "{:?}",
            run_up.velocity
        );
    }

    fn world_long() -> shagger_shared::collision::shared_collision_world::SharedCollisionWorld<Boxes>
    {
        world(floor(4096.0))
    }
}

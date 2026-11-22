//! Getting out of slime and lava, after TypeScript `LiquidEscape.ts` and
//! `StateMachineBrain._escapeLiquid`. Both hurt every second a body is in
//! them, so a bot in either jumps for the nearest dry ground: the nearest
//! graph node out of the liquid, one in sight first, climbs counted double
//! since a bank above the surface takes longer to reach than one level with
//! it. Aim and fire stay.

use shagger_shared::net::game_state::Vec3;

use crate::games::washoi::{Sight, hosting::net::protocol::PlayerInput};

/// Heights above the origin checked for liquid: the feet, the waist, the chest.
const BODY_POINTS: [f64; 3] = [-20.0, 0.0, 20.0];
/// How far a way out is looked for.
const MAX_ESCAPE_DISTANCE: f64 = 1500.0;
/// How far below the bot a way out may be: a pool is left by its banks, not
/// through its floor.
const MAX_ESCAPE_DROP: f64 = 64.0;
/// `LIQUID_ESCAPE_REPLAN_TICKS`, a quarter second at 125 Hz.
const REPLAN_TICKS: u32 = 31;

const FORWARD: u16 = 1;
const BACK: u16 = 1 << 1;
const LEFT: u16 = 1 << 2;
const RIGHT: u16 = 1 << 3;
const JUMP: u16 = 1 << 4;
const CROUCH: u16 = 1 << 5;
const SPRINT: u16 = 1 << 6;

/// Whether any part of a body at `origin` is in slime or lava.
pub(in crate::games::washoi) fn in_harmful_liquid(sight: &Sight, origin: Vec3) -> bool {
    BODY_POINTS
        .iter()
        .any(|dy| sight.liquid_at([origin.x, origin.y + dy, origin.z]) != 0)
}

/// The nearest dry node in sight of `origin`, else the nearest at all.
fn escape_target(sight: &Sight, origin: Vec3, nodes: &[[f64; 3]]) -> Option<[f64; 3]> {
    let mut candidates: Vec<_> = nodes
        .iter()
        .copied()
        .filter(|&[x, y, z]| {
            (x - origin.x).hypot(z - origin.z) <= MAX_ESCAPE_DISTANCE
                && y >= origin.y - MAX_ESCAPE_DROP
                && !in_harmful_liquid(sight, Vec3::new(x, y + 20.0, z))
        })
        .map(|node| {
            let cost = (node[0] - origin.x).hypot(node[2] - origin.z)
                + 2.0 * (node[1] - origin.y).max(0.0);
            (cost, node)
        })
        .collect();
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
    let eye = [origin.x, origin.y + 20.0, origin.z];
    candidates
        .iter()
        .find(|(_, [x, y, z])| sight.clear(eye, [*x, y + 20.0, *z]))
        .or(candidates.first())
        .map(|&(_, node)| node)
}

#[derive(Default)]
pub(in crate::games::washoi) struct LiquidEscape {
    target: Option<(Option<[f64; 3]>, u32)>,
}

impl LiquidEscape {
    pub(in crate::games::washoi) fn reset(&mut self) {
        self.target = None;
    }

    /// The decision's input, turned into a jump for dry ground while the bot
    /// is in slime or lava, or None out of it.
    pub(in crate::games::washoi) fn steer(
        &mut self,
        sight: &Sight,
        nodes: &[[f64; 3]],
        tick: u32,
        position: Vec3,
        input: PlayerInput,
    ) -> Option<PlayerInput> {
        if !in_harmful_liquid(sight, position) {
            self.target = None;
            return None;
        }
        if self
            .target
            .is_none_or(|(_, chosen)| tick.saturating_sub(chosen) >= REPLAN_TICKS)
        {
            self.target = Some((escape_target(sight, position, nodes), tick));
        }
        let (target, _) = self.target.expect("chosen above");
        let heading = match target {
            Some([x, _, z]) => (x - position.x).atan2(z - position.z),
            None => input.direction[0].atan2(input.direction[2]),
        };
        // Jump held: it hops off the bottom and swims up.
        let mut out = input;
        out.direction = [heading.sin(), 0.0, heading.cos()];
        out.buttons = (input.buttons & !(FORWARD | BACK | LEFT | RIGHT | JUMP | CROUCH))
            | FORWARD
            | SPRINT
            | JUMP;
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::perception::sight::{
        LAVA,
        tests::{cube, encode_v2},
    };

    const FIRE: u16 = 1 << 8;

    fn pool() -> Sight {
        // A floor, and a lava pool from x = 0 to 200 sunk 40 below it,
        // with a wall at x = 400 between the pool and a far bank.
        Sight::parse(&encode_v2(
            "test",
            &[
                cube([-600.0, -50.0, -600.0], [800.0, -40.0, 600.0]),
                cube([400.0, -40.0, -600.0], [410.0, 200.0, 600.0]),
            ],
            &[],
            &[(cube([0.0, -40.0, -100.0], [200.0, 10.0, 100.0]), LAVA)],
        ))
        .unwrap()
    }

    #[test]
    fn finds_lava_by_body_point_and_reads_v2_without_changing_sight() {
        let sight = pool();
        assert!(in_harmful_liquid(&sight, Vec3::new(100.0, -10.0, 0.0)));
        assert!(in_harmful_liquid(&sight, Vec3::new(100.0, 25.0, 0.0)));
        assert!(!in_harmful_liquid(&sight, Vec3::new(-100.0, 0.0, 0.0)));
        // The liquid doesn't block sight.
        assert!(sight.clear([-50.0, 0.0, 0.0], [250.0, 0.0, 0.0]));
        // A v2 file without liquids answers like the v1 file it extends.
        let walls = [cube([400.0, -40.0, -600.0], [410.0, 200.0, 600.0])];
        let v1 = Sight::parse(&crate::games::washoi::perception::sight::tests::encode(
            "test",
            &walls,
            &[],
        ))
        .unwrap();
        let v2 = Sight::parse(&encode_v2("test", &walls, &[], &[])).unwrap();
        for to in [[500.0, 0.0, 0.0], [300.0, 0.0, 0.0], [500.0, 300.0, 0.0]] {
            assert_eq!(v1.clear([0.0, 0.0, 0.0], to), v2.clear([0.0, 0.0, 0.0], to));
        }
        assert_eq!(v2.liquid_at([100.0, 0.0, 0.0]), 0);
    }

    #[test]
    fn jumps_for_the_nearest_dry_node_in_sight_keeping_aim_and_fire() {
        let sight = pool();
        // A node in the lava, one behind the wall, and one on the near bank.
        let nodes = [
            [100.0, -16.0, 0.0],
            [500.0, -16.0, 0.0],
            [-150.0, -16.0, 0.0],
        ];
        let mut escape = LiquidEscape::default();
        let aiming = PlayerInput {
            direction: [1.0, 0.0, 0.0],
            yaw: 1.0,
            buttons: FORWARD | FIRE,
            ..PlayerInput::default()
        };
        let out = escape
            .steer(&sight, &nodes, 0, Vec3::new(150.0, -10.0, 0.0), aiming)
            .unwrap();
        assert!(
            out.direction[0] < -0.99,
            "toward the near bank, not through the wall"
        );
        assert_eq!(out.buttons, FORWARD | SPRINT | JUMP | FIRE);
        assert_eq!(out.yaw, 1.0);
        // Out of the liquid, nothing to do.
        assert!(
            escape
                .steer(&sight, &nodes, 10, Vec3::new(-200.0, 0.0, 0.0), aiming)
                .is_none()
        );
    }
}

//! A ledge to fight from with a splash weapon, from TypeScript
//! `bot/BotCombatMovement.ts` `findHighGround` and `states/BotFight.ts`
//! `highGround`. Rockets into the enemy's feet from above are hard to avoid.
//!
//! Every two seconds of such a fight within 900 units, at `dodgeSkill` odds,
//! the bot looks for the highest graph node within 300 units that is at
//! least 64 units above both it and the enemy, no farther than 900 from the
//! enemy, and still in sight of it. The spot is dropped once reached.
//!
//! TypeScript nodes are floor points. The baked Washoi graph stores standing
//! origins, 24 units higher, so the comparisons use the floor under a node.

use super::{Vec3, random::Random};

/// `HIGH_GROUND_SEARCH_TICKS`: two seconds.
const SEARCH_TICKS: u32 = 250;
const RADIUS: f64 = 300.0;
const RISE: f64 = 64.0;
const MAX_RANGE: f64 = 900.0;
/// `findHighGround` looks for the enemy from 40 units above the floor.
const LOOKOUT_HEIGHT: f64 = 40.0;
/// A spot this close horizontally is reached.
const REACHED: f64 = 48.0;
/// The baker lifts floor nodes to a standing origin (`STANDING_LIFT`).
const STANDING_LIFT: f64 = 24.0;

#[derive(Default)]
pub(super) struct HighGround {
    spot: Option<Vec3>,
    search_at: u32,
}

impl HighGround {
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    /// `highGround`: the ledge to make for, if any. `reachable` says whether
    /// a walk on the graph leads to a node.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn spot(
        &mut self,
        tick: u32,
        nodes: &[[f64; 3]],
        position: Vec3,
        enemy: Vec3,
        skill: f64,
        can_see: &dyn Fn([f64; 3], [f64; 3]) -> bool,
        reachable: &dyn Fn(Vec3) -> bool,
        random: &mut Random,
    ) -> Option<Vec3> {
        if self
            .spot
            .is_some_and(|spot| (spot.x - position.x).hypot(spot.z - position.z) < REACHED)
        {
            self.spot = None;
        }
        if tick >= self.search_at {
            self.search_at = tick + SEARCH_TICKS;
            self.spot = (distance(position, enemy) <= MAX_RANGE && random.next() < skill)
                .then(|| find(nodes, position, enemy, can_see, reachable))
                .flatten();
        }
        self.spot
    }
}

/// `findHighGround`, in standing origins: the highest qualifying node.
fn find(
    nodes: &[[f64; 3]],
    position: Vec3,
    enemy: Vec3,
    can_see: &dyn Fn([f64; 3], [f64; 3]) -> bool,
    reachable: &dyn Fn(Vec3) -> bool,
) -> Option<Vec3> {
    let mut best: Option<Vec3> = None;
    for &[x, y, z] in nodes {
        let floor = y - STANDING_LIFT;
        if floor < position.y + RISE || floor < enemy.y + RISE {
            continue;
        }
        if best.is_some_and(|best| y <= best.y) {
            continue;
        }
        if (x - position.x).hypot(z - position.z) > RADIUS {
            continue;
        }
        if distance(Vec3::new(x, floor, z), enemy) > MAX_RANGE {
            continue;
        }
        if !can_see([x, floor + LOOKOUT_HEIGHT, z], [enemy.x, enemy.y, enemy.z]) {
            continue;
        }
        let node = Vec3::new(x, y, z);
        // Not in TypeScript: the graph planner must find a walk there.
        if !reachable(node) {
            continue;
        }
        best = Some(node);
    }
    best
}

fn distance(a: Vec3, b: Vec3) -> f64 {
    (a.x - b.x).hypot(a.z - b.z).hypot(a.y - b.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_highest_ledge_in_reach_that_sees_the_enemy() {
        // A low step, a ledge, a higher ledge out of sight, one out of
        // reach, and one too far.
        let nodes = [
            [50.0, 74.0, 0.0],
            [100.0, 184.0, 0.0],
            [120.0, 300.0, 0.0],
            [-150.0, 250.0, 0.0],
            [400.0, 400.0, 0.0],
        ];
        let me = Vec3::new(0.0, 24.0, 0.0);
        let enemy = Vec3::new(0.0, 24.0, 600.0);
        let sees = |from: [f64; 3], _: [f64; 3]| from[1] < 290.0;
        let walks = |node: Vec3| node.x > 0.0;
        assert_eq!(
            find(&nodes, me, enemy, &sees, &walks),
            Some(Vec3::new(100.0, 184.0, 0.0))
        );
        // Rolled every two seconds, kept until reached.
        let mut high = HighGround::default();
        let mut random = Random::new(1);
        let at = high.spot(0, &nodes, me, enemy, 1.0, &sees, &walks, &mut random);
        assert_eq!(at, Some(Vec3::new(100.0, 184.0, 0.0)));
        let there = Vec3::new(90.0, 184.0, 0.0);
        assert_eq!(
            high.spot(10, &nodes, there, enemy, 1.0, &sees, &walks, &mut random),
            None
        );
        // Without skill, or too far from the enemy, no search succeeds.
        let mut unskilled = HighGround::default();
        assert_eq!(
            unskilled.spot(0, &nodes, me, enemy, 0.0, &sees, &walks, &mut random),
            None
        );
        let far = Vec3::new(0.0, 24.0, 2000.0);
        assert_eq!(
            HighGround::default().spot(0, &nodes, me, far, 1.0, &sees, &walks, &mut random),
            None
        );
    }
}

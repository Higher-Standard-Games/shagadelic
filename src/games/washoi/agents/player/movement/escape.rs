//! The way out of a fight going badly, from TypeScript `states/BotRoam.ts`
//! `escapePoint`: a graph node 192 to 900 units off, not toward the enemy,
//! out of its sight, nearest by travel. Held 3 seconds or until reached.

use crate::games::washoi::{Sight, Vec3, agents::player::strategy::considerations};

const REACHED: f64 = 64.0;
const MIN_DISTANCE: f64 = 192.0;
const MAX_DISTANCE: f64 = 900.0;
/// Candidates may lead at most this far toward the enemy (the cosine).
const MAX_TOWARD: f64 = 0.2;
/// Sight lines and costs for the nearest few only.
const CANDIDATES: usize = 24;
const HOLD_TICKS: u32 = 375;
const RETRY_TICKS: u32 = 60;
/// The enemy's eye, and the height a spot is seen at.
const EYE_HEIGHT: f64 = 36.0;

#[derive(Default)]
pub(in crate::games::washoi) struct Escape {
    point: Option<(Vec3, u32)>,
    retry_at: u32,
}

impl Escape {
    pub(in crate::games::washoi) fn reset(&mut self) {
        self.point = None;
    }

    /// `escapePoint`: where to run from `threat`, or None.
    pub(in crate::games::washoi) fn point(
        &mut self,
        tick: u32,
        me: Vec3,
        threat: Vec3,
        nodes: &[[f64; 3]],
        sight: &Sight,
    ) -> Option<Vec3> {
        if let Some((point, until)) = self.point
            && tick < until
        {
            if distance(point, me) > REACHED {
                return Some(point);
            }
            self.point = None;
            return None;
        }
        if tick < self.retry_at {
            return None;
        }
        let (dx, dz) = (threat.x - me.x, threat.z - me.z);
        let length = dx.hypot(dz).max(1e-9);
        let toward = (dx / length, dz / length);
        let mut candidates: Vec<(Vec3, f64)> = nodes
            .iter()
            .map(|&[x, y, z]| Vec3::new(x, y, z))
            .filter_map(|at| {
                let d = distance(at, me);
                let away = ((at.x - me.x) * toward.0 + (at.z - me.z) * toward.1) / d.max(1.0);
                ((MIN_DISTANCE..=MAX_DISTANCE).contains(&d) && away <= MAX_TOWARD)
                    .then_some((at, d))
            })
            .collect();
        candidates.sort_by(|a, b| a.1.total_cmp(&b.1));
        let eye = [threat.x, threat.y + EYE_HEIGHT, threat.z];
        let best = candidates
            .into_iter()
            .take(CANDIDATES)
            .filter(|(at, _)| !sight.clear(eye, [at.x, at.y + EYE_HEIGHT, at.z]))
            .map(|(at, _)| (at, considerations::estimate_route_cost(me, at)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(at, _)| at);
        match best {
            Some(at) => self.point = Some((at, tick + HOLD_TICKS)),
            None => {
                self.point = None;
                self.retry_at = tick + RETRY_TICKS;
            }
        }
        best
    }
}

fn distance(a: Vec3, b: Vec3) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::perception::sight::tests::{cube, encode};

    #[test]
    fn runs_behind_cover_away_from_the_enemy() {
        // A wall at x = 300 between the bot's side and the enemy's.
        let sight = Sight::parse(&encode(
            "test",
            &[cube([280.0, -100.0, -400.0], [320.0, 300.0, 400.0])],
            &[],
        ))
        .unwrap();
        let nodes = [
            // Sideways, both hidden; the nearer wins.
            [0.0, 0.0, 400.0],
            [0.0, 0.0, -300.0],
            // Toward the enemy.
            [500.0, 0.0, 0.0],
        ];
        let mut escape = Escape::default();
        let me = Vec3::new(0.0, 0.0, 0.0);
        let enemy = Vec3::new(600.0, 0.0, 0.0);
        assert_eq!(
            escape.point(0, me, enemy, &nodes, &sight),
            Some(Vec3::new(0.0, 0.0, -300.0))
        );
        // From the other side, the wall hides the node behind it.
        let mut escape = Escape::default();
        let enemy = Vec3::new(0.0, 0.0, 0.0);
        let me = Vec3::new(200.0, 0.0, 0.0);
        let nodes = [[500.0, 0.0, 0.0], [-100.0, 0.0, 0.0]];
        assert_eq!(
            escape.point(0, me, enemy, &nodes, &sight),
            Some(Vec3::new(500.0, 0.0, 0.0))
        );
        assert_eq!(
            escape.point(100, me, enemy, &nodes, &sight),
            Some(Vec3::new(500.0, 0.0, 0.0))
        );
        // Nowhere hidden: none, and no retry for 60 ticks.
        let mut open = Escape::default();
        let bare = Sight::parse(&encode("test", &[], &[])).unwrap();
        assert_eq!(open.point(0, me, enemy, &nodes, &bare), None);
    }
}

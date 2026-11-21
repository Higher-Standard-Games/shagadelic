//! Holding ground in a mode that keeps a territory, from TypeScript
//! `bot/BotTerritory.ts`: walk to a high floor node, preferring one that
//! overlooks the enemy's last known spot, and move on to another after a
//! while, never back to one just held.

use crate::games::washoi::{Sight, Vec3, agents::player::strategy::considerations};

/// A point held this long before choosing another, in server ticks.
const HOLD_TICKS: u32 = 1000;
/// A node held is not chosen again for this long.
const VISITED_TICKS: u32 = 625;
/// An enemy sighting this old still steers the choice.
const SIGHTING_TICKS: u32 = 625;
const ARRIVED: f64 = 96.0;
const MIN_DISTANCE: f64 = 160.0;
const MAX_DROP: f64 = 48.0;
/// With a center, points stay this close to it.
const CENTER_RADIUS: f64 = 384.0;
const MAX_COST: f64 = 5000.0;
const HEIGHT_SCORE: f64 = 2.5;
const OVERLOOK_SCORE: f64 = 160.0;
const COST_SCORE: f64 = 0.08;
/// Candidates tried for a walk, best first.
const TRIED: usize = 8;
const EYE_HEIGHT: f64 = 32.0;

#[derive(Default)]
pub(in crate::games::washoi) struct Territory {
    target: Option<(Vec3, u32)>,
    visited: Vec<(usize, u32)>,
}

impl Territory {
    pub(in crate::games::washoi) fn reset(&mut self) {
        self.target = None;
    }

    /// `choose`: the point to hold, or None. `walkable` says whether a walk
    /// leads from the bot to a point.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::games::washoi) fn choose(
        &mut self,
        tick: u32,
        me: Vec3,
        nodes: &[[f64; 3]],
        enemy: Option<(Vec3, u32)>,
        sight: &Sight,
        center: Option<Vec3>,
        walkable: impl Fn(Vec3) -> bool,
    ) -> Option<Vec3> {
        let near_center =
            |at: Vec3| center.is_none_or(|center| distance(center, at) <= CENTER_RADIUS);
        if let Some((at, until)) = self.target
            && tick < until
            && distance(me, at) > ARRIVED
            && at.y >= me.y - MAX_DROP
            && near_center(at)
        {
            return Some(at);
        }
        self.target = None;
        self.visited.retain(|(_, until)| tick < *until);
        let enemy = enemy
            .filter(|(_, seen)| tick.saturating_sub(*seen) < SIGHTING_TICKS)
            .map(|(at, _)| at);
        let mut candidates: Vec<(usize, Vec3, f64)> = nodes
            .iter()
            .enumerate()
            .map(|(node, &[x, y, z])| (node, Vec3::new(x, y, z)))
            .filter(|(node, at)| {
                !self.visited.iter().any(|(visited, _)| visited == node)
                    && distance(me, *at) >= MIN_DISTANCE
                    && at.y >= me.y - MAX_DROP
                    && near_center(*at)
            })
            .filter_map(|(node, at)| {
                let cost = considerations::estimate_route_cost(me, at);
                (cost <= MAX_COST).then(|| {
                    let overlooks = enemy.is_some_and(|enemy| {
                        sight.clear(
                            [at.x, at.y + EYE_HEIGHT, at.z],
                            [enemy.x, enemy.y + EYE_HEIGHT, enemy.z],
                        )
                    });
                    let score = (at.y - me.y) * HEIGHT_SCORE
                        + if overlooks { OVERLOOK_SCORE } else { 0.0 }
                        - cost * COST_SCORE;
                    (node, at, score)
                })
            })
            .collect();
        candidates.sort_by(|a, b| b.2.total_cmp(&a.2));
        let (node, at, _) = candidates
            .into_iter()
            .take(TRIED)
            .find(|(_, at, _)| walkable(*at))?;
        self.visited.push((node, tick + VISITED_TICKS));
        self.target = Some((at, tick + HOLD_TICKS));
        Some(at)
    }
}

fn distance(a: Vec3, b: Vec3) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::perception::sight::tests::encode;

    #[test]
    fn holds_high_ground_for_a_while_then_moves_on() {
        let sight = Sight::parse(&encode("test", &[], &[])).unwrap();
        let nodes = [
            [0.0, 0.0, 0.0],
            [400.0, 0.0, 0.0],
            [0.0, 128.0, 400.0],
            [400.0, 128.0, 400.0],
            [0.0, -200.0, 600.0],
        ];
        let mut territory = Territory::default();
        let me = Vec3::new(0.0, 0.0, 0.0);
        let open = |_| true;
        // The nearer ledge, not the floor beside the bot or the pit.
        let ledge = Vec3::new(0.0, 128.0, 400.0);
        assert_eq!(
            territory.choose(0, me, &nodes, None, &sight, None, open),
            Some(ledge)
        );
        assert_eq!(
            territory.choose(500, me, &nodes, None, &sight, None, open),
            Some(ledge)
        );
        // Arrived: move on along the high ground, not back down.
        let next = territory.choose(600, ledge, &nodes, None, &sight, None, open);
        assert_eq!(next, Some(Vec3::new(400.0, 128.0, 400.0)));
        // Nowhere walkable: nothing to hold.
        territory.reset();
        assert_eq!(
            territory.choose(2000, me, &nodes, None, &sight, None, |_| false),
            None
        );
    }
}

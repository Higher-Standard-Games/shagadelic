//! Chasing an enemy that went out of sight, after TypeScript
//! `states/BotRoam.ts` `chase`: walk toward where it should be by now, watch
//! that spot, and fire into it when close and the memory is fresh. The walk
//! comes from the graph planner's objective walks.

use std::time::Duration;

use crate::games::washoi::SimTime;
use shagger_shared::net::game_state::{Vec3, Velocity};

use crate::games::washoi::{
    Sight, WashoiNavigation, agents::player::navigation::ObjectiveWalk,
    hosting::net::protocol::PlayerInput,
};

/// Close enough to the predicted spot to stop walking.
const ARRIVAL_RADIUS: f64 = 64.0;
/// A prediction this far from the walk's target starts a new walk.
const REPLAN_DISTANCE: f64 = 96.0;
const RETRY: Duration = Duration::from_millis(500);

#[derive(Default)]
pub(in crate::games::washoi) struct Chase {
    walk: Option<(Vec3, ObjectiveWalk)>,
    retry_at: Option<SimTime>,
}

impl Chase {
    pub(in crate::games::washoi) fn reset(&mut self) {
        *self = Self::default();
    }

    /// For traces: where this decision's walk line came from, and the lines
    /// built since the last record.
    pub(in crate::games::washoi) fn line_trace(&self) -> (Option<&'static str>, u32) {
        self.walk
            .as_ref()
            .map_or((None, 0), |(_, walk)| walk.line_trace())
    }

    pub(in crate::games::washoi) fn clear_line_trace(&mut self) {
        if let Some((_, walk)) = &mut self.walk {
            walk.clear_line_trace();
        }
    }

    /// Movement toward `target`, or none when no walk leads there.
    pub(in crate::games::washoi) fn steer(
        &mut self,
        navigation: &WashoiNavigation,
        sight: &Sight,
        position: Vec3,
        velocity: Option<Velocity>,
        target: Vec3,
        now: SimTime,
    ) -> Option<PlayerInput> {
        if self
            .walk
            .as_ref()
            .is_some_and(|(previous, _)| distance(*previous, target) > REPLAN_DISTANCE)
        {
            self.walk = None;
        }
        if self.walk.is_none() && self.retry_at.is_none_or(|at| now >= at) {
            self.retry_at = Some(now + RETRY);
            self.walk = navigation
                .objective_walk(position, target, ARRIVAL_RADIUS, None, Some(sight))
                .map(|walk| (target, walk));
        }
        let (_, walk) = self.walk.as_mut()?;
        let input = walk.steer(position, velocity, Some(sight));
        if input.direction == [0.0; 3] {
            // Arrived, or the walk ran out: plan again after the retry delay.
            self.walk = None;
        }
        Some(input)
    }
}

fn distance(a: Vec3, b: Vec3) -> f64 {
    (a.x - b.x).hypot(a.z - b.z).hypot(a.y - b.y)
}

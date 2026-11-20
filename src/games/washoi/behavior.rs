use std::time::Duration;

use shagger_shared::net::game_state::GameState;
use tokio::time::Instant;

use super::protocol::{PlayerInput, SnapshotEvent};

pub(crate) const INPUT_PERIOD: Duration = Duration::from_millis(8);
const MAX_BURST: u64 = 4;
const MAX_DEBT: u64 = 25;
const MAX_OBSERVATION_AGE: Duration = Duration::from_millis(500);

/// The first Washoi behavior walks forward while its own entity is alive.
pub struct BotPlayer {
    entity_id: String,
    dead: bool,
    last_visible: Option<Instant>,
    started: Option<Instant>,
    sent: u64,
}

impl BotPlayer {
    pub fn new(entity_id: String) -> Self {
        Self {
            entity_id,
            dead: false,
            last_visible: None,
            started: None,
            sent: 0,
        }
    }

    pub(crate) fn observe(&mut self, world: &GameState<SnapshotEvent>, now: Instant) {
        for event in &world.events {
            match event {
                SnapshotEvent::CombatState {
                    entity_id, dead, ..
                } if *entity_id == self.entity_id => {
                    self.dead = *dead;
                }
                SnapshotEvent::Death { entity_id, .. } if *entity_id == self.entity_id => {
                    self.dead = true;
                }
                _ => {}
            }
        }
        self.last_visible =
            (!self.dead && world.entities.get(&self.entity_id).is_some()).then_some(now);
        if self.last_visible.is_none() {
            self.started = None;
        }
    }

    pub(crate) fn inputs(&mut self, now: Instant) -> impl Iterator<Item = PlayerInput> {
        let count = if self
            .last_visible
            .is_some_and(|at| now - at <= MAX_OBSERVATION_AGE)
        {
            self.commands_due(now)
        } else {
            self.started = None;
            0
        };
        (0..count).map(|_| PlayerInput {
            direction: [0.0, 0.0, 1.0],
            ..PlayerInput::default()
        })
    }

    fn commands_due(&mut self, now: Instant) -> u64 {
        let started = *self.started.get_or_insert_with(|| {
            self.sent = 0;
            now
        });
        let owed = ((now - started).as_millis() / INPUT_PERIOD.as_millis()) as u64 + 1;
        let backlog = owed.saturating_sub(self.sent);
        if backlog > MAX_DEBT {
            self.sent = owed;
            return 1;
        }
        let count = backlog.min(MAX_BURST);
        self.sent += count;
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_debt_is_bounded_and_stale_observations_stop_movement() {
        let start = Instant::now();
        let mut bot = BotPlayer::new("bot".into());
        assert_eq!(bot.inputs(start).count(), 0);
        bot.last_visible = Some(start);
        assert_eq!(bot.inputs(start).count(), 1);
        assert_eq!(bot.inputs(start + Duration::from_millis(4)).count(), 0);
        assert_eq!(bot.inputs(start + Duration::from_millis(40)).count(), 4);
        assert_eq!(bot.inputs(start + Duration::from_millis(48)).count(), 2);
        assert_eq!(bot.inputs(start + Duration::from_millis(300)).count(), 1);
        assert_eq!(bot.inputs(start + Duration::from_millis(501)).count(), 0);
    }
}

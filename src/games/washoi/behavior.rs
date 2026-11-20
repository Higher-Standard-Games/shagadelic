use std::{sync::Arc, time::Duration};

use shagger_shared::net::game_state::GameState;
use tokio::time::Instant;

use super::WashoiNavigation;
use super::protocol::{PlayerInput, SnapshotEvent};

pub(crate) const INPUT_PERIOD: Duration = Duration::from_millis(8);
const MAX_BURST: u64 = 4;
const MAX_DEBT: u64 = 25;
const MAX_OBSERVATION_AGE: Duration = Duration::from_millis(500);
const RESPAWN_DELAY: Duration = Duration::from_millis(2500);

/// The pilot walks forward or follows an authored route while alive.
pub struct BotPlayer {
    entity_id: String,
    respawn_at: Option<Instant>,
    own_entity_present: bool,
    last_snapshot: Option<Instant>,
    started: Option<Instant>,
    sent: u64,
    navigation: Option<Arc<WashoiNavigation>>,
    route: Option<(usize, usize)>,
    planned_input: PlayerInput,
}

impl BotPlayer {
    pub fn new(entity_id: String) -> Self {
        Self {
            entity_id,
            respawn_at: None,
            own_entity_present: false,
            last_snapshot: None,
            started: None,
            sent: 0,
            navigation: None,
            route: None,
            planned_input: PlayerInput {
                direction: [0.0, 0.0, 1.0],
                ..PlayerInput::default()
            },
        }
    }

    pub fn with_navigation(mut self, navigation: Arc<WashoiNavigation>) -> Self {
        self.navigation = Some(navigation);
        self.planned_input = PlayerInput::default();
        self
    }

    pub(crate) fn observe(&mut self, world: &GameState<SnapshotEvent>, now: Instant) {
        for event in &world.events {
            match event {
                SnapshotEvent::CombatState {
                    entity_id, dead, ..
                } if *entity_id == self.entity_id => {
                    if *dead {
                        self.respawn_at.get_or_insert(now + RESPAWN_DELAY);
                    } else {
                        self.respawn_at = None;
                    }
                }
                SnapshotEvent::Death { entity_id, .. } if *entity_id == self.entity_id => {
                    self.respawn_at.get_or_insert(now + RESPAWN_DELAY);
                }
                _ => {}
            }
        }
        self.last_snapshot = Some(now);
        let own = world.entities.get(&self.entity_id);
        self.own_entity_present = own.is_some();
        if self.respawn_at.is_some() || !self.own_entity_present {
            self.started = None;
            self.route = None;
        }
        if let Some(navigation) = &self.navigation {
            self.planned_input = PlayerInput::default();
            if self.respawn_at.is_none()
                && let Some(position) = own.and_then(|entity| entity.position)
                && [position.x, position.y, position.z]
                    .into_iter()
                    .all(f64::is_finite)
            {
                if self.route.is_none() {
                    self.route = navigation.route_at(position).map(|route| (route, 0));
                }
                if let Some((route, next)) = &mut self.route {
                    self.planned_input = navigation.steer(*route, next, position);
                }
            }
        }
    }

    pub(crate) fn request_respawn(&mut self, now: Instant) -> bool {
        if self.has_fresh_snapshot(now) && self.respawn_at.is_some_and(|at| now >= at) {
            self.respawn_at = Some(now + RESPAWN_DELAY);
            return true;
        }
        false
    }

    fn has_fresh_snapshot(&self, now: Instant) -> bool {
        self.last_snapshot
            .is_some_and(|at| now - at <= MAX_OBSERVATION_AGE)
    }

    pub(crate) fn inputs(&mut self, now: Instant) -> impl Iterator<Item = PlayerInput> {
        let count =
            if self.respawn_at.is_none() && self.own_entity_present && self.has_fresh_snapshot(now)
            {
                self.commands_due(now)
            } else {
                self.started = None;
                0
            };
        std::iter::repeat_n(self.planned_input, count as usize)
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
        bot.last_snapshot = Some(start);
        bot.own_entity_present = true;
        assert_eq!(bot.inputs(start).count(), 1);
        assert_eq!(bot.inputs(start + Duration::from_millis(4)).count(), 0);
        assert_eq!(bot.inputs(start + Duration::from_millis(40)).count(), 4);
        assert_eq!(bot.inputs(start + Duration::from_millis(48)).count(), 2);
        assert_eq!(bot.inputs(start + Duration::from_millis(300)).count(), 1);
        assert_eq!(bot.inputs(start + Duration::from_millis(501)).count(), 0);
    }

    #[test]
    fn dead_bots_wait_for_a_fresh_snapshot_before_requesting_respawn() {
        let start = Instant::now();
        let mut bot = BotPlayer::new("bot".into());
        let mut world = GameState::default();
        world.entities.get_or_insert_with("bot", Default::default);
        world.events.push(SnapshotEvent::CombatState {
            entity_id: "bot".into(),
            health: 0,
            armor: 0,
            dead: true,
        });
        bot.observe(&world, start);
        assert_eq!(bot.inputs(start).count(), 0);
        let due = start + RESPAWN_DELAY;
        assert!(!bot.request_respawn(due));
        bot.observe(&world, due);
        assert!(bot.request_respawn(due));
        assert!(!bot.request_respawn(due));
        world.events[0] = SnapshotEvent::CombatState {
            entity_id: "bot".into(),
            health: 100,
            armor: 0,
            dead: false,
        };
        bot.observe(&world, due);
        assert_eq!(bot.inputs(due).count(), 1);
    }
}

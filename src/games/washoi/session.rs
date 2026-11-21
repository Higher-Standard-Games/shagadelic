use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use shagger_shared::net::game_state::GameState;
use tokio::{sync::mpsc, task::JoinHandle, time::Instant};

use crate::runtime::{Deadlines, EntityKey, GameSession, SessionEvent, Update};

use super::{
    BotPlayer, Difficulty, LearnedRoutes, Sight, WashoiNavigation,
    behavior::INPUT_PERIOD,
    protocol::{AdmissionError, InputBatch, SnapshotEvent, WashoiEvents, connect},
};

const MAX_BOTS: usize = 64;

pub struct SpawnBot {
    pub name: String,
    pub ticket: Option<String>,
    pub team: Option<String>,
    pub difficulty: Difficulty,
}

pub enum BotReport {
    Admitted {
        key: EntityKey,
        player_id: String,
    },
    Ended {
        key: EntityKey,
        reason: &'static str,
    },
}

/// Per-map data shared by every bot in the session.
#[derive(Clone, Default)]
struct MapAssets {
    navigation: Option<Arc<WashoiNavigation>>,
    learned: Option<Arc<LearnedRoutes>>,
    sight: Option<Arc<Sight>>,
}

impl MapAssets {
    fn maps(&self) -> impl Iterator<Item = &str> {
        [
            self.navigation.as_ref().map(|navigation| navigation.map()),
            self.learned.as_ref().map(|learned| learned.map()),
            self.sight.as_ref().map(|sight| sight.map()),
        ]
        .into_iter()
        .flatten()
    }
}

// Retain the entity positions and server tick that gave each transient event
// its meaning (for example a sound or a jump-pad contact).
const MAX_PENDING_FRAMES: usize = 32;

enum SocketEvent {
    Admitted {
        key: EntityKey,
        player_id: String,
        entity_id: String,
    },
    Snapshot(EntityKey, GameState<SnapshotEvent>),
    Ended(EntityKey, &'static str),
}

struct BotTask {
    task: JoinHandle<()>,
    actions: mpsc::Sender<InputBatch>,
    difficulty: Difficulty,
    player: Option<BotPlayer>,
    pending: VecDeque<(Duration, GameState<SnapshotEvent>)>,
}

/// One lobby. Each connection handles I/O while the session owns its bot brain.
pub struct WashoiSession {
    endpoint: String,
    assets: MapAssets,
    bots: BTreeMap<EntityKey, BotTask>,
    send_event: mpsc::Sender<SocketEvent>,
    events: mpsc::Receiver<SocketEvent>,
    // Bridge the current brain's Instant API to the caller's simulation clock.
    epoch: Instant,
    now: Duration,
    reports: VecDeque<BotReport>,
}

impl WashoiSession {
    pub fn new(endpoint: String) -> Self {
        let (send_event, events) = mpsc::channel(2 * MAX_BOTS);
        Self {
            endpoint,
            assets: MapAssets::default(),
            bots: BTreeMap::new(),
            epoch: Instant::now(),
            now: Duration::ZERO,
            send_event,
            events,
            reports: VecDeque::new(),
        }
    }

    pub fn with_navigation(mut self, navigation: WashoiNavigation) -> Self {
        self.assets.navigation = Some(Arc::new(navigation));
        self
    }

    pub fn with_learned_routes(mut self, learned: LearnedRoutes) -> Self {
        self.assets.learned = Some(Arc::new(learned));
        self
    }

    pub fn with_sight(mut self, sight: Sight) -> Self {
        self.assets.sight = Some(Arc::new(sight));
        self
    }

    pub fn take_report(&mut self) -> Option<BotReport> {
        self.reports.pop_front()
    }
}

impl GameSession for WashoiSession {
    type Spawn = SpawnBot;
    type SpawnError = &'static str;

    fn begin_spawn(&mut self, key: &EntityKey, bot: SpawnBot) -> Result<(), Self::SpawnError> {
        if self.bots.len() >= MAX_BOTS {
            return Err("bot_limit");
        }
        if bot.name.trim().is_empty() {
            return Err("empty_name");
        }
        let executor = tokio::runtime::Handle::try_current().map_err(|_| "executor_unavailable")?;
        let (actions, outgoing) = mpsc::channel(1);
        let difficulty = bot.difficulty;
        let task = executor.spawn(run_bot(
            self.endpoint.clone(),
            key.clone(),
            bot,
            outgoing,
            self.send_event.clone(),
            self.assets.clone(),
        ));
        self.bots.insert(
            key.clone(),
            BotTask {
                task,
                actions,
                difficulty,
                player: None,
                pending: VecDeque::new(),
            },
        );
        Ok(())
    }

    fn remove(&mut self, key: &EntityKey) {
        if let Some(bot) = self.bots.remove(key) {
            bot.task.abort();
        }
    }

    fn poll_event(&mut self) -> Option<SessionEvent> {
        if self.reports.len() >= 2 * MAX_BOTS {
            return None;
        }
        match self.events.try_recv().ok()? {
            SocketEvent::Admitted {
                key,
                entity_id,
                player_id,
            } => {
                if let Some(bot) = self.bots.get_mut(&key) {
                    let mut player = BotPlayer::new(entity_id).with_difficulty(bot.difficulty);
                    if let Some(nav) = &self.assets.navigation {
                        player = player.with_navigation(nav.clone());
                    }
                    if let Some(learned) = &self.assets.learned {
                        player = player.with_learned_routes(learned.clone());
                    }
                    if let Some(sight) = &self.assets.sight {
                        player = player.with_sight(sight.clone());
                    }
                    bot.player = Some(player);
                    self.reports.push_back(BotReport::Admitted {
                        key: key.clone(),
                        player_id,
                    });
                }
                Some(SessionEvent::Admitted(
                    key,
                    Deadlines {
                        input: Some(Duration::ZERO),
                        decision: None,
                    },
                ))
            }
            SocketEvent::Snapshot(key, state) => {
                if let Some(bot) = self.bots.get_mut(&key) {
                    if bot.pending.len() >= MAX_PENDING_FRAMES {
                        self.remove(&key);
                        self.reports.push_back(BotReport::Ended {
                            key: key.clone(),
                            reason: "observation_backlog",
                        });
                        return Some(SessionEvent::Ended(key));
                    }
                    bot.pending.push_back((self.now, state));
                }
                Some(SessionEvent::WakeDecision(key))
            }
            SocketEvent::Ended(key, reason) => {
                if self.bots.remove(&key).is_some() {
                    self.reports.push_back(BotReport::Ended {
                        key: key.clone(),
                        reason,
                    });
                }
                Some(SessionEvent::Ended(key))
            }
        }
    }

    fn poll_event_at(&mut self, now: Duration) -> Option<SessionEvent> {
        self.now = now;
        self.poll_event()
    }

    fn update(&mut self, key: &EntityKey, kind: Update, now: Duration) -> Option<Duration> {
        let bot = self.bots.get_mut(key)?;
        let player = bot.player.as_mut()?;
        match kind {
            Update::Decision => {
                let (received, state) = bot.pending.pop_front()?;
                player.observe(&state, self.epoch + received);
                // One frame per permit including its ordered events. A later
                // grant resumes the queue, even without another network wake.
                (!bot.pending.is_empty()).then_some(now)
            }
            Update::Input => {
                // Reserve before advancing an exact maneuver or respawn timer.
                if let Ok(slot) = bot.actions.try_reserve() {
                    slot.send(InputBatch::from_player(player, self.epoch + now));
                }
                now.checked_add(INPUT_PERIOD)
            }
        }
    }

    fn begin_shutdown(&mut self) {
        self.events.close();
    }
}

impl Drop for WashoiSession {
    fn drop(&mut self) {
        for bot in self.bots.values() {
            bot.task.abort();
        }
    }
}

async fn run_bot(
    endpoint: String,
    key: EntityKey,
    bot: SpawnBot,
    actions: mpsc::Receiver<InputBatch>,
    events: mpsc::Sender<SocketEvent>,
    assets: MapAssets,
) {
    let connection = connect(
        &endpoint,
        bot.name,
        bot.ticket,
        bot.team.as_deref(),
        Duration::from_secs(10),
        WashoiEvents,
        |_| {},
    )
    .await;
    let reason = match connection {
        Ok(mut connection) => {
            let server_map = connection
                .welcome
                .as_ref()
                .map(|welcome| welcome.map.as_str());
            if assets.maps().any(|map| Some(map) != server_map) {
                let _ = events
                    .send(SocketEvent::Ended(key, "navigation_map_mismatch"))
                    .await;
                return;
            }
            let admitted = SocketEvent::Admitted {
                key: key.clone(),
                player_id: connection.identity.player_id.clone(),
                entity_id: connection.identity.entity_id.clone(),
            };
            if events.send(admitted).await.is_err() {
                return;
            }
            match connection
                .exchange(actions, |state| {
                    let event = SocketEvent::Snapshot(key.clone(), state);
                    let events = &events;
                    async move { events.send(event).await.is_ok() }
                })
                .await
            {
                Ok(()) | Err(AdmissionError::Disconnected) => "disconnected",
                Err(AdmissionError::Protocol(_)) => "protocol_error",
                Err(_) => "connection_error",
            }
        }
        Err(AdmissionError::Rejected) => "rejected",
        Err(AdmissionError::TimedOut) => "admission_timeout",
        Err(AdmissionError::Protocol(_)) => "protocol_error",
        Err(_) => "admission_error",
    };
    let _ = events.send(SocketEvent::Ended(key, reason)).await;
}

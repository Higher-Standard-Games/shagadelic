use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use shagger_shared::net::game_state::GameState;
use tokio::{sync::mpsc, task::JoinHandle};

use crate::runtime::{Deadlines, EntityKey, GameSession, SessionEvent, Update};

use crate::games::washoi::{
    Baseline, BotPlayer, Difficulty, LearnedRoutes, Locations, MapCollision, PeopleProfile, Sight,
    SimTime, WashoiNavigation,
    agents::NetworkedPlayer,
    hosting::net::protocol::{
        self, AdmissionError, InputBatch, SnapshotEvent, WashoiEvents, connect,
    },
    observation::INPUT_PERIOD,
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
    collision: Option<Arc<MapCollision>>,
    locations: Option<Arc<Locations>>,
    people: Option<Arc<PeopleProfile>>,
    baseline: Option<Arc<Baseline>>,
}

impl MapAssets {
    fn maps(&self) -> impl Iterator<Item = &str> {
        [
            self.navigation.as_ref().map(|navigation| navigation.map()),
            self.learned.as_ref().map(|learned| learned.map()),
            self.sight.as_ref().map(|sight| sight.map()),
            self.collision.as_ref().map(|collision| collision.map()),
            self.locations.as_ref().map(|locations| locations.map()),
            self.baseline.as_ref().map(|baseline| baseline.map()),
        ]
        .into_iter()
        .flatten()
    }
}

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
    name: String,
    /// Built as a `BotPlayer`, driven only as a `NetworkedPlayer`.
    player: Option<Box<dyn NetworkedPlayer>>,
}

/// One lobby. Each connection handles I/O while the session owns its bot brain.
pub struct WashoiSession {
    endpoint: String,
    assets: MapAssets,
    bots: BTreeMap<EntityKey, BotTask>,
    send_event: mpsc::Sender<SocketEvent>,
    events: mpsc::Receiver<SocketEvent>,
    // The caller's simulation clock, also the brain's time.
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

    /// The map's collision, for the bots' movement rollouts. Each bot gets
    /// a collision world of its own over it.
    pub fn with_collision(mut self, collision: MapCollision) -> Self {
        self.assets.collision = Some(Arc::new(collision));
        self
    }

    pub fn with_locations(mut self, locations: Locations) -> Self {
        self.assets.locations = Some(Arc::new(locations));
        self
    }

    pub fn with_people(mut self, people: PeopleProfile) -> Self {
        self.assets.people = Some(Arc::new(people));
        self
    }

    pub fn with_baseline(mut self, baseline: Baseline) -> Self {
        self.assets.baseline = Some(Arc::new(baseline));
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
        let name = bot.name.clone();
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
                name,
                player: None,
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
                    let mut player = BotPlayer::new(entity_id)
                        .with_difficulty(bot.difficulty)
                        .with_name(&bot.name);
                    if let Some(nav) = &self.assets.navigation {
                        player = player.with_navigation(nav.clone());
                    }
                    if let Some(learned) = &self.assets.learned {
                        player = player.with_learned_routes(learned.clone());
                    }
                    if let Some(sight) = &self.assets.sight {
                        player = player.with_sight(sight.clone());
                    }
                    if let Some(collision) = &self.assets.collision {
                        player = player.with_collision(collision.world());
                    }
                    if let Some(dir) = std::env::var_os("SHAGADELIC_TRACE_DIR") {
                        player = player.with_trace_dir(std::path::Path::new(&dir));
                    }
                    if let Some(locations) = &self.assets.locations {
                        player = player.with_locations(locations.clone());
                    }
                    if let Some(people) = &self.assets.people {
                        player = player.with_people(people.clone());
                    }
                    if let Some(baseline) = &self.assets.baseline {
                        player = player.with_baseline(baseline.clone());
                    }
                    bot.player = Some(Box::new(player));
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
                // Observing only stores the latest view and queues its events
                // in order, so it runs on arrival. Planning waits for a
                // budgeted decision.
                if let Some(player) = self.bots.get_mut(&key).and_then(|bot| bot.player.as_mut()) {
                    player.observe_at(protocol::observe(&state), SimTime::from_elapsed(self.now));
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
                // One decision covers every observation since the last one.
                // The next snapshot wakes the following decision.
                player.decide(SimTime::from_elapsed(now));
                None
            }
            Update::Input => {
                // Reserve before advancing an exact maneuver or respawn timer.
                if let Ok(slot) = bot.actions.try_reserve() {
                    slot.send(InputBatch::from_player(
                        player.as_mut(),
                        SimTime::from_elapsed(now),
                    ));
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

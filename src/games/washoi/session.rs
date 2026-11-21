use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use tokio::{sync::mpsc, task::JoinHandle};

use crate::runtime::{Deadlines, EntityKey, GameSession, SessionEvent, Update};

use super::{
    BotPlayer, Difficulty, LearnedRoutes, Sight, WashoiNavigation,
    behavior::INPUT_PERIOD,
    protocol::{AdmissionError, WashoiEvents, connect},
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

struct BotTask {
    task: JoinHandle<()>,
    pulse: mpsc::Sender<()>,
}

/// One Washoi game session. Socket tasks own I/O and receive input pulses from
/// the caller-driven scheduler. Each task has room for one pending pulse.
pub struct WashoiSession {
    endpoint: String,
    assets: MapAssets,
    bots: BTreeMap<EntityKey, BotTask>,
    send_event: mpsc::Sender<BotReport>,
    events: mpsc::Receiver<BotReport>,
    reports: VecDeque<BotReport>,
}

impl WashoiSession {
    pub fn new(endpoint: String) -> Self {
        let (send_event, events) = mpsc::channel(2 * MAX_BOTS);
        Self {
            endpoint,
            assets: MapAssets::default(),
            bots: BTreeMap::new(),
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
        let (pulse, pulses) = mpsc::channel(1);
        let task = executor.spawn(run_bot(
            self.endpoint.clone(),
            key.clone(),
            bot,
            pulses,
            self.send_event.clone(),
            self.assets.clone(),
        ));
        self.bots.insert(key.clone(), BotTask { task, pulse });
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
        let report = self.events.try_recv().ok()?;
        let (event, current) = match &report {
            BotReport::Admitted { key, .. } => (
                SessionEvent::Admitted(
                    key.clone(),
                    Deadlines {
                        input: Some(Duration::ZERO),
                        decision: None,
                    },
                ),
                self.bots.contains_key(key),
            ),
            BotReport::Ended { key, .. } => (
                SessionEvent::Ended(key.clone()),
                self.bots.remove(key).is_some(),
            ),
        };
        if current {
            self.reports.push_back(report);
        }
        Some(event)
    }

    fn update(&mut self, key: &EntityKey, kind: Update, now: Duration) -> Option<Duration> {
        if kind == Update::Input {
            let bot = self.bots.get(key)?;
            let _ = bot.pulse.try_send(());
            return now.checked_add(INPUT_PERIOD);
        }
        None
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
    mut pulses: mpsc::Receiver<()>,
    events: mpsc::Sender<BotReport>,
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
                    .send(BotReport::Ended {
                        key,
                        reason: "navigation_map_mismatch",
                    })
                    .await;
                return;
            }
            let report = BotReport::Admitted {
                key: key.clone(),
                player_id: connection.identity.player_id.clone(),
            };
            if events.send(report).await.is_err() {
                return;
            }
            let mut bot = BotPlayer::new(connection.identity.entity_id.clone())
                .with_difficulty(bot.difficulty);
            if let Some(navigation) = assets.navigation {
                bot = bot.with_navigation(navigation);
            }
            if let Some(learned) = assets.learned {
                bot = bot.with_learned_routes(learned);
            }
            if let Some(sight) = assets.sight {
                bot = bot.with_sight(sight);
            }
            let pulses = futures_util::stream::poll_fn(|cx| pulses.poll_recv(cx));
            match connection.play_on(bot, |_| true, pulses).await {
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
    let _ = events.send(BotReport::Ended { key, reason }).await;
}

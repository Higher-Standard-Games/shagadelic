use std::{
    io::{self, BufRead, Write},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use shagadelic::{
    games::washoi::{
        Baseline, BotReport, Difficulty, LearnedRoutes, Locations, PeopleProfile, Sight, SpawnBot,
        WashoiNavigation, WashoiSession,
    },
    runtime::{Budget, EntityKey, EntityStatus, Runtime, SpawnError},
};
use tokio::{
    sync::mpsc,
    time::{Instant, MissedTickBehavior, interval_at},
};

use super::{Command, emit, invalid, read_command};

const PERIOD: Duration = Duration::from_millis(8);
const BUDGET: Budget = Budget {
    events: 128,
    inputs: 64,
    decisions: 64,
};

#[derive(Deserialize)]
pub(super) struct Spawn {
    id: String,
    name: String,
    ticket: Option<String>,
    team: Option<String>,
    difficulty: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event<'a> {
    #[serde(rename = "bot_status")]
    Status {
        id: &'a str,
        generation: u64,
        status: &'static str,
    },
    #[serde(rename = "bot_admitted")]
    Admitted {
        id: &'a str,
        generation: u64,
        player_id: &'a str,
    },
    #[serde(rename = "bot_ended")]
    Ended {
        id: &'a str,
        generation: u64,
        reason: &'static str,
    },
    #[serde(rename = "bot_request_failed")]
    RequestFailed { id: &'a str, reason: &'static str },
}

pub(super) struct MapAssets {
    pub(super) navigation: Option<WashoiNavigation>,
    pub(super) learned: Option<LearnedRoutes>,
    pub(super) sight: Option<Sight>,
    pub(super) locations: Option<Locations>,
    pub(super) people: Option<PeopleProfile>,
    pub(super) baseline: Option<Baseline>,
}

pub(super) fn run(
    mut reader: impl BufRead + Send + 'static,
    writer: &mut impl Write,
    endpoint: String,
    assets: MapAssets,
) -> io::Result<()> {
    let (send, receive) = mpsc::channel(32);
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    std::thread::Builder::new()
        .name("host-control".into())
        .spawn(move || {
            let mut record = Vec::new();
            loop {
                let command = read_command(&mut reader, &mut record).transpose();
                let Some(command) = command else { return };
                let terminal = matches!(command, Ok(Command::Shutdown) | Err(_));
                if send.blocking_send(command).is_err() || terminal {
                    return;
                }
            }
        })?;
    let mut session = WashoiSession::new(endpoint);
    if let Some(navigation) = assets.navigation {
        session = session.with_navigation(navigation);
    }
    if let Some(learned) = assets.learned {
        session = session.with_learned_routes(learned);
    }
    if let Some(sight) = assets.sight {
        session = session.with_sight(sight);
    }
    if let Some(locations) = assets.locations {
        session = session.with_locations(locations);
    }
    if let Some(people) = assets.people {
        session = session.with_people(people);
    }
    if let Some(baseline) = assets.baseline {
        session = session.with_baseline(baseline);
    }
    executor.block_on(drive(receive, writer, session))
}

async fn drive(
    mut receive: mpsc::Receiver<io::Result<Command>>,
    writer: &mut impl Write,
    session: WashoiSession,
) -> io::Result<()> {
    let mut runtime = Runtime::new(session);
    let start = Instant::now();
    let mut ticks = interval_at(start, PERIOD);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            command = receive.recv() => match command.transpose()? {
                Some(Command::Spawn(spawn)) => spawn_bot(&mut runtime, writer, spawn)?,
                Some(Command::Remove { id, generation }) => {
                    let key = EntityKey { id, generation };
                    let event = if runtime.remove(&key) {
                        Event::Ended { id: &key.id, generation, reason: "removed" }
                    } else {
                        Event::RequestFailed { id: &key.id, reason: "unknown_generation" }
                    };
                    emit(writer, &event)?;
                }
                Some(Command::Shutdown) | None => {
                    runtime.shutdown();
                    return Ok(());
                }
                Some(Command::Initialize(_)) => return Err(invalid("lobby already initialized")),
            },
            tick = ticks.tick() => {
                runtime.advance(tick - start, BUDGET).map_err(|_| invalid("session clock regressed"))?;
                while let Some(report) = runtime.session_mut().take_report() {
                    let event = match &report {
                        BotReport::Admitted { key, player_id } => Event::Admitted { id: &key.id, generation: key.generation, player_id },
                        BotReport::Ended { key, reason } => Event::Ended { id: &key.id, generation: key.generation, reason },
                    };
                    emit(writer, &event)?;
                }
            }
        }
    }
}

fn spawn_bot(
    runtime: &mut Runtime<WashoiSession>,
    writer: &mut impl Write,
    spawn: Spawn,
) -> io::Result<()> {
    if spawn.id.trim().is_empty() {
        return emit(
            writer,
            &Event::RequestFailed {
                id: &spawn.id,
                reason: "empty_id",
            },
        );
    }
    let difficulty = match spawn.difficulty {
        None => Difficulty::default(),
        Some(name) => match Difficulty::from_name(&name) {
            Some(difficulty) => difficulty,
            None => {
                return emit(
                    writer,
                    &Event::RequestFailed {
                        id: &spawn.id,
                        reason: "unknown_difficulty",
                    },
                );
            }
        },
    };
    let options = SpawnBot {
        name: spawn.name,
        ticket: spawn.ticket,
        team: spawn.team,
        difficulty,
    };
    let event = match runtime.spawn(spawn.id.clone(), options) {
        Ok(status) => {
            let label = match &status {
                EntityStatus::Pending(_) => "pending",
                EntityStatus::Active(_) => "active",
            };
            Event::Status {
                id: &spawn.id,
                generation: status.key().generation,
                status: label,
            }
        }
        Err(error) => {
            let reason = match error {
                SpawnError::Closing => "closing",
                SpawnError::GenerationExhausted => "generation_exhausted",
                SpawnError::Integration(reason) => reason,
            };
            Event::RequestFailed {
                id: &spawn.id,
                reason,
            }
        }
    };
    emit(writer, &event)
}

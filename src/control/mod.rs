use std::io::{self, BufRead, Read, Write};

use serde::{Deserialize, Serialize};

#[cfg(feature = "washoi")]
mod washoi;

const VERSION: u32 = 1;
const MAX_RECORD_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
struct Initialization {
    version: u32,
    lobby_id: String,
    pilot: Option<Pilot>,
    washoi_endpoint: Option<String>,
    navigation: Option<Navigation>,
    learned_routes_file: Option<std::path::PathBuf>,
    sight_file: Option<std::path::PathBuf>,
}

#[cfg(feature = "washoi")]
type Navigation = shagadelic::games::washoi::WashoiNavigation;
#[cfg(not(feature = "washoi"))]
type Navigation = serde_json::Value;

#[cfg(feature = "washoi")]
#[derive(Deserialize)]
struct Pilot {
    endpoint: String,
    name: String,
    ticket: Option<String>,
    team: Option<String>,
}

#[cfg(not(feature = "washoi"))]
type Pilot = serde_json::Value;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Command {
    Initialize(Box<Initialization>),
    Shutdown,
    #[cfg(feature = "washoi")]
    Spawn(washoi::Spawn),
    #[cfg(feature = "washoi")]
    Remove {
        id: String,
        generation: u64,
    },
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event<'a> {
    Ready {
        version: u32,
        lobby_id: &'a str,
    },
    #[cfg(feature = "washoi")]
    PilotAdmitted {
        player_id: &'a str,
    },
    #[cfg(feature = "washoi")]
    PilotEnded {
        reason: &'static str,
    },
}

pub fn run(mut reader: impl BufRead + Send + 'static, writer: &mut impl Write) -> io::Result<()> {
    let mut record = Vec::new();
    let Some(command) = read_command(&mut reader, &mut record)? else {
        return Ok(());
    };
    let Command::Initialize(init) = command else {
        return Err(invalid("expected initialization"));
    };
    let init = *init;
    if init.version != VERSION {
        return Err(invalid("unsupported control version"));
    }
    if init.lobby_id.trim().is_empty() {
        return Err(invalid("lobby ID is empty"));
    }
    #[cfg(not(feature = "washoi"))]
    if init.pilot.is_some()
        || init.washoi_endpoint.is_some()
        || init.navigation.is_some()
        || init.learned_routes_file.is_some()
        || init.sight_file.is_some()
    {
        return Err(invalid("Washoi support is disabled"));
    }
    #[cfg(feature = "washoi")]
    if (init.navigation.is_some()
        || init.learned_routes_file.is_some()
        || init.sight_file.is_some())
        && init.washoi_endpoint.is_none()
    {
        return Err(invalid("navigation requires a Washoi session"));
    }
    #[cfg(feature = "washoi")]
    if let Some(endpoint) = &init.washoi_endpoint {
        if init.pilot.is_some() {
            return Err(invalid("select either Washoi session or pilot"));
        }
        if !endpoint.starts_with("ws://") || endpoint.len() == 5 {
            return Err(invalid("expected a plain WebSocket game endpoint"));
        }
    }

    #[cfg(feature = "washoi")]
    let learned = init
        .learned_routes_file
        .as_ref()
        .map(shagadelic::games::washoi::LearnedRoutes::load)
        .transpose()?;
    #[cfg(feature = "washoi")]
    let sight = init
        .sight_file
        .as_ref()
        .map(shagadelic::games::washoi::Sight::load)
        .transpose()?;
    #[cfg(feature = "washoi")]
    {
        let maps = [
            init.navigation.as_ref().map(|navigation| navigation.map()),
            learned.as_ref().map(|learned| learned.map()),
            sight.as_ref().map(|sight| sight.map()),
        ];
        let mut maps = maps.into_iter().flatten();
        if let Some(first) = maps.next()
            && maps.any(|map| map != first)
        {
            return Err(invalid("map assets refer to different maps"));
        }
    }

    #[cfg(feature = "washoi")]
    if let Some(navigation) = &init.navigation {
        navigation
            .validate_learned_links(learned.as_ref())
            .map_err(invalid)?;
    }

    emit(
        writer,
        &Event::Ready {
            version: VERSION,
            lobby_id: &init.lobby_id,
        },
    )?;

    #[cfg(feature = "washoi")]
    if let Some(pilot) = init.pilot {
        return run_pilot(reader, writer, pilot);
    }
    #[cfg(feature = "washoi")]
    if let Some(endpoint) = init.washoi_endpoint {
        return washoi::run(
            reader,
            writer,
            endpoint,
            washoi::MapAssets {
                navigation: init.navigation,
                learned,
                sight,
            },
        );
    }
    await_shutdown(&mut reader, &mut record)
}

fn emit(writer: &mut impl Write, event: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, event)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn await_shutdown(reader: &mut impl BufRead, record: &mut Vec<u8>) -> io::Result<()> {
    match read_command(reader, record)? {
        Some(Command::Initialize(_)) => Err(invalid("lobby already initialized")),
        Some(Command::Shutdown) | None => Ok(()),
        #[cfg(feature = "washoi")]
        Some(_) => Err(invalid("command requires a Washoi session")),
    }
}

#[cfg(feature = "washoi")]
fn run_pilot(
    mut reader: impl BufRead + Send + 'static,
    writer: &mut impl Write,
    pilot: Pilot,
) -> io::Result<()> {
    let (complete, mut shutdown) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("host-control".into())
        .spawn(move || {
            let _ = complete.send(await_shutdown(&mut reader, &mut Vec::new()));
        })?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            tokio::select! {
                result = &mut shutdown => return result.map_err(io::Error::other)?,
                result = drive_pilot(writer, pilot) => result?,
            }
            shutdown.await.map_err(io::Error::other)?
        })
}

#[cfg(feature = "washoi")]
async fn drive_pilot(writer: &mut impl Write, pilot: Pilot) -> io::Result<()> {
    use shagadelic::games::washoi::{
        BotPlayer,
        protocol::{AdmissionError, WashoiEvents, connect},
    };

    let joined = connect(
        &pilot.endpoint,
        pilot.name,
        pilot.ticket,
        pilot.team.as_deref(),
        std::time::Duration::from_secs(10),
        WashoiEvents,
        |_| {},
    )
    .await;
    let reason = match joined {
        Ok(mut connection) => {
            emit(
                writer,
                &Event::PilotAdmitted {
                    player_id: &connection.identity.player_id,
                },
            )?;
            let bot = BotPlayer::new(connection.identity.entity_id.clone());
            match connection.play(bot, |_| true).await {
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
    emit(writer, &Event::PilotEnded { reason })
}

fn read_command(reader: &mut impl BufRead, record: &mut Vec<u8>) -> io::Result<Option<Command>> {
    record.clear();
    let count = reader
        .take((MAX_RECORD_BYTES + 1) as u64)
        .read_until(b'\n', record)?;
    if count == 0 {
        return Ok(None);
    }
    if count > MAX_RECORD_BYTES {
        return Err(invalid("control record exceeds size limit"));
    }
    if record.last() != Some(&b'\n') {
        // A dead host may leave an incomplete command in the pipe.
        return Ok(None);
    }
    serde_json::from_slice(record)
        .map(Some)
        .map_err(|_| invalid("invalid control command"))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

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
    washoi_map: Option<String>,
    navigation: Option<Navigation>,
    navigation_file: Option<std::path::PathBuf>,
    learned_routes_file: Option<std::path::PathBuf>,
    sight_file: Option<std::path::PathBuf>,
    /// The map's collision, as `tools/bake-washoi-collision.mjs` writes it.
    collision_file: Option<std::path::PathBuf>,
    /// The map's location names, for team callouts.
    locations_file: Option<std::path::PathBuf>,
    /// SHAG's `config/bot-intel/people.json` and `<map>.json`.
    people_file: Option<std::path::PathBuf>,
    /// A HUNCH goal model (`washoi.goal.v0`), and whether it only shadows
    /// the arbiter (`shadow`, the default) or decides (`on`).
    hunch_file: Option<std::path::PathBuf>,
    hunch_mode: Option<String>,
    baseline_file: Option<std::path::PathBuf>,
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
        || init.washoi_map.is_some()
        || init.navigation.is_some()
        || init.navigation_file.is_some()
        || init.learned_routes_file.is_some()
        || init.sight_file.is_some()
        || init.collision_file.is_some()
        || init.locations_file.is_some()
        || init.people_file.is_some()
        || init.hunch_file.is_some()
        || init.hunch_mode.is_some()
        || init.baseline_file.is_some()
    {
        return Err(invalid("Washoi support is disabled"));
    }
    #[cfg(feature = "washoi")]
    if (init.washoi_map.is_some()
        || init.navigation.is_some()
        || init.navigation_file.is_some()
        || init.learned_routes_file.is_some()
        || init.sight_file.is_some()
        || init.collision_file.is_some()
        || init.locations_file.is_some()
        || init.people_file.is_some()
        || init.baseline_file.is_some()
        || init.hunch_file.is_some()
        || init.hunch_mode.is_some())
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
    let navigation = match (init.navigation, init.navigation_file) {
        (Some(_), Some(_)) => return Err(invalid("select either inline navigation or a file")),
        (navigation, None) => navigation,
        (None, Some(path)) => Some(Navigation::load(path)?),
    };
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
    let collision = init
        .collision_file
        .as_ref()
        .map(shagadelic::games::washoi::MapCollision::load)
        .transpose()?;
    #[cfg(feature = "washoi")]
    let locations = init
        .locations_file
        .as_ref()
        .map(shagadelic::games::washoi::Locations::load)
        .transpose()?;
    #[cfg(feature = "washoi")]
    let people = init
        .people_file
        .as_ref()
        .map(shagadelic::games::washoi::PeopleProfile::load)
        .transpose()?;
    // Validated once for the lobby against the goal decision's spec: format,
    // decision, schema, feature names in order, and dimensions.
    #[cfg(feature = "washoi")]
    let hunch = {
        let mode = match init.hunch_mode.as_deref() {
            None | Some("shadow") => shagadelic::games::washoi::HunchMode::Shadow,
            Some("on") => shagadelic::games::washoi::HunchMode::On,
            Some(_) => return Err(invalid("HUNCH mode must be shadow or on")),
        };
        match &init.hunch_file {
            None if init.hunch_mode.is_some() => {
                return Err(invalid("HUNCH mode requires a HUNCH model"));
            }
            None => None,
            Some(path) => {
                let spec = shagadelic::games::washoi::BotPlayer::hunch_spec();
                let loaded = shagadelic::hunch::Model::parse(&std::fs::read(path)?, &spec)
                    .map_err(|error| match error {
                        shagadelic::hunch::ModelError::Features => {
                            invalid("HUNCH model features differ")
                        }
                        error => error.into(),
                    })?;
                Some((loaded.model, mode))
            }
        }
    };
    #[cfg(feature = "washoi")]
    let baseline = init
        .baseline_file
        .as_ref()
        .map(shagadelic::games::washoi::Baseline::load)
        .transpose()?;
    #[cfg(feature = "washoi")]
    {
        let maps = [
            init.washoi_map.as_deref(),
            navigation.as_ref().map(|navigation| navigation.map()),
            learned.as_ref().map(|learned| learned.map()),
            sight.as_ref().map(|sight| sight.map()),
            collision.as_ref().map(|collision| collision.map()),
            locations.as_ref().map(|locations| locations.map()),
            baseline.as_ref().map(|baseline| baseline.map()),
        ];
        let mut maps = maps.into_iter().flatten();
        if let Some(first) = maps.next()
            && maps.any(|map| map != first)
        {
            return Err(invalid("map assets refer to different maps"));
        }
    }

    #[cfg(feature = "washoi")]
    if let Some(navigation) = &navigation {
        navigation
            .validate_learned_links(learned.as_ref())
            .map_err(invalid)?;
    }
    // A BSP names no map: its hash ties the collision to the navigation
    // baked from the same BSP.
    #[cfg(feature = "washoi")]
    if let Some(collision) = &collision
        && navigation
            .as_ref()
            .and_then(|navigation| navigation.bsp_sha256())
            != Some(collision.bsp_sha256())
    {
        return Err(invalid("collision BSP differs from the navigation's"));
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
                navigation,
                learned,
                sight,
                collision,
                locations,
                people,
                baseline,
                hunch,
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

#[cfg(all(test, feature = "washoi"))]
mod tests {
    use super::*;

    /// `tests/fixtures/box.collision` names map "box" and this BSP hash.
    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// The child's answer to an initialization with the box's collision and
    /// a walk graph baked from `graph_sha` on `graph_map`, or no graph.
    fn initialize(graph: Option<(&str, &str)>) -> io::Result<String> {
        let collision = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/box.collision");
        let navigation = graph.map(|(map, sha)| {
            serde_json::json!({"map": map, "graph": {
                "bsp_sha256": sha,
                "nodes": [[0, 0, 0], [64, 0, 0]], "edges": [[0, 1], [1, 0]],
                "entries": [0], "goals": [1]
            }})
        });
        let init = serde_json::json!({
            "type": "initialize", "version": VERSION, "lobby_id": "lobby",
            "washoi_endpoint": "ws://127.0.0.1:9", "collision_file": collision,
            "navigation": navigation,
        });
        let mut output = Vec::new();
        run(io::Cursor::new(format!("{init}\n")), &mut output)?;
        Ok(String::from_utf8(output).unwrap())
    }

    #[test]
    fn a_collision_file_must_come_from_the_navigations_bsp() {
        // The same BSP, its hash in either case: ready.
        let ready = initialize(Some(("box", &SHA.to_ascii_uppercase()))).unwrap();
        assert!(ready.contains(r#""type":"ready""#), "{ready}");
        let refused = |graph| initialize(graph).unwrap_err().to_string();
        let other = "f".repeat(64);
        assert_eq!(
            refused(Some(("box", &other))),
            "collision BSP differs from the navigation's"
        );
        assert_eq!(refused(None), "collision BSP differs from the navigation's");
        assert_eq!(
            refused(Some(("arena", SHA))),
            "map assets refer to different maps"
        );
    }

    /// The lobby loads a HUNCH model once and refuses one built for other
    /// features, a mode it does not know, or a mode with no model.
    #[test]
    fn a_hunch_model_must_match_the_goal_schema() {
        let (schema, names) = shagadelic::games::washoi::BotPlayer::hunch_schema();
        let dir = std::env::temp_dir().join(format!("shagadelic-hunch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |file: &str, names: Vec<String>| {
            let width = names.len();
            // Format 1, as `hunch train` wrote it before decisions were named.
            let model = shagadelic::hunch::Model {
                format: 1,
                decision: String::new(),
                schema: schema.into(),
                features: names,
                mean: vec![0.0; width],
                scale: vec![1.0; width],
                weights: vec![0.0; width],
            };
            let path = dir.join(file);
            std::fs::write(&path, serde_json::to_string(&model).unwrap()).unwrap();
            path
        };
        let goal = write("goal.json", names.iter().map(|n| (*n).to_owned()).collect());
        let mut swapped: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
        swapped.swap(0, 1);
        let other = write("other.json", swapped);
        let answer = |file: Option<&std::path::Path>, mode: Option<&str>| {
            let init = serde_json::json!({
                "type": "initialize", "version": VERSION, "lobby_id": "lobby",
                "washoi_endpoint": "ws://127.0.0.1:9",
                "hunch_file": file, "hunch_mode": mode,
            });
            let mut output = Vec::new();
            run(
                io::Cursor::new(format!(
                    "{init}
"
                )),
                &mut output,
            )
            .map(|()| String::from_utf8(output).unwrap())
            .map_err(|error| error.to_string())
        };
        for mode in [None, Some("shadow"), Some("on")] {
            let ready = answer(Some(&goal), mode).unwrap();
            assert!(ready.contains(r#""type":"ready""#), "{mode:?}: {ready}");
        }
        assert_eq!(
            answer(Some(&other), None).unwrap_err(),
            "HUNCH model features differ"
        );
        assert_eq!(
            answer(Some(&goal), Some("loud")).unwrap_err(),
            "HUNCH mode must be shadow or on"
        );
        assert_eq!(
            answer(None, Some("on")).unwrap_err(),
            "HUNCH mode requires a HUNCH model"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! HUNCH's offline tools (`docs/research/HUNCH.md`).
//!
//! `hunch extract <demo.shagdemo> --nav <map.json> [--sight <map.sight>]
//! [--seat <name>]` replays each human seat of a recorded match through the
//! bot's own perception and goal planner, never reading the raw snapshot
//! for features. Every half second out of a fight it writes one JSON line:
//! the eligible goal candidates and their `washoi.goal.v0` rows, the
//! arbiter's choice, and the label. The label is the first candidate pickup
//! the player takes within 10 seconds, or none.
//!
//! `hunch train <records.jsonl>... --out <model.json> [--seed <n>]` fits a
//! model on the labelled records.

use std::{
    collections::VecDeque,
    io::{self, BufRead, Write},
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use shagadelic::{
    games::washoi::{
        BotPlayer, Difficulty, Sight, SimTime, WashoiNavigation,
        protocol::{SnapshotEvent, WashoiEvents, entity_keys, observe},
    },
    hunch::{self, Decision, Training},
};
use shagger_shared::demo::demo_reader::DemoReader;

const TICK_RATE: u32 = 125;
/// A decision point every half second.
const SAMPLE_TICKS: u32 = TICK_RATE / 2;
/// The pickup taken within this long labels the decision.
const LABEL_TICKS: u32 = 10 * TICK_RATE;
/// `SELF_TAKE_RADIUS`: an item vanishing this close, the player took. A
/// standing origin is about 40 above the item.
const SELF_TAKE_RADIUS: f64 = 180.0;
const SELF_TAKE_HEIGHT: f64 = 96.0;

#[derive(Serialize, Deserialize)]
struct Record {
    schema: String,
    demo: String,
    seat: String,
    tick: u32,
    candidates: Vec<String>,
    features: Vec<f32>,
    /// The arbiter's choice, the rule baseline.
    rule: Option<String>,
    /// What the player went for, when the record could be labelled.
    label: Option<String>,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("extract") => extract(&args[1..]),
        Some("train") => train(&args[1..]),
        Some("evaluate") => evaluate(&args[1..]),
        _ => Err(io::Error::other(
            "usage: hunch extract <demo> --nav <map.json> [--sight <map.sight>] [--seat <name>]\n       hunch train <records.jsonl>... --out <model.json> [--seed <n>]
       hunch evaluate <model.json> <records.jsonl>...",
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hunch: {error}");
            ExitCode::FAILURE
        }
    }
}

fn option(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn positional(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
        } else if arg.starts_with("--") {
            skip = true;
        } else {
            out.push(arg.clone());
        }
    }
    out
}

fn extract(args: &[String]) -> io::Result<()> {
    let demo_path = positional(args)
        .into_iter()
        .next()
        .ok_or_else(|| io::Error::other("a demo is required"))?;
    let nav = option(args, "--nav").ok_or_else(|| io::Error::other("--nav is required"))?;
    let navigation = Arc::new(WashoiNavigation::load(nav)?);
    let sight = option(args, "--sight")
        .map(|path| Sight::load(path).map(Arc::new))
        .transpose()?;
    let only = option(args, "--seat");
    let mut reader = DemoReader::new(std::fs::read(&demo_path)?)
        .map_err(|error| io::Error::other(format!("{error:?}")))?;
    let header = reader
        .read_header()
        .map_err(|error| io::Error::other(format!("{error:?}")))?
        .clone();
    if header.map != navigation.map() {
        return Err(io::Error::other(format!(
            "demo map {} but navigation for {}",
            header.map,
            navigation.map()
        )));
    }
    let frames = reader
        .read_all_frames(&WashoiEvents)
        .map_err(|error| io::Error::other(format!("{error:?}")))?;
    let demo = PathBuf::from(&demo_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (schema, _) = BotPlayer::hunch_schema();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut written = 0usize;
    eprintln!(
        "{demo}: {} {} frames, seats {:?}",
        header.gametype,
        frames.len(),
        header
            .players
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
    );
    for seat in &header.players {
        if seat.name.is_empty()
            || only.as_ref().is_some_and(|name| *name != seat.name)
            || shagadelic::games::washoi::is_bot_name(&seat.name)
        {
            continue;
        }
        // The seat's entity, from the first frame that lists the player.
        let Some(entity) = frames.iter().find_map(|frame| {
            // The header's player ID need not be the snapshot's key; the
            // name matches.
            frame
                .state
                .players
                .get(&seat.player_id)
                .or_else(|| frame.state.players.values().find(|p| p.name == seat.name))
                .map(|player| player.entity_id.clone())
                .filter(|entity| !entity.is_empty())
        }) else {
            continue;
        };
        let pickups = navigation.pickup_spots();
        let mut bot = BotPlayer::new(entity.clone())
            .with_difficulty(Difficulty::Hard)
            .with_navigation(navigation.clone())
            .with_hunch_recording();
        if let Some(sight) = &sight {
            bot = bot.with_sight(sight.clone());
        }
        let mut pending: VecDeque<Record> = VecDeque::new();
        let (mut decided, mut recorded) = (0usize, 0usize);
        let mut next_sample = 0u32;
        let mut mask: Vec<bool> = Vec::new();
        for frame in &frames {
            let tick = frame.tick;
            let now = SimTime::ZERO + Duration::from_millis(u64::from(tick) * 8);
            // Labels: a pickup this seat took settles the records before it,
            // named by the event, or seen vanishing beside the seat.
            let mut taken: Vec<String> = Vec::new();
            if let Some(at) = frame.state.entities.get(&entity).and_then(|e| e.position) {
                for (id, (&before, &now_up)) in
                    mask.iter().zip(&frame.state.pickup_active_mask).enumerate()
                {
                    if before
                        && !now_up
                        && let Some((_, spot)) = pickups.iter().find(|(known, _)| *known == id)
                        && (spot[0] - at.x).hypot(spot[2] - at.z) <= SELF_TAKE_RADIUS
                        && (spot[1] - at.y).abs() <= SELF_TAKE_HEIGHT
                    {
                        taken.push(format!("pickup:{id}"));
                    }
                }
            }
            if !frame.state.pickup_active_mask.is_empty() {
                mask.clone_from(&frame.state.pickup_active_mask);
            }
            for taken in taken {
                for record in pending.iter_mut().filter(|r| r.label.is_none()) {
                    if record.candidates.contains(&taken) {
                        record.label = Some(taken.clone());
                    }
                }
            }
            for event in frame.state.events.iter().map(entity_keys) {
                if let SnapshotEvent::ItemPickup {
                    pickup_id,
                    entity_id,
                    ..
                } = event
                    && entity_id == entity
                    && pickup_id >= 0
                {
                    let taken = format!("pickup:{pickup_id}");
                    for record in pending.iter_mut().filter(|r| r.label.is_none()) {
                        if record.candidates.contains(&taken) {
                            record.label = Some(taken.clone());
                        }
                    }
                }
            }
            while pending
                .front()
                .is_some_and(|record| record.label.is_some() || tick > record.tick + LABEL_TICKS)
            {
                let record = pending.pop_front().expect("front");
                serde_json::to_writer(&mut out, &record)?;
                out.write_all(b"\n")?;
                written += 1;
            }
            bot.observe_at(observe(&frame.state), now);
            bot.decide(now);
            decided += 1;
            recorded += usize::from(bot.hunch_record().is_some());
            if tick >= next_sample
                && let Some((candidates, rows)) = bot.hunch_record()
                && candidates.len() >= 2
            {
                next_sample = tick + SAMPLE_TICKS;
                pending.push_back(Record {
                    schema: schema.to_owned(),
                    demo: demo.clone(),
                    seat: seat.name.clone(),
                    tick,
                    candidates: candidates.iter().map(|id| (*id).to_owned()).collect(),
                    features: rows.to_vec(),
                    rule: bot
                        .goal_explain()
                        .split_whitespace()
                        .next()
                        .and_then(|first| first.strip_prefix("goal=").map(str::to_owned)),
                    label: None,
                });
            }
        }
        eprintln!(
            "  {} as {entity}: {decided} frames, {recorded} arbitrated",
            seat.name
        );
        for record in pending {
            serde_json::to_writer(&mut out, &record)?;
            out.write_all(b"\n")?;
            written += 1;
        }
    }
    eprintln!("{written} decisions from {demo}");
    Ok(())
}

/// Labelled records with their chosen index, from JSON lines files.
fn labelled(paths: &[String]) -> io::Result<Vec<(Record, usize)>> {
    let (schema, names) = BotPlayer::hunch_schema();
    let width = names.len();
    let mut out = Vec::new();
    for path in paths {
        for line in io::BufReader::new(std::fs::File::open(path)?).lines() {
            let record: Record = serde_json::from_str(&line?).map_err(io::Error::other)?;
            if record.schema != schema || record.features.len() != record.candidates.len() * width {
                continue;
            }
            let Some(chosen) = record
                .label
                .as_ref()
                .and_then(|label| record.candidates.iter().position(|id| id == label))
            else {
                continue;
            };
            out.push((record, chosen));
        }
    }
    Ok(out)
}

/// `hunch evaluate`: top-1 agreement of the model and of the arbiter with
/// the player's choice, on records the model was not trained on.
fn evaluate(args: &[String]) -> io::Result<()> {
    let paths = positional(args);
    let (model_path, records) = paths
        .split_first()
        .ok_or_else(|| io::Error::other("a model and records are required"))?;
    let (schema, _) = BotPlayer::hunch_schema();
    let model = hunch::Model::load(model_path, schema)?;
    let records = labelled(records)?;
    let width = model.width();
    let mut scores = Vec::new();
    let (mut model_agrees, mut rule_agrees, mut chance) = (0usize, 0usize, 0.0f64);
    let (_, names) = BotPlayer::hunch_schema();
    let route = names.iter().position(|name| *name == "log_route_units");
    let mut nearest_agrees = 0usize;
    for (record, chosen) in &records {
        scores.resize(record.features.len() / width, 0.0);
        model_agrees += usize::from(model.choose(&record.features, &mut scores) == Some(*chosen));
        rule_agrees += usize::from(record.rule.as_ref() == record.label.as_ref());
        chance += 1.0 / record.candidates.len() as f64;
        // The nearest candidate by walk, the simplest rule.
        if let Some(route) = route {
            let nearest = record
                .features
                .chunks_exact(width)
                .enumerate()
                .min_by(|a, b| a.1[route].total_cmp(&b.1[route]))
                .map(|(index, _)| index);
            nearest_agrees += usize::from(nearest == Some(*chosen));
        }
    }
    let n = records.len().max(1) as f64;
    println!(
        "{} decisions: model {:.1} %, arbiter {:.1} %, nearest {:.1} %, uniform chance {:.1} %",
        records.len(),
        100.0 * model_agrees as f64 / n,
        100.0 * rule_agrees as f64 / n,
        100.0 * nearest_agrees as f64 / n,
        100.0 * chance / n
    );
    Ok(())
}

fn train(args: &[String]) -> io::Result<()> {
    let out = option(args, "--out").ok_or_else(|| io::Error::other("--out is required"))?;
    let seed = option(args, "--seed")
        .map(|seed| seed.parse::<u64>().map_err(io::Error::other))
        .transpose()?
        .unwrap_or(1);
    let (schema, names) = BotPlayer::hunch_schema();
    let width = names.len();
    let mut decisions = Vec::new();
    let mut rule_agrees = 0usize;
    for path in positional(args) {
        for line in io::BufReader::new(std::fs::File::open(&path)?).lines() {
            let record: Record = serde_json::from_str(&line?).map_err(io::Error::other)?;
            let Some(label) = &record.label else { continue };
            if record.schema != schema || record.features.len() != record.candidates.len() * width {
                continue;
            }
            let Some(chosen) = record.candidates.iter().position(|id| id == label) else {
                continue;
            };
            rule_agrees += usize::from(record.rule.as_ref() == Some(label));
            decisions.push(Decision {
                features: record.features,
                chosen,
            });
        }
    }
    if decisions.is_empty() {
        return Err(io::Error::other("no labelled decisions"));
    }
    let model = hunch::train(
        schema,
        names.iter().map(|name| (*name).to_owned()).collect(),
        &decisions,
        Training {
            seed,
            ..Training::default()
        },
    );
    let mut scores = Vec::new();
    let agrees = decisions
        .iter()
        .filter(|d| {
            scores.resize(d.features.len() / width, 0.0);
            model.choose(&d.features, &mut scores) == Some(d.chosen)
        })
        .count();
    std::fs::write(
        &out,
        serde_json::to_string_pretty(&model).map_err(io::Error::other)?,
    )?;
    eprintln!(
        "{} decisions: model agrees on {agrees}, the arbiter on {rule_agrees} (training data, not held out)",
        decisions.len()
    );
    Ok(())
}

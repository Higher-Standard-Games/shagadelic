//! HUNCH's offline tools (`docs/research/HUNCH.md`).
//!
//! `hunch extract <demo.shagdemo> --nav <map.json> [--sight <map.sight>]
//! [--seat <name>]` replays each human seat of a recorded match through the
//! bot's own perception and goal planner, never reading the raw snapshot
//! for features. Every half second out of a fight it writes one JSON line:
//! the eligible goal candidates and their `washoi.goal.v0` rows, the
//! arbiter's choice, and the label. The label is the first candidate pickup
//! the player takes within 10 seconds of the decision, in the same life and
//! round, or none. A pickup event naming the seat labels first. An item seen
//! vanishing beside the seat labels only a frame later, and only when no
//! event named someone else for it. Each record keeps the evidence, the
//! life, the pickup tick, whether another player stood by the item, and why
//! the record closed.
//!
//! Records carry HUNCH's episode identity: the decision (`washoi.goal`),
//! the group a split keeps together (the demo's match), and the source.
//! A demo does not say who played a seat, so records are `unverified`
//! unless `--source human` says the recording is known to be a person's.
//!
//! `hunch train <records.jsonl>... --out <model.json> [--seed <n>]` fits a
//! model on the labelled, unambiguous records through `hunch::train`.

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
    hunch::{self, Episode, Label, Source, Training},
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

fn goal_decision() -> String {
    BotPlayer::hunch_spec().decision.to_owned()
}

fn unverified() -> Source {
    Source::Unverified
}

#[derive(Serialize, Deserialize)]
struct Record {
    /// The HUNCH decision, `washoi.goal`. Records from before it was named
    /// read as that.
    #[serde(default = "goal_decision")]
    decision: String,
    schema: String,
    /// What a split keeps together: the match, by default the demo.
    #[serde(default)]
    group: String,
    #[serde(default = "unverified")]
    source: Source,
    demo: String,
    seat: String,
    tick: u32,
    candidates: Vec<String>,
    features: Vec<f32>,
    /// The arbiter's choice, the rule baseline.
    rule: Option<String>,
    /// What the player went for, when the record could be labelled.
    label: Option<String>,
    /// How the label was found: "event", a pickup event naming the seat, or
    /// "mask", the item vanishing beside it.
    #[serde(default)]
    evidence: Option<String>,
    /// Another player stood within taking distance of the item too.
    #[serde(default)]
    ambiguous: bool,
    #[serde(default)]
    pickup_tick: Option<u32>,
    /// The seat's life, counting from 0, at the decision.
    #[serde(default)]
    life: u32,
    /// Why the record closed: "labelled", "horizon", "death", "round_end",
    /// or "end" of the demo.
    #[serde(default)]
    closed: Option<String>,
}

impl Record {
    /// The training episode this record gives: labelled, unambiguous, and
    /// for the goal decision's schema. Its group falls back to the demo.
    fn episode(&self) -> Option<Episode> {
        let spec = BotPlayer::hunch_spec();
        if self.ambiguous || self.decision != spec.decision || self.schema != spec.schema {
            return None;
        }
        let label = self.label.as_ref()?;
        Some(Episode {
            decision: self.decision.clone(),
            schema: self.schema.clone(),
            group: if self.group.is_empty() {
                self.demo.clone()
            } else {
                self.group.clone()
            },
            source: self.source,
            ids: self.candidates.clone(),
            rows: self.features.clone(),
            label: Label::Chose { id: label.clone() },
        })
    }
}

/// The open decisions of one seat, and the items seen vanishing beside it
/// that wait a frame for an event naming who took them.
#[derive(Default)]
struct Labels {
    pending: VecDeque<Record>,
    vanished: Vec<(usize, u32, bool)>,
}

impl Labels {
    /// One frame's evidence, in precedence order. `named` holds each pickup
    /// event's item and whether it named the seat. `seen` holds the items
    /// that vanished beside the seat this frame, with whether another player
    /// stood by them too. `close` ends every open decision, at a death or
    /// the end of a round or the match. Returns the records written.
    fn frame(
        &mut self,
        tick: u32,
        named: &[(i16, bool)],
        seen: &[(usize, bool)],
        close: Option<&str>,
        out: &mut impl Write,
    ) -> io::Result<usize> {
        let named_any = |id: usize| {
            named
                .iter()
                .any(|&(named, _)| usize::try_from(named) == Ok(id))
        };
        for &(id, own) in named {
            if own {
                self.settle(&format!("pickup:{id}"), tick, "event", false);
            }
        }
        for (id, at, ambiguous) in std::mem::take(&mut self.vanished) {
            if !named_any(id) {
                self.settle(&format!("pickup:{id}"), at, "mask", ambiguous);
            }
        }
        self.vanished.extend(
            seen.iter()
                .filter(|&&(id, _)| !named_any(id))
                .map(|&(id, ambiguous)| (id, tick, ambiguous)),
        );
        let mut written = self.flush(tick, None, out)?;
        if let Some(reason) = close {
            self.vanished.clear();
            written += self.flush(tick, Some(reason), out)?;
        }
        Ok(written)
    }

    /// Label every open record within the horizon that offered `taken`.
    fn settle(&mut self, taken: &str, tick: u32, evidence: &str, ambiguous: bool) {
        for record in self.pending.iter_mut().filter(|record| {
            record.label.is_none()
                && tick > record.tick
                && tick.saturating_sub(record.tick) <= LABEL_TICKS
        }) {
            if record.candidates.iter().any(|id| id == taken) {
                record.label = Some(taken.to_owned());
                record.evidence = Some(evidence.to_owned());
                record.ambiguous = ambiguous;
                record.pickup_tick = Some(tick);
                record.closed = Some("labelled".to_owned());
            }
        }
    }

    /// Write the front records that are settled: labelled, past the
    /// horizon, or every one when `close` gives a reason.
    fn flush(&mut self, tick: u32, close: Option<&str>, out: &mut impl Write) -> io::Result<usize> {
        let mut written = 0;
        while let Some(record) = self.pending.front_mut() {
            if record.label.is_none() {
                if let Some(reason) = close {
                    record.closed = Some(reason.to_owned());
                } else if tick.saturating_sub(record.tick) > LABEL_TICKS {
                    record.closed = Some("horizon".to_owned());
                } else {
                    break;
                }
            }
            let record = self.pending.pop_front().expect("front");
            serde_json::to_writer(&mut *out, &record)?;
            out.write_all(b"\n")?;
            written += 1;
        }
        Ok(written)
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("extract") => extract(&args[1..]),
        Some("train") => train(&args[1..]),
        Some("evaluate") => evaluate(&args[1..]),
        _ => Err(io::Error::other(
            "usage: hunch extract <demo> --nav <map.json> [--sight <map.sight>] [--seat <name>] [--source human]\n       hunch train <records.jsonl>... --out <model.json> [--seed <n>]
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
    let source = match option(args, "--source").as_deref() {
        None => Source::Unverified,
        Some("human") => Source::Human,
        Some(other) => {
            return Err(io::Error::other(format!(
                "--source {other}: a demo seat is human or unverified"
            )));
        }
    };
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
        let mut labels = Labels::default();
        let (mut decided, mut recorded) = (0usize, 0usize);
        let mut next_sample = 0u32;
        let mut mask: Vec<bool> = Vec::new();
        let mut life = 0u32;
        for frame in &frames {
            let tick = frame.tick;
            let now = SimTime::ZERO + Duration::from_millis(u64::from(tick) * 8);
            let events: Vec<SnapshotEvent> = frame.state.events.iter().map(entity_keys).collect();
            let named: Vec<(i16, bool)> = events
                .iter()
                .filter_map(|event| match event {
                    SnapshotEvent::ItemPickup {
                        pickup_id,
                        entity_id,
                        ..
                    } if *pickup_id >= 0 => Some((*pickup_id, *entity_id == entity)),
                    _ => None,
                })
                .collect();
            // Items vanishing beside the seat this frame.
            let mut seen = Vec::new();
            if let Some(at) = frame.state.entities.get(&entity).and_then(|e| e.position) {
                let near = |spot: &[f64; 3], x: f64, y: f64, z: f64| {
                    (spot[0] - x).hypot(spot[2] - z) <= SELF_TAKE_RADIUS
                        && (spot[1] - y).abs() <= SELF_TAKE_HEIGHT
                };
                for (id, (&before, &now_up)) in
                    mask.iter().zip(&frame.state.pickup_active_mask).enumerate()
                {
                    let Some((_, spot)) = pickups.iter().find(|(known, _)| *known == id) else {
                        continue;
                    };
                    if before && !now_up && near(spot, at.x, at.y, at.z) {
                        let ambiguous = frame.state.players.values().any(|player| {
                            player.entity_id != entity
                                && frame
                                    .state
                                    .entities
                                    .get(&player.entity_id)
                                    .and_then(|e| e.position)
                                    .is_some_and(|other| near(spot, other.x, other.y, other.z))
                        });
                        seen.push((id, ambiguous));
                    }
                }
            }
            if !frame.state.pickup_active_mask.is_empty() {
                mask.clone_from(&frame.state.pickup_active_mask);
            }
            // A death or the end of a round or match closes every open
            // record: what the seat goes for next is a new decision.
            let died = events
                .iter()
                .any(|event| matches!(event, SnapshotEvent::Death { entity_id, .. } if *entity_id == entity));
            let round_over = events.iter().any(|event| {
                matches!(event, SnapshotEvent::MatchState(state)
                    if matches!(state.get("phase").and_then(|phase| phase.as_f64()), Some(3.0 | 4.0)))
            });
            let close = if died {
                Some("death")
            } else if round_over {
                Some("round_end")
            } else {
                None
            };
            written += labels.frame(tick, &named, &seen, close, &mut out)?;
            if died {
                life += 1;
            }
            bot.observe_at(observe(&frame.state), now);
            bot.decide(now);
            decided += 1;
            recorded += usize::from(bot.hunch_record().is_some());
            if close.is_none()
                && tick >= next_sample
                && let Some((candidates, rows)) = bot.hunch_record()
                && candidates.len() >= 2
            {
                next_sample = tick + SAMPLE_TICKS;
                labels.pending.push_back(Record {
                    decision: goal_decision(),
                    schema: schema.to_owned(),
                    group: demo.clone(),
                    source,
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
                    evidence: None,
                    ambiguous: false,
                    pickup_tick: None,
                    life,
                    closed: None,
                });
            }
        }
        eprintln!(
            "  {} as {entity}: {decided} frames, {recorded} arbitrated",
            seat.name
        );
        written += labels.flush(u32::MAX, Some("end"), &mut out)?;
    }
    eprintln!("{written} decisions from {demo}");
    Ok(())
}

/// Labelled, unambiguous records with their chosen index, from JSON lines
/// files.
fn labelled(paths: &[String]) -> io::Result<Vec<(Record, usize)>> {
    let spec = BotPlayer::hunch_spec();
    let mut out = Vec::new();
    for path in paths {
        for line in io::BufReader::new(std::fs::File::open(path)?).lines() {
            let record: Record = serde_json::from_str(&line?).map_err(io::Error::other)?;
            let Some(decision) = record
                .episode()
                .and_then(|episode| episode.decision(&spec).ok())
            else {
                continue;
            };
            out.push((record, decision.chosen));
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
    let model = hunch::Model::load(model_path, &BotPlayer::hunch_spec())?.model;
    let (_, names) = BotPlayer::hunch_schema();
    let records = labelled(records)?;
    let width = model.width();
    let mut scores = Vec::new();
    let (mut model_agrees, mut rule_agrees, mut chance) = (0usize, 0usize, 0.0f64);
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
    let spec = BotPlayer::hunch_spec();
    let mut episodes = Vec::new();
    let mut rule_agrees = 0usize;
    let mut sources = [0usize; 4];
    for path in positional(args) {
        for line in io::BufReader::new(std::fs::File::open(&path)?).lines() {
            let record: Record = serde_json::from_str(&line?).map_err(io::Error::other)?;
            let Some(episode) = record.episode() else {
                continue;
            };
            rule_agrees += usize::from(record.rule.as_ref() == record.label.as_ref());
            sources[episode.source as usize] += 1;
            episodes.push(episode);
        }
    }
    if episodes.is_empty() {
        return Err(io::Error::other("no labelled decisions"));
    }
    let model = hunch::train(
        &spec,
        &episodes,
        Training {
            seed,
            ..Training::default()
        },
    )
    .map_err(io::Error::other)?;
    let mut scores = Vec::new();
    let agrees = episodes
        .iter()
        .filter_map(|episode| episode.decision(&spec).ok())
        .filter(|d| {
            scores.resize(d.features.len() / spec.width(), 0.0);
            model.choose(&d.features, &mut scores) == Some(d.chosen)
        })
        .count();
    std::fs::write(&out, model.to_bytes())?;
    let [human, designer, rule, unverified] = sources;
    eprintln!(
        "{} decisions ({human} human, {designer} designer, {rule} rule, {unverified} unverified): \
         model agrees on {agrees}, the arbiter on {rule_agrees} (training data, not held out)",
        episodes.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(tick: u32) -> Record {
        Record {
            decision: goal_decision(),
            schema: "test".into(),
            group: "demo".into(),
            source: Source::Unverified,
            demo: "demo".into(),
            seat: "seat".into(),
            tick,
            candidates: vec!["pickup:1".into(), "pickup:2".into()],
            features: Vec::new(),
            rule: None,
            label: None,
            evidence: None,
            ambiguous: false,
            pickup_tick: None,
            life: 0,
            closed: None,
        }
    }

    fn written(out: &[u8]) -> Vec<Record> {
        out.split(|&byte| byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect()
    }

    /// A record gives its episode: labelled and unambiguous, by ID, for the
    /// goal schema. Records written before episodes were named still read,
    /// as unverified goal decisions grouped by their demo.
    #[test]
    fn a_labelled_record_gives_its_episode_and_old_records_still_read() {
        let (schema, names) = BotPlayer::hunch_schema();
        let mut record = decision(0);
        record.schema = schema.into();
        record.features = vec![0.0; 2 * names.len()];
        assert!(record.episode().is_none(), "unlabelled");
        record.label = Some("pickup:2".into());
        let episode = record.episode().unwrap();
        assert_eq!(
            episode.label,
            Label::Chose {
                id: "pickup:2".into()
            }
        );
        assert_eq!(
            episode.decision(&BotPlayer::hunch_spec()).unwrap().chosen,
            1
        );
        record.ambiguous = true;
        assert!(record.episode().is_none(), "ambiguous");
        let old = format!(
            r#"{{"schema":"{schema}","demo":"m1.shagdemo","seat":"p","tick":0,
            "candidates":["pickup:1","pickup:2"],"features":{rows:?},"rule":null,"label":"pickup:1"}}"#,
            rows = vec![0.0; 2 * names.len()]
        );
        let old: Record = serde_json::from_str(&old).unwrap();
        let episode = old.episode().unwrap();
        assert_eq!(
            (
                episode.decision.as_str(),
                episode.group.as_str(),
                episode.source
            ),
            ("washoi.goal", "m1.shagdemo", Source::Unverified)
        );
    }

    #[test]
    fn an_event_naming_the_seat_labels_at_once() {
        let mut labels = Labels::default();
        labels.pending.push_back(decision(0));
        let mut out = Vec::new();
        labels
            .frame(100, &[(2, true)], &[], None, &mut out)
            .unwrap();
        let record = &written(&out)[0];
        assert_eq!(record.label.as_deref(), Some("pickup:2"));
        assert_eq!(record.evidence.as_deref(), Some("event"));
        assert_eq!(record.pickup_tick, Some(100));
        assert_eq!(record.closed.as_deref(), Some("labelled"));
    }

    #[test]
    fn a_vanished_item_labels_a_frame_later_unless_an_event_names_another_taker() {
        let mut out = Vec::new();
        // Seen vanishing beside the seat, with no event: a mask label, at
        // the tick it vanished, one frame on.
        let mut labels = Labels::default();
        labels.pending.push_back(decision(0));
        labels
            .frame(100, &[], &[(1, true)], None, &mut out)
            .unwrap();
        assert!(written(&out).is_empty());
        labels.frame(102, &[], &[], None, &mut out).unwrap();
        let record = &written(&out)[0];
        assert_eq!(
            (record.label.as_deref(), record.evidence.as_deref()),
            (Some("pickup:1"), Some("mask"))
        );
        assert_eq!((record.pickup_tick, record.ambiguous), (Some(100), true));
        // An event the next frame naming another player vetoes it.
        let mut out = Vec::new();
        let mut labels = Labels::default();
        labels.pending.push_back(decision(0));
        labels
            .frame(100, &[], &[(1, false)], None, &mut out)
            .unwrap();
        labels
            .frame(102, &[(1, false)], &[], None, &mut out)
            .unwrap();
        labels
            .frame(100 + LABEL_TICKS, &[], &[], None, &mut out)
            .unwrap();
        let record = &written(&out)[0];
        assert_eq!(
            (record.label.as_ref(), record.closed.as_deref()),
            (None, Some("horizon"))
        );
        // And in the same frame too.
        let mut labels = Labels::default();
        labels.pending.push_back(decision(0));
        labels
            .frame(100, &[(1, false)], &[(1, false)], None, &mut Vec::new())
            .unwrap();
        labels.frame(102, &[], &[], None, &mut Vec::new()).unwrap();
        assert!(labels.pending[0].label.is_none());
    }

    #[test]
    fn the_horizon_holds_before_a_label_is_assigned() {
        let mut out = Vec::new();
        let mut labels = Labels::default();
        labels.pending.push_back(decision(0));
        // A pickup one tick past the horizon, in the frame that would have
        // flushed the record, labels nothing.
        labels
            .frame(LABEL_TICKS + 1, &[(1, true)], &[], None, &mut out)
            .unwrap();
        let record = &written(&out)[0];
        assert_eq!(
            (record.label.as_ref(), record.closed.as_deref()),
            (None, Some("horizon"))
        );
    }

    #[test]
    fn a_death_or_the_rounds_end_closes_open_decisions() {
        for reason in ["death", "round_end"] {
            let mut out = Vec::new();
            let mut labels = Labels::default();
            labels.pending.push_back(decision(0));
            labels.pending.push_back(decision(62));
            // Seen vanishing as the seat dies: it never labels.
            labels
                .frame(100, &[], &[(1, false)], Some(reason), &mut out)
                .unwrap();
            labels
                .frame(102, &[(1, true)], &[], None, &mut out)
                .unwrap();
            let records = written(&out);
            assert_eq!(records.len(), 2);
            assert!(
                records.iter().all(
                    |record| record.label.is_none() && record.closed.as_deref() == Some(reason)
                )
            );
        }
    }
}

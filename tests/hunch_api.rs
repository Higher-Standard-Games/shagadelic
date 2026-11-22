//! HUNCH's public API, from outside the crate with Washoi disabled: the
//! acceptance cases in `docs/research/HUNCH.md`, "RPG and hockey fixtures
//! for the public API". Each game keeps its typed actions beside candidate
//! IDs and feature rows; HUNCH sees only the rows and the IDs.

use std::sync::Arc;

use shagadelic::hunch::{
    Binding, Bindings, Episode, Label, Loaded, Mode, Model, ModelError, Source, Spec, Training,
    train,
};

// An isometric RPG healer: heal from here, move behind a pillar to heal,
// or interrupt a caster. Four observable qualities per action.
const HEALER_FEATURES: [&str; 4] = [
    "ally_deficit",
    "cast_seconds",
    "exposure",
    "interrupt_value",
];
const HEALER: Spec<'static> = Spec {
    decision: "rpg.healer.action",
    schema: "rpg.healer.action.v0",
    features: &HEALER_FEATURES,
};

#[derive(Clone, Debug, PartialEq)]
enum HealerAction {
    Heal { target: u32 },
    MoveAndHeal { tile: (i32, i32), target: u32 },
    Interrupt { caster: u32 },
}

// A hockey puck carrier: pass to a particular winger, shoot, or carry wide.
// The same width as the healer, so only identity tells the artifacts apart.
const PUCK_FEATURES: [&str; 4] = [
    "lane_clearance",
    "receive_seconds",
    "pressure",
    "shot_angle",
];
const PUCK: Spec<'static> = Spec {
    decision: "hockey.puck",
    schema: "hockey.puck.v0",
    features: &PUCK_FEATURES,
};

#[derive(Clone, Debug, PartialEq)]
enum PuckAction {
    Pass { receiver: u32 },
    Shoot { far_post: bool },
    Carry,
}

/// The game's side of one decision: typed actions, their IDs, their rows.
struct Offered<A> {
    actions: Vec<A>,
    ids: Vec<String>,
    rows: Vec<f32>,
}

impl<A: Clone> Offered<A> {
    fn new(candidates: Vec<(&str, A, [f32; 4])>) -> Self {
        let mut offered = Self {
            actions: Vec::new(),
            ids: Vec::new(),
            rows: Vec::new(),
        };
        for (id, action, row) in candidates {
            offered.ids.push(id.to_owned());
            offered.actions.push(action);
            offered.rows.extend(row);
        }
        offered
    }

    /// The model's pick, back as the game's own action and its ID.
    fn pick(&self, model: &Model) -> (A, &str) {
        let mut scores = vec![0.0; self.actions.len()];
        let index = model
            .choose(&self.rows, &mut scores)
            .expect("finite scores");
        (self.actions[index].clone(), &self.ids[index])
    }
}

fn healer_snapshot() -> Offered<HealerAction> {
    Offered::new(vec![
        (
            "heal:2",
            HealerAction::Heal { target: 2 },
            [0.6, 1.5, 0.9, 0.0],
        ),
        (
            "pillar_heal:2",
            HealerAction::MoveAndHeal {
                tile: (4, 7),
                target: 2,
            },
            [0.6, 2.5, 0.1, 0.0],
        ),
        (
            "interrupt:9",
            HealerAction::Interrupt { caster: 9 },
            [0.0, 0.5, 0.7, 0.8],
        ),
    ])
}

/// What the carrier (player 7) sees in one snapshot. The winger (player
/// 11) sees the same snapshot from its own position.
fn puck_snapshot(player: u32) -> Offered<PuckAction> {
    let lean = if player == 7 { 0.0 } else { 0.3 };
    Offered::new(vec![
        (
            "pass:11",
            PuckAction::Pass { receiver: 11 },
            [0.9 - lean, 0.4, 0.2, 0.0],
        ),
        (
            "pass:4",
            PuckAction::Pass { receiver: 4 },
            [0.3, 0.7, 0.6, 0.0],
        ),
        (
            "shoot:far",
            PuckAction::Shoot { far_post: true },
            [0.0, 0.0, 0.5, 0.2 + lean],
        ),
        ("carry", PuckAction::Carry, [0.5, 0.0, 0.8, 0.0]),
    ])
}

fn episode<A>(spec: &Spec, offered: &Offered<A>, source: Source, label: Label) -> Episode {
    Episode {
        decision: spec.decision.to_owned(),
        schema: spec.schema.to_owned(),
        group: "session-1".to_owned(),
        source,
        ids: offered.ids.clone(),
        rows: offered.rows.clone(),
        label,
    }
}

/// Train from one labelled episode and round-trip the artifact through
/// bytes, as an embedded host would hold it.
fn artifact(spec: &Spec, episode: Episode) -> Vec<u8> {
    train(spec, &[episode], Training::default())
        .expect("a valid episode")
        .to_bytes()
}

#[test]
fn a_designer_preference_and_a_recorded_choice_train_load_and_replay_by_id() {
    // A designer prefers healing from behind the pillar to healing in the
    // open. Nothing is said about the interrupt, which was not compared.
    let healer = healer_snapshot();
    let preference = episode(
        &HEALER,
        &healer,
        Source::Designer,
        Label::Preferred {
            better: "pillar_heal:2".into(),
            worse: "heal:2".into(),
        },
    );
    let healer_bytes = artifact(&HEALER, preference);
    // A recorded carrier passed to winger 11 among four choices.
    let carrier = puck_snapshot(7);
    let choice = episode(
        &PUCK,
        &carrier,
        Source::Human,
        Label::Chose {
            id: "pass:11".into(),
        },
    );
    let puck_bytes = artifact(&PUCK, choice);

    let healer_model = Model::parse(&healer_bytes, &HEALER).unwrap();
    let puck_model = Model::parse(&puck_bytes, &PUCK).unwrap();
    assert_ne!(healer_model.digest, puck_model.digest);

    // Replayed, the preferred pair goes the designer's way, back as the
    // game's typed action.
    let pair = Offered::new(vec![
        (
            "heal:2",
            HealerAction::Heal { target: 2 },
            [0.6, 1.5, 0.9, 0.0],
        ),
        (
            "pillar_heal:2",
            HealerAction::MoveAndHeal {
                tile: (4, 7),
                target: 2,
            },
            [0.6, 2.5, 0.1, 0.0],
        ),
    ]);
    let (action, id) = pair.pick(&healer_model.model);
    assert_eq!(id, "pillar_heal:2");
    assert_eq!(
        action,
        HealerAction::MoveAndHeal {
            tile: (4, 7),
            target: 2
        }
    );
    let (action, id) = carrier.pick(&puck_model.model);
    assert_eq!((action, id), (PuckAction::Pass { receiver: 11 }, "pass:11"));

    // The same widths, so only identity stops a swapped artifact.
    assert!(matches!(
        Model::parse(&healer_bytes, &PUCK),
        Err(ModelError::Decision { .. })
    ));
    assert!(matches!(
        Model::parse(&puck_bytes, &HEALER),
        Err(ModelError::Decision { .. })
    ));
    let revised = Spec {
        schema: "hockey.puck.v1",
        ..PUCK
    };
    assert!(matches!(
        Model::parse(&puck_bytes, &revised),
        Err(ModelError::Schema { .. })
    ));
}

#[test]
fn players_deciding_on_one_snapshot_get_the_same_result_in_either_order() {
    let carrier = puck_snapshot(7);
    let bytes = artifact(
        &PUCK,
        episode(
            &PUCK,
            &carrier,
            Source::Human,
            Label::Chose {
                id: "pass:11".into(),
            },
        ),
    );
    let model = Model::parse(&bytes, &PUCK).unwrap().model;
    let decide = |players: &[u32]| -> Vec<(u32, PuckAction)> {
        let mut picks: Vec<(u32, PuckAction)> = players
            .iter()
            .map(|&player| (player, puck_snapshot(player).pick(&model).0))
            .collect();
        picks.sort_by_key(|(player, _)| *player);
        picks
    };
    let forward = decide(&[7, 11]);
    assert_eq!(forward, decide(&[11, 7]));
    // The model is shared read-only: another thread scores the same.
    let shared = Arc::clone(&model);
    let elsewhere = std::thread::spawn(move || puck_snapshot(11).pick(&shared).0)
        .join()
        .unwrap();
    assert_eq!(elsewhere, forward[1].1);
}

/// The game's call site: the rule's choice, and the bound model's when one
/// applies. Shadow records the model's pick and executes the rule's.
fn act<A: Clone>(
    bindings: &Bindings,
    archetype: &str,
    decision: &str,
    offered: &Offered<A>,
    rule: usize,
) -> (A, Option<String>) {
    let ruled = offered.actions[rule].clone();
    match bindings.resolve(archetype, decision) {
        None => (ruled, None),
        Some((mode, Loaded { model, .. })) => {
            let (picked, id) = offered.pick(model);
            match mode {
                Mode::On => (picked, Some(id.to_owned())),
                _ => (ruled, Some(id.to_owned())),
            }
        }
    }
}

#[test]
fn bindings_pick_a_model_per_archetype_and_shadow_executes_the_rule() {
    let carrier = puck_snapshot(7);
    let puck_bytes = artifact(
        &PUCK,
        episode(
            &PUCK,
            &carrier,
            Source::Human,
            Label::Chose {
                id: "pass:11".into(),
            },
        ),
    );
    let healer = healer_snapshot();
    let healer_bytes = artifact(
        &HEALER,
        episode(
            &HEALER,
            &healer,
            Source::Designer,
            Label::Preferred {
                better: "pillar_heal:2".into(),
                worse: "heal:2".into(),
            },
        ),
    );
    let bind = |archetype: Option<&str>, spec: &Spec, mode, bytes: Option<&[u8]>| Binding {
        archetype: archetype.map(str::to_owned),
        decision: spec.decision.to_owned(),
        mode,
        model: bytes.map(|bytes| Model::parse(bytes, spec).unwrap()),
    };
    let bindings = Bindings::new(vec![
        // Every skater decides passes with the model; wingers only shadow
        // it, and the goalie keeps the rule.
        bind(None, &PUCK, Mode::On, Some(&puck_bytes)),
        bind(Some("winger"), &PUCK, Mode::Shadow, Some(&puck_bytes)),
        bind(Some("goalie"), &PUCK, Mode::Off, None),
        // One entity, a second decision: the cleric's heals.
        bind(None, &HEALER, Mode::On, Some(&healer_bytes)),
    ])
    .unwrap();
    assert_eq!(bindings.models(), 2);

    // The rule would carry (index 3).
    let (on, picked) = act(&bindings, "center", PUCK.decision, &carrier, 3);
    assert_eq!(on, PuckAction::Pass { receiver: 11 });
    assert_eq!(picked.as_deref(), Some("pass:11"));
    let (shadow, recorded) = act(&bindings, "winger", PUCK.decision, &carrier, 3);
    let (off, none) = act(&bindings, "goalie", PUCK.decision, &carrier, 3);
    assert_eq!(shadow, PuckAction::Carry, "shadow executes the rule");
    assert_eq!(shadow, off, "shadow and off act alike");
    assert_eq!(recorded.as_deref(), Some("pass:11"));
    assert_eq!(none, None);
    // The healer decision resolves on its own binding; a decision nobody
    // bound is the rule's.
    assert!(bindings.resolve("cleric", HEALER.decision).is_some());
    assert!(bindings.resolve("cleric", "rpg.healer.move").is_none());
}

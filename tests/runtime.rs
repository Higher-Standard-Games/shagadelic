use std::{collections::VecDeque, convert::Infallible, time::Duration};

use shagadelic::runtime::{
    Budget, Deadlines, EntityKey, EntityStatus, GameSession, Runtime, SessionEvent, SpawnError,
    Update,
};

const BUDGET: Budget = Budget {
    events: 16,
    inputs: 16,
    decisions: 16,
};

#[derive(Default)]
struct FakeGame {
    events: VecDeque<SessionEvent>,
    spawned: Vec<EntityKey>,
    removed: Vec<EntityKey>,
    actions: Vec<(String, Update, Duration)>,
    closed: bool,
}

impl GameSession for FakeGame {
    type Spawn = Option<Deadlines>;
    type SpawnError = Infallible;

    fn begin_spawn(&mut self, key: &EntityKey, spawn: Self::Spawn) -> Result<(), Infallible> {
        self.spawned.push(key.clone());
        if let Some(deadlines) = spawn {
            self.events
                .push_back(SessionEvent::Admitted(key.clone(), deadlines));
        }
        Ok(())
    }

    fn remove(&mut self, key: &EntityKey) {
        self.removed.push(key.clone());
    }

    fn poll_event(&mut self) -> Option<SessionEvent> {
        self.events.pop_front()
    }

    fn update(&mut self, key: &EntityKey, kind: Update, now: Duration) -> Option<Duration> {
        self.actions.push((key.id.clone(), kind, now));
        let period = match (key.id.as_str(), kind) {
            ("skater", Update::Input) => 10,
            ("skater", Update::Decision) => 30,
            (_, Update::Input) => 20,
            (_, Update::Decision) => 50,
        };
        Some(now + Duration::from_millis(period))
    }

    fn begin_shutdown(&mut self) {
        self.closed = true;
    }
}

#[test]
fn the_game_drives_two_entity_rates_with_its_simulation_clock() {
    let mut runtime = Runtime::new(FakeGame::default());
    let start = Duration::ZERO;
    for id in ["skater", "goalie"] {
        runtime
            .spawn(
                id.into(),
                Some(Deadlines {
                    input: Some(start),
                    decision: Some(start),
                }),
            )
            .unwrap();
    }
    let first = runtime.advance(start, BUDGET).unwrap();
    assert_eq!((first.inputs, first.decisions), (2, 2));
    assert_eq!(runtime.advance(start, BUDGET).unwrap().inputs, 0);
    for tick in 1..=5 {
        runtime
            .advance(Duration::from_millis(tick * 10), BUDGET)
            .unwrap();
    }
    let actions = &runtime.session_mut().actions;
    let ticks = |id: &str, update| {
        actions
            .iter()
            .filter(|(name, kind, _)| name == id && *kind == update)
            .map(|(_, _, time)| time.as_millis())
            .collect::<Vec<_>>()
    };
    assert_eq!(ticks("skater", Update::Input), [0, 10, 20, 30, 40, 50]);
    assert_eq!(ticks("goalie", Update::Input), [0, 20, 40]);
    assert_eq!(ticks("skater", Update::Decision), [0, 30]);
    assert_eq!(ticks("goalie", Update::Decision), [0, 50]);
    assert!(runtime.advance(start, BUDGET).is_err());
}

#[test]
fn cancelled_admission_cannot_activate_a_replacement() {
    let mut runtime = Runtime::new(FakeGame::default());
    let old = runtime.spawn("bot".into(), None).unwrap();
    assert_eq!(runtime.spawn("bot".into(), None).unwrap(), old);
    assert_eq!(runtime.session_mut().spawned.len(), 1);
    assert!(runtime.remove(old.key()));
    let new = runtime.spawn("bot".into(), None).unwrap();
    assert_ne!(old, new);
    runtime.session_mut().events.extend([
        SessionEvent::Admitted(old.key().clone(), Deadlines::default()),
        SessionEvent::Admitted(new.key().clone(), Deadlines::default()),
        SessionEvent::Ended(old.key().clone()),
    ]);
    runtime.advance(Duration::ZERO, BUDGET).unwrap();
    assert!(!runtime.remove(old.key()));
    assert_eq!(
        runtime.status("bot"),
        Some(&EntityStatus::Active(new.key().clone()))
    );
    assert_eq!(
        runtime.session_mut().removed,
        [old.key().clone(), old.key().clone()]
    );

    runtime
        .session_mut()
        .events
        .push_back(SessionEvent::WakeDecision(new.key().clone()));
    assert_eq!(
        runtime.advance(Duration::ZERO, BUDGET).unwrap().decisions,
        1
    );
    runtime
        .session_mut()
        .events
        .push_back(SessionEvent::SessionEnded);
    runtime.advance(Duration::ZERO, BUDGET).unwrap();
    assert!(runtime.session_mut().closed);
    assert_eq!(runtime.next_deadline(), None);
    assert_eq!(runtime.spawn("bot".into(), None), Err(SpawnError::Closing));
}

/// A snapshot that wakes a decision has that decision's input sent in the
/// same advance, not one input period later.
#[test]
fn a_woken_decision_runs_before_the_inputs_due_with_it() {
    let mut runtime = Runtime::new(FakeGame::default());
    let bot = runtime
        .spawn(
            "skater".into(),
            Some(Deadlines {
                input: Some(Duration::ZERO),
                decision: None,
            }),
        )
        .unwrap();
    runtime.advance(Duration::ZERO, BUDGET).unwrap();
    runtime
        .session_mut()
        .events
        .push_back(SessionEvent::WakeDecision(bot.key().clone()));
    runtime.advance(Duration::from_millis(10), BUDGET).unwrap();
    let kinds: Vec<_> = runtime.session_mut().actions[1..]
        .iter()
        .map(|(_, kind, _)| *kind)
        .collect();
    assert_eq!(kinds, [Update::Decision, Update::Input]);
}

#[test]
fn budget_leaves_due_work_for_the_next_advance() {
    let mut runtime = Runtime::new(FakeGame::default());
    for id in ["b", "a"] {
        runtime
            .spawn(
                id.into(),
                Some(Deadlines {
                    input: Some(Duration::ZERO),
                    decision: None,
                }),
            )
            .unwrap();
    }
    let limited = Budget {
        events: 1,
        inputs: 0,
        decisions: 0,
    };
    assert_eq!(runtime.advance(Duration::ZERO, limited).unwrap().events, 1);
    assert_eq!(runtime.next_deadline(), Some(Duration::ZERO));
    let limited = Budget {
        events: 1,
        inputs: 1,
        decisions: 0,
    };
    assert_eq!(runtime.advance(Duration::ZERO, limited).unwrap().inputs, 1);
    assert_eq!(runtime.advance(Duration::ZERO, limited).unwrap().inputs, 1);
    assert_eq!(runtime.session_mut().actions[0].0, "a");
    assert_eq!(runtime.session_mut().actions[1].0, "b");
}

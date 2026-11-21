mod scheduler;

use std::{collections::BTreeMap, time::Duration};

use scheduler::Scheduler;

/// Identity of one admission attempt. A replacement gets a fresh generation.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EntityKey {
    pub id: String,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Deadlines {
    pub input: Option<Duration>,
    pub decision: Option<Duration>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Update {
    Input,
    Decision,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Budget {
    pub events: usize,
    pub inputs: usize,
    pub decisions: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Progress {
    pub events: usize,
    pub inputs: usize,
    pub decisions: usize,
}

pub enum SessionEvent {
    Admitted(EntityKey, Deadlines),
    Ended(EntityKey),
    WakeDecision(EntityKey),
    SessionEnded,
}

/// An integration owns game-specific entities, observations, and actions.
/// All callbacks should return promptly and enqueue asynchronous work if needed.
pub trait GameSession {
    type Spawn;
    type SpawnError;

    /// Start admission. An error means the attempt was never started.
    fn begin_spawn(&mut self, key: &EntityKey, spawn: Self::Spawn) -> Result<(), Self::SpawnError>;
    /// Cancel a pending attempt or release an active entity. Must be idempotent.
    fn remove(&mut self, key: &EntityKey);
    fn poll_event(&mut self) -> Option<SessionEvent>;
    /// Timestamp incoming observations with the same simulation clock as updates.
    fn poll_event_at(&mut self, _now: Duration) -> Option<SessionEvent> {
        self.poll_event()
    }
    /// Return the next deadline for this update kind, or disable it with None.
    /// Deadlines at or before now are moved to the next representable time.
    fn update(&mut self, key: &EntityKey, kind: Update, now: Duration) -> Option<Duration>;
    fn begin_shutdown(&mut self);
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntityStatus {
    Pending(EntityKey),
    Active(EntityKey),
}

#[derive(Debug, Eq, PartialEq)]
pub enum SpawnError<E> {
    Closing,
    GenerationExhausted,
    Integration(E),
}

#[derive(Debug, Eq, PartialEq)]
pub struct TimeWentBackwards;

pub struct Runtime<S> {
    session: S,
    entities: BTreeMap<String, EntityStatus>,
    scheduler: Scheduler,
    generation: u64,
    now: Duration,
    closing: bool,
}

impl<S: GameSession> Runtime<S> {
    pub fn new(session: S) -> Self {
        Self {
            session,
            entities: BTreeMap::new(),
            scheduler: Scheduler::default(),
            generation: 0,
            now: Duration::ZERO,
            closing: false,
        }
    }

    /// Access game-specific data, for example to supply a world view or drain actions.
    pub fn session_mut(&mut self) -> &mut S {
        &mut self.session
    }

    pub fn status(&self, id: &str) -> Option<&EntityStatus> {
        self.entities.get(id)
    }

    /// A duplicate live ID returns its existing status. An ended ID may be reused.
    pub fn spawn(
        &mut self,
        id: String,
        spawn: S::Spawn,
    ) -> Result<EntityStatus, SpawnError<S::SpawnError>> {
        if self.closing {
            return Err(SpawnError::Closing);
        }
        if let Some(status) = self.status(&id) {
            return Ok(status.clone());
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(SpawnError::GenerationExhausted)?;
        let key = EntityKey {
            id,
            generation: self.generation,
        };
        self.session
            .begin_spawn(&key, spawn)
            .map_err(SpawnError::Integration)?;
        let status = EntityStatus::Pending(key.clone());
        self.entities.insert(key.id.clone(), status.clone());
        Ok(status)
    }

    pub fn remove(&mut self, key: &EntityKey) -> bool {
        if !self.is_current(key) {
            return false;
        }
        self.forget(key);
        self.session.remove(key);
        true
    }

    pub fn shutdown(&mut self) {
        if self.closing {
            return;
        }
        self.closing = true;
        for status in std::mem::take(&mut self.entities).into_values() {
            self.session.remove(status.key());
        }
        self.scheduler = Scheduler::default();
        self.session.begin_shutdown();
    }

    /// Advance at a caller-supplied time. Repeating a timestamp services pending
    /// events and overdue work without moving simulation time forward.
    pub fn advance(
        &mut self,
        now: Duration,
        budget: Budget,
    ) -> Result<Progress, TimeWentBackwards> {
        if now < self.now {
            return Err(TimeWentBackwards);
        }
        self.now = now;
        let mut progress = Progress::default();
        while progress.events < budget.events {
            let Some(event) = self.session.poll_event_at(now) else {
                break;
            };
            self.handle_event(event);
            progress.events += 1;
        }
        progress.inputs = self.run_due(Update::Input, budget.inputs);
        progress.decisions = self.run_due(Update::Decision, budget.decisions);
        Ok(progress)
    }

    pub fn next_deadline(&self) -> Option<Duration> {
        self.scheduler.next_deadline()
    }

    fn run_due(&mut self, kind: Update, limit: usize) -> usize {
        let due = self.scheduler.take_due(kind, self.now, limit);
        let count = due.len();
        for key in due {
            if let Some(next) = self.session.update(&key, kind, self.now) {
                let next = if next <= self.now {
                    let Some(next) = self.now.checked_add(Duration::from_nanos(1)) else {
                        continue;
                    };
                    next
                } else {
                    next
                };
                self.scheduler.schedule(kind, key, next);
            }
        }
        count
    }

    fn handle_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::Admitted(key, deadlines) => match self.status(&key.id) {
                Some(EntityStatus::Pending(current)) if *current == key => {
                    self.entities
                        .insert(key.id.clone(), EntityStatus::Active(key.clone()));
                    self.scheduler.activate(key, deadlines);
                }
                Some(EntityStatus::Active(current)) if *current == key => {}
                _ => self.session.remove(&key),
            },
            SessionEvent::Ended(key) if self.is_current(&key) => self.forget(&key),
            SessionEvent::WakeDecision(key) if matches!(self.status(&key.id), Some(EntityStatus::Active(current)) if *current == key) =>
            {
                self.scheduler.wake_decision(key, self.now);
            }
            SessionEvent::SessionEnded => self.shutdown(),
            _ => {}
        }
    }

    fn is_current(&self, key: &EntityKey) -> bool {
        self.status(&key.id)
            .is_some_and(|status| status.key() == key)
    }

    fn forget(&mut self, key: &EntityKey) {
        self.entities.remove(&key.id);
        self.scheduler.remove(key);
    }
}

impl EntityStatus {
    pub fn key(&self) -> &EntityKey {
        match self {
            Self::Pending(key) | Self::Active(key) => key,
        }
    }
}

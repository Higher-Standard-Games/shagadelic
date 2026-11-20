use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use super::{Deadlines, EntityKey, Update};

#[derive(Default)]
pub(super) struct Scheduler {
    input: Queue,
    decision: Queue,
}

#[derive(Default)]
struct Queue {
    by_time: BTreeSet<(Duration, EntityKey)>,
    by_entity: BTreeMap<EntityKey, Duration>,
}

impl Scheduler {
    pub(super) fn activate(&mut self, key: EntityKey, deadlines: Deadlines) {
        if let Some(at) = deadlines.input {
            self.input.schedule(key.clone(), at);
        }
        if let Some(at) = deadlines.decision {
            self.decision.schedule(key, at);
        }
    }

    pub(super) fn schedule(&mut self, kind: Update, key: EntityKey, at: Duration) {
        self.queue(kind).schedule(key, at);
    }

    pub(super) fn wake_decision(&mut self, key: EntityKey, now: Duration) {
        if self.decision.by_entity.get(&key).is_none_or(|at| *at > now) {
            self.decision.schedule(key, now);
        }
    }

    pub(super) fn remove(&mut self, key: &EntityKey) {
        self.input.remove(key);
        self.decision.remove(key);
    }

    pub(super) fn next_deadline(&self) -> Option<Duration> {
        [&self.input, &self.decision]
            .into_iter()
            .filter_map(|queue| queue.by_time.first().map(|(at, _)| *at))
            .min()
    }

    pub(super) fn take_due(&mut self, kind: Update, now: Duration, limit: usize) -> Vec<EntityKey> {
        let queue = self.queue(kind);
        let mut due = Vec::new();
        while due.len() < limit && queue.by_time.first().is_some_and(|(at, _)| *at <= now) {
            let (_, key) = queue.by_time.pop_first().expect("checked first deadline");
            queue.by_entity.remove(&key);
            due.push(key);
        }
        due
    }

    fn queue(&mut self, kind: Update) -> &mut Queue {
        match kind {
            Update::Input => &mut self.input,
            Update::Decision => &mut self.decision,
        }
    }
}

impl Queue {
    fn schedule(&mut self, key: EntityKey, at: Duration) {
        self.remove(&key);
        self.by_entity.insert(key.clone(), at);
        self.by_time.insert((at, key));
    }

    fn remove(&mut self, key: &EntityKey) {
        if let Some(at) = self.by_entity.remove(key) {
            self.by_time.remove(&(at, key.clone()));
        }
    }
}

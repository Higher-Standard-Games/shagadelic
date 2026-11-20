//! Baseline retention follows SHAGGER's `src/network/client/snapshot_receiver.rs`
//! and SHAG's `src/engine/network/client/SnapshotReceiver.ts`.

use std::collections::VecDeque;

use shagger_shared::net::{
    DecodeError,
    game_state::GameState,
    message_codec::{decode_snapshot, encode_ack, read_snapshot_header},
    snapshot::EventCodec,
};

const HISTORY_SIZE: usize = 128;

pub struct ReceivedSnapshot<E> {
    pub state: GameState<E>,
    pub acknowledgment: Vec<u8>,
}

/// Retain one receiver per connection, including across respawns.
pub struct SnapshotReceiver<C: EventCodec> {
    codec: C,
    history: VecDeque<GameState<C::Event>>,
}

impl<C: EventCodec> SnapshotReceiver<C> {
    pub fn new(codec: C) -> Self {
        Self {
            codec,
            history: VecDeque::new(),
        }
    }

    /// Stale ticks and deltas with missing baselines return no state or ACK.
    pub fn receive(
        &mut self,
        frame: &[u8],
    ) -> Result<Option<ReceivedSnapshot<C::Event>>, DecodeError> {
        let header = read_snapshot_header(frame)?;
        if self
            .history
            .back()
            .is_some_and(|state| state.tick >= header.tick)
        {
            return Ok(None);
        }
        let state = match decode_snapshot(
            frame,
            |tick| self.history.iter().rev().find(|state| state.tick == tick),
            &self.codec,
        ) {
            Ok(state) => state,
            Err(DecodeError::MissingBaseline { .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
        // Baselines need only player and entity fields. Keep one-tick events
        // in the returned state, independent of the retained history.
        if self.history.len() == HISTORY_SIZE {
            self.history.pop_front();
        }
        self.history.push_back(GameState {
            tick: state.tick,
            entities: state.entities.clone(),
            players: state.players.clone(),
            ..GameState::default()
        });
        Ok(Some(ReceivedSnapshot {
            acknowledgment: encode_ack(state.tick),
            state,
        }))
    }
}

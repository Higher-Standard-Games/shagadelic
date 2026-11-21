#![cfg(feature = "washoi")]

use shagadelic::games::washoi::protocol::{SnapshotEvent, SnapshotReceiver, WashoiEvents, observe};
use shagger_shared::net::snapshot::NoEvents;

fn empty_snapshot(tick: u32, baseline: Option<u32>) -> Vec<u8> {
    let mut bytes = vec![0x0b, u8::from(baseline.is_some())];
    bytes.extend_from_slice(&baseline.unwrap_or_default().to_le_bytes());
    bytes.extend_from_slice(&tick.to_le_bytes());
    // Zero timestamp, input sequence, entity count, player count and event count.
    bytes.extend_from_slice(&[0; 18]);
    bytes
}

#[test]
fn washoi_events_survive_snapshot_decoding_in_wire_order() {
    let mut bytes = empty_snapshot(1, None);
    bytes.truncate(bytes.len() - 2);
    bytes.extend_from_slice(b"\x02\x00\x01\x01\x00e\x64\x00\x00\x00\x00\x02\x02\x00{}");
    let snapshot = SnapshotReceiver::new(WashoiEvents)
        .receive(&bytes)
        .unwrap()
        .unwrap();
    assert!(matches!(
        &snapshot.state.events[..],
        [SnapshotEvent::CombatState { entity_id, health: 100, dead: false, .. }, SnapshotEvent::MatchState(_)]
            if entity_id == "e"
    ));
    let observation = observe(&snapshot.state);
    assert_eq!(observation.tick, 1);
    assert_eq!(observation.events, snapshot.state.events);
}

#[test]
fn player_look_is_consumed_before_events_and_retained_by_deltas() {
    let mut receiver = SnapshotReceiver::new(WashoiEvents);
    let mut first = empty_snapshot(1, None);
    first.truncate(24);
    first.extend_from_slice(b"\x01\x00\x01\x00p\x40\x03\x01\x00\x02\x02\x00{}");
    let state = receiver.receive(&first).unwrap().unwrap().state;
    assert_eq!(state.players.get("p").unwrap().look, Some(3));
    assert!(matches!(&state.events[..], [SnapshotEvent::MatchState(_)]));

    let mut delta = empty_snapshot(2, Some(1));
    delta.truncate(24);
    delta.extend_from_slice(b"\x01\x00\x01\x00p\x00\x00\x00");
    let state = receiver.receive(&delta).unwrap().unwrap().state;
    assert_eq!(state.players.get("p").unwrap().look, Some(3));
}

#[test]
fn snapshots_ack_only_reconstructed_new_ticks_and_recover_after_baseline_loss() {
    let mut receiver = SnapshotReceiver::new(NoEvents);
    let first = empty_snapshot(10, None);
    assert_eq!(
        receiver.receive(&first).unwrap().unwrap().acknowledgment,
        b"\x08\x0a\x00\x00\x00",
    );
    assert!(receiver.receive(&first).unwrap().is_none());
    assert!(
        receiver
            .receive(&empty_snapshot(11, Some(9)))
            .unwrap()
            .is_none()
    );
    assert!(
        receiver
            .receive(&empty_snapshot(11, Some(10))[..11])
            .is_err()
    );
    assert_eq!(
        receiver
            .receive(&empty_snapshot(11, Some(10)))
            .unwrap()
            .unwrap()
            .state
            .tick,
        11,
    );
    for tick in 12..=139 {
        assert!(
            receiver
                .receive(&empty_snapshot(tick, None))
                .unwrap()
                .is_some()
        );
    }
    assert!(
        receiver
            .receive(&empty_snapshot(140, Some(11)))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        receiver
            .receive(&empty_snapshot(140, None))
            .unwrap()
            .unwrap()
            .state
            .tick,
        140,
    );
}

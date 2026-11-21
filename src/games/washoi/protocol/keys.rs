//! Washoi view-relative movement keys from engine/network/components/InputComponent.ts.

use std::f64::consts::FRAC_1_SQRT_2;

use super::PlayerInput;
use crate::games::washoi::Controls;

const FORWARD: u16 = 1 << 0;
const BACK: u16 = 1 << 1;
const LEFT: u16 = 1 << 2;
const RIGHT: u16 = 1 << 3;
const MOVEMENT: u16 = FORWARD | BACK | LEFT | RIGHT;
const MIN_HOLD_TICKS: u8 = 3;

#[derive(Clone, Copy)]
struct Chord {
    right: f64,
    forward: f64,
    buttons: u16,
}

const DIAGONAL: f64 = FRAC_1_SQRT_2;
const CHORDS: [Chord; 8] = [
    Chord {
        right: 0.0,
        forward: 1.0,
        buttons: FORWARD,
    },
    Chord {
        right: DIAGONAL,
        forward: DIAGONAL,
        buttons: FORWARD | RIGHT,
    },
    Chord {
        right: 1.0,
        forward: 0.0,
        buttons: RIGHT,
    },
    Chord {
        right: DIAGONAL,
        forward: -DIAGONAL,
        buttons: BACK | RIGHT,
    },
    Chord {
        right: 0.0,
        forward: -1.0,
        buttons: BACK,
    },
    Chord {
        right: -DIAGONAL,
        forward: -DIAGONAL,
        buttons: BACK | LEFT,
    },
    Chord {
        right: -1.0,
        forward: 0.0,
        buttons: LEFT,
    },
    Chord {
        right: -DIAGONAL,
        forward: DIAGONAL,
        buttons: FORWARD | LEFT,
    },
];

/// Tracks the movement keys held by one network client.
#[derive(Default)]
pub(super) struct MovementKeys {
    held: Option<(usize, u8)>,
}

impl MovementKeys {
    /// Encode one brain command. Exact controls go out unchanged and release
    /// any held keys, so they don't leak into the next ordinary command.
    pub(super) fn encode(&mut self, controls: Controls) -> PlayerInput {
        let input = PlayerInput {
            direction: controls.direction,
            pitch: controls.pitch,
            yaw: controls.yaw,
            buttons: controls.buttons,
            weapon_slot: controls.weapon_slot,
        };
        if controls.exact {
            self.held = None;
            return input;
        }
        self.apply(input)
    }

    pub(super) fn apply(&mut self, mut input: PlayerInput) -> PlayerInput {
        let [x, _, z] = input.direction;
        let length = x.hypot(z);
        input.buttons &= !MOVEMENT;
        if !length.is_finite() || length <= 0.001 || !input.yaw.is_finite() {
            self.held = None;
            input.direction = [0.0; 3];
            return input;
        }

        // Same Y-up basis as Washoi's client command encoder. Convert intent
        // into view space to pick keys, then reconstruct world-space wishdir
        // from the keys and the view angle actually sent in this command.
        let (sin, cos) = input.yaw.sin_cos();
        let right = (x * cos - z * sin) / length;
        let forward = (x * sin + z * cos) / length;
        let dot = |index: usize| CHORDS[index].right * right + CHORDS[index].forward * forward;
        let wanted = (1..CHORDS.len()).fold(
            0,
            |best, index| {
                if dot(index) > dot(best) { index } else { best }
            },
        );
        let (index, held_ticks) = match self.held {
            Some((held, ticks))
                if held == wanted || (ticks < MIN_HOLD_TICKS && dot(held) > 0.0) =>
            {
                (held, ticks.saturating_add(1))
            }
            _ => (wanted, 1),
        };
        self.held = Some((index, held_ticks));
        let chord = CHORDS[index];
        input.buttons |= chord.buttons;
        input.direction = [
            chord.right * cos + chord.forward * sin,
            0.0,
            chord.forward * cos - chord.right * sin,
        ];
        input
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(direction: [f64; 3], yaw: f64) -> PlayerInput {
        PlayerInput {
            direction,
            yaw,
            buttons: BACK | (1 << 8),
            ..PlayerInput::default()
        }
    }

    #[test]
    fn forward_and_diagonal_keys_match_the_sent_view_and_keep_action_keys() {
        let mut keys = MovementKeys::default();
        let aimed_right = keys.apply(input([1.0, 0.0, 0.0], std::f64::consts::FRAC_PI_2));
        assert_eq!(aimed_right.buttons, FORWARD | (1 << 8));
        assert!((aimed_right.direction[0] - 1.0).abs() < 1e-12);
        let diagonal = MovementKeys::default().apply(input([1.0, 0.0, 1.0], 0.0));
        assert_eq!(diagonal.direction, [DIAGONAL, 0.0, DIAGONAL]);
        assert_eq!(diagonal.buttons, FORWARD | RIGHT | (1 << 8));
    }

    #[test]
    fn holds_short_turns_but_releases_on_stops_or_reversals() {
        let mut keys = MovementKeys::default();
        let forward = input([0.0, 0.0, 1.0], 0.0);
        let diagonal = input([1.0, 0.0, 1.0], 0.0);
        assert_eq!(keys.apply(forward).buttons & MOVEMENT, FORWARD);
        assert_eq!(keys.apply(diagonal).buttons & MOVEMENT, FORWARD);
        assert_eq!(keys.apply(diagonal).buttons & MOVEMENT, FORWARD);
        assert_eq!(keys.apply(diagonal).buttons & MOVEMENT, FORWARD | RIGHT);
        assert_eq!(keys.apply(input([0.0; 3], 0.0)).buttons, 1 << 8);
        assert_eq!(keys.apply(forward).buttons & MOVEMENT, FORWARD);
        assert_eq!(
            keys.apply(input([0.0, 0.0, -1.0], 0.0)).buttons & MOVEMENT,
            BACK
        );
    }

    #[test]
    fn exact_controls_pass_through_and_release_held_keys() {
        let mut keys = MovementKeys::default();
        let ordinary = |direction| Controls {
            direction,
            buttons: 1 << 8,
            weapon_slot: 2,
            ..Controls::default()
        };
        let forward = ordinary([0.0, 0.0, 1.0]);
        let diagonal = ordinary([1.0, 0.0, 1.0]);
        assert_eq!(keys.encode(forward).buttons & MOVEMENT, FORWARD);
        let exact = Controls {
            direction: [0.3, 0.0, 0.4],
            yaw: 1.0,
            pitch: -0.2,
            buttons: BACK | (1 << 8),
            exact: true,
            ..Controls::default()
        };
        let sent = keys.encode(exact);
        assert_eq!(sent.direction, exact.direction);
        assert_eq!(
            (sent.yaw, sent.pitch, sent.buttons),
            (1.0, -0.2, BACK | (1 << 8))
        );
        // The forward key no longer holds through a short turn.
        let after = keys.encode(diagonal);
        assert_eq!(after.buttons, FORWARD | RIGHT | (1 << 8));
        assert_eq!(after.weapon_slot, 2);
    }

    #[tokio::test]
    async fn the_bot_connection_sends_the_held_forward_key() {
        use std::time::Duration;

        use futures_util::{SinkExt, StreamExt};
        use shagger_shared::net::message_codec::decode_command;
        use tokio::net::TcpListener;
        use tokio_tungstenite::{accept_async, tungstenite::Message};

        use crate::games::washoi::{
            BotPlayer,
            protocol::{WashoiEvents, connect},
        };

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let server = async {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            assert_eq!(socket.next().await.unwrap().unwrap().into_data()[0], 0x81);
            socket
                .send(Message::binary(&b"\x02\x01\x00c\x01\x00p\x00\x00"[..]))
                .await
                .unwrap();
            assert_eq!(socket.next().await.unwrap().unwrap().into_data()[0], 0x86);
            socket
                .send(Message::binary(&b"\x02\x01\x00c\x01\x00p\x01\x00e"[..]))
                .await
                .unwrap();

            let mut snapshot = vec![0x0b, 0, 0, 0, 0, 0, 1, 0, 0, 0];
            snapshot.extend_from_slice(&[0; 12]); // Timestamp and input acknowledgment.
            snapshot.extend_from_slice(b"\x01\x00\x01\x00e\x01"); // Entity e has a position.
            snapshot.extend_from_slice(&[0; 16]); // Position, player count, event count.
            socket.send(Message::binary(snapshot)).await.unwrap();

            assert_eq!(socket.next().await.unwrap().unwrap().into_data()[0], 0x08);
            let command = socket.next().await.unwrap().unwrap().into_data();
            let (player, _, input) = decode_command(&command).unwrap();
            assert_eq!(player, "p");
            assert!((input.direction[0] - input.yaw.sin()).abs() < 1e-6);
            assert!((input.direction[2] - input.yaw.cos()).abs() < 1e-6);
            assert_eq!(input.buttons & MOVEMENT, FORWARD);
            socket.close(None).await.unwrap();
        };
        let client = async {
            let mut connection = connect(
                &endpoint,
                "key-test".into(),
                None,
                None,
                Duration::from_secs(2),
                WashoiEvents,
                |_| {},
            )
            .await
            .unwrap();
            connection
                .play(BotPlayer::new("e".into()), |_| true)
                .await
                .unwrap();
        };
        tokio::time::timeout(Duration::from_secs(4), async {
            tokio::join!(server, client)
        })
        .await
        .unwrap();
    }
}

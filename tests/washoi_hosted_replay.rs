#![cfg(feature = "washoi")]

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use shagadelic::{
    games::washoi::{BotReport, Difficulty, LearnedRoutes, SpawnBot, WashoiSession},
    runtime::{Budget, Progress, Runtime},
};
use shagger_shared::net::{
    binary_protocol::BinaryEncoder,
    message_codec::{
        JoinAccept, MessageType, Welcome, decode_command, encode_ack, encode_join_accept,
        encode_welcome,
    },
};
use tokio::{net::TcpListener, time::timeout};
use tokio_tungstenite::{WebSocketStream, accept_async, tungstenite::Message};

type Socket = WebSocketStream<tokio::net::TcpStream>;

#[derive(Deserialize)]
struct Trace {
    entity_id: String,
    learned_routes: LearnedRoutes,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
struct Step {
    #[serde(default)]
    inputs: Vec<ExpectedInput>,
}

#[derive(Debug, Deserialize, PartialEq)]
struct ExpectedInput([f64; 3], f64, f64, u16, u8);

fn advance(
    session: &mut Runtime<WashoiSession>,
    ms: u64,
    inputs: usize,
    decisions: usize,
) -> Progress {
    session
        .advance(
            Duration::from_millis(ms),
            Budget {
                events: 64,
                inputs,
                decisions,
            },
        )
        .unwrap()
}

async fn receive(socket: &mut Socket) -> Vec<u8> {
    socket.next().await.unwrap().unwrap().into_data().to_vec()
}

async fn admit(socket: &mut Socket, entity: &str) {
    assert_eq!(receive(socket).await[0], MessageType::Join as u8);
    let welcome = Welcome {
        server_id: "local".into(),
        server_name: "Replay".into(),
        map: "replay".into(),
        tick_rate: 60,
        max_players: 2,
        current_players: 1,
    };
    socket
        .send(Message::binary(encode_welcome(&welcome)))
        .await
        .unwrap();
    let mut accept = JoinAccept {
        client_id: "client".into(),
        player_id: "player".into(),
        ..Default::default()
    };
    socket
        .send(Message::binary(encode_join_accept(&accept)))
        .await
        .unwrap();
    assert_eq!(receive(socket).await[0], MessageType::JoinGame as u8);
    accept.entity_id = entity.into();
    socket
        .send(Message::binary(encode_join_accept(&accept)))
        .await
        .unwrap();
}

// Full Washoi snapshot with an entity at the grounded takeoff pose and an optional
// one-tick combat-state event. Layout follows SHAG src/shared/net/BinaryProtocol.ts.
async fn send_snapshot(socket: &mut Socket, entity: &str, tick: u32, death: bool) {
    let mut out = BinaryEncoder::new();
    out.write_uint8(MessageType::Snapshot as u8);
    out.write_uint8(0);
    out.write_uint32(0);
    out.write_uint32(tick);
    out.write_float64(0.0);
    out.write_uint32(0);
    out.write_uint16(1);
    out.write_string(entity);
    out.write_uint8(0x05); // Position and grounded state.
    for _ in 0..3 {
        out.write_float32(0.0);
    }
    out.write_bool(true);
    out.write_uint16(0); // Players.
    out.write_uint16(u16::from(death));
    if death {
        out.write_uint8(1);
        out.write_string(entity);
        out.write_uint16(0);
        out.write_uint16(0);
        out.write_bool(true);
    }
    socket.send(Message::binary(out.into_data())).await.unwrap();
    assert_eq!(receive(socket).await, encode_ack(tick));
}

#[tokio::test]
async fn hosted_brain_obeys_decision_budget_and_replays_the_golden_actions() {
    timeout(Duration::from_secs(5), async {
        let trace: Trace =
            serde_json::from_str(include_str!("fixtures/washoi_decision_replay.json")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let mut session =
            Runtime::new(WashoiSession::new(endpoint).with_learned_routes(trace.learned_routes));
        session
            .spawn(
                "golden".into(),
                SpawnBot {
                    name: "golden".into(),
                    team: None,
                    ticket: None,
                    difficulty: Difficulty::default(),
                },
            )
            .unwrap();
        let (tcp, _) = listener.accept().await.unwrap();
        tcp.set_nodelay(true).unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        admit(&mut socket, &trace.entity_id).await;
        loop {
            advance(&mut session, 0, 0, 0);
            if let Some(report) = session.session_mut().take_report() {
                assert!(matches!(report, BotReport::Admitted { .. }));
                break;
            }
            tokio::task::yield_now().await;
        }
        send_snapshot(&mut socket, &trace.entity_id, 10, false).await;
        assert_eq!(advance(&mut session, 0, 1, 0).decisions, 0);
        assert!(
            timeout(Duration::from_millis(20), socket.next())
                .await
                .is_err(),
            "input issued before first budgeted decision"
        );
        assert_eq!(advance(&mut session, 0, 0, 1).decisions, 1);
        advance(&mut session, 16, 1, 0);
        let expected = trace.steps.into_iter().flat_map(|step| step.inputs);
        for (i, expected) in expected.enumerate() {
            let bytes = receive(&mut socket).await;
            let (player, sequence, cmd) = decode_command(&bytes).unwrap();
            assert_eq!(
                (player.as_str(), sequence, cmd.tick),
                ("player", i as u32, 10)
            );
            assert_eq!(
                ExpectedInput(
                    cmd.direction,
                    cmd.yaw,
                    cmd.pitch,
                    cmd.buttons,
                    cmd.weapon_slot
                ),
                expected
            );
        }

        send_snapshot(&mut socket, &trace.entity_id, 11, true).await;
        send_snapshot(&mut socket, &trace.entity_id, 12, false).await;
        advance(&mut session, 24, 0, 0);
        assert_eq!(advance(&mut session, 24, 0, 1).decisions, 1);
        assert_eq!(advance(&mut session, 32, 0, 1).decisions, 1);
        send_snapshot(&mut socket, &trace.entity_id, 160, false).await;
        advance(&mut session, 2523, 0, 1);
        advance(&mut session, 2524, 1, 0);
        assert_eq!(
            receive(&mut socket).await,
            [MessageType::RequestSpawn as u8]
        );
        session.shutdown();
    })
    .await
    .expect("hosted replay timed out");
}

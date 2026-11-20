#![cfg(feature = "washoi")]

use shagadelic::games::washoi::protocol::{Admission, AdmissionEvent};

// MessageCodec.ts uses one message byte and u16 length-prefixed strings.
const SPECTATOR_ACCEPT: &[u8] = b"\x02\x01\x00c\x01\x00p\x00\x00";
const PLAYER_ACCEPT: &[u8] = b"\x02\x01\x00c\x01\x00p\x01\x00e";

#[test]
fn a_bot_is_admitted_only_after_the_playing_accept() {
    let (mut admission, join) = Admission::start("bot".into(), Some("ticket".into()));
    assert_eq!(join, b"\x81\x03\x00bot\x01\x06\x00ticket");
    let Some(AdmissionEvent::Send(join_game)) =
        admission.receive(SPECTATOR_ACCEPT, Some("red")).unwrap()
    else {
        panic!("the spectator accept should send JoinGame");
    };
    assert_eq!(join_game, b"\x86\x01\x03\x00red");

    let Some(AdmissionEvent::Admitted(playing)) = admission.receive(PLAYER_ACCEPT, None).unwrap()
    else {
        panic!("the playing accept should admit the bot");
    };
    assert_eq!(playing.player_id, "p");
    assert_eq!(playing.entity_id, "e");
    assert!(admission.receive(PLAYER_ACCEPT, None).unwrap().is_none());
}

#[test]
fn rejection_ends_the_attempt_and_invalid_accept_does_not_advance_it() {
    let (mut admission, _) = Admission::start("bot".into(), None);
    assert!(admission.receive(b"\x02", None).is_err());
    assert!(matches!(
        admission.receive(SPECTATOR_ACCEPT, None).unwrap(),
        Some(AdmissionEvent::Send(_))
    ));
    assert!(matches!(
        admission.receive(b"\x03", None).unwrap(),
        Some(AdmissionEvent::Rejected)
    ));
    assert!(admission.receive(PLAYER_ACCEPT, None).unwrap().is_none());
}

#[tokio::test]
async fn websocket_admission_handles_success_and_close_between_accepts() {
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use shagadelic::games::washoi::protocol::{AdmissionError, connect};
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    let deadline = Duration::from_secs(5);
    for finish in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            let join = socket.next().await.unwrap().unwrap().into_data();
            assert_eq!(&join[..], b"\x81\x03\x00bot\x01\x06\x00ticket");
            socket
                .send(Message::binary(SPECTATOR_ACCEPT))
                .await
                .unwrap();
            let join_game = socket.next().await.unwrap().unwrap().into_data();
            assert_eq!(&join_game[..], b"\x86\x00");
            if finish {
                socket.send(Message::binary(PLAYER_ACCEPT)).await.unwrap();
            } else {
                socket.close(None).await.unwrap();
            }
        });
        let result = connect(
            &endpoint,
            "bot".into(),
            Some("ticket".into()),
            None,
            deadline,
        )
        .await;
        if finish {
            assert_eq!(result.unwrap().identity.entity_id, "e");
        } else {
            assert!(matches!(result, Err(AdmissionError::Disconnected)));
        }
        tokio::time::timeout(deadline, server)
            .await
            .unwrap()
            .unwrap();
    }
}

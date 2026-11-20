use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use shagger_shared::net::{
    DecodeError,
    game_state::GameState,
    message_codec::{JoinAccept, MessageType, Welcome, peek_message_type},
    snapshot::EventCodec,
};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Error as WebSocketError, Message},
};

use super::{Admission, AdmissionEvent, SnapshotReceiver};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct Connection<C: EventCodec> {
    pub identity: JoinAccept,
    pub welcome: Option<Welcome>,
    socket: Socket,
    snapshots: SnapshotReceiver<C>,
}

#[derive(Debug)]
pub enum AdmissionError {
    Transport(Box<WebSocketError>),
    Protocol(DecodeError),
    Rejected,
    Disconnected,
    TimedOut,
}

impl From<WebSocketError> for AdmissionError {
    fn from(error: WebSocketError) -> Self {
        Self::Transport(Box::new(error))
    }
}

impl<C: EventCodec> Connection<C> {
    /// Read the next reconstructed world state and ACK it on this connection.
    pub async fn next_snapshot(&mut self) -> Result<GameState<C::Event>, AdmissionError> {
        loop {
            let frame = next_binary(&mut self.socket).await?;
            if let Some(state) = acknowledge(&mut self.socket, &mut self.snapshots, &frame).await? {
                return Ok(state);
            }
        }
    }
}

/// Connect to a Washoi game server over WebSocket and complete both join stages.
/// Dropping the future cancels the attempt and drops its socket.
pub async fn connect<C: EventCodec>(
    endpoint: &str,
    name: String,
    ticket: Option<String>,
    team: Option<&str>,
    timeout: Duration,
    codec: C,
    on_snapshot: impl FnMut(GameState<C::Event>),
) -> Result<Connection<C>, AdmissionError> {
    tokio::time::timeout(
        timeout,
        admit(endpoint, name, ticket, team, codec, on_snapshot),
    )
    .await
    .map_err(|_| AdmissionError::TimedOut)?
}

async fn admit<C: EventCodec>(
    endpoint: &str,
    name: String,
    ticket: Option<String>,
    team: Option<&str>,
    codec: C,
    mut on_snapshot: impl FnMut(GameState<C::Event>),
) -> Result<Connection<C>, AdmissionError> {
    let (mut socket, _) = connect_async(endpoint).await?;
    let (mut admission, join) = Admission::start(name, ticket);
    socket.send(Message::binary(join)).await?;
    let mut welcome = None;
    let mut snapshots = SnapshotReceiver::new(codec);

    loop {
        let frame = next_binary(&mut socket).await?;
        match admission
            .receive(&frame, team)
            .map_err(AdmissionError::Protocol)?
        {
            Some(AdmissionEvent::Send(bytes)) => socket.send(Message::binary(bytes)).await?,
            Some(AdmissionEvent::Welcome(info)) => welcome = Some(info),
            Some(AdmissionEvent::Admitted(identity)) => {
                return Ok(Connection {
                    identity,
                    welcome,
                    socket,
                    snapshots,
                });
            }
            Some(AdmissionEvent::Rejected) => return Err(AdmissionError::Rejected),
            None => {
                if let Some(state) = acknowledge(&mut socket, &mut snapshots, &frame).await? {
                    on_snapshot(state);
                }
            }
        }
    }
}

async fn next_binary(
    socket: &mut Socket,
) -> Result<tokio_tungstenite::tungstenite::Bytes, AdmissionError> {
    loop {
        match socket.next().await.transpose()? {
            Some(Message::Binary(frame)) => return Ok(frame),
            Some(Message::Ping(_)) => socket.flush().await?,
            Some(Message::Close(_)) | None => return Err(AdmissionError::Disconnected),
            Some(_) => {}
        }
    }
}

async fn acknowledge<C: EventCodec>(
    socket: &mut Socket,
    receiver: &mut SnapshotReceiver<C>,
    frame: &[u8],
) -> Result<Option<GameState<C::Event>>, AdmissionError> {
    match peek_message_type(frame) {
        Some(kind)
            if kind == MessageType::Snapshot as u8 || kind == MessageType::LegacySnapshot as u8 => {
        }
        _ => return Ok(None),
    }
    let Some(snapshot) = receiver.receive(frame).map_err(AdmissionError::Protocol)? else {
        return Ok(None);
    };
    socket
        .send(Message::binary(snapshot.acknowledgment))
        .await?;
    Ok(Some(snapshot.state))
}

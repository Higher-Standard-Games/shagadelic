//! The Washoi connection, independent of the agent kind that speaks over it:
//! WebSocket frames, pings and close, snapshot reconstruction with ACKs, and
//! command sequence numbers and ticks. What an agent sends and how it joins
//! belong to its `WireAgent` codec. The player's is `player::PlayerWire`.

use futures_util::{SinkExt, StreamExt};
use shagger_shared::net::{
    DecodeError,
    command::Command,
    game_state::GameState,
    message_codec::{MessageType, encode_command, peek_message_type},
    snapshot::EventCodec,
};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Bytes, Error as WebSocketError, Message},
};

use super::SnapshotReceiver;
use crate::games::washoi::{Observation, hosting::net::SnapshotEvent};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug)]
pub enum AdmissionError {
    Transport(Box<WebSocketError>),
    Protocol(DecodeError),
    Rejected,
    Disconnected,
    TimedOut,
    AwaitingSnapshot,
}

impl From<WebSocketError> for AdmissionError {
    fn from(error: WebSocketError) -> Self {
        Self::Transport(Box::new(error))
    }
}

/// One step of an agent's handshake with the server.
pub(crate) enum Admit<I> {
    /// Send this frame and keep waiting.
    Send(Vec<u8>),
    Admitted(I),
    Rejected,
    /// Not a handshake frame. The connection treats it as a snapshot.
    Pass,
}

/// One frame an agent's action becomes. The connection fills in a command's
/// tick and sequence number.
pub(crate) enum Wire {
    Command { sender: String, command: Command },
    Frame(Vec<u8>),
}

/// What one agent kind speaks over a Washoi connection. SHAG defines the
/// messages. The agent kind chooses which server interface it speaks.
pub(crate) trait WireAgent {
    /// Who the server admitted, for example the player and entity IDs.
    type Identity;
    /// What the agent sends per input opportunity.
    type Action;

    /// The first handshake frame.
    fn join(&mut self) -> Vec<u8>;
    /// How one server frame advances the handshake.
    fn admit(&mut self, frame: &[u8]) -> Result<Admit<Self::Identity>, DecodeError>;
    /// One reconstructed snapshot, as this agent observes it.
    fn observe(&self, identity: &Self::Identity, state: &GameState<SnapshotEvent>) -> Observation;
    /// One action as wire frames, in order.
    fn encode(&mut self, identity: &Self::Identity, action: Self::Action, out: &mut Vec<Wire>);
}

/// The socket and its snapshot history. Retain one per connection, including
/// across respawns.
pub(crate) struct Link<C: EventCodec> {
    socket: Socket,
    snapshots: SnapshotReceiver<C>,
    next_sequence: u32,
}

impl<C: EventCodec> Link<C> {
    /// Open the socket and run `wire`'s handshake. Snapshots that arrive
    /// before admission are acknowledged and handed to `on_snapshot`.
    pub(crate) async fn admit<W: WireAgent>(
        endpoint: &str,
        wire: &mut W,
        codec: C,
        mut on_snapshot: impl FnMut(GameState<C::Event>),
    ) -> Result<(Self, W::Identity), AdmissionError> {
        let (socket, _) = connect_async(endpoint).await?;
        let mut link = Self {
            socket,
            snapshots: SnapshotReceiver::new(codec),
            next_sequence: 0,
        };
        link.send_frame(wire.join()).await?;
        loop {
            let frame = link.next_binary().await?;
            match wire.admit(&frame).map_err(AdmissionError::Protocol)? {
                Admit::Send(bytes) => link.send_frame(bytes).await?,
                Admit::Admitted(identity) => return Ok((link, identity)),
                Admit::Rejected => return Err(AdmissionError::Rejected),
                Admit::Pass => {
                    if let Some(state) = link.acknowledge(&frame).await? {
                        on_snapshot(state);
                    }
                }
            }
        }
    }

    /// Send a command with the last accepted snapshot tick and the next
    /// sequence number of this connection.
    pub(crate) async fn send_command(
        &mut self,
        sender: &str,
        mut command: Command,
    ) -> Result<(), AdmissionError> {
        command.tick = self
            .snapshots
            .latest_tick()
            .ok_or(AdmissionError::AwaitingSnapshot)?;
        let bytes = encode_command(sender, self.next_sequence, &command);
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.send_frame(bytes).await
    }

    pub(crate) async fn send_frame(&mut self, bytes: Vec<u8>) -> Result<(), AdmissionError> {
        self.socket.send(Message::binary(bytes)).await?;
        Ok(())
    }

    pub(crate) async fn send(&mut self, frames: Vec<Wire>) -> Result<(), AdmissionError> {
        for frame in frames {
            match frame {
                Wire::Command { sender, command } => self.send_command(&sender, command).await?,
                Wire::Frame(bytes) => self.send_frame(bytes).await?,
            }
        }
        Ok(())
    }

    /// Read the next reconstructed world state and ACK it.
    pub(crate) async fn next_snapshot(&mut self) -> Result<GameState<C::Event>, AdmissionError> {
        loop {
            let frame = self.next_binary().await?;
            if let Some(state) = self.acknowledge(&frame).await? {
                return Ok(state);
            }
        }
    }

    /// The next socket message, for a caller that also waits on other work.
    pub(crate) async fn next_message(&mut self) -> Option<Result<Message, WebSocketError>> {
        self.socket.next().await
    }

    /// Handle one socket message: reconstruct and ACK a snapshot, answer a
    /// ping, or report the close.
    pub(crate) async fn receive(
        &mut self,
        message: Option<Result<Message, WebSocketError>>,
    ) -> Result<Option<GameState<C::Event>>, AdmissionError> {
        match message.transpose()? {
            Some(Message::Binary(frame)) => self.acknowledge(&frame).await,
            Some(Message::Ping(_)) => {
                self.socket.flush().await?;
                Ok(None)
            }
            Some(Message::Close(_)) | None => Err(AdmissionError::Disconnected),
            Some(_) => Ok(None),
        }
    }

    async fn next_binary(&mut self) -> Result<Bytes, AdmissionError> {
        loop {
            match self.socket.next().await.transpose()? {
                Some(Message::Binary(frame)) => return Ok(frame),
                Some(Message::Ping(_)) => self.socket.flush().await?,
                Some(Message::Close(_)) | None => return Err(AdmissionError::Disconnected),
                Some(_) => {}
            }
        }
    }

    async fn acknowledge(
        &mut self,
        frame: &[u8],
    ) -> Result<Option<GameState<C::Event>>, AdmissionError> {
        match peek_message_type(frame) {
            Some(kind)
                if kind == MessageType::Snapshot as u8
                    || kind == MessageType::LegacySnapshot as u8 => {}
            _ => return Ok(None),
        }
        let Some(snapshot) = self
            .snapshots
            .receive(frame)
            .map_err(AdmissionError::Protocol)?
        else {
            return Ok(None);
        };
        self.send_frame(snapshot.acknowledgment).await?;
        Ok(Some(snapshot.state))
    }
}

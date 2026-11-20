use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use shagger_shared::net::{
    DecodeError,
    message_codec::{JoinAccept, Welcome},
};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Error as WebSocketError, Message},
};

use super::{Admission, AdmissionEvent};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct Connection {
    pub identity: JoinAccept,
    pub welcome: Option<Welcome>,
    socket: Socket,
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

impl Connection {
    /// Drive gameplay frames from the same socket used for admission.
    pub fn socket_mut(&mut self) -> &mut Socket {
        &mut self.socket
    }
}

/// Connect to a Washoi game server over WebSocket and complete both join stages.
/// Dropping the future cancels the attempt and drops its socket.
pub async fn connect(
    endpoint: &str,
    name: String,
    ticket: Option<String>,
    team: Option<&str>,
    timeout: Duration,
) -> Result<Connection, AdmissionError> {
    tokio::time::timeout(timeout, admit(endpoint, name, ticket, team))
        .await
        .map_err(|_| AdmissionError::TimedOut)?
}

async fn admit(
    endpoint: &str,
    name: String,
    ticket: Option<String>,
    team: Option<&str>,
) -> Result<Connection, AdmissionError> {
    let (mut socket, _) = connect_async(endpoint).await?;
    let (mut admission, join) = Admission::start(name, ticket);
    socket.send(Message::binary(join)).await?;
    let mut welcome = None;

    loop {
        match socket.next().await.transpose()? {
            Some(Message::Binary(frame)) => {
                match admission
                    .receive(&frame, team)
                    .map_err(AdmissionError::Protocol)?
                {
                    Some(AdmissionEvent::Send(bytes)) => {
                        socket.send(Message::binary(bytes)).await?;
                    }
                    Some(AdmissionEvent::Welcome(info)) => welcome = Some(info),
                    Some(AdmissionEvent::Admitted(identity)) => {
                        return Ok(Connection {
                            identity,
                            welcome,
                            socket,
                        });
                    }
                    Some(AdmissionEvent::Rejected) => return Err(AdmissionError::Rejected),
                    None => {}
                }
            }
            Some(Message::Ping(_)) => socket.flush().await?,
            Some(Message::Close(_)) | None => return Err(AdmissionError::Disconnected),
            Some(_) => {}
        }
    }
}

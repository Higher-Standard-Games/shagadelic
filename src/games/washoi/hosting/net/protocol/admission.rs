//! Admission follows Washoi's `src/games/fps/bot/BotClient.ts`.

use shagger_shared::net::{
    DecodeError,
    message_codec::{
        Join, JoinAccept, JoinGame, MessageType, Welcome, decode_join_accept, decode_welcome,
        encode_join, encode_join_game, peek_message_type,
    },
};

#[derive(Debug, PartialEq, Eq)]
enum State {
    Connecting,
    Joining,
    Finished,
}

/// One WebSocket admission attempt. The caller owns the socket and timeout.
pub struct Admission {
    state: State,
}

pub enum AdmissionEvent {
    Welcome(Welcome),
    Send(Vec<u8>),
    Admitted(JoinAccept),
    Rejected,
}

impl Admission {
    /// Construct the first packet to send as soon as the WebSocket opens.
    pub fn start(name: String, ticket: Option<String>) -> (Self, Vec<u8>) {
        let join = encode_join(&Join {
            player_name: name,
            is_bot: true,
            admission_ticket: ticket,
        });
        (
            Self {
                state: State::Connecting,
            },
            join,
        )
    }

    /// Unhandled frames can be routed to the snapshot receiver or gameplay handler.
    /// For automatic team choice, supply the current smaller team when known.
    pub fn receive(
        &mut self,
        frame: &[u8],
        team: Option<&str>,
    ) -> Result<Option<AdmissionEvent>, DecodeError> {
        if self.state == State::Finished {
            return Ok(None);
        }
        let kind = peek_message_type(frame);
        let event = match kind {
            Some(kind) if kind == MessageType::Welcome as u8 => {
                AdmissionEvent::Welcome(decode_welcome(frame)?)
            }
            Some(kind) if kind == MessageType::JoinReject as u8 => {
                self.state = State::Finished;
                AdmissionEvent::Rejected
            }
            Some(kind) if kind == MessageType::JoinAccept as u8 => {
                let accept = decode_join_accept(frame)?;
                if self.state == State::Connecting {
                    self.state = State::Joining;
                    AdmissionEvent::Send(encode_join_game(&JoinGame {
                        team_id: team.map(str::to_owned),
                        ..JoinGame::default()
                    }))
                } else {
                    self.state = State::Finished;
                    AdmissionEvent::Admitted(accept)
                }
            }
            _ => return Ok(None),
        };
        Ok(Some(event))
    }
}

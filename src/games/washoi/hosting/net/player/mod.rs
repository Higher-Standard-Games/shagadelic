//! The player's wire codec: Washoi player admission (Join, Welcome,
//! JoinAccept, JoinGame, team, seat ticket), the snapshot to `Observation`
//! mapping, and `Controls` through virtual movement keys into commands. The
//! generic `Link` carries it.

mod admission;
mod keys;
mod observation;

use std::time::Duration;

use shagger_shared::net::{
    DecodeError,
    command::Command,
    game_state::GameState,
    message_codec::{JoinAccept, Welcome, encode_chat, encode_request_spawn},
    snapshot::EventCodec,
};
use tokio_tungstenite::tungstenite::Message;

pub use admission::{Admission, AdmissionEvent};
pub use observation::observe;

use super::{
    SnapshotEvent, WashoiEvents,
    connection::{AdmissionError, Admit, Link, Wire, WireAgent},
};
use crate::games::washoi::{Controls, Observation, SimTime, agents::NetworkedPlayer};

/// One Washoi input in world coordinates, produced by the game integration.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayerInput {
    pub direction: [f64; 3],
    pub pitch: f64,
    pub yaw: f64,
    pub buttons: u16,
    pub weapon_slot: u8,
}

impl PlayerInput {
    /// The command for this input. The link fills in its tick.
    fn command(self) -> Command {
        Command {
            direction: self.direction,
            pitch: self.pitch,
            yaw: self.yaw,
            buttons: self.buttons,
            tick: 0,
            weapon_slot: self.weapon_slot,
        }
    }
}

/// `NetworkGameHost.sendChat` trims a line to this many characters, plus
/// the `/say_team ` prefix.
const CHAT_LIMIT: usize = 138;

/// One input opportunity: an optional respawn request, then the paced
/// commands in order.
pub(crate) struct InputBatch {
    controls: Vec<Controls>,
    respawn: bool,
    chat: Vec<String>,
}

impl InputBatch {
    pub(crate) fn from_player(bot: &mut dyn NetworkedPlayer, now: SimTime) -> Self {
        let respawn = bot.request_respawn_at(now);
        let controls = bot.inputs_at(now);
        let chat = bot.chat_at(now);
        Self {
            controls,
            respawn,
            chat,
        }
    }
}

/// What a bot player speaks: its admission, its view of a snapshot, and its
/// held movement keys.
pub(crate) struct PlayerWire {
    name: String,
    ticket: Option<String>,
    team: Option<String>,
    admission: Option<Admission>,
    welcome: Option<Welcome>,
    keys: keys::MovementKeys,
}

impl PlayerWire {
    fn new(name: String, ticket: Option<String>, team: Option<&str>) -> Self {
        Self {
            name,
            ticket,
            team: team.map(str::to_owned),
            admission: None,
            welcome: None,
            keys: keys::MovementKeys::default(),
        }
    }
}

impl WireAgent for PlayerWire {
    type Identity = JoinAccept;
    type Action = InputBatch;

    fn join(&mut self) -> Vec<u8> {
        let (admission, join) = Admission::start(self.name.clone(), self.ticket.take());
        self.admission = Some(admission);
        join
    }

    fn admit(&mut self, frame: &[u8]) -> Result<Admit<JoinAccept>, DecodeError> {
        let Some(admission) = &mut self.admission else {
            return Ok(Admit::Pass);
        };
        Ok(match admission.receive(frame, self.team.as_deref())? {
            Some(AdmissionEvent::Send(bytes)) => Admit::Send(bytes),
            Some(AdmissionEvent::Welcome(welcome)) => {
                self.welcome = Some(welcome);
                Admit::Pass
            }
            Some(AdmissionEvent::Admitted(identity)) => Admit::Admitted(identity),
            Some(AdmissionEvent::Rejected) => Admit::Rejected,
            None => Admit::Pass,
        })
    }

    fn observe(&self, _identity: &JoinAccept, state: &GameState<SnapshotEvent>) -> Observation {
        observe(state)
    }

    fn encode(&mut self, identity: &JoinAccept, batch: InputBatch, out: &mut Vec<Wire>) {
        if batch.respawn {
            out.push(Wire::Frame(encode_request_spawn()));
        }
        // The server caps a line at 128 characters.
        for line in batch.chat {
            let line: String = line.chars().take(CHAT_LIMIT).collect();
            out.push(Wire::Frame(encode_chat(&line)));
        }
        for controls in batch.controls {
            out.push(Wire::Command {
                sender: identity.player_id.clone(),
                command: self.keys.encode(controls).command(),
            });
        }
    }
}

/// A bot player's connection to a Washoi game server.
pub struct Connection<C: EventCodec> {
    pub identity: JoinAccept,
    pub welcome: Option<Welcome>,
    link: Link<C>,
    wire: PlayerWire,
}

impl<C: EventCodec> Connection<C> {
    /// Send input using the last accepted snapshot tick and connection sequence.
    pub async fn send_input(&mut self, input: PlayerInput) -> Result<(), AdmissionError> {
        self.link
            .send_command(&self.identity.player_id, input.command())
            .await
    }

    /// Read the next reconstructed world state and ACK it on this connection.
    pub async fn next_snapshot(&mut self) -> Result<GameState<C::Event>, AdmissionError> {
        self.link.next_snapshot().await
    }
}

impl Connection<WashoiEvents> {
    /// Drive the pilot behavior. Return false from the observer to stop.
    pub async fn play(
        &mut self,
        bot: impl NetworkedPlayer,
        observe: impl FnMut(&GameState<SnapshotEvent>) -> bool,
    ) -> Result<(), AdmissionError> {
        use crate::games::washoi::observation::INPUT_PERIOD;
        use tokio::time::{MissedTickBehavior, interval};

        let mut updates = interval(INPUT_PERIOD);
        updates.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut bot = bot;
        let mut observe = observe;
        // The standalone pilot keeps its own simulation clock from the start of play.
        let start = tokio::time::Instant::now();
        let now = || SimTime::from_elapsed(start.elapsed());
        loop {
            tokio::select! {
                message = self.link.next_message() => {
                    if matches!(message, None | Some(Ok(Message::Close(_)))) { return Ok(()); }
                    if let Some(state) = self.link.receive(message).await? {
                        {
                            let now = now();
                            bot.observe_at(self.wire.observe(&self.identity, &state), now);
                            bot.decide(now);
                        }
                        if !observe(&state) {
                            return Ok(());
                        }
                    }
                }
                _ = updates.tick() => {
                    let actions = InputBatch::from_player(&mut bot, now());
                    self.send_batch(actions).await?;
                }
            }
        }
    }

    /// The hosted socket owns the codec, acknowledgments, and wire sequence.
    /// Brain updates and command production run on the session scheduler.
    pub(crate) async fn exchange<F: Future<Output = bool>>(
        &mut self,
        mut actions: tokio::sync::mpsc::Receiver<InputBatch>,
        mut deliver: impl FnMut(GameState<SnapshotEvent>) -> F,
    ) -> Result<(), AdmissionError> {
        loop {
            tokio::select! {
                message = self.link.next_message() => {
                    if let Some(state) = self.link.receive(message).await?
                        && !deliver(state).await {
                        return Ok(());
                    }
                }
                batch = actions.recv() => {
                    let Some(batch) = batch else { return Ok(()) };
                    self.send_batch(batch).await?;
                }
            }
        }
    }

    async fn send_batch(&mut self, batch: InputBatch) -> Result<(), AdmissionError> {
        let mut frames = Vec::new();
        self.wire.encode(&self.identity, batch, &mut frames);
        self.link.send(frames).await
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
    let mut wire = PlayerWire::new(name, ticket, team);
    let (link, identity) = tokio::time::timeout(
        timeout,
        Link::admit(endpoint, &mut wire, codec, on_snapshot),
    )
    .await
    .map_err(|_| AdmissionError::TimedOut)??;
    Ok(Connection {
        identity,
        welcome: wire.welcome.take(),
        link,
        wire,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use shagger_shared::net::message_codec::decode_chat;

    #[test]
    fn sends_chat_lines_as_chat_messages_before_the_commands() {
        let mut wire = PlayerWire::new("Ramen".into(), None, None);
        let identity = JoinAccept {
            player_id: "p1".into(),
            entity_id: "player_1".into(),
            ..JoinAccept::default()
        };
        let long = "x".repeat(200);
        let batch = InputBatch {
            controls: vec![Controls::default()],
            respawn: false,
            chat: vec!["/say_team Enemy at Warehouse A".into(), long],
        };
        let mut out = Vec::new();
        wire.encode(&identity, batch, &mut out);
        let lines: Vec<String> = out
            .iter()
            .filter_map(|wire| match wire {
                Wire::Frame(bytes) => decode_chat(bytes).ok(),
                Wire::Command { .. } => None,
            })
            .collect();
        assert_eq!(lines[0], "/say_team Enemy at Warehouse A");
        assert_eq!(lines[1].len(), CHAT_LIMIT);
        assert!(matches!(out.last(), Some(Wire::Command { .. })));
    }
}

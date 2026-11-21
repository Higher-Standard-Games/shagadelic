//! What a bot says in chat, from TypeScript `bot/chat/BotChat.ts`: a greeting
//! when the match starts, a line about a human it fragged or was fragged by,
//! and gg near the end. Only when a human is in the match, at a pace a
//! person would type, and as chatty or sore as its name makes it.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use crate::games::washoi::{SimTime, agents::player::chat_lines as lines, random::Random};

/// One bot says at most a line per 20 seconds, and all bots together one per 5.
const BOT_GAP: Duration = Duration::from_secs(20);
const GLOBAL_GAP: Duration = Duration::from_secs(5);
const TYPING_MIN: Duration = Duration::from_millis(800);
const TYPING_MAX: Duration = Duration::from_millis(2500);
const GG_SECONDS_LEFT: f64 = 30.0;
const PHASE_ACTIVE: u8 = 2;
const PHASE_POSTGAME: u8 = 3;
const MAX_LENGTH: usize = 128;

/// `lastAnyLineMs`: the last line any bot in this process said.
static LAST_ANY_LINE: Mutex<Option<Instant>> = Mutex::new(None);

/// `isBotName`: one of the bot names, ignoring case and a `_<n>` suffix.
pub(in crate::games::washoi) fn is_bot_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    let base = match lower.rsplit_once('_') {
        Some((base, digits))
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) =>
        {
            base
        }
        _ => lower.as_str(),
    };
    lines::BOT_NAMES
        .iter()
        .any(|known| known.to_lowercase() == base)
}

/// `personalityOf`: how chatty and how sore, from an FNV-1a hash of the name.
fn personality(name: &str) -> (f64, f64) {
    let mut hash: u32 = 2_166_136_261;
    for unit in name.encode_utf16() {
        hash = (hash ^ u32::from(unit)).wrapping_mul(16_777_619);
    }
    let chatty = 0.25 + f64::from(hash & 0xff) / 255.0 * 0.35;
    let sore = 0.15 + f64::from((hash >> 8) & 0xff) / 255.0 * 0.5;
    (chatty, sore)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Moment {
    Greet,
    KilledHuman,
    BladeKill,
    Streak,
    Gracious,
    Mad,
    Sulk,
    Gg,
}

impl Moment {
    fn lines(self) -> &'static [&'static str] {
        match self {
            Self::Greet => lines::GREET,
            Self::KilledHuman => lines::KILLED_HUMAN,
            Self::BladeKill => lines::BLADE_KILL,
            Self::Streak => lines::STREAK,
            Self::Gracious => lines::GRACIOUS,
            Self::Mad => lines::MAD,
            Self::Sulk => lines::SULK,
            Self::Gg => lines::GG,
        }
    }
}

/// A frag the bot took part in: whom, and with what, when it fragged.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::games::washoi) enum Frag {
    Kill { victim: String, weapon: String },
    Death { killer: String },
}

/// `ChatView`: what chat sees of one decision.
pub(in crate::games::washoi) struct View<'a> {
    pub(in crate::games::washoi) now: SimTime,
    pub(in crate::games::washoi) phase: u8,
    pub(in crate::games::washoi) time_remaining: Option<f64>,
    /// Player names by entity.
    pub(in crate::games::washoi) names: &'a [(&'a str, &'a str)],
    pub(in crate::games::washoi) tilted: bool,
    pub(in crate::games::washoi) kill_streak: u32,
    pub(in crate::games::washoi) death_streak: u32,
    pub(in crate::games::washoi) frags: &'a [Frag],
}

pub(in crate::games::washoi) struct Chat {
    chatty: f64,
    sore: f64,
    phase: Option<u8>,
    said_gg: bool,
    last_line: Option<SimTime>,
    pending: Option<(String, SimTime)>,
    recent: Vec<&'static str>,
    random: Random,
}

impl Chat {
    pub(in crate::games::washoi) fn new(name: &str, random: Random) -> Self {
        let (chatty, sore) = personality(name);
        Self {
            chatty,
            sore,
            phase: None,
            said_gg: false,
            last_line: None,
            pending: None,
            recent: Vec::new(),
            random,
        }
    }

    pub(in crate::games::washoi) fn observe(&mut self, view: &View) {
        let humans: Vec<&str> = view
            .names
            .iter()
            .map(|(_, name)| *name)
            .filter(|name| !is_bot_name(name))
            .collect();
        let previous = self.phase.replace(view.phase);
        if view.phase != PHASE_ACTIVE && view.phase != PHASE_POSTGAME {
            self.said_gg = false;
        }
        if humans.is_empty() {
            return;
        }
        let pick = (self.random.next() * humans.len() as f64) as usize % humans.len();
        let any_human = humans[pick];
        if view.phase == PHASE_ACTIVE && previous.is_some_and(|phase| phase < PHASE_ACTIVE) {
            self.maybe(view.now, Moment::Greet, any_human, 0.5, false);
        }
        let near_end = (view.phase == PHASE_ACTIVE
            && view
                .time_remaining
                .is_some_and(|left| left <= GG_SECONDS_LEFT))
            || (view.phase == PHASE_POSTGAME && previous == Some(PHASE_ACTIVE));
        if near_end && !self.said_gg {
            self.said_gg = true;
            self.maybe(view.now, Moment::Gg, any_human, 0.7, true);
        }
        let name_of = |entity: &str| {
            view.names
                .iter()
                .find(|(known, _)| *known == entity)
                .map(|(_, name)| *name)
        };
        for frag in view.frags {
            match frag {
                Frag::Kill { victim, weapon } => {
                    let Some(other) = name_of(victim).filter(|name| !is_bot_name(name)) else {
                        continue;
                    };
                    let moment = if weapon == "blade" {
                        Moment::BladeKill
                    } else if view.kill_streak >= 3 {
                        Moment::Streak
                    } else {
                        Moment::KilledHuman
                    };
                    self.maybe(view.now, moment, other, self.chatty, false);
                }
                Frag::Death { killer } => {
                    let Some(other) = name_of(killer).filter(|name| !is_bot_name(name)) else {
                        continue;
                    };
                    let sore = if view.tilted { 0.8 } else { self.sore };
                    let moment = if view.death_streak >= 3 && self.random.next() < 0.5 {
                        Moment::Sulk
                    } else if self.random.next() < sore {
                        Moment::Mad
                    } else {
                        Moment::Gracious
                    };
                    self.maybe(view.now, moment, other, self.chatty, false);
                }
            }
        }
    }

    /// `poll`: the line due by now, once its typing time has passed.
    pub(in crate::games::washoi) fn poll(&mut self, now: SimTime) -> Option<String> {
        if self.pending.as_ref().is_some_and(|(_, at)| now >= *at) {
            return self.pending.take().map(|(text, _)| text);
        }
        None
    }

    fn maybe(&mut self, now: SimTime, moment: Moment, about: &str, chance: f64, force: bool) {
        if self.pending.is_some() && !force {
            return;
        }
        if self.last_line.is_some_and(|at| now - at < BOT_GAP) || !global_gap_passed() {
            return;
        }
        if self.random.next() >= chance {
            return;
        }
        let mut text: String = self.pick(moment).replace("{name}", about);
        if text.chars().count() > MAX_LENGTH {
            text = text.chars().take(MAX_LENGTH).collect();
        }
        let typing = TYPING_MIN + (TYPING_MAX - TYPING_MIN).mul_f64(self.random.next());
        self.pending = Some((text, now + typing));
        self.last_line = Some(now);
        if let Ok(mut last) = LAST_ANY_LINE.lock() {
            *last = Some(Instant::now());
        }
    }

    /// `_pick`: a line not said lately, in four tries.
    fn pick(&mut self, moment: Moment) -> &'static str {
        let lines = moment.lines();
        let mut line = lines[0];
        for _ in 0..4 {
            line = lines[(self.random.next() * lines.len() as f64) as usize % lines.len()];
            if !self.recent.contains(&line) {
                break;
            }
        }
        self.recent.push(line);
        if self.recent.len() > 6 {
            self.recent.remove(0);
        }
        line
    }
}

/// All bots in the process keep 5 seconds apart. Tests run bots in parallel
/// threads, so they leave the shared pace out.
fn global_gap_passed() -> bool {
    if cfg!(test) {
        return true;
    }
    LAST_ANY_LINE
        .lock()
        .map(|last| last.is_none_or(|at| at.elapsed() >= GLOBAL_GAP))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view<'a>(
        now: SimTime,
        phase: u8,
        names: &'a [(&'a str, &'a str)],
        frags: &'a [Frag],
    ) -> View<'a> {
        View {
            now,
            phase,
            time_remaining: Some(300.0),
            names,
            tilted: false,
            kill_streak: 1,
            death_streak: 0,
            frags,
        }
    }

    #[test]
    fn knows_bot_names_and_a_stable_personality() {
        assert!(is_bot_name("Ramen"));
        assert!(is_bot_name("ramen_2"));
        assert!(!is_bot_name("Tarmo"));
        assert_eq!(personality("Tarmo"), personality("Tarmo"));
        let (chatty, sore) = personality("Miso");
        assert!((0.25..=0.6).contains(&chatty) && (0.15..=0.65).contains(&sore));
    }

    #[test]
    fn speaks_only_to_humans_after_typing() {
        let start = SimTime::ZERO;
        let bots = [("a", "Ramen"), ("b", "Miso")];
        let mut chat = Chat::new("Ramen", Random::new(4));
        chat.observe(&view(start, 1, &bots, &[]));
        chat.observe(&view(start, PHASE_ACTIVE, &bots, &[]));
        assert_eq!(chat.poll(start + Duration::from_secs(5)), None);
        // With a human, gg near the end goes out after typing.
        let people = [("a", "Ramen"), ("h", "Tarmo")];
        let mut chat = Chat::new("Ramen", Random::new(4));
        chat.observe(&view(start, PHASE_ACTIVE, &people, &[]));
        let mut end = view(start, PHASE_ACTIVE, &people, &[]);
        end.time_remaining = Some(20.0);
        let mut said = None;
        for seed in 0..20 {
            let mut chat = Chat::new("Ramen", Random::new(seed));
            chat.observe(&view(start, PHASE_ACTIVE, &people, &[]));
            chat.observe(&end);
            assert_eq!(chat.poll(start + Duration::from_millis(700)), None);
            if let Some(line) = chat.poll(start + Duration::from_secs(3)) {
                said = Some(line);
                break;
            }
        }
        let said = said.expect("gg in some seed");
        assert!(
            lines::GG
                .iter()
                .any(|line| line.replace("{name}", "Tarmo") == said)
        );
        // A kill of a bot says nothing.
        let mut chat = Chat::new("Ramen", Random::new(1));
        let frags = [Frag::Kill {
            victim: "a".into(),
            weapon: "rlauncher".into(),
        }];
        chat.observe(&view(start, PHASE_ACTIVE, &people, &frags));
        assert_eq!(chat.poll(start + Duration::from_secs(5)), None);
    }
}

//! A strategy for the match as it goes, from TypeScript
//! `strategy/Strategist.ts`: read what is going wrong or right (a
//! diagnosis), answer it with a strategy that scales the gametype's weights
//! and sets a stance, hold it a while, and with enough match sense learn
//! which answer pays.
//!
//! Learned priors from earlier matches (`intel/`) are not ported: the rule
//! answer starts with a small prior and the rest at zero.

use crate::games::washoi::{
    agents::player::strategy::{match_read::MatchRead, weights::Weights},
    random::Random,
};

const TICK_RATE: u32 = 125;
/// The strategy is reconsidered once a second.
const INTERVAL_TICKS: u32 = TICK_RATE;
/// A strategy holds 6 seconds unless a much stronger reason comes.
const HOLD_TICKS: u32 = 6 * TICK_RATE;
const OVERRIDE_MARGIN: f64 = 0.35;
const LATE_SECONDS: f64 = 90.0;
const LATE_GAP: i32 = 2;
const FOCUS_SECONDS: f64 = 20.0;
const CONTROL_STACK_MARGIN: f64 = 25.0;
const STACKED_OWN: f64 = 150.0;
const STACKED_MARGIN: f64 = 40.0;
/// From this much match sense the bot learns which answer pays.
const ADAPT_SENSE: f64 = 0.6;
/// A strategy is judged after 30 seconds.
const EPISODE_TICKS: u32 = 30 * TICK_RATE;
const RULE_PRIOR: f64 = 0.3;
const PRIOR_SD: f64 = 1.0;
const REWARD_PER_FRAG: f64 = 1.0;
const REWARD_PER_ITEM: f64 = 0.3;
const REWARD_PER_STACK: f64 = 0.2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::games::washoi) enum DiagnosisId {
    Understacked,
    Stacked,
    LosingItems,
    EnemyWeak,
    AheadLate,
    BehindLate,
    Focused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::games::washoi) enum StrategyId {
    Standard,
    Recover,
    Control,
    Pressure,
    ProtectLead,
}

impl StrategyId {
    pub(in crate::games::washoi) fn name(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Recover => "recover",
            Self::Control => "control",
            Self::Pressure => "pressure",
            Self::ProtectLead => "protect_lead",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(in crate::games::washoi) struct Diagnosis {
    pub(in crate::games::washoi) id: DiagnosisId,
    pub(in crate::games::washoi) strength: f64,
}

/// `Stance`: how the strategy plays out beyond the weights.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::games::washoi) struct Stance {
    /// Scales the health at which the bot backs off.
    pub(in crate::games::washoi) retreat_scale: f64,
    /// Scales how much an item or a hunt must be worth to go for it.
    pub(in crate::games::washoi) seek_scale: f64,
    /// Shops for items even with 25 armor or more.
    pub(in crate::games::washoi) shop_armored: bool,
    /// Chases an enemy out of sight.
    pub(in crate::games::washoi) chase: bool,
    /// Fades out of a fight going badly.
    pub(in crate::games::washoi) fade: bool,
}

const STANDARD_STANCE: Stance = Stance {
    retreat_scale: 1.0,
    seek_scale: 1.0,
    shop_armored: false,
    chase: true,
    fade: false,
};

/// `STRATEGIES`: multipliers on the profile, then the stance.
fn strategy(id: StrategyId) -> (Multipliers, Stance) {
    let none = Multipliers::default();
    match id {
        StrategyId::Standard => (none, STANDARD_STANCE),
        StrategyId::Recover => (
            Multipliers {
                risk_tolerance: 0.5,
                item_greed: 1.3,
                aggression: 0.5,
                ..none
            },
            Stance {
                retreat_scale: 1.6,
                seek_scale: 0.6,
                shop_armored: true,
                chase: false,
                fade: true,
            },
        ),
        StrategyId::Control => (
            Multipliers {
                item_timing: 0.3,
                major_item_bias: 1.3,
                ..none
            },
            Stance {
                retreat_scale: 1.0,
                seek_scale: 0.7,
                shop_armored: true,
                chase: true,
                fade: false,
            },
        ),
        StrategyId::Pressure => (
            Multipliers {
                aggression: 1.5,
                risk_tolerance: 1.6,
                ..none
            },
            Stance {
                retreat_scale: 0.6,
                seek_scale: 1.6,
                shop_armored: false,
                chase: true,
                fade: false,
            },
        ),
        StrategyId::ProtectLead => (
            Multipliers {
                risk_tolerance: 0.5,
                aggression: 0.6,
                major_item_bias: 1.2,
                ..none
            },
            Stance {
                retreat_scale: 1.4,
                seek_scale: 0.8,
                shop_armored: true,
                chase: false,
                fade: true,
            },
        ),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Multipliers {
    item_greed: f64,
    major_item_bias: f64,
    item_timing: f64,
    aggression: f64,
    risk_tolerance: f64,
}

impl Default for Multipliers {
    fn default() -> Self {
        Self {
            item_greed: 1.0,
            major_item_bias: 1.0,
            item_timing: 1.0,
            aggression: 1.0,
            risk_tolerance: 1.0,
        }
    }
}

/// `ANSWER`: the rule answer to a diagnosis and the match sense it needs.
fn answer(diagnosis: DiagnosisId) -> (StrategyId, f64) {
    match diagnosis {
        DiagnosisId::AheadLate => (StrategyId::ProtectLead, 0.3),
        DiagnosisId::BehindLate => (StrategyId::Pressure, 0.6),
        DiagnosisId::Understacked => (StrategyId::Recover, 0.3),
        DiagnosisId::Stacked => (StrategyId::Pressure, 0.6),
        DiagnosisId::EnemyWeak => (StrategyId::Pressure, 0.6),
        DiagnosisId::LosingItems => (StrategyId::Control, 0.6),
        DiagnosisId::Focused => (StrategyId::Recover, 0.6),
    }
}

/// `CHOICES`: the answers a learning bot tries for a diagnosis.
fn choices(diagnosis: DiagnosisId) -> [StrategyId; 2] {
    use StrategyId::*;
    match diagnosis {
        DiagnosisId::AheadLate => [ProtectLead, Control],
        DiagnosisId::BehindLate => [Pressure, Control],
        DiagnosisId::Understacked => [Recover, Control],
        DiagnosisId::Stacked => [Pressure, Standard],
        DiagnosisId::EnemyWeak => [Pressure, Standard],
        DiagnosisId::LosingItems => [Control, Pressure],
        DiagnosisId::Focused => [Recover, Control],
    }
}

/// What `diagnose` reads beyond the match read: the bot's own stack, the
/// freshest living enemy's estimated stack and its uncertainty, and how
/// many major items enemies took in the last seconds.
pub(in crate::games::washoi) struct Reading<'a> {
    pub(in crate::games::washoi) own_stack: f64,
    pub(in crate::games::washoi) enemy: Option<(f64, f64)>,
    pub(in crate::games::washoi) others_takes: &'a dyn Fn(f64) -> usize,
    /// A major item the bot can be at when it returns.
    pub(in crate::games::washoi) timed_item_in_reach: bool,
}

/// `diagnose`: every diagnosis that applies, strongest first.
pub(in crate::games::washoi) fn diagnose(read: &MatchRead, reading: &Reading) -> Vec<Diagnosis> {
    let mut out = Vec::new();
    let clamp = |v: f64| v.clamp(0.0, 1.0);
    let score = read.score();
    let gap = score.own - score.best_other;
    let late = score.time_remaining.is_some_and(|left| left < LATE_SECONDS);
    if late && gap >= LATE_GAP {
        out.push(Diagnosis {
            id: DiagnosisId::AheadLate,
            strength: clamp(0.6 + f64::from(gap) / 10.0),
        });
    }
    if late && gap <= -LATE_GAP {
        out.push(Diagnosis {
            id: DiagnosisId::BehindLate,
            strength: clamp(0.6 - f64::from(gap) / 10.0),
        });
    }
    let own = reading.own_stack;
    let deficits: Vec<f64> = read
        .fights(90.0)
        .map(|fight| fight.enemy_stack_at_start - fight.own_stack_at_start)
        .collect();
    let fight_deficit = if deficits.is_empty() {
        0.0
    } else {
        deficits.iter().sum::<f64>() / deficits.len() as f64
    };
    let now_deficit = reading
        .enemy
        .filter(|(_, uncertainty)| *uncertainty < 0.5)
        .map_or(0.0, |(stack, _)| stack - own);
    let deficit = fight_deficit.max(now_deficit);
    if deficit > 40.0 && own < 150.0 {
        out.push(Diagnosis {
            id: DiagnosisId::Understacked,
            strength: clamp(deficit / 120.0),
        });
    }
    if let Some((stack, uncertainty)) = reading.enemy {
        if own >= STACKED_OWN && own >= stack + STACKED_MARGIN {
            out.push(Diagnosis {
                id: DiagnosisId::Stacked,
                strength: clamp(0.5 + (own - stack) / 200.0),
            });
        }
        if uncertainty < 0.3 && stack < 70.0 && own >= 100.0 {
            out.push(Diagnosis {
                id: DiagnosisId::EnemyWeak,
                strength: clamp((100.0 - stack) / 80.0),
            });
        }
    }
    let attackers = read.attackers(FOCUS_SECONDS);
    if attackers >= 2 {
        out.push(Diagnosis {
            id: DiagnosisId::Focused,
            strength: clamp(0.4 + 0.2 * attackers as f64),
        });
    }
    let (own_takes, others) = (read.own_takes(90.0), (reading.others_takes)(90.0));
    if others >= 2 && others > own_takes {
        out.push(Diagnosis {
            id: DiagnosisId::LosingItems,
            strength: clamp(others as f64 / (own_takes + others) as f64),
        });
    }
    out.sort_by(|a, b| b.strength.total_cmp(&a.strength));
    out
}

struct Episode {
    diagnosis: DiagnosisId,
    strategy: StrategyId,
    start: u32,
    gap: i32,
}

pub(in crate::games::washoi) struct Strategist {
    sense: f64,
    current: StrategyId,
    because: Option<Diagnosis>,
    since: Option<u32>,
    next: u32,
    /// Learned payoff per diagnosis and answer: episodes and mean reward.
    arms: Vec<((DiagnosisId, StrategyId), (u32, f64))>,
    episode: Option<Episode>,
    random: Random,
}

impl Strategist {
    pub(in crate::games::washoi) fn new(sense: f64, random: Random) -> Self {
        Self {
            sense,
            current: StrategyId::Standard,
            because: None,
            since: None,
            next: 0,
            arms: Vec::new(),
            episode: None,
            random,
        }
    }

    #[cfg(test)]
    pub(in crate::games::washoi) fn current(&self) -> StrategyId {
        self.current
    }

    /// The strategy and the diagnosis it answers, on one line.
    pub(in crate::games::washoi) fn explain(&self) -> String {
        match &self.because {
            Some(because) => format!("{} ({:?})", self.current.name(), because.id).to_lowercase(),
            None => self.current.name().to_owned(),
        }
    }

    /// The gametype's weights with the strategy's multipliers
    /// (`applyStrategyWeights`). Risk tolerance and item timing stay at most 1.
    pub(in crate::games::washoi) fn weights(&self, base: Weights) -> Weights {
        let (scale, _) = strategy(self.current);
        Weights {
            item_greed: base.item_greed * scale.item_greed,
            major_item_bias: base.major_item_bias * scale.major_item_bias,
            item_timing: (base.item_timing * scale.item_timing).min(1.0),
            aggression: base.aggression * scale.aggression,
            risk_tolerance: (base.risk_tolerance * scale.risk_tolerance).min(1.0),
            ..base
        }
    }

    pub(in crate::games::washoi) fn stance(&self) -> Stance {
        strategy(self.current).1
    }

    /// `update`: reconsider once a second.
    pub(in crate::games::washoi) fn update(&mut self, read: &MatchRead, reading: &Reading) {
        let tick = read.tick();
        if tick < self.next {
            return;
        }
        self.next = tick + INTERVAL_TICKS;
        if self.sense < 0.3 {
            return;
        }
        let top = diagnose(read, reading)
            .into_iter()
            .find(|diagnosis| self.sense >= answer(diagnosis.id).1);
        let episode_over = self
            .episode
            .as_ref()
            .is_some_and(|episode| tick.saturating_sub(episode.start) >= EPISODE_TICKS);
        if episode_over {
            self.judge(read, reading, tick);
        }
        let outstacked = self.current == StrategyId::Control
            && fit(StrategyId::Control, reading) != StrategyId::Control;
        let same_reason =
            matches!((&top, &self.because), (Some(top), Some(because)) if top.id == because.id);
        if same_reason && !episode_over && !outstacked {
            self.because = top;
            return;
        }
        let next = match &top {
            Some(top) => fit(self.choose(top.id), reading),
            None => StrategyId::Standard,
        };
        if next == self.current {
            if let Some(top) = &top
                && self.episode.is_none()
            {
                self.open(top.id, next, read, tick);
            }
            self.because = top;
            return;
        }
        let held = self
            .since
            .is_some_and(|since| tick.saturating_sub(since) < HOLD_TICKS);
        let stronger = top.as_ref().is_some_and(|top| {
            self.because
                .as_ref()
                .is_none_or(|because| top.strength >= because.strength + OVERRIDE_MARGIN)
        });
        if held && !stronger && !outstacked {
            return;
        }
        if self.episode.is_some() {
            self.judge(read, reading, tick);
        }
        self.current = next;
        self.since = Some(tick);
        if let Some(top) = &top {
            self.open(top.id, next, read, tick);
        }
        self.because = top;
    }

    /// `_choose`: the rule answer, or with enough sense a draw over the
    /// answers by what they paid so far.
    fn choose(&mut self, diagnosis: DiagnosisId) -> StrategyId {
        let rule = answer(diagnosis).0;
        if self.sense < ADAPT_SENSE {
            return rule;
        }
        let mut best = rule;
        let mut best_draw = f64::NEG_INFINITY;
        for strategy in choices(diagnosis) {
            let (n, mean) = self
                .arms
                .iter()
                .find(|(key, _)| *key == (diagnosis, strategy))
                .map_or((0, 0.0), |(_, arm)| *arm);
            let prior = if strategy == rule { RULE_PRIOR } else { 0.0 };
            let n = f64::from(n);
            let expected = (prior + mean * n) / (1.0 + n);
            let draw = expected + PRIOR_SD / (1.0 + n).sqrt() * gaussian(&mut self.random);
            if draw > best_draw {
                best_draw = draw;
                best = strategy;
            }
        }
        best
    }

    fn open(&mut self, diagnosis: DiagnosisId, strategy: StrategyId, read: &MatchRead, tick: u32) {
        let score = read.score();
        self.episode = Some(Episode {
            diagnosis,
            strategy,
            start: tick,
            gap: score.own - score.best_other,
        });
    }

    /// `_judge`: frags gained on the best other, major items won, and the
    /// stack kept, over the episode.
    fn judge(&mut self, read: &MatchRead, reading: &Reading, tick: u32) {
        let Some(episode) = self.episode.take() else {
            return;
        };
        let seconds =
            (f64::from(tick.saturating_sub(episode.start)) / f64::from(TICK_RATE)).max(1.0);
        let score = read.score();
        let items = read.own_takes(seconds) as f64 - (reading.others_takes)(seconds) as f64;
        let stack = read
            .average_own_stack(seconds)
            .map_or(0.0, |stack| (stack - 100.0) / 100.0);
        let reward = REWARD_PER_FRAG * f64::from(score.own - score.best_other - episode.gap)
            + REWARD_PER_ITEM * items
            + REWARD_PER_STACK * stack;
        let key = (episode.diagnosis, episode.strategy);
        match self.arms.iter_mut().find(|(known, _)| *known == key) {
            Some((_, (n, mean))) => {
                *n += 1;
                *mean += (reward - *mean) / f64::from(*n);
            }
            None => self.arms.push((key, (1, reward))),
        }
    }
}

/// `_fit`: Control only while the bot can contest the items: stacked as
/// well as the enemy, give or take 25, or with a timed item in reach.
/// Otherwise it recovers first.
fn fit(strategy: StrategyId, reading: &Reading) -> StrategyId {
    if strategy != StrategyId::Control || reading.timed_item_in_reach {
        return strategy;
    }
    match reading.enemy {
        Some((stack, _)) if reading.own_stack >= stack - CONTROL_STACK_MARGIN => {
            StrategyId::Control
        }
        _ => StrategyId::Recover,
    }
}

/// A standard normal draw, Box-Muller.
fn gaussian(random: &mut Random) -> f64 {
    let u = random.next().max(1e-12);
    (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * random.next()).cos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::{
        Observation, PlayerView,
        agents::player::strategy::{match_read::Moment, weights},
    };

    fn reading(own: f64, enemy: Option<(f64, f64)>) -> Reading<'static> {
        Reading {
            own_stack: own,
            enemy,
            others_takes: &|_| 0,
            timed_item_in_reach: false,
        }
    }

    fn read_at(tick: u32, own_score: i32, other: i32, left: f64) -> MatchRead {
        let mut read = MatchRead::default();
        let mut world = Observation {
            tick,
            ..Observation::default()
        };
        for (id, score) in [("bot", own_score), ("foe", other)] {
            world.players.push(PlayerView {
                entity_id: id.into(),
                score,
                ..PlayerView::default()
            });
        }
        let stack = |_: &str| None;
        read.update(
            &world,
            &Moment {
                own: "bot",
                health: Some(100),
                armor: Some(0),
                dead: false,
                seen: None,
                enemy_stack: &stack,
                time_remaining: Some(left),
            },
        );
        read
    }

    #[test]
    fn diagnoses_the_match_and_answers_by_sense() {
        let late_ahead = read_at(1000, 5, 2, 60.0);
        let found = diagnose(&late_ahead, &reading(100.0, None));
        assert_eq!(found[0].id, DiagnosisId::AheadLate);
        // Medium sense answers a lead, the rule way.
        let mut medium = Strategist::new(0.45, Random::new(1));
        medium.update(&late_ahead, &reading(100.0, None));
        assert_eq!(medium.current(), StrategyId::ProtectLead);
        assert!(!medium.stance().chase);
        let scaled = medium.weights(weights::DEFAULT);
        assert_eq!(scaled.risk_tolerance, 0.25);
        // An easy bot reads nothing.
        let mut easy = Strategist::new(0.0, Random::new(1));
        easy.update(&late_ahead, &reading(100.0, None));
        assert_eq!(easy.current(), StrategyId::Standard);
        // Understacked against a fresh estimate: recover.
        let early = read_at(1000, 0, 0, 500.0);
        let found = diagnose(&early, &reading(60.0, Some((200.0, 0.1))));
        assert_eq!(found[0].id, DiagnosisId::Understacked);
        // Stacked against a weak enemy: pressure, for a bot that senses it.
        let found = diagnose(&early, &reading(200.0, Some((50.0, 0.1))));
        assert!(found.iter().any(|d| d.id == DiagnosisId::Stacked));
        assert!(found.iter().any(|d| d.id == DiagnosisId::EnemyWeak));
        let mut hard = Strategist::new(0.8, Random::new(3));
        hard.update(&early, &reading(200.0, Some((50.0, 0.1))));
        assert!(matches!(
            hard.current(),
            StrategyId::Pressure | StrategyId::Standard
        ));
    }

    #[test]
    fn holds_a_strategy_unless_a_much_stronger_reason_comes() {
        let mut strategist = Strategist::new(0.45, Random::new(1));
        let ahead = read_at(1000, 4, 2, 60.0);
        strategist.update(&ahead, &reading(100.0, None));
        assert_eq!(strategist.current(), StrategyId::ProtectLead);
        // A second later the lead is gone: the strategy holds for 6 s.
        let level = read_at(1125, 2, 2, 59.0);
        strategist.update(&level, &reading(100.0, None));
        assert_eq!(strategist.current(), StrategyId::ProtectLead);
        let later = read_at(1000 + HOLD_TICKS, 2, 2, 54.0);
        strategist.update(&later, &reading(100.0, None));
        assert_eq!(strategist.current(), StrategyId::Standard);
        // Control is unfit while outstacked: recover first.
        assert_eq!(
            fit(StrategyId::Control, &reading(60.0, Some((200.0, 0.1)))),
            StrategyId::Recover
        );
        assert_eq!(
            fit(StrategyId::Control, &reading(190.0, Some((200.0, 0.1)))),
            StrategyId::Control
        );
    }
}

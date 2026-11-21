//! The match as the bot has seen it, from TypeScript `strategy/MatchRead.ts`:
//! its own stack over time, the major items it took, the fights it had and
//! how they started, who hurt it lately, and the score. Enemy estimates live
//! in `perception::estimate`. The strategist reads all of it.

use crate::games::washoi::{Observation, hosting::net::protocol::SnapshotEvent};

const TICK_RATE: u32 = 125;
/// A fight with no contact for 3 seconds is over, the two apart.
const FIGHT_GAP_TICKS: u32 = 3 * TICK_RATE;
const STACK_SAMPLE_TICKS: u32 = TICK_RATE / 2;
/// Everything older than three minutes is forgotten.
const MEMORY_TICKS: u32 = 180 * TICK_RATE;
const SPAWN_HEALTH: f64 = 100.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::games::washoi) enum Outcome {
    Open,
    Kill,
    Death,
    Apart,
}

#[derive(Clone, Debug, PartialEq)]
pub(in crate::games::washoi) struct Fight {
    pub(in crate::games::washoi) enemy: String,
    pub(in crate::games::washoi) start: u32,
    pub(in crate::games::washoi) last_contact: u32,
    pub(in crate::games::washoi) own_stack_at_start: f64,
    pub(in crate::games::washoi) enemy_stack_at_start: f64,
    pub(in crate::games::washoi) dealt: f64,
    pub(in crate::games::washoi) taken: f64,
    pub(in crate::games::washoi) outcome: Outcome,
}

/// `ScoreRead`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(in crate::games::washoi) struct Score {
    pub(in crate::games::washoi) own: i32,
    pub(in crate::games::washoi) deaths: u32,
    pub(in crate::games::washoi) best_other: i32,
    pub(in crate::games::washoi) time_remaining: Option<f64>,
}

/// One decision's view for the read.
pub(in crate::games::washoi) struct Moment<'a> {
    pub(in crate::games::washoi) own: &'a str,
    pub(in crate::games::washoi) health: Option<u16>,
    pub(in crate::games::washoi) armor: Option<u16>,
    pub(in crate::games::washoi) dead: bool,
    /// The enemy in sight, if any.
    pub(in crate::games::washoi) seen: Option<&'a str>,
    /// The estimated stack of an enemy, when the bot has one.
    pub(in crate::games::washoi) enemy_stack: &'a dyn Fn(&str) -> Option<f64>,
    pub(in crate::games::washoi) time_remaining: Option<f64>,
}

#[derive(Default)]
pub(in crate::games::washoi) struct MatchRead {
    tick: u32,
    stacks: Vec<(u32, f64)>,
    last_sample: Option<u32>,
    /// Ticks the bot took a major item itself.
    own_takes: Vec<u32>,
    fights: Vec<Fight>,
    attacks: Vec<(String, u32)>,
    previous: Option<(f64, f64)>,
    score: Score,
}

impl MatchRead {
    pub(in crate::games::washoi) fn update(&mut self, world: &Observation, moment: &Moment) {
        let tick = world.tick;
        self.tick = tick;
        // Own stack, and own major pickups from what the bot gained.
        if !moment.dead
            && let (Some(health), Some(armor)) = (moment.health, moment.armor)
        {
            let (health, armor) = (f64::from(health), f64::from(armor));
            let (before_health, before_armor) = self.previous.unwrap_or((health, armor));
            if armor - before_armor >= 50.0 {
                self.own_takes.push(tick);
            }
            if health - before_health >= 75.0 {
                self.own_takes.push(tick);
            }
            self.previous = Some((health, armor));
            if self
                .last_sample
                .is_none_or(|at| tick.saturating_sub(at) >= STACK_SAMPLE_TICKS)
            {
                self.stacks.push((tick, health + armor));
                self.last_sample = Some(tick);
            }
        } else {
            self.previous = None;
        }
        let own_stack =
            f64::from(moment.health.unwrap_or(0)) + f64::from(moment.armor.unwrap_or(0));
        for event in &world.events {
            match event {
                SnapshotEvent::Damage {
                    entity_id,
                    attacker_id,
                    damage,
                    ..
                } => {
                    let damage = f64::from(*damage);
                    if attacker_id == moment.own && entity_id != moment.own {
                        self.contact(entity_id, tick, own_stack, moment, damage, 0.0);
                    } else if entity_id == moment.own
                        && !attacker_id.is_empty()
                        && attacker_id != moment.own
                    {
                        self.contact(attacker_id, tick, own_stack, moment, 0.0, damage);
                        self.attacks.push((attacker_id.clone(), tick));
                    }
                }
                SnapshotEvent::Death {
                    entity_id,
                    killer_id,
                    ..
                } => {
                    if entity_id == moment.own {
                        self.end_all(Outcome::Death);
                    } else if killer_id == moment.own {
                        self.end(entity_id, Outcome::Kill);
                    }
                }
                _ => {}
            }
        }
        if let Some(seen) = moment.seen {
            self.contact(seen, tick, own_stack, moment, 0.0, 0.0);
        }
        for fight in &mut self.fights {
            if fight.outcome == Outcome::Open
                && tick.saturating_sub(fight.last_contact) > FIGHT_GAP_TICKS
            {
                fight.outcome = Outcome::Apart;
            }
        }
        self.score = read_score(world, moment);
        self.forget(tick);
    }

    /// `averageOwnStack`: the bot's mean health plus armor over the last
    /// seconds, None without samples.
    pub(in crate::games::washoi) fn average_own_stack(&self, seconds: f64) -> Option<f64> {
        let from = self.since(seconds);
        let recent: Vec<f64> = self
            .stacks
            .iter()
            .filter(|(at, _)| *at >= from)
            .map(|(_, stack)| *stack)
            .collect();
        (!recent.is_empty()).then(|| recent.iter().sum::<f64>() / recent.len() as f64)
    }

    /// Major items the bot took itself in the last seconds.
    pub(in crate::games::washoi) fn own_takes(&self, seconds: f64) -> usize {
        let from = self.since(seconds);
        self.own_takes.iter().filter(|at| **at >= from).count()
    }

    /// The fight under way with an enemy.
    pub(in crate::games::washoi) fn open_fight(&self, enemy: &str) -> Option<&Fight> {
        self.fights
            .iter()
            .find(|fight| fight.enemy == enemy && fight.outcome == Outcome::Open)
    }

    /// Fights that started in the last seconds, oldest first.
    pub(in crate::games::washoi) fn fights(&self, seconds: f64) -> impl Iterator<Item = &Fight> {
        let from = self.since(seconds);
        self.fights.iter().filter(move |fight| fight.start >= from)
    }

    pub(in crate::games::washoi) fn score(&self) -> Score {
        self.score
    }

    /// How many enemies hurt the bot in the last seconds.
    pub(in crate::games::washoi) fn attackers(&self, seconds: f64) -> usize {
        let from = self.since(seconds);
        let mut seen: Vec<&str> = Vec::new();
        for (attacker, at) in &self.attacks {
            if *at >= from && !seen.contains(&attacker.as_str()) {
                seen.push(attacker);
            }
        }
        seen.len()
    }

    pub(in crate::games::washoi) fn tick(&self) -> u32 {
        self.tick
    }

    fn since(&self, seconds: f64) -> u32 {
        self.tick
            .saturating_sub((seconds * f64::from(TICK_RATE)).round() as u32)
    }

    fn contact(
        &mut self,
        enemy: &str,
        tick: u32,
        own_stack: f64,
        moment: &Moment,
        dealt: f64,
        taken: f64,
    ) {
        let index = match self
            .fights
            .iter()
            .position(|fight| fight.enemy == enemy && fight.outcome == Outcome::Open)
        {
            Some(index) => index,
            None => {
                self.fights.push(Fight {
                    enemy: enemy.to_owned(),
                    start: tick,
                    last_contact: tick,
                    own_stack_at_start: own_stack,
                    enemy_stack_at_start: (moment.enemy_stack)(enemy).unwrap_or(SPAWN_HEALTH),
                    dealt: 0.0,
                    taken: 0.0,
                    outcome: Outcome::Open,
                });
                self.fights.len() - 1
            }
        };
        let fight = &mut self.fights[index];
        fight.last_contact = tick;
        fight.dealt += dealt;
        fight.taken += taken;
    }

    fn end(&mut self, enemy: &str, outcome: Outcome) {
        for fight in &mut self.fights {
            if fight.enemy == enemy && fight.outcome == Outcome::Open {
                fight.outcome = outcome;
            }
        }
    }

    fn end_all(&mut self, outcome: Outcome) {
        for fight in &mut self.fights {
            if fight.outcome == Outcome::Open {
                fight.outcome = outcome;
            }
        }
    }

    fn forget(&mut self, tick: u32) {
        let from = tick.saturating_sub(MEMORY_TICKS);
        self.stacks.retain(|(at, _)| *at >= from);
        self.own_takes.retain(|at| *at >= from);
        self.attacks.retain(|(_, at)| *at >= from);
        self.fights
            .retain(|fight| fight.start >= from || fight.outcome == Outcome::Open);
    }
}

/// `readScore`: the bot's own row and the best other.
fn read_score(world: &Observation, moment: &Moment) -> Score {
    let mut score = Score {
        time_remaining: moment.time_remaining,
        ..Score::default()
    };
    let mut best_other = None;
    for player in &world.players {
        if player.entity_id == moment.own {
            score.own = player.score;
            score.deaths = player.deaths;
        } else if player.team != "spectator" {
            best_other = Some(best_other.map_or(player.score, |best: i32| best.max(player.score)));
        }
    }
    score.best_other = best_other.unwrap_or(0);
    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::washoi::{PlayerView, hosting::net::protocol::Point};

    fn damage(victim: &str, attacker: &str, amount: u16) -> SnapshotEvent {
        SnapshotEvent::Damage {
            entity_id: victim.into(),
            attacker_id: attacker.into(),
            damage: amount,
            weapon_id: "rlauncher".into(),
            at: Point {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            hit_location: None,
            pain_tier: None,
        }
    }

    fn moment<'a>(stack: &'a dyn Fn(&str) -> Option<f64>, health: u16, armor: u16) -> Moment<'a> {
        Moment {
            own: "bot",
            health: Some(health),
            armor: Some(armor),
            dead: false,
            seen: None,
            enemy_stack: stack,
            time_remaining: Some(60.0),
        }
    }

    #[test]
    fn reads_fights_attackers_takes_and_the_score() {
        let stack = |_: &str| Some(150.0);
        let mut read = MatchRead::default();
        let mut world = Observation {
            tick: 100,
            ..Observation::default()
        };
        for (id, score) in [("bot", 3), ("foe", 5), ("other", 1)] {
            world.players.push(PlayerView {
                entity_id: id.into(),
                score,
                ..PlayerView::default()
            });
        }
        world.events = vec![damage("bot", "foe", 30), damage("bot", "other", 10)];
        read.update(&world, &moment(&stack, 70, 0));
        assert_eq!(read.attackers(20.0), 2);
        let fight = read.open_fight("foe").unwrap();
        assert_eq!(
            (fight.own_stack_at_start, fight.enemy_stack_at_start),
            (70.0, 150.0)
        );
        assert_eq!(fight.taken, 30.0);
        assert_eq!(
            read.score(),
            Score {
                own: 3,
                deaths: 0,
                best_other: 5,
                time_remaining: Some(60.0),
            }
        );
        // A Mega: a take of its own. Three seconds without contact ends the fights.
        world.tick = 200;
        world.events.clear();
        read.update(&world, &moment(&stack, 170, 0));
        assert_eq!(read.own_takes(90.0), 1);
        world.tick = 600;
        read.update(&world, &moment(&stack, 170, 0));
        assert!(read.open_fight("foe").is_none());
        assert_eq!(read.fights(90.0).count(), 2);
        assert_eq!(
            read.average_own_stack(90.0),
            Some((70.0 + 170.0 + 170.0) / 3.0)
        );
    }
}

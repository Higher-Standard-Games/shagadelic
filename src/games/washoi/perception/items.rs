//! What a bot believes about the map's items, from TypeScript
//! `bot/BotPerception.ts` `_believeItems` (lines 366-476) and
//! `_spotInSight`/`_believedUp`. The server sends every client the full
//! pickup mask. This is the one reader of it for decisions: the bot believes
//! an item gone when it took it, heard it taken, saw it vanish, or found
//! its spot empty, and believes it back once its estimated respawn passes or
//! it sees it there.
//!
//! - Its own take is known to the tick, by event or by the item vanishing
//!   within `SELF_TAKE_RADIUS`.
//! - A pickup heard within `HEARING_RANGE.majorPickup` (2,500 units, 900
//!   for other items) is noted with probability `itemTimingSkill`, and the
//!   remembered time is off by up to ± max(0.5, 7.83 − 8.33·skill) s.
//!   Major pickups are noted at `major_hearing` instead, which departs
//!   from TypeScript at Hard and above.
//! - A major item seen vanishing is remembered with the same error.
//! - A major item's spot in sight is watched every decision. Found gone, it
//!   was taken midway between when the bot last saw it there and now, or
//!   half a respawn ago. Other items' spots in sight are checked too.
//!
//! The planner's return times come from these beliefs, as
//! `goals/ItemGoalSource.ts` `respawnTickOf` (lines 41-83) feeds
//! `goals/ItemGoalPlanner.ts`'s `not_yet` (lines 102-110), led by the
//! strategy's `itemTiming` (`strategy/Strategist.ts`).
//!
//! Departures: the spot is the pickup's baked goal node, at standing height,
//! where TypeScript looks 24 units under the item's origin. Beliefs update
//! on each decision, with every event since the last, where TypeScript
//! updates on each snapshot. TypeScript checks four other spots a snapshot,
//! the ones the server says it is wrong about first, and fills pickups it
//! has no entry for from the server's mask. Both read server truth, so here
//! every spot in sight is checked each update, and a pickup without a spot
//! is believed up. When the server restores the items, as a match goes
//! live or a round freezes, the bot knows the rule every player does:
//! earlier takes are void, other items are up, and the majors spawn 15 s
//! later. TypeScript keeps its earlier beliefs. A spot beside the bot,
//! within `SELF_TAKE_RADIUS`, is checked whichever way it looks, as a
//! player standing on an empty spot knows it is empty. Without a sight
//! file, as TypeScript without a map, the bot keeps the server's mask.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use shagger_shared::net::game_state::Vec3;

use crate::games::washoi::{
    Observation, Sight, hosting::net::protocol::SnapshotEvent, random::Random,
};

const TICK_RATE: f64 = 125.0;
/// `HEARING_RANGE.majorPickup` and `.pickup`.
const MAJOR_PICKUP_RANGE: f64 = 2500.0;
const PICKUP_RANGE: f64 = 900.0;
/// `ITEM_SIGHT_RANGE`: items further than this can't be told from the spot.
const ITEM_SIGHT_RANGE: f64 = 2000.0;
/// `SELF_TAKE_RADIUS`: an item vanishing this close, the bot took.
const SELF_TAKE_RADIUS: f64 = 180.0;
const VIEW_HALF_ANGLE: f64 = 55.0 * PI / 180.0;
const EYE_HEIGHT: f64 = 32.0;
/// `MAJOR_ITEM_FIRST_SPAWN_SECONDS` (`shared/items/ItemRespawn.ts`): at
/// match start, majors spawn this long after the others.
const MAJOR_FIRST_SPAWN_TICKS: i64 = 15 * 125;

/// One map pickup as the bot knows it.
#[derive(Clone, Copy, Debug)]
pub(in crate::games::washoi) struct Spot {
    pub(in crate::games::washoi) id: usize,
    pub(in crate::games::washoi) at: Vec3,
    pub(in crate::games::washoi) major: bool,
    pub(in crate::games::washoi) respawn_ticks: u32,
}

/// The bot's side of one update.
#[derive(Clone, Copy)]
pub(in crate::games::washoi) struct ItemSenses<'a> {
    pub(in crate::games::washoi) own: &'a str,
    pub(in crate::games::washoi) position: Vec3,
    pub(in crate::games::washoi) view_yaw: f64,
    pub(in crate::games::washoi) sight: Option<&'a Sight>,
    /// `itemTimingSkill`, 0 to 1.
    pub(in crate::games::washoi) skill: f64,
    /// How often a heard major pickup is noted: `skill` in TypeScript,
    /// higher here at Hard and above (`Difficulty::major_pickup_hearing`).
    pub(in crate::games::washoi) major_hearing: f64,
}

/// What one update learned of others' takes, for the opponent model.
#[derive(Debug, Default, PartialEq)]
pub(in crate::games::washoi) struct ItemNews {
    /// Major items seen vanishing away from the bot.
    pub(in crate::games::washoi) vanished: Vec<usize>,
    /// Major items found gone, with the estimated taken tick.
    pub(in crate::games::washoi) found_gone: Vec<(usize, u32)>,
}

/// Believed against true, for the trace alone: no decision reads it.
#[derive(Debug, Default, Clone, PartialEq)]
pub(in crate::games::washoi) struct ItemTruth {
    /// Major items believed up while the server has them down.
    pub(in crate::games::washoi) wrong_up: Vec<usize>,
    /// Major items believed down while the server has them up.
    pub(in crate::games::washoi) wrong_down: Vec<usize>,
    /// For each major item back since the last update that the bot had a
    /// return time for: the believed return minus the real one, in ticks,
    /// and how the bot learned of the take.
    pub(in crate::games::washoi) return_errors: Vec<(usize, i64, Learned)>,
}

/// How the bot learned of a take, for the trace alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::games::washoi) enum Learned {
    Own = 0,
    Heard = 1,
    Seen = 2,
    Found = 3,
    /// The match-start rule.
    Rule = 4,
}

#[derive(Default)]
pub(in crate::games::washoi) struct ItemBeliefs {
    /// Items believed down, by ID: the remembered taken tick, with this
    /// bot's error in it.
    down: BTreeMap<usize, i64>,
    /// The latest tick each major item was seen on its spot.
    up_since: BTreeMap<usize, i64>,
    /// The server's mask at the last update, to see items vanish.
    before: Vec<bool>,
    mask: Vec<bool>,
    /// Diagnostics: the believed return of each major item, by ID.
    expected: BTreeMap<usize, (i64, Learned)>,
    truth: ItemTruth,
}

impl ItemBeliefs {
    /// Forget everything, as on a new map.
    pub(in crate::games::washoi) fn reset(&mut self) {
        *self = Self::default();
    }

    /// The server restored every item at `tick`, as a match went live or a
    /// round froze
    /// (`PickupSystem.resetForMatch`), majors after
    /// `MAJOR_FIRST_SPAWN_TICKS`. Warmup takes and timings are void.
    pub(in crate::games::washoi) fn match_started(&mut self, tick: u32, spots: &[Spot]) {
        let before = std::mem::take(&mut self.before);
        self.reset();
        self.before = before;
        let back = i64::from(tick) + MAJOR_FIRST_SPAWN_TICKS;
        for spot in spots.iter().filter(|spot| spot.major) {
            self.exact(spot, back - i64::from(spot.respawn_ticks), Learned::Rule);
        }
    }

    /// A decision without the bot's own position, such as while dead: it
    /// learns nothing, and an item gone meanwhile isn't seen to vanish.
    pub(in crate::games::washoi) fn unseen(&mut self, world: &Observation) {
        self.before.clone_from(&world.server_pickup_mask);
    }

    /// Read one decision's view: the server's mask and every event since
    /// the last update.
    pub(in crate::games::washoi) fn believe(
        &mut self,
        senses: ItemSenses<'_>,
        world: &Observation,
        spots: &[Spot],
        random: &mut Random,
    ) -> ItemNews {
        let server = world.server_pickup_mask.as_slice();
        let before = std::mem::replace(&mut self.before, server.to_vec());
        let tick = world.tick;
        let mut news = ItemNews::default();
        self.truth.return_errors.clear();
        for spot in spots.iter().filter(|spot| spot.major) {
            if server.get(spot.id) != Some(&true) {
                continue;
            }
            // Back, or up all along: a return time from before is spent.
            let expected = self.expected.remove(&spot.id);
            if before.get(spot.id) == Some(&false)
                && let Some((expected, learned)) = expected
            {
                self.truth
                    .return_errors
                    .push((spot.id, expected - i64::from(tick), learned));
            }
        }
        let Some(sight) = senses
            .sight
            .filter(|_| !server.is_empty() && !spots.is_empty())
        else {
            self.mask = server.to_vec();
            self.down.clear();
            self.record_truth(server, spots);
            return news;
        };
        let me = senses.position;
        let skill = senses.skill.clamp(0.0, 1.0);
        let error_ticks = (7.83 - 8.33 * skill).max(0.5) * TICK_RATE;
        let major_hearing = senses.major_hearing.clamp(0.0, 1.0);
        let mut in_sight: Vec<Option<bool>> = vec![None; spots.len()];
        let mut sees = |index: usize| -> bool {
            *in_sight[index]
                .get_or_insert_with(|| spot_in_sight(sight, me, senses.view_yaw, spots[index].at))
        };
        let up_on_server = |id: usize| server.get(id).copied().unwrap_or(true);

        // Pickups heard, or made, since the last update.
        for event in &world.events {
            let SnapshotEvent::ItemPickup {
                pickup_id,
                entity_id,
                ..
            } = event
            else {
                continue;
            };
            let Some(spot) = usize::try_from(*pickup_id)
                .ok()
                .and_then(|id| spots.iter().find(|spot| spot.id == id))
            else {
                continue;
            };
            let range = if spot.major {
                MAJOR_PICKUP_RANGE
            } else {
                PICKUP_RANGE
            };
            if entity_id == senses.own {
                self.exact(spot, i64::from(tick), Learned::Own);
            } else if distance(me, spot.at) <= range
                && random.next() < if spot.major { major_hearing } else { skill }
            {
                self.remember(spot, i64::from(tick), error_ticks, random, Learned::Heard);
            }
        }

        // Items seen being taken. One that vanishes beside the bot, it took.
        for (index, spot) in spots.iter().enumerate() {
            if before.get(spot.id) != Some(&true) || server.get(spot.id) != Some(&false) {
                continue;
            }
            if distance(me, spot.at) <= SELF_TAKE_RADIUS {
                self.exact(spot, i64::from(tick), Learned::Own);
                continue;
            }
            if !spot.major || !sees(index) {
                continue;
            }
            self.remember(spot, i64::from(tick), error_ticks, random, Learned::Seen);
            news.vanished.push(spot.id);
        }

        // Every spot in sight, or beside the bot whichever way it looks,
        // every update: up there, or found gone. Only a major item's
        // sighting dates a later find.
        for (index, spot) in spots.iter().enumerate() {
            if distance(me, spot.at) > SELF_TAKE_RADIUS && !sees(index) {
                continue;
            }
            if up_on_server(spot.id) {
                self.down.remove(&spot.id);
                if spot.major {
                    self.up_since.insert(spot.id, i64::from(tick));
                }
            } else if !self.down.contains_key(&spot.id) {
                self.found_gone(spot, tick, error_ticks, random, &mut news);
            }
        }

        // A pickup with no spot the bot knows is believed up.
        let mut mask = vec![true; server.len()];
        for spot in spots {
            let up = self.believed_up(spot, tick);
            if let Some(slot) = mask.get_mut(spot.id) {
                *slot = up;
            }
        }
        self.mask = mask;
        self.record_truth(server, spots);
        news
    }

    /// Whether the bot believes the item is up, or `None` when the server
    /// sent no mask.
    pub(in crate::games::washoi) fn up(&self, id: usize) -> Option<bool> {
        self.mask.get(id).copied()
    }

    /// The believed mask, indexed by pickup ID like the server's.
    pub(in crate::games::washoi) fn mask(&self) -> &[bool] {
        &self.mask
    }

    /// When an item believed down is expected back (`respawnTickOf`).
    pub(in crate::games::washoi) fn returns_tick(&self, spot: &Spot) -> Option<u32> {
        let taken = *self.down.get(&spot.id)?;
        u32::try_from(taken + i64::from(spot.respawn_ticks)).ok()
    }

    pub(in crate::games::washoi) fn truth(&self) -> &ItemTruth {
        &self.truth
    }

    fn believed_up(&mut self, spot: &Spot, tick: u32) -> bool {
        let Some(&taken) = self.down.get(&spot.id) else {
            return true;
        };
        if i64::from(tick) >= taken + i64::from(spot.respawn_ticks) {
            self.down.remove(&spot.id);
            return true;
        }
        false
    }

    fn exact(&mut self, spot: &Spot, taken: i64, learned: Learned) {
        self.down.insert(spot.id, taken);
        if spot.major {
            self.expected
                .insert(spot.id, (taken + i64::from(spot.respawn_ticks), learned));
        }
    }

    fn remember(
        &mut self,
        spot: &Spot,
        taken: i64,
        error_ticks: f64,
        random: &mut Random,
        learned: Learned,
    ) {
        let off = (random.next() * 2.0 - 1.0) * error_ticks;
        self.exact(spot, taken + off.round() as i64, learned);
    }

    /// Found gone: taken midway between now and when the bot last saw it
    /// there, and no earlier than a respawn period ago.
    fn found_gone(
        &mut self,
        spot: &Spot,
        tick: u32,
        error_ticks: f64,
        random: &mut Random,
        news: &mut ItemNews,
    ) {
        let respawn = i64::from(spot.respawn_ticks);
        let now = i64::from(tick);
        let since = self
            .up_since
            .get(&spot.id)
            .map_or(now - respawn, |&since| since.max(now - respawn));
        let taken = ((since + now) as f64 / 2.0).round() as i64;
        self.remember(spot, taken, error_ticks, random, Learned::Found);
        if spot.major
            && let Ok(taken) = u32::try_from(taken)
        {
            news.found_gone.push((spot.id, taken));
        }
    }

    fn record_truth(&mut self, server: &[bool], spots: &[Spot]) {
        self.truth.wrong_up.clear();
        self.truth.wrong_down.clear();
        for spot in spots.iter().filter(|spot| spot.major) {
            let (Some(&real), Some(&believed)) = (server.get(spot.id), self.mask.get(spot.id))
            else {
                continue;
            };
            if believed && !real {
                self.truth.wrong_up.push(spot.id);
            } else if !believed && real {
                self.truth.wrong_down.push(spot.id);
            }
        }
    }
}

/// `_spotInSight`: whether the bot can tell from here if the item is there.
fn spot_in_sight(sight: &Sight, me: Vec3, view_yaw: f64, at: Vec3) -> bool {
    if distance(me, at) > ITEM_SIGHT_RANGE {
        return false;
    }
    let wanted = (at.x - me.x).atan2(at.z - me.z);
    let off = wanted - view_yaw;
    if off.sin().atan2(off.cos()).abs() > VIEW_HALF_ANGLE {
        return false;
    }
    sight.clear(
        [me.x, me.y + EYE_HEIGHT, me.z],
        [at.x, at.y + EYE_HEIGHT, at.z],
    )
}

fn distance(a: Vec3, b: Vec3) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use std::f64::consts::FRAC_PI_2;

    use super::*;
    use crate::games::washoi::hosting::net::protocol::Point;

    const MEGA: Spot = Spot {
        id: 0,
        at: Vec3 {
            x: 1000.0,
            y: 0.0,
            z: 0.0,
        },
        major: true,
        respawn_ticks: 7500,
    };
    const SHELLS: Spot = Spot {
        id: 1,
        at: Vec3 {
            x: 0.0,
            y: 0.0,
            z: 1000.0,
        },
        major: false,
        respawn_ticks: 1875,
    };
    const SPOTS: [Spot; 2] = [MEGA, SHELLS];
    /// Facing away from both items; `FRAC_PI_2` faces the mega.
    const BEHIND: f64 = -FRAC_PI_2;
    const AT: Vec3 = Vec3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    };

    fn sight() -> Sight {
        Sight::parse(&super::super::sight::tests::encode("test", &[], &[])).unwrap()
    }

    fn world(tick: u32, mask: [bool; 2], events: Vec<SnapshotEvent>) -> Observation {
        Observation {
            tick,
            server_pickup_mask: mask.to_vec(),
            events,
            ..Observation::default()
        }
    }

    fn picked(by: &str, id: i16) -> SnapshotEvent {
        SnapshotEvent::ItemPickup {
            pickup_id: id,
            weapon_id: String::new(),
            entity_id: by.into(),
            dropped_weapon_id: String::new(),
            at: Point {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
        }
    }

    /// One bot's beliefs, updated as it stands at `position` looking along
    /// `yaw` with timing skill `skill`.
    struct Bot {
        sight: Sight,
        items: ItemBeliefs,
        random: Random,
        position: Vec3,
        yaw: f64,
        skill: f64,
        hearing: f64,
    }

    impl Bot {
        fn new(seed: u64, skill: f64) -> Self {
            Self {
                sight: sight(),
                items: ItemBeliefs::default(),
                random: Random::new(seed),
                position: AT,
                yaw: BEHIND,
                skill,
                hearing: skill,
            }
        }

        fn see(&mut self, world: &Observation) -> ItemNews {
            let senses = ItemSenses {
                own: "bot",
                position: self.position,
                view_yaw: self.yaw,
                sight: Some(&self.sight),
                skill: self.skill,
                major_hearing: self.hearing,
            };
            self.items.believe(senses, world, &SPOTS, &mut self.random)
        }
    }

    #[test]
    fn its_own_take_is_known_to_the_tick_and_the_item_is_back_after_its_respawn() {
        let mut bot = Bot::new(1, 0.0);
        bot.see(&world(100, [true, true], vec![]));
        bot.see(&world(110, [false, true], vec![picked("bot", 0)]));
        assert_eq!(bot.items.up(0), Some(false));
        assert_eq!(bot.items.returns_tick(&MEGA), Some(110 + 7500));
        // Back on time, whatever the server says meanwhile.
        bot.see(&world(7609, [true, true], vec![]));
        assert_eq!(bot.items.up(0), Some(false));
        assert_eq!(bot.items.truth().wrong_down, [0]);
        bot.see(&world(7610, [true, true], vec![]));
        assert_eq!(bot.items.up(0), Some(true));
        assert!(bot.items.truth().wrong_down.is_empty());
    }

    #[test]
    fn an_item_vanishing_beside_the_bot_is_its_own_take() {
        let mut bot = Bot::new(1, 0.0);
        bot.position = Vec3::new(900.0, 0.0, 0.0);
        bot.see(&world(100, [true, true], vec![]));
        bot.see(&world(101, [false, true], vec![]));
        assert_eq!(bot.items.returns_tick(&MEGA), Some(101 + 7500));
    }

    #[test]
    fn a_heard_take_is_noted_at_the_skill_rate_within_the_error_band() {
        // Hard: noted seven times in ten, off by up to 2 s (250 ticks).
        let error = (7.83 - 8.33 * 0.7) * TICK_RATE;
        let mut noted = 0;
        for seed in 0..400 {
            let mut bot = Bot::new(seed, 0.7);
            bot.see(&world(100, [true, true], vec![]));
            bot.see(&world(110, [false, true], vec![picked("player_2", 0)]));
            let Some(back) = bot.items.returns_tick(&MEGA) else {
                assert_eq!(bot.items.up(0), Some(true), "unnoticed, still up");
                continue;
            };
            noted += 1;
            assert!((f64::from(back) - (110.0 + 7500.0)).abs() <= error + 0.5);
        }
        assert!((240..=320).contains(&noted), "noted {noted} of 400");
    }

    #[test]
    fn a_heard_major_take_is_noted_at_the_hearing_rate() {
        let mut noted = 0;
        for seed in 0..400 {
            let mut bot = Bot::new(seed, 0.7);
            bot.hearing = 0.95;
            bot.see(&world(100, [true, true], vec![]));
            bot.see(&world(110, [false, false], vec![picked("player_2", 0)]));
            noted += usize::from(bot.items.up(0) == Some(false));
        }
        assert!((365..=395).contains(&noted), "noted {noted} of 400");
    }

    #[test]
    fn a_take_out_of_earshot_goes_unheard() {
        let mut bot = Bot::new(1, 1.0);
        // Major items carry 2,500 units, others 900.
        bot.position = Vec3::new(-1600.0, 0.0, 0.0);
        bot.see(&world(100, [true, true], vec![]));
        let both = || vec![picked("player_2", 0), picked("player_2", 1)];
        bot.see(&world(110, [false, false], both()));
        assert_eq!(bot.items.mask(), [true, true]);
        assert_eq!(bot.items.truth().wrong_up, [0], "only major items count");
        // Nearer, the mega is heard and the shells, 1,414 units off, aren't.
        bot.position = Vec3::new(-1000.0, 0.0, 0.0);
        bot.see(&world(120, [false, false], both()));
        assert_eq!(bot.items.mask(), [false, true]);
    }

    #[test]
    fn an_item_seen_vanishing_is_remembered_and_reported() {
        let mut bot = Bot::new(3, 1.0);
        bot.yaw = FRAC_PI_2;
        bot.see(&world(100, [true, true], vec![]));
        let news = bot.see(&world(110, [false, true], vec![]));
        assert_eq!(news.vanished, [0]);
        // At full skill the error is half a second.
        let back = f64::from(bot.items.returns_tick(&MEGA).unwrap());
        assert!((back - 7610.0).abs() <= 0.5 * TICK_RATE + 0.5);
        // Looking away, a vanish goes unseen.
        let mut bot = Bot::new(3, 1.0);
        bot.see(&world(100, [true, true], vec![]));
        let news = bot.see(&world(110, [false, true], vec![]));
        assert!(news.vanished.is_empty());
        assert_eq!(bot.items.up(0), Some(true));
    }

    #[test]
    fn a_spot_found_empty_dates_the_take_midway_since_it_was_seen_there() {
        let mut bot = Bot::new(5, 1.0);
        bot.yaw = FRAC_PI_2;
        bot.see(&world(1000, [true, true], vec![]));
        // Gone while the bot looked away, then seen empty.
        bot.yaw = BEHIND;
        bot.see(&world(1500, [false, true], vec![]));
        assert_eq!(bot.items.up(0), Some(true));
        bot.yaw = FRAC_PI_2;
        let news = bot.see(&world(2000, [false, true], vec![]));
        assert_eq!(news.found_gone, [(0, 1500)]);
        let back = f64::from(bot.items.returns_tick(&MEGA).unwrap());
        assert!((back - (1500.0 + 7500.0)).abs() <= 0.5 * TICK_RATE + 0.5);
        // Seen there again, it is up at once.
        bot.see(&world(2010, [true, true], vec![]));
        assert_eq!(bot.items.up(0), Some(true));
        // Never seen there, it was taken half a respawn ago.
        let mut bot = Bot::new(5, 1.0);
        bot.yaw = FRAC_PI_2;
        let news = bot.see(&world(9000, [false, true], vec![]));
        assert_eq!(news.found_gone, [(0, 9000 - 3750)]);
    }

    #[test]
    fn a_believed_return_does_not_date_a_later_find() {
        let mut bot = Bot::new(5, 1.0);
        bot.see(&world(100, [true, true], vec![]));
        bot.see(&world(110, [false, true], vec![picked("bot", 0)]));
        // Believed back at 7,610, and taken unseen after.
        bot.see(&world(7610, [true, true], vec![]));
        assert_eq!(bot.items.up(0), Some(true));
        bot.see(&world(8000, [false, true], vec![]));
        bot.yaw = FRAC_PI_2;
        let news = bot.see(&world(9000, [false, true], vec![]));
        // A late estimate would date the find late too, against an opponent
        // who took it on its real return: half a respawn back, as never seen.
        assert_eq!(news.found_gone, [(0, 9000 - 3750)]);
    }

    #[test]
    fn a_spot_beside_the_bot_is_found_empty_whichever_way_it_looks() {
        let mut bot = Bot::new(5, 1.0);
        bot.see(&world(10_000, [true, true], vec![]));
        bot.see(&world(10_100, [false, true], vec![]));
        assert_eq!(bot.items.up(0), Some(true), "taken unseen and unheard");
        // Walked onto the spot facing away.
        bot.position = Vec3::new(900.0, 0.0, 0.0);
        let news = bot.see(&world(10_200, [false, true], vec![]));
        assert_eq!(news.found_gone, [(0, 10_200 - 3750)]);
        assert_eq!(bot.items.up(0), Some(false));
    }

    #[test]
    fn at_match_start_warmup_takes_are_void_and_majors_spawn_late() {
        let mut bot = Bot::new(1, 0.0);
        bot.see(&world(100, [true, true], vec![]));
        // Taken in warmup: believed back at 7,610.
        bot.see(&world(
            110,
            [false, false],
            vec![picked("bot", 0), picked("bot", 1)],
        ));
        bot.items.match_started(1000, &SPOTS);
        bot.see(&world(1001, [false, true], vec![]));
        assert_eq!(bot.items.mask(), [false, true]);
        assert_eq!(bot.items.returns_tick(&MEGA), Some(1000 + 1875));
        bot.see(&world(2875, [true, true], vec![]));
        assert_eq!(bot.items.up(0), Some(true));
        assert_eq!(
            bot.items.truth().return_errors,
            [(0, 0, Learned::Rule)],
            "back on the rule's time"
        );
    }

    #[test]
    fn any_item_in_sight_is_checked_and_one_without_a_spot_is_believed_up() {
        let mut bot = Bot::new(5, 1.0);
        // Facing the shells, not the mega.
        bot.yaw = 0.0;
        let mut shells_gone = world(100, [true, false], vec![]);
        // A third pickup the bot has no spot for, down on the server.
        shells_gone.server_pickup_mask.push(false);
        let news = bot.see(&shells_gone);
        assert!(news.found_gone.is_empty(), "news is of major items");
        assert_eq!(bot.items.mask(), [true, false, true]);
        let back = f64::from(bot.items.returns_tick(&SHELLS).unwrap());
        assert!((back - (100.0 - 938.0 + 1875.0)).abs() <= 0.5 * TICK_RATE + 0.5);
    }

    #[test]
    fn a_return_time_is_scored_against_the_real_return() {
        let mut bot = Bot::new(1, 0.0);
        bot.see(&world(100, [true, true], vec![]));
        bot.see(&world(110, [false, true], vec![picked("bot", 0)]));
        bot.see(&world(7700, [true, true], vec![]));
        assert_eq!(
            bot.items.truth().return_errors,
            [(0, 7610 - 7700, Learned::Own)]
        );
        bot.see(&world(7710, [true, true], vec![]));
        assert!(bot.items.truth().return_errors.is_empty());
    }

    #[test]
    fn without_a_sight_file_the_bot_keeps_the_servers_mask() {
        let mut items = ItemBeliefs::default();
        let blind = ItemSenses {
            own: "bot",
            position: AT,
            view_yaw: BEHIND,
            sight: None,
            skill: 1.0,
            major_hearing: 1.0,
        };
        items.believe(
            blind,
            &world(100, [false, true], vec![]),
            &SPOTS,
            &mut Random::new(1),
        );
        assert_eq!(items.mask(), [false, true]);
    }

    /// Bot decisions read the server's pickup mask only through this module.
    #[test]
    fn no_agent_code_reads_the_servers_pickup_mask() {
        fn visit(dir: &std::path::Path, found: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path, found);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    // Tests build observations by hand; only the code
                    // before a file's tests module counts.
                    let code = text.split("\nmod tests").next().unwrap_or_default();
                    if code.contains("server_pickup_mask") || code.contains("pickup_active_mask") {
                        found.push(path.display().to_string());
                    }
                }
            }
        }
        let agents =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/games/washoi/agents");
        let mut found = Vec::new();
        visit(&agents, &mut found);
        assert!(
            found.is_empty(),
            "read it through perception::items: {found:?}"
        );
    }
}

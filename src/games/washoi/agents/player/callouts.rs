//! Team callouts, from TypeScript `bot/BotCallouts.ts`: say where enemies
//! are, where the bomb carrier is, and where the bomb was dropped or
//! planted, by the map's location names, and read teammates' calls of
//! enemies as somewhere to hunt.

use std::{io, path::Path, time::Duration};

use serde::Deserialize;

use crate::games::washoi::{SimTime, Vec3, random::Random};

/// A bot calls at most every 3 seconds, the same call every 8, and not
/// within 1.2 seconds of anyone on its team.
const BOT_GAP: Duration = Duration::from_secs(3);
const REPEAT: Duration = Duration::from_secs(8);
const TEAM_QUIET: Duration = Duration::from_millis(1200);
/// News of the bomb goes out within 2 seconds, as a person types.
const NEWS_DELAY: Duration = Duration::from_secs(2);

/// The map's `target_location` names, from `tools/bake-washoi-locations.mjs`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Locations {
    map: String,
    locations: Vec<Location>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Location {
    name: String,
    /// Game space, y up.
    position: [f64; 3],
}

impl Locations {
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub fn map(&self) -> &str {
        &self.map
    }

    /// `locationName`: the nearest location's name.
    pub(in crate::games::washoi) fn name_at(&self, at: Vec3) -> Option<&str> {
        self.locations
            .iter()
            .min_by(|a, b| distance2(a.position, at).total_cmp(&distance2(b.position, at)))
            .map(|location| location.name.as_str())
    }

    /// `placePosition`: the middle of the locations with this name.
    pub(in crate::games::washoi) fn position_of(&self, place: &str) -> Option<Vec3> {
        let named: Vec<[f64; 3]> = self
            .locations
            .iter()
            .filter(|location| location.name == place)
            .map(|location| location.position)
            .collect();
        if named.is_empty() {
            return None;
        }
        let n = named.len() as f64;
        let sum = named
            .iter()
            .fold([0.0; 3], |s, p| [s[0] + p[0], s[1] + p[1], s[2] + p[2]]);
        Some(Vec3::new(sum[0] / n, sum[1] / n, sum[2] / n))
    }

    fn is_empty(&self) -> bool {
        self.locations.is_empty()
    }
}

fn distance2(a: [f64; 3], b: Vec3) -> f64 {
    (a[0] - b.x).powi(2) + (a[1] - b.y).powi(2) + (a[2] - b.z).powi(2)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::games::washoi) enum Kind {
    Enemies,
    Carrier,
    Dropped,
    Planted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::games::washoi) struct Callout {
    pub(in crate::games::washoi) kind: Kind,
    pub(in crate::games::washoi) count: usize,
    pub(in crate::games::washoi) place: String,
}

/// `calloutText`.
pub(in crate::games::washoi) fn text(call: &Callout) -> String {
    match call.kind {
        Kind::Enemies if call.count > 1 => {
            format!("/say_team {} enemies at {}", call.count, call.place)
        }
        Kind::Enemies => format!("/say_team Enemy at {}", call.place),
        Kind::Carrier => format!("/say_team Bomb carrier at {}", call.place),
        Kind::Dropped => format!("/say_team Bomb dropped at {}", call.place),
        Kind::Planted => format!("/say_team Bomb planted at {}", call.place),
    }
}

/// `parseCallout`: a teammate's line, `[TEAM]` prefix and all.
pub(in crate::games::washoi) fn parse(text: &str) -> Option<Callout> {
    let line = text.trim();
    let line = line.strip_prefix("[TEAM]").unwrap_or(line).trim();
    let call = |kind, count, place: &str| {
        (!place.is_empty()).then(|| Callout {
            kind,
            count,
            place: place.to_owned(),
        })
    };
    if let Some(place) = line.strip_prefix("Enemy at ") {
        return call(Kind::Enemies, 1, place);
    }
    if let Some((count, place)) = line.split_once(" enemies at ")
        && let Ok(count) = count.parse::<usize>()
    {
        return call(Kind::Enemies, count, place);
    }
    for (prefix, kind) in [
        ("Bomb carrier at ", Kind::Carrier),
        ("Bomb dropped at ", Kind::Dropped),
        ("Bomb planted at ", Kind::Planted),
    ] {
        if let Some(place) = line.strip_prefix(prefix) {
            return call(kind, 1, place);
        }
    }
    None
}

/// The S&D bomb as `match_state` describes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(in crate::games::washoi) struct Objective {
    pub(in crate::games::washoi) phase: String,
    pub(in crate::games::washoi) position: Option<Vec3>,
    pub(in crate::games::washoi) carrier: Option<String>,
    pub(in crate::games::washoi) site: Option<String>,
}

/// `CalloutView`.
pub(in crate::games::washoi) struct View<'a> {
    pub(in crate::games::washoi) now: SimTime,
    /// The bot's team, in team play.
    pub(in crate::games::washoi) team: Option<&'a str>,
    /// Enemies in sight.
    pub(in crate::games::washoi) enemies: &'a [(&'a str, Vec3)],
    pub(in crate::games::washoi) objective: Option<&'a Objective>,
}

pub(in crate::games::washoi) struct Callouts {
    last_said: Option<SimTime>,
    last_team: Option<SimTime>,
    made: Vec<((Kind, String), SimTime)>,
    last_phase: Option<String>,
    news: Option<(Callout, SimTime, SimTime)>,
    random: Random,
}

impl Callouts {
    pub(in crate::games::washoi) fn new(random: Random) -> Self {
        Self {
            last_said: None,
            last_team: None,
            made: Vec::new(),
            last_phase: None,
            news: None,
            random,
        }
    }

    /// `heard`: a teammate's line. A call it makes need not be made again.
    pub(in crate::games::washoi) fn heard(&mut self, text: &str, now: SimTime) -> Option<Callout> {
        self.last_team = Some(now);
        let call = parse(text)?;
        self.remember(&call, now);
        if self
            .news
            .as_ref()
            .is_some_and(|(news, _, _)| news.kind == call.kind && news.place == call.place)
        {
            self.news = None;
        }
        Some(call)
    }

    /// `observe`: the line to say now, if any.
    pub(in crate::games::washoi) fn observe(
        &mut self,
        view: &View,
        locations: &Locations,
    ) -> Option<String> {
        self.note_objective(view, locations);
        view.team?;
        if locations.is_empty() {
            return None;
        }
        let now = view.now;
        if self.last_said.is_some_and(|at| now - at < BOT_GAP)
            || self.last_team.is_some_and(|at| now - at < TEAM_QUIET)
        {
            return None;
        }
        if self
            .news
            .as_ref()
            .is_some_and(|(_, at, _)| now - *at >= REPEAT)
        {
            self.news = None;
        }
        let call = match &self.news {
            Some((call, _, due)) if now >= *due => {
                let call = call.clone();
                self.news = None;
                call
            }
            // News not due yet holds back sightings too: it goes first.
            Some(_) => return None,
            None => sighting(view, locations)?,
        };
        let key = (call.kind, call.place.clone());
        if self
            .made
            .iter()
            .any(|(made, at)| *made == key && now - *at < REPEAT)
        {
            return None;
        }
        self.remember(&call, now);
        self.last_said = Some(now);
        Some(text(&call))
    }

    fn remember(&mut self, call: &Callout, now: SimTime) {
        let key = (call.kind, call.place.clone());
        self.made
            .retain(|(made, at)| *made != key && now - *at < REPEAT);
        self.made.push((key, now));
    }

    /// `noteObjective`: news when the carried bomb is dropped or planted.
    fn note_objective(&mut self, view: &View, locations: &Locations) {
        let phase = view.objective.map(|objective| objective.phase.clone());
        let before = std::mem::replace(&mut self.last_phase, phase.clone());
        let Some(objective) = view.objective else {
            return;
        };
        let Some(position) = objective.position else {
            return;
        };
        if phase == before || before.as_deref() != Some("carried") {
            return;
        }
        let place = locations.name_at(position).map(str::to_owned);
        let due = view.now + NEWS_DELAY.mul_f64(self.random.next());
        let news = match objective.phase.as_str() {
            "dropped" => place.map(|place| (Kind::Dropped, place)),
            "planted" | "armed" => objective
                .site
                .clone()
                .or(place)
                .map(|place| (Kind::Planted, place)),
            _ => None,
        };
        if let Some((kind, place)) = news {
            self.news = Some((
                Callout {
                    kind,
                    count: 1,
                    place,
                },
                view.now,
                due,
            ));
        }
    }
}

/// `sighting`: the bomb carrier in sight, else the place with most enemies.
fn sighting(view: &View, locations: &Locations) -> Option<Callout> {
    if let Some(carrier) = view
        .objective
        .and_then(|objective| objective.carrier.as_deref())
        && let Some((_, at)) = view.enemies.iter().find(|(entity, _)| *entity == carrier)
        && let Some(place) = locations.name_at(*at)
    {
        return Some(Callout {
            kind: Kind::Carrier,
            count: 1,
            place: place.to_owned(),
        });
    }
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for (_, at) in view.enemies {
        if let Some(place) = locations.name_at(*at) {
            match counts.iter_mut().find(|(known, _)| *known == place) {
                Some((_, count)) => *count += 1,
                None => counts.push((place, 1)),
            }
        }
    }
    let (place, count) = counts.into_iter().max_by_key(|(_, count)| *count)?;
    Some(Callout {
        kind: Kind::Enemies,
        count,
        place: place.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locations() -> Locations {
        serde_json::from_str(
            r#"{"map":"test","locations":[
                {"name":"Red Armor","position":[0,0,0]},
                {"name":"Lower Hall","position":[1000,0,0]},
                {"name":"Lower Hall","position":[1000,0,200]}
            ]}"#,
        )
        .unwrap()
    }

    #[test]
    fn calls_and_reads_callouts_like_the_typescript_bot() {
        for line in [
            "/say_team Enemy at Red Armor",
            "/say_team 2 enemies at Lower Hall",
            "/say_team Bomb planted at B",
        ] {
            let call = parse(&format!("[TEAM] {}", line.trim_start_matches("/say_team "))).unwrap();
            assert_eq!(text(&call), line);
        }
        assert_eq!(parse("hello"), None);
        assert_eq!(
            locations().position_of("Lower Hall"),
            Some(Vec3::new(1000.0, 0.0, 100.0))
        );
    }

    #[test]
    fn calls_the_busiest_place_and_paces_itself() {
        let places = locations();
        let mut callouts = Callouts::new(Random::new(1));
        let enemies = [
            ("a", Vec3::new(990.0, 0.0, 10.0)),
            ("b", Vec3::new(1010.0, 0.0, 150.0)),
            ("c", Vec3::new(10.0, 0.0, 0.0)),
        ];
        let at = |seconds: u64| View {
            now: SimTime::ZERO + Duration::from_secs(seconds),
            team: Some("red"),
            enemies: &enemies,
            objective: None,
        };
        assert_eq!(
            callouts.observe(&at(10), &places),
            Some("/say_team 2 enemies at Lower Hall".into())
        );
        // Within 3 s nothing; the same call waits 8 s.
        assert_eq!(callouts.observe(&at(12), &places), None);
        assert_eq!(callouts.observe(&at(14), &places), None);
        assert!(callouts.observe(&at(19), &places).is_some());
        // Outside team play, nothing.
        let mut solo = Callouts::new(Random::new(1));
        assert_eq!(
            solo.observe(
                &View {
                    team: None,
                    ..at(10)
                },
                &places
            ),
            None
        );
    }

    #[test]
    fn tells_the_team_where_the_carried_bomb_went() {
        let places = locations();
        let mut callouts = Callouts::new(Random::new(1));
        let carried = Objective {
            phase: "carried".into(),
            position: Some(Vec3::new(1000.0, 0.0, 0.0)),
            carrier: Some("x".into()),
            site: None,
        };
        let dropped = Objective {
            phase: "dropped".into(),
            ..carried.clone()
        };
        let view = |seconds: u64, objective| View {
            now: SimTime::ZERO + Duration::from_secs(seconds),
            team: Some("red"),
            enemies: &[],
            objective: Some(objective),
        };
        assert_eq!(callouts.observe(&view(10, &carried), &places), None);
        assert_eq!(callouts.observe(&view(10, &dropped), &places), None);
        assert_eq!(
            callouts.observe(&view(13, &dropped), &places),
            Some("/say_team Bomb dropped at Lower Hall".into())
        );
    }
}

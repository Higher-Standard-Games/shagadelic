//! What bots know from earlier matches, from TypeScript `bot/intel/`:
//! `people.json`, how a human's health plus armor typically grows with
//! time alive (`PeopleProfile.ts`), and `<map>.json`, what each strategy
//! paid for each diagnosis per gametype (`IntelBaseline.ts`). Both are
//! checked in under SHAG's `config/bot-intel/`.

use std::{collections::HashMap, io, path::Path};

use serde::Deserialize;

/// `STACK_AGE_BUCKET`: seconds alive per bucket.
const STACK_AGE_BUCKET: f64 = 10.0;

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Quartiles {
    median: f64,
}

/// `PeopleProfile`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct PeopleProfile {
    version: u32,
    #[serde(rename = "stackByAge")]
    stack_by_age: Vec<Quartiles>,
}

impl PeopleProfile {
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        let profile: Self = read(path)?;
        if profile.version != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "people profile version",
            ));
        }
        Ok(profile)
    }

    /// `typicalStackAfter`: the median stack after so long alive,
    /// interpolated between buckets.
    pub(in crate::games::washoi) fn typical_stack_after(&self, seconds: f64) -> Option<f64> {
        let buckets = &self.stack_by_age;
        let last = buckets.len().checked_sub(1)?;
        let at = (seconds / STACK_AGE_BUCKET - 0.5).max(0.0);
        let i = (at.floor() as usize).min(last);
        let next = (i + 1).min(last);
        let f = (at - i as f64).min(1.0);
        Some(buckets[i].median + (buckets[next].median - buckets[i].median) * f)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct Payoff {
    diagnosis: String,
    strategy: String,
    n: u32,
    mean: f64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct GametypeBaseline {
    #[serde(default)]
    payoffs: Vec<Payoff>,
}

/// `BaselineFile`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Baseline {
    version: u32,
    map: String,
    gametypes: HashMap<String, GametypeBaseline>,
}

impl Baseline {
    pub fn load(path: impl AsRef<Path>) -> io::Result<Self> {
        let baseline: Self = read(path)?;
        if baseline.version != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "baseline version",
            ));
        }
        Ok(baseline)
    }

    pub fn map(&self) -> &str {
        &self.map
    }

    /// `payoffPriors`: episodes and mean reward by `diagnosis:strategy`, for
    /// a gametype.
    pub(in crate::games::washoi) fn priors(&self, gametype: &str) -> Vec<(String, (u32, f64))> {
        self.gametypes
            .get(gametype)
            .map(|baseline| {
                baseline
                    .payoffs
                    .iter()
                    .map(|p| (format!("{}:{}", p.diagnosis, p.strategy), (p.n, p.mean)))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn read<T: serde::de::DeserializeOwned>(path: impl AsRef<Path>) -> io::Result<T> {
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_typical_stack_and_the_payoffs() {
        let people: PeopleProfile = serde_json::from_str(
            r#"{"version":1,"source":{"demos":2,"players":2},"stackByAge":[
                {"q1":100,"median":100,"q3":125,"n":1},
                {"q1":125,"median":160,"q3":194,"n":1}]}"#,
        )
        .unwrap();
        assert_eq!(people.typical_stack_after(0.0), Some(100.0));
        assert_eq!(people.typical_stack_after(10.0), Some(130.0));
        assert_eq!(people.typical_stack_after(100.0), Some(160.0));
        let baseline: Baseline = serde_json::from_str(
            r#"{"version":1,"map":"washoi_dm2","gametypes":{"Duel":{"matches":3,"fights":[],
                "payoffs":[{"diagnosis":"stacked","strategy":"pressure","n":12,"mean":0.8}]}}}"#,
        )
        .unwrap();
        assert_eq!(
            baseline.priors("Duel"),
            vec![("stacked:pressure".to_owned(), (12, 0.8))]
        );
        assert!(baseline.priors("Deathmatch").is_empty());
    }
}

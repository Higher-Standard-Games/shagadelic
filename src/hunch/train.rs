//! Fitting a HUNCH model: from labelled decision episodes, which name
//! candidates by ID, or from index-labelled decisions, through one seeded
//! softmax trainer.

use serde::{Deserialize, Serialize};

use super::{FORMAT, Model, Spec};

/// Where an episode's label came from. Kept apart, so an evaluation of
/// human or designer choices never counts rule-made examples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A person playing.
    Human,
    /// An author demonstrating or ranking choices.
    Designer,
    /// The game's own rule, to bootstrap a baseline.
    Rule,
    /// A seat a recording does not confirm a person played, such as a
    /// default name in a demo. Never evidence about people.
    Unverified,
}

/// What an episode's label says, by candidate ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Label {
    /// This candidate was chosen among all those offered.
    Chose { id: String },
    /// Of two candidates shown, this one was preferred. It trains as a
    /// choice between those two alone: nothing is inferred about the
    /// options the reviewer did not compare.
    Preferred { better: String, worse: String },
}

/// One labelled decision: every eligible candidate, by ID, with its feature
/// row, as the game built them at the observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Episode {
    pub decision: String,
    pub schema: String,
    /// The match or recording session it came from. Splits keep a group
    /// together.
    pub group: String,
    pub source: Source,
    /// Candidate IDs, unique within the episode, in row order.
    pub ids: Vec<String>,
    /// One row of the schema's width per ID.
    pub rows: Vec<f32>,
    pub label: Label,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EpisodeError {
    Decision,
    Schema,
    /// Rows do not match the IDs at the schema's width.
    Width,
    DuplicateId(String),
    /// The label names a candidate that was not offered.
    UnknownId(String),
    /// A preference compares a candidate with itself.
    SameCandidate,
}

/// The first unusable episode, by index.
#[derive(Debug, PartialEq, Eq)]
pub struct TrainError {
    pub episode: usize,
    pub error: EpisodeError,
}

impl std::fmt::Display for TrainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "episode {}: {:?}", self.episode, self.error)
    }
}

impl std::error::Error for TrainError {}

impl Episode {
    /// The rows the trainer sees and the index chosen among them.
    pub fn decision(&self, spec: &Spec) -> Result<Decision, EpisodeError> {
        if self.decision != spec.decision {
            return Err(EpisodeError::Decision);
        }
        if self.schema != spec.schema {
            return Err(EpisodeError::Schema);
        }
        let width = spec.width();
        if width == 0 || self.ids.is_empty() || self.rows.len() != self.ids.len() * width {
            return Err(EpisodeError::Width);
        }
        for (index, id) in self.ids.iter().enumerate() {
            if self.ids[..index].contains(id) {
                return Err(EpisodeError::DuplicateId(id.clone()));
            }
        }
        let find = |id: &String| {
            self.ids
                .iter()
                .position(|candidate| candidate == id)
                .ok_or_else(|| EpisodeError::UnknownId(id.clone()))
        };
        let row = |index: usize| &self.rows[index * width..(index + 1) * width];
        match &self.label {
            Label::Chose { id } => Ok(Decision {
                features: self.rows.clone(),
                chosen: find(id)?,
            }),
            Label::Preferred { better, worse } => {
                let (better, worse) = (find(better)?, find(worse)?);
                if better == worse {
                    return Err(EpisodeError::SameCandidate);
                }
                Ok(Decision {
                    features: [row(better), row(worse)].concat(),
                    chosen: 0,
                })
            }
        }
    }
}

/// One decision for training, by row index: its candidates' feature rows
/// and the one chosen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub features: Vec<f32>,
    pub chosen: usize,
}

impl Decision {
    fn candidates(&self, width: usize) -> usize {
        self.features.len() / width
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Training {
    pub epochs: usize,
    pub batch: usize,
    pub rate: f32,
    pub l2: f32,
    pub seed: u64,
}

impl Default for Training {
    fn default() -> Self {
        Self {
            epochs: 200,
            batch: 32,
            rate: 0.1,
            l2: 1e-3,
            seed: 1,
        }
    }
}

/// Fit a model for `spec` from labelled episodes. Every episode must be for
/// the spec's decision and schema, with unique IDs and a label naming
/// candidates it offered.
pub fn train(spec: &Spec, episodes: &[Episode], training: Training) -> Result<Model, TrainError> {
    let decisions = episodes
        .iter()
        .enumerate()
        .map(|(episode, e)| {
            e.decision(spec)
                .map_err(|error| TrainError { episode, error })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(fit(spec, &decisions, training))
}

/// Fit a model for `spec` from index-labelled decisions: normalization from
/// their rows, then seeded minibatch gradient descent on the negative log
/// probability of each chosen candidate under a softmax over its own
/// decision's candidates. Malformed decisions are skipped.
pub fn fit(spec: &Spec, decisions: &[Decision], training: Training) -> Model {
    let width = spec.width();
    let (mean, scale) = normalization(width, decisions);
    let mut model = Model {
        format: FORMAT,
        decision: spec.decision.to_owned(),
        schema: spec.schema.to_owned(),
        features: spec
            .features
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        mean,
        scale,
        weights: vec![0.0; width],
    };
    let usable: Vec<&Decision> = decisions
        .iter()
        .filter(|d| width > 0 && d.features.len() % width == 0 && d.chosen < d.candidates(width))
        .collect();
    if usable.is_empty() {
        return model;
    }
    let mut order: Vec<usize> = (0..usable.len()).collect();
    let mut state = training.seed;
    let mut gradient = vec![0.0f32; width];
    let mut scores = Vec::new();
    for _ in 0..training.epochs {
        shuffle(&mut order, &mut state);
        for batch in order.chunks(training.batch.max(1)) {
            gradient.iter_mut().for_each(|g| *g = 0.0);
            for &index in batch {
                let decision = usable[index];
                let n = decision.candidates(width);
                scores.resize(n, 0.0);
                model.score_into(&decision.features, &mut scores);
                let top = scores.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let total: f32 = scores.iter().map(|s| (s - top).exp()).sum();
                for (candidate, row) in decision.features.chunks_exact(width).enumerate() {
                    let p = (scores[candidate] - top).exp() / total;
                    let target = if candidate == decision.chosen {
                        1.0
                    } else {
                        0.0
                    };
                    for (k, x) in row.iter().enumerate() {
                        let normalized = (x - model.mean[k]) / model.scale[k];
                        gradient[k] += (p - target) * normalized;
                    }
                }
            }
            let n = batch.len() as f32;
            for (weight, g) in model.weights.iter_mut().zip(&gradient) {
                *weight -= training.rate * (g / n + training.l2 * *weight);
            }
        }
    }
    model
}

/// Mean and spread of each feature over every candidate row. A constant
/// feature keeps scale 1.
fn normalization(width: usize, decisions: &[Decision]) -> (Vec<f32>, Vec<f32>) {
    let mut sum = vec![0.0f64; width];
    let mut squares = vec![0.0f64; width];
    let mut rows = 0.0f64;
    for decision in decisions {
        if width == 0 {
            break;
        }
        for row in decision.features.chunks_exact(width) {
            rows += 1.0;
            for (k, x) in row.iter().enumerate() {
                sum[k] += f64::from(*x);
                squares[k] += f64::from(*x) * f64::from(*x);
            }
        }
    }
    let rows = rows.max(1.0);
    let mean: Vec<f64> = sum.iter().map(|s| s / rows).collect();
    let scale = squares
        .iter()
        .zip(&mean)
        .map(|(sq, m)| {
            let spread = (sq / rows - m * m).max(0.0).sqrt();
            if spread > 1e-9 { spread as f32 } else { 1.0 }
        })
        .collect();
    (mean.into_iter().map(|m| m as f32).collect(), scale)
}

/// Fisher-Yates with SplitMix64, so a seed fixes the order.
fn shuffle(order: &mut [usize], state: &mut u64) {
    for i in (1..order.len()).rev() {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        order.swap(i, (z % (i as u64 + 1)) as usize);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::SPEC;
    use super::*;

    fn episode(label: Label) -> Episode {
        Episode {
            decision: SPEC.decision.into(),
            schema: SPEC.schema.into(),
            group: "match-1".into(),
            source: Source::Designer,
            ids: vec!["a".into(), "b".into(), "c".into()],
            rows: vec![0.9, 100.0, 0.1, 50.0, 0.5, 10.0],
            label,
        }
    }

    #[test]
    fn a_preference_trains_on_its_two_candidates_alone() {
        let preferred = episode(Label::Preferred {
            better: "c".into(),
            worse: "a".into(),
        });
        let decision = preferred.decision(&SPEC).unwrap();
        assert_eq!(decision.features, vec![0.5, 10.0, 0.9, 100.0]);
        assert_eq!(decision.chosen, 0);
        let chose = episode(Label::Chose { id: "b".into() });
        assert_eq!(chose.decision(&SPEC).unwrap().chosen, 1);
    }

    #[test]
    fn refuses_episodes_it_cannot_read() {
        let error = |episode: Episode| episode.decision(&SPEC).unwrap_err();
        let mut other = episode(Label::Chose { id: "a".into() });
        other.decision = "test.other".into();
        assert_eq!(error(other), EpisodeError::Decision);
        let mut revised = episode(Label::Chose { id: "a".into() });
        revised.schema = "test.pick.v1".into();
        assert_eq!(error(revised), EpisodeError::Schema);
        let mut short = episode(Label::Chose { id: "a".into() });
        short.rows.pop();
        assert_eq!(error(short), EpisodeError::Width);
        let mut twice = episode(Label::Chose { id: "a".into() });
        twice.ids[2] = "a".into();
        assert_eq!(error(twice), EpisodeError::DuplicateId("a".into()));
        assert_eq!(
            error(episode(Label::Chose { id: "z".into() })),
            EpisodeError::UnknownId("z".into())
        );
        assert_eq!(
            error(episode(Label::Preferred {
                better: "a".into(),
                worse: "a".into()
            })),
            EpisodeError::SameCandidate
        );
        let bad = [
            episode(Label::Chose { id: "a".into() }),
            episode(Label::Chose { id: "z".into() }),
        ];
        assert_eq!(
            train(&SPEC, &bad, Training::default()).unwrap_err(),
            TrainError {
                episode: 1,
                error: EpisodeError::UnknownId("z".into())
            }
        );
    }
}

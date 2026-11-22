//! HUNCH, a small learned decision policy (`docs/research/HUNCH.md`): a
//! linear scorer over the candidates of one decision, the seeded softmax
//! trainer that fits it, and the bindings that say which model each kind of
//! entity uses for each decision. Game-independent.
//!
//! A game declares each decision it lets HUNCH score with a [`Spec`]: a
//! stable decision ID, a feature schema, and the feature names in row order.
//! For one decision it builds one row per candidate, in a stable order, and
//! keeps its own typed actions beside them: score `i` belongs to the
//! caller's candidate `i`. Candidate IDs name candidates in training labels
//! and traces only.
//!
//! Training and scoring share `Model::score_into`, so a trace scores the
//! same in both. Scoring is pure: a model is shared read-only by every
//! caller, and each call brings its own score buffer.

use std::{fmt, io, path::Path, sync::Arc};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

mod bindings;
mod train;

pub use bindings::{Binding, BindingError, Bindings};
pub use train::{Decision, Episode, EpisodeError, Label, Source, TrainError, Training, fit, train};

/// The artifact format this scorer writes: a linear model over normalized
/// features, for one named decision. Format 1, before decisions were named,
/// is still read; its decision is its schema without the revision.
pub const FORMAT: u32 = 2;

/// A decision a game lets HUNCH score.
#[derive(Clone, Copy, Debug)]
pub struct Spec<'a> {
    /// Stable across schema revisions, such as `washoi.goal`.
    pub decision: &'a str,
    /// The feature schema and its revision, such as `washoi.goal.v0`. A
    /// change in any column's meaning takes a new one.
    pub schema: &'a str,
    /// Feature names, in row order.
    pub features: &'a [&'a str],
}

impl Spec<'_> {
    pub fn width(&self) -> usize {
        self.features.len()
    }
}

/// How a binding uses its model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The game's rule decides, and the model is not consulted.
    Off,
    /// The rule decides; the model's choice is recorded beside it.
    #[default]
    Shadow,
    /// The model decides among the candidates the rule's gates let through.
    On,
}

/// A trained scorer, as stored in its artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    /// The artifact format, [`FORMAT`]. Format 1 artifacts call it `version`.
    #[serde(alias = "version")]
    pub format: u32,
    /// The decision this model scores. Format 1 artifacts have none.
    #[serde(default)]
    pub decision: String,
    /// The feature schema it was trained on, such as `washoi.goal.v0`.
    pub schema: String,
    /// Feature names, in row order.
    pub features: Vec<String>,
    /// Normalization fitted on the training split: `(x - mean) / scale`.
    pub mean: Vec<f32>,
    pub scale: Vec<f32>,
    pub weights: Vec<f32>,
}

/// The SHA-256 of an artifact's bytes: identical artifacts share it, and a
/// trace or a session pins a model by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

/// A validated model and the digest of the bytes it came from, shared by
/// every caller.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub model: Arc<Model>,
    pub digest: Digest,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ModelError {
    /// The bytes are not an artifact.
    Parse(String),
    /// The artifact is in a format this scorer does not read.
    Format {
        found: u32,
    },
    /// The artifact scores another decision.
    Decision {
        expected: String,
        found: String,
    },
    Schema {
        expected: String,
        found: String,
    },
    /// The artifact's feature names, in order, are not the game's.
    Features,
    Dimensions,
    NotFinite,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "model artifact unreadable: {error}"),
            Self::Format { found } => write!(f, "model format {found}, expected {FORMAT}"),
            Self::Decision { expected, found } => {
                write!(f, "model decision {found}, expected {expected}")
            }
            Self::Schema { expected, found } => {
                write!(f, "model schema {found}, expected {expected}")
            }
            Self::Features => f.write_str("model feature names differ from the schema's"),
            Self::Dimensions => f.write_str("model dimensions disagree"),
            Self::NotFinite => f.write_str("model has a non-finite parameter"),
        }
    }
}

impl std::error::Error for ModelError {}

impl From<ModelError> for io::Error {
    fn from(error: ModelError) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, error)
    }
}

/// A format 1 artifact's decision: its schema without the trailing
/// revision, `washoi.goal.v0` to `washoi.goal`.
fn legacy_decision(schema: &str) -> &str {
    match schema.rsplit_once('.') {
        Some((decision, revision))
            if revision.len() > 1
                && revision.starts_with('v')
                && revision[1..].bytes().all(|b| b.is_ascii_digit()) =>
        {
            decision
        }
        _ => schema,
    }
}

impl Model {
    /// Read and validate an artifact for `spec` from memory, as an embedded
    /// host supplies it.
    pub fn parse(bytes: &[u8], spec: &Spec) -> Result<Loaded, ModelError> {
        let mut model: Self =
            serde_json::from_slice(bytes).map_err(|error| ModelError::Parse(error.to_string()))?;
        if model.format == 1 && model.decision.is_empty() {
            model.decision = legacy_decision(&model.schema).to_owned();
        }
        model.validate(spec)?;
        Ok(Loaded {
            model: Arc::new(model),
            digest: Digest::of(bytes),
        })
    }

    /// Read and validate an artifact file for `spec`.
    pub fn load(path: impl AsRef<Path>, spec: &Spec) -> io::Result<Loaded> {
        Ok(Self::parse(&std::fs::read(path)?, spec)?)
    }

    /// The artifact's bytes, as `parse` reads them back.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("a model serializes")
    }

    /// Check the artifact against the decision a game declares: format,
    /// decision, schema, feature names in order, dimensions, and finite
    /// parameters.
    pub fn validate(&self, spec: &Spec) -> Result<(), ModelError> {
        if self.format != FORMAT && self.format != 1 {
            return Err(ModelError::Format { found: self.format });
        }
        if self.decision != spec.decision {
            return Err(ModelError::Decision {
                expected: spec.decision.to_owned(),
                found: self.decision.clone(),
            });
        }
        if self.schema != spec.schema {
            return Err(ModelError::Schema {
                expected: spec.schema.to_owned(),
                found: self.schema.clone(),
            });
        }
        if self.features.len() != spec.width()
            || self.features.iter().zip(spec.features).any(|(a, b)| a != b)
        {
            return Err(ModelError::Features);
        }
        let width = self.features.len();
        if width == 0
            || self.mean.len() != width
            || self.scale.len() != width
            || self.weights.len() != width
        {
            return Err(ModelError::Dimensions);
        }
        let finite = |values: &[f32]| values.iter().all(|value| value.is_finite());
        if !finite(&self.mean)
            || !finite(&self.weights)
            || !finite(&self.scale)
            || self.scale.iter().any(|scale| *scale <= 0.0)
        {
            return Err(ModelError::NotFinite);
        }
        Ok(())
    }

    pub fn width(&self) -> usize {
        self.features.len()
    }

    /// Score each candidate. `features` holds one row of `width()` values
    /// per candidate, and `scores` one slot per row. Allocates nothing.
    ///
    /// # Panics
    /// When the buffers disagree with the model's width.
    pub fn score_into(&self, features: &[f32], scores: &mut [f32]) {
        let width = self.width();
        assert_eq!(features.len(), scores.len() * width, "feature rows");
        for (row, score) in features.chunks_exact(width).zip(scores.iter_mut()) {
            *score = row
                .iter()
                .zip(&self.mean)
                .zip(&self.scale)
                .zip(&self.weights)
                .map(|(((x, mean), scale), weight)| (x - mean) / scale * weight)
                .sum();
        }
    }

    /// The best candidate, ties to the lowest index. None when any score is
    /// not finite, as from a non-finite feature: the caller keeps its rule.
    pub fn choose(&self, features: &[f32], scores: &mut [f32]) -> Option<usize> {
        self.score_into(features, scores);
        if !scores.iter().all(|score| score.is_finite()) {
            return None;
        }
        scores
            .iter()
            .enumerate()
            .fold(
                None,
                |best: Option<(usize, f32)>, (index, &score)| match best {
                    Some((_, top)) if top >= score => best,
                    _ => Some((index, score)),
                },
            )
            .map(|(index, _)| index)
    }
}

// A model is shared read-only across entities and threads.
const _: () = {
    const fn shared<T: Send + Sync>() {}
    shared::<Model>();
    shared::<Loaded>();
    shared::<Bindings>();
};

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const NAMES: [&str; 2] = ["value", "distance"];
    pub(crate) const SPEC: Spec<'static> = Spec {
        decision: "test.pick",
        schema: "test.pick.v0",
        features: &NAMES,
    };

    /// A chooser that takes the most valuable candidate, distance aside.
    pub(crate) fn decisions() -> Vec<Decision> {
        let mut state = 7u64;
        let mut next = || {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            ((state >> 33) % 1000) as f32 / 1000.0
        };
        (0..200)
            .map(|_| {
                let rows: Vec<[f32; 2]> = (0..3).map(|_| [next(), next() * 2000.0]).collect();
                let chosen = (0..3)
                    .max_by(|a, b| rows[*a][0].total_cmp(&rows[*b][0]))
                    .unwrap();
                Decision {
                    features: rows.concat(),
                    chosen,
                }
            })
            .collect()
    }

    #[test]
    fn learns_what_the_chooser_wants_and_scores_identically_after_a_round_trip() {
        let data = decisions();
        let model = fit(&SPEC, &data, Training::default());
        assert!(model.weights[0] > 1.0, "{:?}", model.weights);
        assert!(
            model.weights[0] > 5.0 * model.weights[1].abs(),
            "{:?}",
            model.weights
        );
        let mut scores = vec![0.0; 3];
        let agree = data
            .iter()
            .filter(|d| model.choose(&d.features, &mut scores) == Some(d.chosen))
            .count();
        assert!(agree >= 190, "agreed on {agree} of 200");
        // The artifact scores the same after saving and loading.
        let bytes = model.to_bytes();
        let loaded = Model::parse(&bytes, &SPEC).unwrap();
        assert_eq!(*loaded.model, model);
        assert_eq!(loaded.digest, Digest::of(&bytes));
        let mut again = vec![0.0; 3];
        model.score_into(&data[0].features, &mut scores);
        loaded.model.score_into(&data[0].features, &mut again);
        assert_eq!(scores, again);
        // Training is reproducible from its seed.
        assert_eq!(fit(&SPEC, &data, Training::default()), model);
    }

    #[test]
    fn a_non_finite_score_chooses_nothing() {
        let model = fit(&SPEC, &decisions(), Training::default());
        let mut scores = [0.0; 2];
        assert!(model.choose(&[1.0, 0.0, 0.0, 1.0], &mut scores).is_some());
        assert_eq!(model.choose(&[1.0, 0.0, f32::NAN, 1.0], &mut scores), None);
        assert_eq!(
            model.choose(&[f32::INFINITY, 0.0, 0.0, 1.0], &mut scores),
            None
        );
    }

    #[test]
    fn rejects_models_for_another_decision_schema_or_format_or_with_bad_parameters() {
        let model = fit(&SPEC, &decisions(), Training::default());
        let other = Spec {
            decision: "test.other",
            ..SPEC
        };
        assert!(matches!(
            model.validate(&other),
            Err(ModelError::Decision { .. })
        ));
        let revised = Spec {
            schema: "test.pick.v1",
            ..SPEC
        };
        assert!(matches!(
            model.validate(&revised),
            Err(ModelError::Schema { .. })
        ));
        let mut short = model.clone();
        short.weights.pop();
        assert_eq!(short.validate(&SPEC), Err(ModelError::Dimensions));
        let mut renamed = model.clone();
        renamed.features.swap(0, 1);
        assert_eq!(renamed.validate(&SPEC), Err(ModelError::Features));
        let mut later = model.clone();
        later.format = FORMAT + 1;
        assert_eq!(
            later.validate(&SPEC),
            Err(ModelError::Format { found: FORMAT + 1 })
        );
        let mut nan = model;
        nan.weights[0] = f32::NAN;
        assert_eq!(nan.validate(&SPEC), Err(ModelError::NotFinite));
        assert!(matches!(
            Model::parse(b"{}", &SPEC),
            Err(ModelError::Parse(_))
        ));
    }

    /// A format 1 artifact, as `hunch train` wrote it before decisions were
    /// named, still loads for its schema's decision and no other.
    #[test]
    fn reads_a_format_one_artifact_as_its_schemas_decision() {
        let legacy = br#"{"schema":"test.pick.v0","version":1,"features":["value","distance"],
            "mean":[0.5,1000.0],"scale":[0.3,600.0],"weights":[4.0,-0.1]}"#;
        let loaded = Model::parse(legacy, &SPEC).unwrap();
        assert_eq!(loaded.model.decision, "test.pick");
        assert_eq!(loaded.model.format, 1);
        let other = Spec {
            decision: "test.other",
            ..SPEC
        };
        assert!(matches!(
            Model::parse(legacy, &other),
            Err(ModelError::Decision { .. })
        ));
        assert_eq!(legacy_decision("washoi.goal.v0"), "washoi.goal");
        assert_eq!(legacy_decision("washoi.goal"), "washoi.goal");
        assert_eq!(legacy_decision("a.version"), "a.version");
    }
}

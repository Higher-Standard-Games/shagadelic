//! HUNCH, a small learned decision policy (`docs/research/HUNCH.md`): a
//! linear scorer over the candidates of one decision, and the seeded
//! softmax trainer that fits it. Game-independent: a game builds one
//! fixed-order feature vector per candidate under a named schema.
//!
//! Training and scoring share `Model::score_into`, so a trace scores the
//! same in both.

use std::{io, path::Path};

use serde::{Deserialize, Serialize};

/// A trained scorer, as stored in its artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    /// The feature schema the game builds, such as `washoi.goal.v0`.
    pub schema: String,
    pub version: u32,
    /// Feature names, in vector order.
    pub features: Vec<String>,
    /// Normalization fitted on the training split: `(x - mean) / scale`.
    pub mean: Vec<f32>,
    pub scale: Vec<f32>,
    pub weights: Vec<f32>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ModelError {
    Schema {
        expected: String,
        found: String,
    },
    /// The artifact's feature names, in order, are not the game's.
    Features,
    Dimensions,
    NotFinite,
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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

impl Model {
    /// Load and validate an artifact for a schema the caller builds.
    pub fn load(path: impl AsRef<Path>, schema: &str) -> io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let model: Self = serde_json::from_str(&text)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        model
            .validate(schema)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(model)
    }

    pub fn validate(&self, schema: &str) -> Result<(), ModelError> {
        if self.schema != schema {
            return Err(ModelError::Schema {
                expected: schema.to_owned(),
                found: self.schema.clone(),
            });
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

    /// Check the artifact's feature names, in order, against the names the
    /// game builds: a reordered or renamed column must not load silently.
    pub fn validate_features(&self, names: &[&str]) -> Result<(), ModelError> {
        if self.features.len() == names.len()
            && self.features.iter().zip(names).all(|(a, b)| a == b)
        {
            Ok(())
        } else {
            Err(ModelError::Features)
        }
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

    /// The best candidate, ties to the lowest index.
    pub fn choose(&self, features: &[f32], scores: &mut [f32]) -> Option<usize> {
        self.score_into(features, scores);
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

/// One decision for training: its candidates' feature rows and the one
/// chosen.
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

/// Fit a model: normalization from the training decisions, then seeded
/// minibatch gradient descent on the negative log probability of each
/// chosen candidate under a softmax over its own decision's candidates.
pub fn train(
    schema: &str,
    features: Vec<String>,
    decisions: &[Decision],
    training: Training,
) -> Model {
    let width = features.len();
    let (mean, scale) = normalization(width, decisions);
    let mut model = Model {
        schema: schema.to_owned(),
        version: 1,
        features,
        mean,
        scale,
        weights: vec![0.0; width],
    };
    let usable: Vec<&Decision> = decisions
        .iter()
        .filter(|d| d.features.len() % width == 0 && d.chosen < d.candidates(width))
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
    use super::*;

    fn names() -> Vec<String> {
        vec!["value".into(), "distance".into()]
    }

    /// A chooser that takes the most valuable candidate, distance aside.
    fn decisions() -> Vec<Decision> {
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
        let model = train("test.v0", names(), &data, Training::default());
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
        let text = serde_json::to_string(&model).unwrap();
        let loaded: Model = serde_json::from_str(&text).unwrap();
        loaded.validate("test.v0").unwrap();
        let mut again = vec![0.0; 3];
        model.score_into(&data[0].features, &mut scores);
        loaded.score_into(&data[0].features, &mut again);
        assert_eq!(scores, again);
        // Training is reproducible from its seed.
        assert_eq!(train("test.v0", names(), &data, Training::default()), model);
    }

    #[test]
    fn rejects_models_for_another_schema_or_with_bad_parameters() {
        let model = train("test.v0", names(), &decisions(), Training::default());
        assert!(matches!(
            model.validate("washoi.goal.v0"),
            Err(ModelError::Schema { .. })
        ));
        let mut short = model.clone();
        short.weights.pop();
        assert_eq!(short.validate("test.v0"), Err(ModelError::Dimensions));
        let mut renamed = model.clone();
        renamed.features.swap(0, 1);
        assert_eq!(
            renamed.validate_features(&["value", "distance"]),
            Err(ModelError::Features)
        );
        assert_eq!(model.validate_features(&["value", "distance"]), Ok(()));
        let mut nan = model;
        nan.weights[0] = f32::NAN;
        assert_eq!(nan.validate("test.v0"), Err(ModelError::NotFinite));
    }
}

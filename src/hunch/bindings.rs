//! Which model each kind of entity uses for each decision, pinned for a
//! session (`docs/research/HUNCH.md`, "Binding, compatibility, and
//! ownership"). An archetype's own binding wins, then the decision's
//! default, then the game's rule. An explicit off binding selects the rule.

use std::{collections::BTreeMap, sync::Arc};

use super::{Digest, Loaded, Mode, Model};

/// One entry of a session's configuration.
#[derive(Clone, Debug)]
pub struct Binding {
    /// The kind of entity it applies to, or every kind for the decision's
    /// default.
    pub archetype: Option<String>,
    pub decision: String,
    pub mode: Mode,
    /// Required in shadow and on modes; ignored when off.
    pub model: Option<Loaded>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BindingError {
    /// Two bindings for the same archetype, or two defaults, for a decision.
    Duplicate {
        archetype: Option<String>,
        decision: String,
    },
    /// A shadow or on binding without a model.
    MissingModel { decision: String },
    /// The model scores another decision than the binding names.
    Mismatch { decision: String, model: String },
}

impl std::fmt::Display for BindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Duplicate {
                archetype: Some(archetype),
                decision,
            } => write!(f, "two HUNCH bindings for {archetype} on {decision}"),
            Self::Duplicate {
                archetype: None,
                decision,
            } => write!(f, "two default HUNCH bindings for {decision}"),
            Self::MissingModel { decision } => {
                write!(f, "HUNCH binding for {decision} has no model")
            }
            Self::Mismatch { decision, model } => {
                write!(f, "HUNCH binding for {decision} names a model of {model}")
            }
        }
    }
}

impl std::error::Error for BindingError {}

/// What a binding resolves to: off, or a mode and its model.
type Resolved = Option<(Mode, Loaded)>;

/// A session's bindings, fixed at initialization.
#[derive(Clone, Debug, Default)]
pub struct Bindings {
    defaults: BTreeMap<String, Resolved>,
    overrides: BTreeMap<(String, String), Resolved>,
}

impl Bindings {
    /// Check and pin a session's bindings. Identical artifacts, by digest,
    /// share one model.
    pub fn new(bindings: Vec<Binding>) -> Result<Self, BindingError> {
        let mut shared: BTreeMap<Digest, Arc<Model>> = BTreeMap::new();
        let mut pinned = Self::default();
        for binding in bindings {
            let resolved = match binding.mode {
                Mode::Off => None,
                mode => {
                    let Some(loaded) = binding.model else {
                        return Err(BindingError::MissingModel {
                            decision: binding.decision,
                        });
                    };
                    if loaded.model.decision != binding.decision {
                        return Err(BindingError::Mismatch {
                            decision: binding.decision,
                            model: loaded.model.decision.clone(),
                        });
                    }
                    let model = shared
                        .entry(loaded.digest)
                        .or_insert_with(|| loaded.model.clone())
                        .clone();
                    Some((
                        mode,
                        Loaded {
                            model,
                            digest: loaded.digest,
                        },
                    ))
                }
            };
            let duplicate = || BindingError::Duplicate {
                archetype: binding.archetype.clone(),
                decision: binding.decision.clone(),
            };
            match &binding.archetype {
                None => {
                    if pinned.defaults.contains_key(&binding.decision) {
                        return Err(duplicate());
                    }
                    pinned.defaults.insert(binding.decision.clone(), resolved);
                }
                Some(archetype) => {
                    let key = (archetype.clone(), binding.decision.clone());
                    if pinned.overrides.contains_key(&key) {
                        return Err(duplicate());
                    }
                    pinned.overrides.insert(key, resolved);
                }
            }
        }
        Ok(pinned)
    }

    /// The mode and model `archetype` uses for `decision`, or `None` when
    /// the game's rule decides alone.
    pub fn resolve(&self, archetype: &str, decision: &str) -> Option<(Mode, &Loaded)> {
        let resolved = self
            .overrides
            .get(&(archetype.to_owned(), decision.to_owned()))
            .or_else(|| self.defaults.get(decision))?;
        resolved.as_ref().map(|(mode, loaded)| (*mode, loaded))
    }

    /// The distinct models pinned, by digest.
    pub fn models(&self) -> usize {
        let mut digests: Vec<Digest> = self
            .defaults
            .values()
            .chain(self.overrides.values())
            .flatten()
            .map(|(_, loaded)| loaded.digest)
            .collect();
        digests.sort();
        digests.dedup();
        digests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defaults.is_empty() && self.overrides.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Training, fit, tests::*};
    use super::*;

    fn loaded() -> Loaded {
        let bytes = fit(&SPEC, &decisions(), Training::default()).to_bytes();
        Model::parse(&bytes, &SPEC).unwrap()
    }

    fn binding(archetype: Option<&str>, mode: Mode, model: Option<Loaded>) -> Binding {
        Binding {
            archetype: archetype.map(str::to_owned),
            decision: SPEC.decision.into(),
            mode,
            model,
        }
    }

    #[test]
    fn an_archetype_binding_wins_then_the_default_then_the_rule() {
        let model = loaded();
        let bindings = Bindings::new(vec![
            binding(None, Mode::Shadow, Some(model.clone())),
            binding(Some("sniper"), Mode::On, Some(model.clone())),
            binding(Some("turret"), Mode::Off, None),
        ])
        .unwrap();
        let mode = |archetype: &str| bindings.resolve(archetype, SPEC.decision).map(|(m, _)| m);
        assert_eq!(mode("grunt"), Some(Mode::Shadow));
        assert_eq!(mode("sniper"), Some(Mode::On));
        assert_eq!(mode("turret"), None, "explicit off selects the rule");
        assert_eq!(
            bindings.resolve("grunt", "test.other").map(|(m, _)| m),
            None
        );
        // One artifact, loaded twice, is one model.
        assert_eq!(bindings.models(), 1);
        let (_, a) = bindings.resolve("grunt", SPEC.decision).unwrap();
        let (_, b) = bindings.resolve("sniper", SPEC.decision).unwrap();
        assert!(Arc::ptr_eq(&a.model, &b.model));
    }

    #[test]
    fn refuses_duplicates_a_missing_model_and_another_decisions_model() {
        let model = loaded();
        assert!(matches!(
            Bindings::new(vec![
                binding(None, Mode::Shadow, Some(model.clone())),
                binding(None, Mode::Off, None),
            ]),
            Err(BindingError::Duplicate {
                archetype: None,
                ..
            })
        ));
        assert!(matches!(
            Bindings::new(vec![
                binding(Some("a"), Mode::On, Some(model.clone())),
                binding(Some("a"), Mode::Shadow, Some(model.clone())),
            ]),
            Err(BindingError::Duplicate {
                archetype: Some(_),
                ..
            })
        ));
        assert!(matches!(
            Bindings::new(vec![binding(None, Mode::On, None)]),
            Err(BindingError::MissingModel { .. })
        ));
        let mut elsewhere = binding(None, Mode::On, Some(model));
        elsewhere.decision = "test.other".into();
        assert!(matches!(
            Bindings::new(vec![elsewhere]),
            Err(BindingError::Mismatch { .. })
        ));
    }
}

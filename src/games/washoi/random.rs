//! Per-bot random draws. TypeScript uses `Math.random`. This is SplitMix64,
//! seeded per bot, and tests fix the seed.

use std::hash::{BuildHasher, RandomState};

#[derive(Default)]
pub(in crate::games::washoi) struct Random(u64);

impl Random {
    #[cfg(test)]
    pub(in crate::games::washoi) fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// A seed that differs between bots and between runs.
    pub(in crate::games::washoi) fn for_bot(entity_id: &str) -> Self {
        Self(RandomState::new().hash_one(entity_id))
    }

    /// Uniform in [0, 1).
    pub(in crate::games::washoi) fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_are_uniform_and_repeatable() {
        let mut a = Random::new(7);
        let mut b = Random::new(7);
        let draws: Vec<f64> = (0..10_000).map(|_| a.next()).collect();
        assert!(draws.iter().all(|&x| (0.0..1.0).contains(&x)));
        let mean = draws.iter().sum::<f64>() / draws.len() as f64;
        assert!((mean - 0.5).abs() < 0.02);
        assert_eq!(draws[0], b.next());
    }
}

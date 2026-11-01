//! Deterministic, seeded sampling.
//!
//! Uses SplitMix64 and Floyd's algorithm, both implemented here rather than
//! taken from a crate, so a given seed selects the same items forever
//! regardless of dependency upgrades. That makes a sample reproducible from
//! the seed recorded in its manifest.

use std::collections::BTreeSet;

/// SplitMix64 PRNG (Steele, Lea, Flood 2014).
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform integer in `[0, bound)`, without modulo bias.
    pub fn below(&mut self, bound: u64) -> u64 {
        assert!(bound > 0);
        let zone = u64::MAX - (u64::MAX % bound);
        loop {
            let x = self.next_u64();
            if x < zone {
                return x % bound;
            }
        }
    }
}

/// Choose `k` distinct integers uniformly from `[0, n)`, returned sorted.
/// Floyd's algorithm: O(k) time and memory, independent of `n`.
pub fn sample_indices(n: u64, k: u64, seed: u64) -> Vec<u64> {
    assert!(k <= n, "cannot sample {k} items from {n}");
    let mut rng = SplitMix64::new(seed);
    let mut chosen = BTreeSet::new();
    for j in (n - k)..n {
        let t = rng.below(j + 1);
        if !chosen.insert(t) {
            chosen.insert(j);
        }
    }
    chosen.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_distinct_sorted_in_range() {
        let a = sample_indices(3_600_000, 3_907, 786);
        let b = sample_indices(3_600_000, 3_907, 786);
        assert_eq!(a, b, "same seed must give same sample");
        assert_eq!(a.len(), 3_907);
        assert!(a.windows(2).all(|w| w[0] < w[1]));
        assert!(*a.last().unwrap() < 3_600_000);
        assert_ne!(a, sample_indices(3_600_000, 3_907, 787));
    }

    #[test]
    fn known_first_output() {
        // Reference value for SplitMix64 with seed 0.
        assert_eq!(SplitMix64::new(0).next_u64(), 0xE220_A839_7B1D_CDAF);
    }

    #[test]
    fn spread_across_population() {
        // A uniform sample should land in every tenth of the range.
        let s = sample_indices(1_000_000, 1_000, 1);
        for decile in 0..10u64 {
            let lo = decile * 100_000;
            assert!(s.iter().any(|&x| x >= lo && x < lo + 100_000));
        }
    }

    #[test]
    fn full_sample_is_everything() {
        assert_eq!(sample_indices(5, 5, 9), vec![0, 1, 2, 3, 4]);
    }
}

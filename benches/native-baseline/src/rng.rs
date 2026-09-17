//! Deterministic pseudo-randomness for fixture synthesis.
//!
//! The harness never touches the OS RNG. A fixture repository is a pure function
//! of its [`FixtureSpec`](crate::fixture::FixtureSpec), so two machines that build
//! the same spec get byte-identical trees — and therefore identical commit oids,
//! which is what makes a cross-machine benchmark comparison honest.
//!
//! SplitMix64 is used because it is three arithmetic rounds over a single `u64`:
//! no dependency, no platform drift, stable forever.

/// A SplitMix64 generator (Steele/Lea/Flood, public-domain algorithm).
///
/// Statistical quality is irrelevant here; *reproducibility* is the whole point.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Seed the generator.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Seed from several components (spec seed, file index, revision, …) so that
    /// each generated artifact has its own reproducible stream.
    #[must_use]
    pub fn mixed(parts: &[u64]) -> Self {
        let mut acc: u64 = 0x243F_6A88_85A3_08D3;
        for p in parts {
            acc ^= *p;
            let mut s = Self::new(acc);
            acc = s.next_u64();
        }
        Self::new(acc)
    }

    /// The next value in the stream.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound` (`0` when `bound == 0`).
    ///
    /// Modulo bias is accepted: this picks which fixture file to edit, not a key.
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            (self.next_u64() % bound as u64) as usize
        }
    }
}

/// FNV-1a 64 over `bytes`, lowercase hex.
///
/// Cache-key strength only — it names a fixture directory. Integrity hashing in
/// this repo is `rift-hashchain`'s job and never this function's.
#[must_use]
pub fn fnv1a64_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix64_matches_the_reference_stream() {
        // Reference vectors for seed 0 (state incremented before output).
        let mut r = SplitMix64::new(0);
        assert_eq!(r.next_u64(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(r.next_u64(), 0x6E78_9E6A_A1B9_65F4);
        assert_eq!(r.next_u64(), 0x06C4_5D18_8009_454F);
    }

    #[test]
    fn same_seed_replays_and_different_seeds_diverge() {
        let take = |seed| {
            let mut r = SplitMix64::new(seed);
            (0..8).map(|_| r.next_u64()).collect::<Vec<_>>()
        };
        assert_eq!(take(7), take(7));
        assert_ne!(take(7), take(8));
    }

    #[test]
    fn mixed_is_order_sensitive_and_reproducible() {
        assert_eq!(
            SplitMix64::mixed(&[1, 2, 3]).next_u64(),
            SplitMix64::mixed(&[1, 2, 3]).next_u64()
        );
        assert_ne!(
            SplitMix64::mixed(&[1, 2, 3]).next_u64(),
            SplitMix64::mixed(&[3, 2, 1]).next_u64()
        );
    }

    #[test]
    fn below_is_in_range_and_total() {
        let mut r = SplitMix64::new(42);
        assert_eq!(r.below(0), 0);
        for _ in 0..64 {
            assert!(r.below(5) < 5);
        }
    }

    #[test]
    fn fnv1a64_matches_published_vectors() {
        assert_eq!(fnv1a64_hex(b""), "cbf29ce484222325");
        assert_eq!(fnv1a64_hex(b"a"), "af63dc4c8601ec8c");
        assert_eq!(fnv1a64_hex(b"foobar"), "85944171f73967e8");
    }
}

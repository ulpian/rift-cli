//! A small seeded PRNG (PCG32) for the splash animation.
//!
//! The splash must be **deterministic** for a given seed — that is what makes the
//! crack reproducible across runs and lets the skip path jump to an identical
//! final frame (06 §6.3: no global/thread RNG). PCG-XSH-RR is ~30 lines, so we
//! hand-roll it rather than pull a `rand` dependency the workspace doesn't carry.

/// A PCG32 (PCG-XSH-RR 64/32) generator. Seeded and injectable; never reads a
/// global RNG, so the same seed yields the same sequence on every run.
#[derive(Clone, Debug)]
pub struct Pcg32 {
    state: u64,
    inc: u64,
}

/// The canonical PCG32 multiplier.
const MUL: u64 = 6_364_136_223_846_793_005;

impl Pcg32 {
    /// Seed the generator. `seq` selects the output stream (any value).
    #[must_use]
    pub fn seed(seed: u64, seq: u64) -> Self {
        let mut g = Self {
            state: 0,
            inc: (seq << 1) | 1,
        };
        let _ = g.next_u32();
        g.state = g.state.wrapping_add(seed);
        let _ = g.next_u32();
        g
    }

    /// The next pseudo-random `u32`.
    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(MUL).wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// `true` with probability `p` (clamped to `[0, 1]`).
    pub fn gen_bool(&mut self, p: f32) -> bool {
        let p = p.clamp(0.0, 1.0);
        (self.next_u32() as f32 / u32::MAX as f32) < p
    }

    /// A value in `0..n` (`0` when `n == 0`).
    pub fn below(&mut self, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            self.next_u32() % n
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Pcg32::seed(42, 1);
        let mut b = Pcg32::seed(42, 1);
        for _ in 0..1000 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
    }

    #[test]
    fn different_seeds_differ() {
        let mut a = Pcg32::seed(1, 1);
        let mut b = Pcg32::seed(2, 1);
        let da: Vec<u32> = (0..16).map(|_| a.next_u32()).collect();
        let db: Vec<u32> = (0..16).map(|_| b.next_u32()).collect();
        assert_ne!(da, db);
    }

    #[test]
    fn gen_bool_extremes_are_deterministic() {
        let mut g = Pcg32::seed(7, 7);
        assert!(!g.gen_bool(0.0), "p=0 never true");
        assert!(g.gen_bool(1.0), "p=1 always true");
    }

    #[test]
    fn below_stays_in_range() {
        let mut g = Pcg32::seed(9, 3);
        for _ in 0..1000 {
            assert!(g.below(5) < 5);
        }
        assert_eq!(g.below(0), 0);
    }
}

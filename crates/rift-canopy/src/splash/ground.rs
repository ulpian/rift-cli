//! The 3D ground-crack model — deterministic, seeded fracture propagation.
//!
//! The propagation is **independent of the rasterizer**: the same [`CrackField`]
//! drives the Full3d voxel raycast (future, behind `xre-3d`), the 2D Unicode
//! crack, and the static tier — only fidelity changes. It is seeded (no global
//! RNG), terminates within a bounded tick count, and its depth easing is
//! monotone, so the skip path can jump to an identical final frame.

use super::prng::Pcg32;

/// A packed grid of bits (which cells have fractured). Bounds-checked: an
/// out-of-range access is a no-op / `false`, never a panic.
#[derive(Clone, Debug)]
pub struct BitGrid {
    w: u16,
    h: u16,
    words: Vec<u64>,
}

impl BitGrid {
    /// A new all-clear grid of `w × h`.
    #[must_use]
    pub fn new(w: u16, h: u16) -> Self {
        let bits = usize::from(w) * usize::from(h);
        Self {
            w,
            h,
            words: vec![0; bits.div_ceil(64)],
        }
    }

    fn index(&self, x: u16, y: u16) -> Option<usize> {
        if x < self.w && y < self.h {
            Some(usize::from(y) * usize::from(self.w) + usize::from(x))
        } else {
            None
        }
    }

    /// Whether `(x, y)` is set.
    #[must_use]
    #[allow(clippy::indexing_slicing)] // index(x, y) yields an in-bounds bit index
    pub fn get(&self, x: u16, y: u16) -> bool {
        match self.index(x, y) {
            Some(i) => (self.words[i / 64] >> (i % 64)) & 1 == 1,
            None => false,
        }
    }

    /// Set `(x, y)`. Out-of-bounds is a no-op.
    #[allow(clippy::indexing_slicing)] // index(x, y) yields an in-bounds bit index
    pub fn set(&mut self, x: u16, y: u16) {
        if let Some(i) = self.index(x, y) {
            self.words[i / 64] |= 1 << (i % 64);
        }
    }

    /// The number of set cells.
    #[must_use]
    pub fn count(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }
}

/// The probability a frontier cell extends to a given neighbor each tick. < 1 so
/// the crack is a fracture network that grows then dies, not a flood fill.
const ADVANCE_P: f32 = 0.55;

/// The seeded crack-propagation state.
#[derive(Clone, Debug)]
pub struct CrackField {
    w: u16,
    h: u16,
    front: Vec<(u16, u16)>,
    opened: BitGrid,
    rng: Pcg32,
    ticks: u32,
}

impl CrackField {
    /// A fresh crack seeded at `origin` (clamped into the grid).
    #[must_use]
    pub fn new(w: u16, h: u16, origin: (u16, u16), seed: u64) -> Self {
        let mut opened = BitGrid::new(w, h);
        let ox = origin.0.min(w.saturating_sub(1));
        let oy = origin.1.min(h.saturating_sub(1));
        let mut front = Vec::new();
        if w > 0 && h > 0 {
            opened.set(ox, oy);
            front.push((ox, oy));
        }
        Self {
            w,
            h,
            front,
            opened,
            // A fixed stream constant keeps runs reproducible for a given seed.
            rng: Pcg32::seed(seed, 0xda3e_39cb_94b9_5bdb),
            ticks: 0,
        }
    }

    /// Advance one tick: each frontier cell may extend to its un-opened neighbors
    /// and, with `branch_p`, fork to one extra neighbor — a fracture, not a flood.
    pub fn step(&mut self, branch_p: f32) {
        let mut next = Vec::new();
        let front = std::mem::take(&mut self.front);
        for (cx, cy) in front {
            for (nx, ny) in neighbors(cx, cy, self.w, self.h) {
                if !self.opened.get(nx, ny) && self.rng.gen_bool(ADVANCE_P) {
                    self.opened.set(nx, ny);
                    next.push((nx, ny));
                    if self.rng.gen_bool(branch_p) {
                        if let Some((bx, by)) = self.pick_unopened_neighbor(nx, ny) {
                            self.opened.set(bx, by);
                            next.push((bx, by));
                        }
                    }
                }
            }
        }
        self.front = next;
        self.ticks = self.ticks.saturating_add(1);
    }

    /// Pick one still-closed neighbor of `(x, y)`, deterministically by seed.
    fn pick_unopened_neighbor(&mut self, x: u16, y: u16) -> Option<(u16, u16)> {
        let candidates: Vec<(u16, u16)> = neighbors(x, y, self.w, self.h)
            .into_iter()
            .filter(|&(nx, ny)| !self.opened.get(nx, ny))
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let i = self.rng.below(candidates.len() as u32) as usize;
        candidates.get(i).copied()
    }

    /// `true` once the frontier has died out (the crack has settled).
    #[must_use]
    pub fn done(&self) -> bool {
        self.front.is_empty()
    }

    /// The number of ticks advanced so far.
    #[must_use]
    pub fn ticks(&self) -> u32 {
        self.ticks
    }

    /// The current frontier size (for the bounded-memory property).
    #[must_use]
    pub fn frontier_len(&self) -> usize {
        self.front.len()
    }

    /// Whether `(x, y)` has fractured.
    #[must_use]
    pub fn is_open(&self, x: u16, y: u16) -> bool {
        self.opened.get(x, y)
    }

    /// The number of fractured cells.
    #[must_use]
    pub fn open_count(&self) -> usize {
        self.opened.count()
    }

    /// Grid width.
    #[must_use]
    pub fn width(&self) -> u16 {
        self.w
    }

    /// Grid height.
    #[must_use]
    pub fn height(&self) -> u16 {
        self.h
    }

    /// The sink depth of `(x, y)` at animation time `t ∈ [0, 1]`: `0` for a closed
    /// cell, else an ease-in that is **monotone non-decreasing** in `t` (opened
    /// cells only ever sink, never rise).
    #[must_use]
    pub fn depth_at(&self, x: u16, y: u16, t: f32) -> f32 {
        if self.is_open(x, y) {
            let t = t.clamp(0.0, 1.0);
            t * t // ease-in
        } else {
            0.0
        }
    }

    /// Run the crack to completion (frontier empty), bounded by `max_ticks`.
    /// Returns the number of ticks taken. Used by the deterministic final frame
    /// and by tests.
    pub fn run_to_completion(&mut self, branch_p: f32, max_ticks: u32) -> u32 {
        while !self.done() && self.ticks < max_ticks {
            self.step(branch_p);
        }
        self.ticks
    }
}

/// The in-bounds 4-connected neighbors of `(x, y)`.
fn neighbors(x: u16, y: u16, w: u16, h: u16) -> Vec<(u16, u16)> {
    let mut out = Vec::with_capacity(4);
    if x + 1 < w {
        out.push((x + 1, y));
    }
    if x > 0 {
        out.push((x - 1, y));
    }
    if y + 1 < h {
        out.push((x, y + 1));
    }
    if y > 0 {
        out.push((x, y - 1));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn propagation_is_deterministic_for_seed() {
        let mut a = CrackField::new(40, 20, (20, 10), 123);
        let mut b = CrackField::new(40, 20, (20, 10), 123);
        for _ in 0..50 {
            a.step(0.2);
            b.step(0.2);
            // Identical opened set every tick — the parity the skip relies on.
            for y in 0..20 {
                for x in 0..40 {
                    assert_eq!(a.is_open(x, y), b.is_open(x, y));
                }
            }
        }
    }

    #[test]
    fn eventually_terminates() {
        // For any seed, the frontier empties within a bounded tick count.
        for seed in 0..32u64 {
            let mut c = CrackField::new(48, 24, (24, 12), seed);
            let ticks = c.run_to_completion(0.2, 48 * 24);
            assert!(c.done(), "seed {seed} did not terminate (ticks={ticks})");
            assert!(ticks <= 48 * 24);
        }
    }

    #[test]
    fn depth_easing_is_monotonic() {
        let mut c = CrackField::new(20, 10, (10, 5), 5);
        c.run_to_completion(0.2, 200);
        // Find an opened cell and assert depth only rises with t.
        let mut found = false;
        for y in 0..10 {
            for x in 0..20 {
                if c.is_open(x, y) {
                    found = true;
                    let mut prev = -1.0;
                    for i in 0..=10 {
                        let t = i as f32 / 10.0;
                        let d = c.depth_at(x, y, t);
                        assert!(d >= prev, "depth must not decrease");
                        prev = d;
                    }
                }
            }
        }
        assert!(found, "expected at least one opened cell");
    }

    #[test]
    fn closed_cell_has_zero_depth() {
        let c = CrackField::new(10, 10, (5, 5), 1);
        // A far corner is unlikely opened at construction (only the origin is).
        assert_eq!(c.depth_at(0, 0, 1.0), 0.0);
        assert!(c.is_open(5, 5));
    }

    #[test]
    fn tiny_and_zero_grids_never_panic() {
        for (w, h) in [(0u16, 0u16), (1, 1), (2, 1), (1, 3)] {
            let mut c = CrackField::new(w, h, (0, 0), 2);
            let _ = c.run_to_completion(0.3, 32);
            let _ = c.depth_at(0, 0, 0.5);
            let _ = c.is_open(5, 5); // out of range → false, no panic
        }
    }

    proptest::proptest! {
        #[test]
        fn indices_stay_in_bounds_and_memory_bounded(
            w in 1u16..64, h in 1u16..32, seed in 0u64..1000,
        ) {
            let mut c = CrackField::new(w, h, (w / 2, h / 2), seed);
            let cap = u32::from(w) * u32::from(h);
            c.run_to_completion(0.25, cap);
            // Frontier never exceeds the grid; open count never exceeds capacity.
            proptest::prop_assert!(c.frontier_len() <= usize::from(w) * usize::from(h));
            proptest::prop_assert!(c.open_count() <= usize::from(w) * usize::from(h));
        }
    }
}

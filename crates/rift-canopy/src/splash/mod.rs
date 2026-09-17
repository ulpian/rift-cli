//! The startup splash: a deterministic, seeded animation with a fallback ladder.
//!
//! The production splash is a 3D ground-cracking animation with a ramp-shaded
//! "Rift" wordmark rising from it. This module ships the **deterministic core**
//! ([`ground`] crack propagation, [`wordmark`] reveal/shade), the [`mesh3d`]
//! Full3d raster (the mesh splash tier: the ground meshed from the crack field, the
//! wordmark extruded, density-resolved to cells), and the [`director`] that
//! times it, picks a fidelity [`director::Tier`], makes it skippable, holds
//! the Full3d tier to its [`director::FRAME_BUDGET`] (resolution downgrades
//! first, then the tier), and plays it first-run-only — all rendered into
//! Canopy's [`crate::render::Frame`]. The math is feature-free; an `xre-3d`
//! build is what lets live capability detection advertise the 3D tier.
//!
//! [`SplashState`] is the small frame-counter the dashboard model still carries
//! (the in-`App` "is the splash done" flag the key reducer dismisses).

pub mod director;
pub mod ground;
pub mod mesh3d;
pub mod prefs;
pub mod prng;
pub mod render2d;
pub mod wordmark;

pub use director::{
    choose_tier, play_live, run_splash, BudgetLadder, Downgrade, SplashConfig, SplashMode, Tier,
    FRAME_BUDGET,
};
pub use mesh3d::{Resolution, ScenePoint};

/// Total animation frames before the legacy splash timeline auto-completes.
pub const TOTAL_FRAMES: u32 = 48;

/// The splash timeline state carried by [`crate::App`] (the "done" flag the key
/// reducer dismisses; the live animation is driven by the [`director`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplashState {
    frame: u32,
    skipped: bool,
}

impl Default for SplashState {
    fn default() -> Self {
        Self::new()
    }
}

impl SplashState {
    /// A fresh splash (frame 0).
    #[must_use]
    pub fn new() -> Self {
        Self {
            frame: 0,
            skipped: false,
        }
    }

    /// Advance one frame.
    pub fn tick(&mut self) {
        if self.frame < TOTAL_FRAMES {
            self.frame += 1;
        }
    }

    /// Skip straight to completion (e.g. a key press).
    pub fn skip(&mut self) {
        self.skipped = true;
        self.frame = TOTAL_FRAMES;
    }

    /// Whether the splash has finished (played out or skipped).
    #[must_use]
    pub fn done(&self) -> bool {
        self.skipped || self.frame >= TOTAL_FRAMES
    }

    /// Animation progress in `[0.0, 1.0]`.
    #[must_use]
    pub fn progress(&self) -> f32 {
        f64::from(self.frame) as f32 / TOTAL_FRAMES as f32
    }

    /// The current frame index.
    #[must_use]
    pub fn frame(&self) -> u32 {
        self.frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_out_then_done() {
        let mut s = SplashState::new();
        assert!(!s.done());
        for _ in 0..TOTAL_FRAMES {
            s.tick();
        }
        assert!(s.done());
        assert!((s.progress() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn skip_completes_immediately() {
        let mut s = SplashState::new();
        s.skip();
        assert!(s.done());
    }

    #[test]
    fn tick_saturates_at_total() {
        let mut s = SplashState::new();
        for _ in 0..(TOTAL_FRAMES + 10) {
            s.tick();
        }
        assert_eq!(s.frame(), TOTAL_FRAMES);
    }
}

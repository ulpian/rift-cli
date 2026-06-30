//! The startup splash timeline.
//!
//! The production splash is a 3D ground-cracking animation with a ramp-shader
//! "Rift" title rendered by `xre-rs`; this module models its **timeline** (frame
//! progression, completion, skip, and the capability fallback) as pure state, so
//! timing and the ASCII fallback are testable without a GPU/terminal.

/// Total animation frames before the splash auto-completes.
pub const TOTAL_FRAMES: u32 = 48;

/// The splash timeline state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplashState {
    frame: u32,
    skipped: bool,
    /// Whether the rich 3D backend is available (else ASCII fallback splash).
    pub rich: bool,
}

impl Default for SplashState {
    fn default() -> Self {
        Self::new()
    }
}

impl SplashState {
    /// A fresh splash (frame 0, rich backend assumed until told otherwise).
    #[must_use]
    pub fn new() -> Self {
        Self {
            frame: 0,
            skipped: false,
            rich: true,
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

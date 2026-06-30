//! Optional: the context-window % meter ("nic-gauge").
//!
//! Computes how full an agent thread's context window is from rift-shadow's
//! per-turn token capture against the model's known context size — zero extra
//! tokens, fully local. Canopy renders the resulting percentage as a five-state
//! Smart→Dumb bar in column 3.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![allow(clippy::module_name_repetitions, clippy::doc_markdown)]

/// The context fullness of a thread, as a percentage `0..=100`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GaugeReading {
    /// Percentage of the context window currently in use.
    pub pct: u8,
    /// Tokens used.
    pub used: u64,
    /// The model's context window size.
    pub window: u64,
}

/// Compute the gauge reading from used tokens and the model's window size.
/// A zero window degrades to `0%` (no panic).
#[must_use]
pub fn reading(used: u64, window: u64) -> GaugeReading {
    let pct = if window == 0 {
        0
    } else {
        u8::try_from((u128::from(used) * 100 / u128::from(window)).min(100)).unwrap_or(100)
    };
    GaugeReading { pct, used, window }
}

/// The model context-window size for a known model id (best-effort table).
/// Unknown models default to a conservative 200k window.
#[must_use]
pub fn window_for_model(model: &str) -> u64 {
    let m = model.to_ascii_lowercase();
    if m.contains("[1m]") || m.contains("-1m") {
        1_000_000
    } else if m.contains("claude") {
        200_000
    } else if m.contains("gpt-4") {
        128_000
    } else {
        200_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentage_is_used_over_window() {
        assert_eq!(reading(100_000, 200_000).pct, 50);
        assert_eq!(reading(0, 200_000).pct, 0);
    }

    #[test]
    fn over_full_clamps_and_zero_window_is_safe() {
        assert_eq!(reading(300_000, 200_000).pct, 100);
        assert_eq!(reading(10, 0).pct, 0);
    }

    #[test]
    fn window_table() {
        assert_eq!(window_for_model("claude-opus-4-8"), 200_000);
        assert_eq!(window_for_model("claude-opus-4-8[1m]"), 1_000_000);
        assert_eq!(window_for_model("gpt-4o"), 128_000);
    }

    proptest::proptest! {
        /// The percentage is always within 0..=100.
        #[test]
        fn pct_bounded(used in 0u64..10_000_000, window in 0u64..2_000_000) {
            proptest::prop_assert!(reading(used, window).pct <= 100);
        }
    }
}

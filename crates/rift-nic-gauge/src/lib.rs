//! The nic-gauge: context-window fullness → a five-band "Smart→Dumb" meter.
//!
//! **The single owner** (ADR-0012) of the gauge formula, the band mapping, and
//! the model→context-size reference table. The dashboard consumes it through
//! the [`NicGauge`] trait via the data plane, so it can never disagree with the
//! formula it no longer contains. Computed from rift-shadow's per-turn token
//! capture against the model's known window — zero extra tokens, fully local.
//!
//! Unknown models are **explicit in the type** ([`Reading::UnknownModel`]):
//! there is no default window, so absent reference data can never masquerade
//! as headroom (the "silent 200k" this crate used to ship is gone).
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

/// The five gauge bands, named for what a filling context does to the model
/// (the dumbometer concept), at the spec thresholds (ADR-0012):
/// `Smart` (< 50) / `Capable` (< 70) / `Thinking` (< 85) / `Strained` (< 95) /
/// `Dumb` (≥ 95) — percent of the model's context window in use.
///
/// `Ord` follows fullness, so the band mapping is provably monotonic.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Band {
    /// Under 50% full: full reasoning headroom.
    Smart,
    /// 50–69% full: healthy, still comfortable.
    Capable,
    /// 70–84% full: degradation begins to be felt.
    Thinking,
    /// 85–94% full: nearing context collapse.
    Strained,
    /// 95%+ full: effective reasoning has degraded badly.
    Dumb,
}

impl Band {
    /// The band for a context-fullness percentage (clamped to 0–100).
    #[must_use]
    pub fn from_pct(pct: u8) -> Self {
        match pct.min(100) {
            0..=49 => Self::Smart,
            50..=69 => Self::Capable,
            70..=84 => Self::Thinking,
            85..=94 => Self::Strained,
            _ => Self::Dumb,
        }
    }

    /// The display label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Smart => "Smart",
            Self::Capable => "Capable",
            Self::Thinking => "Thinking",
            Self::Strained => "Strained",
            Self::Dumb => "Dumb",
        }
    }
}

/// A measured reading against a **known** context window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KnownReading {
    /// Percentage of the window in use, `0..=100` (floor division, clamped).
    pub pct: u8,
    /// Tokens in use.
    pub used: u64,
    /// The model's context window size (tokens).
    pub window: u64,
}

impl KnownReading {
    /// The band this reading falls in.
    #[must_use]
    pub fn band(self) -> Band {
        Band::from_pct(self.pct)
    }
}

/// A context reading: measured against a known window, or **explicitly**
/// unknown. There is deliberately no way to get a percentage out of
/// [`Reading::UnknownModel`] — the widget renders an indicator instead
/// (PRD-M4 user story 5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reading {
    /// The model's window is in the reference table; the percentage is real.
    Known(KnownReading),
    /// The model is not in the reference table; no percentage exists.
    UnknownModel {
        /// Tokens in use (still known from capture).
        used: u64,
    },
}

/// The gauge seam the data plane consumes (`dyn NicGauge`, ADR-0012): the
/// formula, bands, and reference table stay in this crate; callers hand in a
/// model id and a token count and render whatever comes back.
pub trait NicGauge {
    /// The reading for `used` tokens against `model`'s window —
    /// [`Reading::UnknownModel`] when the reference table has no entry.
    fn reading(&self, model: &str, used: u64) -> Reading;

    /// The model's context window (tokens), if known.
    fn window_for_model(&self, model: &str) -> Option<u64>;
}

/// The reference [`NicGauge`] over the built-in model table.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReferenceGauge;

impl NicGauge for ReferenceGauge {
    fn reading(&self, model: &str, used: u64) -> Reading {
        match self.window_for_model(model) {
            Some(window) => Reading::Known(known_reading(used, window)),
            None => Reading::UnknownModel { used },
        }
    }

    fn window_for_model(&self, model: &str) -> Option<u64> {
        window_for_model(model)
    }
}

/// Compute a [`KnownReading`] from used tokens and a (non-zero) window size.
/// Floor division; over-full clamps to 100; a zero window is treated as a
/// table error and reads 100% (saturated — never a divide-by-zero panic and
/// never fake headroom).
#[must_use]
pub fn known_reading(used: u64, window: u64) -> KnownReading {
    let pct = if window == 0 {
        100
    } else {
        u8::try_from((u128::from(used) * 100 / u128::from(window)).min(100)).unwrap_or(100)
    };
    KnownReading { pct, used, window }
}

/// The model→context-window reference table (tokens), matched on the
/// lowercased model id. `None` means unknown — **never defaulted**.
///
/// Provenance (vendor model documentation as of 2026-07; family rules cover the
/// dated release ids these vendors emit):
/// * `[1m]` / `-1m` id markers — Anthropic's 1M-token context beta
///   (`context-1m-2025-08-07`); the marker is how rift-shadow's capture spells
///   an enlarged-context session.
/// * `claude*` — every shipped Claude family model (Claude 3 through Claude 5:
///   Haiku/Sonnet/Opus/Fable) carries a 200k standard window per Anthropic's
///   model docs.
/// * `gpt-4.1*` — OpenAI GPT-4.1 family: 1M. Listed before the `gpt-4` rule
///   (first match wins).
/// * `gpt-4*` — OpenAI GPT-4o / GPT-4-turbo family: 128k.
/// * `o3*` / `o4-mini*` — OpenAI reasoning models: 200k.
/// * `gemini-1.5-pro*` 2M; other `gemini-1.5*`/`gemini-2*` 1M per Google's
///   model docs.
///
/// Anything else — including new models shipped after this table was pinned —
/// reads as unknown and renders the explicit indicator (the honesty backstop;
/// see the M4 map's fog note on reference-table completeness).
#[must_use]
pub fn window_for_model(model: &str) -> Option<u64> {
    let m = model.to_ascii_lowercase();
    if m.contains("[1m]") || m.contains("-1m") {
        return Some(1_000_000);
    }
    // First match wins: more specific families precede their prefixes.
    const FAMILIES: &[(&str, u64)] = &[
        ("claude", 200_000),
        ("gpt-4.1", 1_000_000),
        ("gpt-4", 128_000),
        ("o4-mini", 200_000),
        ("o3", 200_000),
        ("gemini-1.5-pro", 2_097_152),
        ("gemini-1.5", 1_048_576),
        ("gemini-2", 1_048_576),
    ];
    FAMILIES
        .iter()
        .find(|(family, _)| m.contains(family))
        .map(|&(_, window)| window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_at_spec_thresholds() {
        // ADR-0012: 50/70/85/95.
        assert_eq!(Band::from_pct(0), Band::Smart);
        assert_eq!(Band::from_pct(49), Band::Smart);
        assert_eq!(Band::from_pct(50), Band::Capable);
        assert_eq!(Band::from_pct(69), Band::Capable);
        assert_eq!(Band::from_pct(70), Band::Thinking);
        assert_eq!(Band::from_pct(84), Band::Thinking);
        assert_eq!(Band::from_pct(85), Band::Strained);
        assert_eq!(Band::from_pct(94), Band::Strained);
        assert_eq!(Band::from_pct(95), Band::Dumb);
        assert_eq!(Band::from_pct(100), Band::Dumb);
        assert_eq!(Band::from_pct(255), Band::Dumb); // clamps
    }

    #[test]
    fn labels_keep_the_dumbometer_names() {
        let labels: Vec<&str> = [
            Band::Smart,
            Band::Capable,
            Band::Thinking,
            Band::Strained,
            Band::Dumb,
        ]
        .iter()
        .map(|b| b.label())
        .collect();
        assert_eq!(
            labels,
            vec!["Smart", "Capable", "Thinking", "Strained", "Dumb"]
        );
    }

    #[test]
    fn percentage_is_used_over_window() {
        assert_eq!(known_reading(100_000, 200_000).pct, 50);
        assert_eq!(known_reading(0, 200_000).pct, 0);
        assert_eq!(known_reading(100_000, 200_000).band(), Band::Capable);
    }

    #[test]
    fn over_full_clamps_and_zero_window_saturates() {
        assert_eq!(known_reading(300_000, 200_000).pct, 100);
        // A zero window is a table error: saturate, never fake headroom.
        assert_eq!(known_reading(10, 0).pct, 100);
    }

    #[test]
    fn unknown_model_is_explicit_never_a_default() {
        let g = ReferenceGauge;
        assert_eq!(g.window_for_model("mystery-model-9000"), None);
        assert_eq!(
            g.reading("mystery-model-9000", 42),
            Reading::UnknownModel { used: 42 }
        );
        // No `Reading` path yields a percentage for an unknown model: the only
        // Known constructor requires a table hit (compile-shape guarantee, but
        // pin it behaviorally too).
        match g.reading("mystery-model-9000", 42) {
            Reading::UnknownModel { used } => assert_eq!(used, 42),
            Reading::Known(_) => panic!("unknown model must never read Known"),
        }
    }

    #[test]
    fn window_table_families() {
        assert_eq!(window_for_model("claude-opus-4-8"), Some(200_000));
        assert_eq!(window_for_model("claude-fable-5"), Some(200_000));
        assert_eq!(window_for_model("claude-sonnet-4-5[1m]"), Some(1_000_000));
        assert_eq!(window_for_model("gpt-4o"), Some(128_000));
        assert_eq!(window_for_model("gpt-4.1-mini"), Some(1_000_000));
        assert_eq!(window_for_model("o3"), Some(200_000));
        assert_eq!(window_for_model("gemini-1.5-pro-002"), Some(2_097_152));
        assert_eq!(window_for_model(""), None);
    }

    #[test]
    fn trait_is_dyn_usable() {
        // The data plane consumes `dyn NicGauge` (ADR-0012) — prove object safety.
        let g: &dyn NicGauge = &ReferenceGauge;
        assert!(matches!(g.reading("claude-fable-5", 10), Reading::Known(_)));
    }

    proptest::proptest! {
        /// The band mapping is monotonic in percent (PRD-M4 testing decision).
        #[test]
        fn band_mapping_is_monotonic(a in 0u8..=100, b in 0u8..=100) {
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            proptest::prop_assert!(Band::from_pct(lo) <= Band::from_pct(hi));
        }

        /// The percentage is always within 0..=100, for any inputs.
        #[test]
        fn pct_bounded(used in 0u64..10_000_000, window in 0u64..2_000_000) {
            proptest::prop_assert!(known_reading(used, window).pct <= 100);
        }

        /// The reading constructor is total over arbitrary model ids.
        #[test]
        fn reading_total_over_arbitrary_models(model in ".*", used in 0u64..1_000_000) {
            let _ = ReferenceGauge.reading(&model, used);
        }
    }
}

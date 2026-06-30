//! The nic-gauge: context-window % full → a five-state "Smart→Dumb" meter.
//!
//! Inspired by dumbometer: as a model's context fills, its effective reasoning
//! degrades (the *Lost in the Middle* effect). The gauge reads the per-turn token
//! capture against the model's context size (zero extra tokens, fully local) and
//! renders a bar in Canopy column 3.

/// The five gauge states, by how full the context window is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GaugeState {
    /// 0–20% full.
    Smart,
    /// 20–40% full.
    Capable,
    /// 40–60% full.
    Thinking,
    /// 60–80% full.
    Strained,
    /// 80–100% full.
    Dumb,
}

impl GaugeState {
    /// The state for a context-fullness percentage (clamped to 0–100).
    #[must_use]
    pub fn from_pct(pct: u8) -> Self {
        match pct.min(100) {
            0..=19 => Self::Smart,
            20..=39 => Self::Capable,
            40..=59 => Self::Thinking,
            60..=79 => Self::Strained,
            _ => Self::Dumb,
        }
    }

    /// A short label.
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

/// Render a fixed-width bar for a percentage, e.g. `███░░░░░░░ 32% Capable`.
#[must_use]
pub fn render_bar(pct: u8, width: usize) -> String {
    let pct = pct.min(100);
    let filled = usize::from(pct) * width / 100;
    let empty = width.saturating_sub(filled);
    let bar: String = "█".repeat(filled) + &"░".repeat(empty);
    format!("{bar} {pct}% {}", GaugeState::from_pct(pct).label())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_by_threshold() {
        assert_eq!(GaugeState::from_pct(0), GaugeState::Smart);
        assert_eq!(GaugeState::from_pct(19), GaugeState::Smart);
        assert_eq!(GaugeState::from_pct(20), GaugeState::Capable);
        assert_eq!(GaugeState::from_pct(55), GaugeState::Thinking);
        assert_eq!(GaugeState::from_pct(79), GaugeState::Strained);
        assert_eq!(GaugeState::from_pct(80), GaugeState::Dumb);
        assert_eq!(GaugeState::from_pct(255), GaugeState::Dumb); // clamps
    }

    #[test]
    fn bar_is_fixed_width_and_labeled() {
        let bar = render_bar(50, 10);
        assert!(bar.contains("50%"));
        assert!(bar.contains("Thinking"));
        assert_eq!(bar.chars().take_while(|c| *c == '█').count(), 5);
    }

    proptest::proptest! {
        /// The filled+empty cells always equal `width`.
        #[test]
        fn bar_cells_total_width(pct in 0u8..=120, width in 1usize..40) {
            let bar = render_bar(pct, width);
            let cells = bar.chars().take_while(|c| *c == '█' || *c == '░').count();
            proptest::prop_assert_eq!(cells, width);
        }
    }
}

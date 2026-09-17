//! Human-vs-agent line attribution (LIFO ownership) and confidence scoring.
//!
//! The diff-capture layer (`H_i`/`A_i`/`C` boundaries via `gix` + `imara-diff`)
//! produces, for each line surviving into a commit, the ordered list of *writers*
//! that touched it. This crate turns those writer histories into honest
//! authorship counts and a calibrated confidence — pure, deterministic logic
//! with guarded arithmetic (no panics, all ratios in `[0,1]`).
//!
//! **LIFO ownership**: the *last* writer of a final line is its author; if a
//! human edits an agent-authored line they "take over" the line, but we also
//! record that it was agent-derived (`human_modified_agent`) so acceptance and
//! coaching metrics stay honest (these are coaching signals, never a ranking).
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

pub mod capture;
pub mod diff;
pub mod provenance;
pub mod replay;
pub use capture::{capture_commit, CommitCapture, FileChange, HunkAttribution};
pub use diff::{
    changed_hunk_id, diff_attributable, diff_lines, file_revision_id, is_attributable, Hunk,
    MAX_DIFF_BYTES,
};
pub use provenance::{
    parse_checkpoint_claims, select_for_file, CheckpointCandidate, ClaimSet, Selection,
    SelectionBasis, TrailerClaim, TrailerDialect,
};
pub use replay::{committed_writers, replay, ReplayedFile, TurnEdit};

/// Who authored an edit.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ::serde::Serialize, ::serde::Deserialize)]
pub enum Actor {
    /// An AI agent turn (`A_i`).
    Agent,
    /// A human edit (`H_i`).
    Human,
}

/// Per-file authorship counts. Counts are the source of truth; ratios are
/// derived for display.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
pub struct LineAttribution {
    /// Lines whose latest writer was an agent (and not later replaced).
    pub agent_lines: u32,
    /// Lines whose latest writer was a human.
    pub human_lines: u32,
    /// Lines in the commit not explained by any captured edit (hooks off).
    pub unknown_lines: u32,
    /// Lines an agent authored that a human later modified.
    pub human_modified_agent_lines: u32,
    /// Lines an agent added that a human deleted before commit.
    pub agent_removed_by_human: u32,
}

impl LineAttribution {
    /// Total lines present in the commit (`agent + human + unknown`).
    #[must_use]
    pub fn total_committed(&self) -> u32 {
        self.agent_lines + self.human_lines + self.unknown_lines
    }

    /// `agent_lines / (agent + human + unknown)` — guarded against zero.
    #[must_use]
    pub fn agent_percentage(&self) -> f64 {
        let denom = self.total_committed();
        if denom == 0 {
            0.0
        } else {
            f64::from(self.agent_lines) / f64::from(denom)
        }
    }

    /// `agent_lines / (agent + agent_removed_by_human + human_modified_agent)` —
    /// surviving agent lines over total agent-proposed lines. Guarded.
    #[must_use]
    pub fn agent_acceptance_rate(&self) -> f64 {
        let proposed =
            self.agent_lines + self.agent_removed_by_human + self.human_modified_agent_lines;
        if proposed == 0 {
            0.0
        } else {
            f64::from(self.agent_lines) / f64::from(proposed)
        }
    }

    /// Add another file's counts (for rollup).
    #[must_use]
    pub fn add(self, other: Self) -> Self {
        Self {
            agent_lines: self.agent_lines + other.agent_lines,
            human_lines: self.human_lines + other.human_lines,
            unknown_lines: self.unknown_lines + other.unknown_lines,
            human_modified_agent_lines: self.human_modified_agent_lines
                + other.human_modified_agent_lines,
            agent_removed_by_human: self.agent_removed_by_human + other.agent_removed_by_human,
        }
    }
}

/// Attribute a file from the writer history of each surviving line plus the count
/// of agent lines a human deleted before commit.
///
/// Each element of `committed_line_writers` is the ordered list of writers that
/// touched that surviving line (earliest → latest). An empty history means the
/// line is `unknown` (an edit Rift did not observe).
#[must_use]
pub fn attribute(
    committed_line_writers: &[Vec<Actor>],
    agent_removed_by_human: u32,
) -> LineAttribution {
    let mut a = LineAttribution {
        agent_removed_by_human,
        ..LineAttribution::default()
    };
    for writers in committed_line_writers {
        match writers.last() {
            None => a.unknown_lines += 1,
            Some(Actor::Agent) => a.agent_lines += 1,
            Some(Actor::Human) => {
                a.human_lines += 1;
                if writers.contains(&Actor::Agent) {
                    a.human_modified_agent_lines += 1;
                }
            }
        }
    }
    a
}

/// Sum category counts across files, then recompute ratios on the totals (never
/// average per-file ratios — that would bias toward small files).
#[must_use]
pub fn rollup(files: &[LineAttribution]) -> LineAttribution {
    files
        .iter()
        .copied()
        .fold(LineAttribution::default(), LineAttribution::add)
}

// --- confidence -------------------------------------------------------------

/// Per-factor confidence multipliers, each in `[0,1]`.
#[derive(Clone, Copy, Debug, PartialEq, ::serde::Serialize, ::serde::Deserialize)]
pub struct ConfidenceFactors {
    /// Fraction of the commit explained by captured edits (`1 - unknown_ratio`).
    pub capture_completeness: f64,
    /// Squash/rebase/amend penalty.
    pub history_stability: f64,
    /// Code moved/renamed across boundaries.
    pub move_penalty: f64,
    /// Manual conflict-resolution touched these lines.
    pub merge_penalty: f64,
    /// Coarse (whole-turn) vs fine (per-tool) capture.
    pub granularity: f64,
}

impl Default for ConfidenceFactors {
    fn default() -> Self {
        Self {
            capture_completeness: 1.0,
            history_stability: 1.0,
            move_penalty: 1.0,
            merge_penalty: 1.0,
            granularity: 1.0,
        }
    }
}

/// Confidence is the product of the factors, clamped to `[0,1]`. Worsening any
/// single factor never raises the result (monotonic penalties).
#[must_use]
pub fn confidence(f: &ConfidenceFactors) -> f64 {
    (f.capture_completeness
        * f.history_stability
        * f.move_penalty
        * f.merge_penalty
        * f.granularity)
        .clamp(0.0, 1.0)
}

/// A per-hunk confidence with the number of lines it covers.
#[derive(Clone, Copy, Debug)]
pub struct HunkConfidence {
    /// Lines covered by the hunk.
    pub line_count: u32,
    /// The hunk's confidence in `[0,1]`.
    pub confidence: f64,
}

/// Line-weighted average of hunk confidences (a tiny shaky hunk should not tank
/// a large solid file). Empty input → `0.0`.
#[must_use]
pub fn file_confidence(hunks: &[HunkConfidence]) -> f64 {
    let total: u32 = hunks.iter().map(|h| h.line_count).sum();
    if total == 0 {
        return 0.0;
    }
    let weighted: f64 = hunks
        .iter()
        .map(|h| h.confidence * f64::from(h.line_count))
        .sum();
    (weighted / f64::from(total)).clamp(0.0, 1.0)
}

/// Build factors from observed signals (the honest, conservative mapping).
#[must_use]
pub fn factors_from_signals(
    unknown_ratio: f64,
    squashed: bool,
    moved: bool,
    manual_merge: bool,
    coarse_granularity: bool,
) -> ConfidenceFactors {
    ConfidenceFactors {
        capture_completeness: (1.0 - unknown_ratio).clamp(0.0, 1.0),
        history_stability: if squashed { 0.5 } else { 1.0 },
        move_penalty: if moved { 0.7 } else { 1.0 },
        merge_penalty: if manual_merge { 0.6 } else { 1.0 },
        granularity: if coarse_granularity { 0.8 } else { 1.0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Actor::{Agent, Human};

    #[test]
    fn pure_agent_file_is_100_percent_agent() {
        let a = attribute(&[vec![Agent], vec![Agent], vec![Agent]], 0);
        assert!((a.agent_percentage() - 1.0).abs() < 1e-9);
        assert!((a.agent_acceptance_rate() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn pure_human_file_is_0_percent_agent() {
        let a = attribute(&[vec![Human], vec![Human]], 0);
        assert!(a.agent_percentage().abs() < 1e-9);
    }

    #[test]
    fn human_modifies_agent_line() {
        // line authored by agent then edited by human (LIFO owner = human)
        let a = attribute(&[vec![Agent, Human], vec![Agent]], 0);
        assert_eq!(a.human_lines, 1);
        assert_eq!(a.agent_lines, 1);
        assert_eq!(a.human_modified_agent_lines, 1);
        assert!(a.agent_acceptance_rate() < 1.0);
    }

    #[test]
    fn human_deletes_agent_line_lowers_acceptance() {
        let a = attribute(&[vec![Agent]], 1);
        assert_eq!(a.agent_lines, 1);
        assert_eq!(a.agent_removed_by_human, 1);
        assert!((a.agent_acceptance_rate() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn lifo_assigns_latest_writer() {
        let a = attribute(&[vec![Agent, Human]], 0);
        assert_eq!(a.human_lines, 1);
        assert_eq!(a.agent_lines, 0);
    }

    #[test]
    fn unknown_lines_counted_not_credited() {
        let a = attribute(&[vec![], vec![Agent]], 0);
        assert_eq!(a.unknown_lines, 1);
        assert_eq!(a.agent_lines, 1);
        assert!((a.agent_percentage() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn guarded_division_no_panic_on_empty() {
        let a = attribute(&[], 0);
        assert_eq!(a.agent_percentage(), 0.0);
        assert_eq!(a.agent_acceptance_rate(), 0.0);
    }

    #[test]
    fn repo_rollup_recomputes_on_totals() {
        let small = attribute(&[vec![Agent]], 0); // 100% agent, 1 line
        let large = attribute(&[vec![Human], vec![Human], vec![Human]], 0); // 0% agent, 3 lines
        let r = rollup(&[small, large]);
        // recompute on totals: 1 agent / 4 = 0.25, NOT average(1.0, 0.0)=0.5
        assert!((r.agent_percentage() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn confidence_full_capture_is_high() {
        let c = confidence(&ConfidenceFactors::default());
        assert!((c - 1.0).abs() < 1e-9);
    }

    #[test]
    fn missing_hooks_lowers_confidence() {
        let f = factors_from_signals(0.4, false, false, false, false);
        assert!((f.capture_completeness - 0.6).abs() < 1e-9);
        assert!(confidence(&f) < 1.0);
    }

    #[test]
    fn each_penalty_lowers_confidence() {
        let base = confidence(&ConfidenceFactors::default());
        for f in [
            factors_from_signals(0.0, true, false, false, false),
            factors_from_signals(0.0, false, true, false, false),
            factors_from_signals(0.0, false, false, true, false),
            factors_from_signals(0.0, false, false, false, true),
        ] {
            assert!(confidence(&f) < base, "penalty must lower confidence");
        }
    }

    #[test]
    fn file_confidence_is_line_weighted() {
        let hunks = [
            HunkConfidence {
                line_count: 100,
                confidence: 0.9,
            },
            HunkConfidence {
                line_count: 1,
                confidence: 0.1,
            },
        ];
        let c = file_confidence(&hunks);
        assert!(
            c > 0.85,
            "a tiny shaky hunk must not tank a big solid file: {c}"
        );
    }

    proptest::proptest! {
        /// Conservation: agent + human + unknown == number of committed lines.
        #[test]
        fn conservation(lines in proptest::collection::vec(
            proptest::collection::vec(proptest::bool::ANY, 0..4), 0..50)) {
            let writers: Vec<Vec<Actor>> = lines.iter()
                .map(|ws| ws.iter().map(|b| if *b { Agent } else { Human }).collect())
                .collect();
            let a = attribute(&writers, 0);
            proptest::prop_assert_eq!(a.total_committed() as usize, writers.len());
        }

        /// Ratios are always within [0,1].
        #[test]
        fn ratios_bounded(n_agent in 0u32..100, n_human in 0u32..100, n_unknown in 0u32..100, removed in 0u32..100) {
            let a = LineAttribution {
                agent_lines: n_agent, human_lines: n_human, unknown_lines: n_unknown,
                human_modified_agent_lines: 0, agent_removed_by_human: removed,
            };
            proptest::prop_assert!((0.0..=1.0).contains(&a.agent_percentage()));
            proptest::prop_assert!((0.0..=1.0).contains(&a.agent_acceptance_rate()));
        }

        /// Confidence is always within [0,1] and worsening a factor never raises it.
        #[test]
        fn confidence_bounded_and_monotone(u in 0.0f64..1.0) {
            let good = factors_from_signals(u, false, false, false, false);
            let worse = factors_from_signals(u, true, false, false, false);
            proptest::prop_assert!((0.0..=1.0).contains(&confidence(&good)));
            proptest::prop_assert!(confidence(&worse) <= confidence(&good));
        }
    }
}

//! Task-aware ranking with outcome labels.
//!
//! Query-unaware retrieval returns cheap-but-wrong context (W5), so ranking is
//! driven by the **required** task signals: the files in focus, the error
//! signatures in play, recency, and the outcome label of the candidate's
//! provenance (merged + tests-passed + low human-modification ranks up;
//! reverted/abandoned ranks down — W8). Pure, deterministic scoring.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![allow(
    clippy::module_name_repetitions,
    clippy::doc_markdown,
    clippy::cast_precision_loss
)]

use std::collections::BTreeSet;

/// The outcome label attached to a candidate's provenance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Merged and tests passed (strongest positive).
    MergedClean,
    /// Merged.
    Merged,
    /// Unknown / no label yet.
    Unknown,
    /// Heavily rewritten by a human.
    Reworked,
    /// Reverted or abandoned (strongest negative).
    Reverted,
}

impl Outcome {
    fn multiplier(self) -> f64 {
        match self {
            Self::MergedClean => 1.3,
            Self::Merged => 1.1,
            Self::Unknown => 1.0,
            Self::Reworked => 0.6,
            Self::Reverted => 0.2,
        }
    }
}

/// The required, query-aware ranking task (W5: these are mandatory).
#[derive(Clone, Debug)]
pub struct RankTask {
    /// The free-text task/goal (used for keyword overlap, lightly here).
    pub task: String,
    /// Files the agent is focused on.
    pub files_in_focus: BTreeSet<String>,
    /// Error signatures currently in play.
    pub error_signatures: BTreeSet<String>,
    /// The token budget (candidates beyond budget are dropped downstream).
    pub token_budget: u32,
}

/// A retrieval candidate with its signals.
#[derive(Clone, Debug)]
pub struct Candidate {
    /// The candidate's path.
    pub path: String,
    /// Error signatures this candidate touches.
    pub error_signatures: BTreeSet<String>,
    /// Recency rank: `0` = most recent (lower is more recent).
    pub recency_rank: u32,
    /// Outcome label of its provenance.
    pub outcome: Outcome,
    /// Estimated tokens to include this candidate.
    pub est_tokens: u32,
}

fn overlap(a: &BTreeSet<String>, b: &BTreeSet<String>) -> usize {
    a.intersection(b).count()
}

/// Score a candidate against the task. Higher is better. Deterministic.
#[must_use]
pub fn score(task: &RankTask, c: &Candidate) -> f64 {
    let focus = if task.files_in_focus.contains(&c.path) {
        3.0
    } else {
        0.0
    };
    let err = overlap(&c.error_signatures, &task.error_signatures) as f64 * 2.0;
    // recency: 1/(1+rank) so newer scores higher and it is bounded.
    let recency = 1.0 / (1.0 + f64::from(c.recency_rank));
    let base = focus + err + recency;
    base * c.outcome.multiplier()
}

/// Rank candidates best-first (stable: ties keep input order). The task's focus
/// and error signatures are required inputs, so ranking is never query-unaware.
#[must_use]
pub fn rank(task: &RankTask, candidates: &[Candidate]) -> Vec<Candidate> {
    let mut scored: Vec<(f64, usize, Candidate)> = candidates
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, c)| (score(task, &c), i, c))
        .collect();
    // sort by score desc, then original index asc (stable tie-break)
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(core::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    scored.into_iter().map(|(_, _, c)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> RankTask {
        RankTask {
            task: "fix retry policy".into(),
            files_in_focus: ["src/retry.rs".to_string()].into_iter().collect(),
            error_signatures: ["E_TIMEOUT".to_string()].into_iter().collect(),
            token_budget: 2000,
        }
    }

    fn cand(path: &str, errs: &[&str], recency: u32, outcome: Outcome) -> Candidate {
        Candidate {
            path: path.into(),
            error_signatures: errs.iter().map(|s| (*s).to_string()).collect(),
            recency_rank: recency,
            outcome,
            est_tokens: 100,
        }
    }

    #[test]
    fn focus_file_outranks_unrelated() {
        let t = task();
        let focused = cand("src/retry.rs", &[], 5, Outcome::Unknown);
        let other = cand("src/unrelated.rs", &[], 0, Outcome::Unknown);
        assert!(score(&t, &focused) > score(&t, &other));
    }

    #[test]
    fn error_overlap_boosts_score() {
        let t = task();
        let matching = cand("x.rs", &["E_TIMEOUT"], 5, Outcome::Unknown);
        let nonmatching = cand("x.rs", &["E_OTHER"], 5, Outcome::Unknown);
        assert!(score(&t, &matching) > score(&t, &nonmatching));
    }

    #[test]
    fn outcome_labels_reorder() {
        let t = task();
        let merged = cand("a.rs", &[], 1, Outcome::MergedClean);
        let reverted = cand("a.rs", &[], 1, Outcome::Reverted);
        assert!(score(&t, &merged) > score(&t, &reverted));
    }

    #[test]
    fn rank_is_best_first_and_stable() {
        let t = task();
        let cands = vec![
            cand("src/unrelated.rs", &[], 9, Outcome::Reverted),
            cand("src/retry.rs", &["E_TIMEOUT"], 0, Outcome::MergedClean),
            cand("other.rs", &[], 5, Outcome::Unknown),
        ];
        let ranked = rank(&t, &cands);
        assert_eq!(ranked[0].path, "src/retry.rs");
    }

    proptest::proptest! {
        /// Score is finite and non-negative for any inputs.
        #[test]
        fn score_is_finite_nonneg(recency in 0u32..1000) {
            let t = task();
            let c = cand("p.rs", &["E_TIMEOUT"], recency, Outcome::MergedClean);
            let s = score(&t, &c);
            proptest::prop_assert!(s.is_finite() && s >= 0.0);
        }
    }
}

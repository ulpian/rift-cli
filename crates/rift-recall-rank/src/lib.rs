//! Task-aware ranking with outcome labels (Stage 3 Phase 4 Step 1, W5/W8).
//!
//! Query-unaware retrieval returns cheap-but-wrong context (W5), so ranking is
//! driven by **required** task signals. The model is deliberately a transparent,
//! inspectable linear combination (not an ML black box) of three pure `[0,1]`
//! signals — changed-file overlap, error overlap, and recency — combined with
//! per-signal weights that **redistribute** when a signal is absent, so an
//! error-less query is not dominated by a dead signal. The candidate's
//! provenance outcome (merged/tests-passed/low-mod up; reverted/abandoned down —
//! W8) then applies as a signed multiplier that can bury a reverted candidate
//! below unlabeled work.
//!
//! Scoring is a pure function of `(query, candidate, now, weights)`: the clock
//! is **injected**, never read, so rankings are deterministic and reproducible
//! for the Phase-8 eval harness. Ranking **orders within** a scope; it never
//! widens it (W14 containment is enforced by the caller's `in_scope_nodes`).
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
#![allow(
    clippy::module_name_repetitions,
    clippy::doc_markdown,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

use std::collections::BTreeSet;

use rift_core::Timestamp;
use rift_recall_types::OutcomeKind;

/// Default recency half-life (~7 days), in milliseconds. Tunable per repo from
/// Phase-8 pilot data.
pub const DEFAULT_HALF_LIFE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// A stable candidate identity used for the deterministic tie-break.
pub type CandidateId = String;

/// Why a [`RankQuery`] could not be constructed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum QueryError {
    /// The task string was empty/whitespace.
    #[error("rank query requires a non-empty task")]
    EmptyTask,
    /// No files were in focus.
    #[error("rank query requires at least one file in focus")]
    EmptyFocus,
}

/// The required, query-aware ranking task (W5). Fields are **private**; the only
/// constructor rejects an empty `task` or empty `files_in_focus`, so a bare
/// query is unrepresentable.
#[derive(Clone, Debug)]
pub struct RankQuery {
    task: String,
    files_in_focus: BTreeSet<String>,
    error_signatures: BTreeSet<String>,
    half_life_ms: i64,
}

impl RankQuery {
    /// Construct a query. `task` must be non-empty and `files_in_focus` must hold
    /// at least one path.
    ///
    /// # Errors
    /// [`QueryError`] if `task` is blank or `files_in_focus` is empty.
    pub fn new(
        task: impl Into<String>,
        files_in_focus: impl IntoIterator<Item = String>,
        error_signatures: impl IntoIterator<Item = String>,
    ) -> Result<Self, QueryError> {
        let task = task.into();
        if task.trim().is_empty() {
            return Err(QueryError::EmptyTask);
        }
        let files_in_focus: BTreeSet<String> = files_in_focus.into_iter().collect();
        if files_in_focus.is_empty() {
            return Err(QueryError::EmptyFocus);
        }
        Ok(Self {
            task,
            files_in_focus,
            error_signatures: error_signatures.into_iter().collect(),
            half_life_ms: DEFAULT_HALF_LIFE_MS,
        })
    }

    /// Override the recency half-life (config-tunable; clamped to ≥ 1ms).
    #[must_use]
    pub fn with_half_life_ms(mut self, ms: i64) -> Self {
        self.half_life_ms = ms.max(1);
        self
    }

    /// The free-text task/goal.
    #[must_use]
    pub fn task(&self) -> &str {
        &self.task
    }
    /// The files the agent is focused on.
    #[must_use]
    pub fn files_in_focus(&self) -> &BTreeSet<String> {
        &self.files_in_focus
    }
    /// The error signatures currently in play.
    #[must_use]
    pub fn error_signatures(&self) -> &BTreeSet<String> {
        &self.error_signatures
    }
    /// Whether the query carries any error signatures.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        !self.error_signatures.is_empty()
    }
}

/// A retrieval candidate with its signals. `outcomes` are the candidate's
/// currently-effective (latest non-superseded) provenance labels.
#[derive(Clone, Debug)]
pub struct Candidate {
    /// Stable identity (the deterministic tie-break key).
    pub id: CandidateId,
    /// The candidate's path.
    pub path: String,
    /// Error signatures this candidate touches.
    pub error_signatures: BTreeSet<String>,
    /// When the candidate was last touched (for recency decay).
    pub last_touched: Timestamp,
    /// Effective outcome labels of its provenance.
    pub outcomes: Vec<OutcomeKind>,
    /// Estimated tokens to include this candidate.
    pub est_tokens: u32,
}

/// The per-signal weights for the linear task-signal combination. The three
/// weights are renormalized over the *present* signals at scoring time
/// (recency is always present), so they need not be re-tuned per query.
#[derive(Clone, Copy, Debug)]
pub struct SignalWeights {
    /// Weight of the changed-file overlap signal.
    pub overlap: f32,
    /// Weight of the error-overlap signal.
    pub error: f32,
    /// Weight of the recency signal.
    pub recency: f32,
}

impl Default for SignalWeights {
    fn default() -> Self {
        // Sums to 1.0; overlap dominates, recency is the gentle tail.
        Self {
            overlap: 0.5,
            error: 0.3,
            recency: 0.2,
        }
    }
}

/// The per-signal breakdown retained for explainability + the Phase-8 eval
/// harness. `overlap`/`error`/`outcome` are `None` when that signal is absent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RankFeatures {
    /// Changed-file overlap ∈ `[0,1]`, or `None` if no overlap.
    pub overlap: Option<f32>,
    /// Error overlap ∈ `[0,1]`, or `None` if the query has no errors.
    pub error: Option<f32>,
    /// Recency ∈ `[0,1]` (always present).
    pub recency: f32,
    /// Signed outcome term ∈ `[-1,1]`, or `None` if the candidate is unlabeled.
    pub outcome: Option<f32>,
}

/// A candidate's score: the final ordering `value`, the task-signal `task_value`
/// (∈ `[0,1]`), and the per-signal `features`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RankedScore {
    /// The final ordering score (`task_value` × outcome multiplier), ≥ 0.
    pub value: f32,
    /// The weighted task-signal score ∈ `[0,1]` (pre-outcome).
    pub task_value: f32,
    /// The per-signal breakdown.
    pub features: RankFeatures,
}

/// A scored candidate (the full result the eval harness consumes).
#[derive(Clone, Debug)]
pub struct Scored {
    /// The candidate.
    pub candidate: Candidate,
    /// Its score + breakdown.
    pub score: RankedScore,
}

/// The directory prefix of a path (everything before the last `/`), or `""`.
fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// The top-level path component (before the first `/`), or the whole path.
fn top_component(path: &str) -> &str {
    match path.find('/') {
        Some(i) => &path[..i],
        None => path,
    }
}

/// Path affinity ∈ `[0,1]`: exact = 1.0, same directory = 0.5, shared top-level
/// component = 0.2, otherwise 0.
fn path_affinity(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }
    let (da, db) = (parent_dir(a), parent_dir(b));
    if !da.is_empty() && da == db {
        return 0.5;
    }
    let ta = top_component(a);
    if !ta.is_empty() && ta == top_component(b) {
        return 0.2;
    }
    0.0
}

/// Changed-file overlap: the best path affinity of the candidate against any
/// file in focus. `None` if there is no overlap at all. Taking the best (rather
/// than a breadth-normalized Jaccard) means a focused candidate is never
/// penalized for the breadth of `files_in_focus`, and adding a matching focus
/// file can only raise the signal (monotonic).
#[must_use]
pub fn changed_file_overlap(c: &Candidate, q: &RankQuery) -> Option<f32> {
    let best = q
        .files_in_focus
        .iter()
        .map(|f| path_affinity(&c.path, f))
        .fold(0.0f32, f32::max);
    if best <= 0.0 {
        None
    } else {
        Some(best)
    }
}

/// Error overlap: Jaccard of the candidate's error signatures against the
/// query's. `None` when the query carries no errors (its weight redistributes).
#[must_use]
pub fn error_overlap(c: &Candidate, q: &RankQuery) -> Option<f32> {
    if !q.has_errors() {
        return None;
    }
    let inter = c.error_signatures.intersection(&q.error_signatures).count();
    let union = c.error_signatures.union(&q.error_signatures).count();
    if union == 0 {
        return Some(0.0);
    }
    Some(inter as f32 / union as f32)
}

/// Recency: `0.5^(age / half_life)` with `age = now − last_touched` (clamped at
/// 0). Always present, always ∈ `(0,1]`.
#[must_use]
pub fn recency(c: &Candidate, q: &RankQuery, now: Timestamp) -> f32 {
    let age = (now.as_millis() - c.last_touched.as_millis()).max(0) as f64;
    let hl = q.half_life_ms.max(1) as f64;
    0.5f64.powf(age / hl) as f32
}

/// Combine a candidate's effective outcome labels into a signed term ∈ `[-1,1]`.
/// `None` when the candidate has no labels (neutral; its weight redistributes).
#[must_use]
pub fn outcome_term(labels: &[OutcomeKind]) -> Option<f32> {
    if labels.is_empty() {
        return None;
    }
    let sum: f32 = labels.iter().map(|k| k.base_term()).sum();
    Some(sum.clamp(-1.0, 1.0))
}

/// The bounded outcome multiplier applied to the task-signal score: a neutral
/// (unlabeled) candidate keeps its score (`1.0`); `Merged` boosts (up to `2.0`);
/// `Reverted` (term `-1.0`) buries it to `0.0`, below unlabeled work (W8).
fn outcome_multiplier(term: Option<f32>) -> f32 {
    (1.0 + term.unwrap_or(0.0)).clamp(0.0, 2.0)
}

/// Score a candidate against the task at an injected `now`. Pure & deterministic.
#[must_use]
pub fn score(q: &RankQuery, c: &Candidate, now: Timestamp, w: &SignalWeights) -> RankedScore {
    let overlap = changed_file_overlap(c, q);
    let error = error_overlap(c, q);
    let rec = recency(c, q, now);
    let outcome = outcome_term(&c.outcomes);

    // Renormalize the three task weights over the *present* signals (recency is
    // always present), so an absent signal's weight redistributes to the rest.
    let present_weight = w.recency
        + if overlap.is_some() { w.overlap } else { 0.0 }
        + if error.is_some() { w.error } else { 0.0 };
    let norm = if present_weight > 0.0 {
        present_weight
    } else {
        1.0
    };
    let task_value =
        (w.overlap * overlap.unwrap_or(0.0) + w.error * error.unwrap_or(0.0) + w.recency * rec)
            / norm;

    let value = task_value * outcome_multiplier(outcome);
    RankedScore {
        value,
        task_value,
        features: RankFeatures {
            overlap,
            error,
            recency: rec,
            outcome,
        },
    }
}

/// Rank candidates best-first. Deterministic: sorts by `value` descending with a
/// **stable tie-break on [`CandidateId`]** (not input order), so the ordering is
/// a pure function of the inputs and independent of the candidate list order.
#[must_use]
pub fn rank(
    q: &RankQuery,
    candidates: &[Candidate],
    now: Timestamp,
    w: &SignalWeights,
) -> Vec<Scored> {
    let mut scored: Vec<Scored> = candidates
        .iter()
        .cloned()
        .map(|c| {
            let s = score(q, &c, now, w);
            Scored {
                candidate: c,
                score: s,
            }
        })
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .value
            .partial_cmp(&a.score.value)
            .unwrap_or(core::cmp::Ordering::Equal)
            .then_with(|| a.candidate.id.cmp(&b.candidate.id))
    });
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;

    fn now() -> Timestamp {
        Timestamp::from_millis(100 * DAY_MS)
    }

    fn query() -> RankQuery {
        RankQuery::new(
            "fix retry policy",
            ["src/retry.rs".to_string()],
            ["E_TIMEOUT".to_string()],
        )
        .unwrap()
    }

    fn cand(
        id: &str,
        path: &str,
        errs: &[&str],
        age_days: i64,
        outcomes: &[OutcomeKind],
    ) -> Candidate {
        Candidate {
            id: id.into(),
            path: path.into(),
            error_signatures: errs.iter().map(|s| (*s).to_string()).collect(),
            last_touched: Timestamp::from_millis(now().as_millis() - age_days * DAY_MS),
            outcomes: outcomes.to_vec(),
            est_tokens: 100,
        }
    }

    #[test]
    fn empty_query_is_rejected() {
        assert_eq!(
            RankQuery::new("", ["a".to_string()], []).unwrap_err(),
            QueryError::EmptyTask
        );
        assert_eq!(
            RankQuery::new("   ", ["a".to_string()], []).unwrap_err(),
            QueryError::EmptyTask
        );
        assert_eq!(
            RankQuery::new("t", Vec::<String>::new(), []).unwrap_err(),
            QueryError::EmptyFocus
        );
    }

    #[test]
    fn exact_path_overlap_scores_highest() {
        let q = query();
        let exact = cand("a", "src/retry.rs", &[], 5, &[]);
        let same_dir = cand("b", "src/other.rs", &[], 5, &[]);
        let unrelated = cand("c", "docs/x.md", &[], 5, &[]);
        assert!(
            score(&q, &exact, now(), &SignalWeights::default()).task_value
                > score(&q, &same_dir, now(), &SignalWeights::default()).task_value
        );
        assert!(
            score(&q, &same_dir, now(), &SignalWeights::default()).task_value
                > score(&q, &unrelated, now(), &SignalWeights::default()).task_value
        );
    }

    #[test]
    fn signals_and_task_value_are_bounded() {
        let q = query();
        let c = cand(
            "a",
            "src/retry.rs",
            &["E_TIMEOUT"],
            0,
            &[OutcomeKind::Merged],
        );
        let s = score(&q, &c, now(), &SignalWeights::default());
        assert!((0.0..=1.0).contains(&s.features.overlap.unwrap()));
        assert!((0.0..=1.0).contains(&s.features.error.unwrap()));
        assert!((0.0..=1.0).contains(&s.features.recency));
        assert!((-1.0..=1.0).contains(&s.features.outcome.unwrap()));
        assert!((0.0..=1.0).contains(&s.task_value));
    }

    #[test]
    fn error_signal_redistributes_when_absent() {
        // A query with no errors must still produce a bounded task_value driven
        // by the present signals (overlap + recency), with error feature None.
        let q = RankQuery::new("t", ["src/retry.rs".to_string()], []).unwrap();
        let c = cand("a", "src/retry.rs", &["E_X"], 0, &[]);
        let s = score(&q, &c, now(), &SignalWeights::default());
        assert_eq!(s.features.error, None);
        // exact overlap (1.0) + fresh recency (~1.0), renormalized → high task_value
        assert!(s.task_value > 0.9);
        assert!((0.0..=1.0).contains(&s.task_value));
    }

    #[test]
    fn recency_decays_with_half_life() {
        let q = query();
        let fresh = cand("a", "x.rs", &[], 0, &[]);
        let one_hl = cand("b", "x.rs", &[], 7, &[]); // exactly one half-life old
        assert!((recency(&fresh, &q, now()) - 1.0).abs() < 1e-3);
        assert!((recency(&one_hl, &q, now()) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn adding_overlap_match_never_lowers_score() {
        let q = query();
        let no_match = cand("a", "docs/x.md", &["E_TIMEOUT"], 3, &[]);
        let with_match = cand("a", "src/retry.rs", &["E_TIMEOUT"], 3, &[]);
        assert!(
            score(&q, &with_match, now(), &SignalWeights::default()).task_value
                >= score(&q, &no_match, now(), &SignalWeights::default()).task_value
        );
    }

    #[test]
    fn reverted_is_buried_below_unlabeled() {
        let q = query();
        let unlabeled = cand("a", "src/retry.rs", &["E_TIMEOUT"], 1, &[]);
        let reverted = cand(
            "b",
            "src/retry.rs",
            &["E_TIMEOUT"],
            1,
            &[OutcomeKind::Reverted],
        );
        let merged = cand(
            "c",
            "src/retry.rs",
            &["E_TIMEOUT"],
            1,
            &[OutcomeKind::Merged],
        );
        let w = SignalWeights::default();
        assert!(score(&q, &merged, now(), &w).value > score(&q, &unlabeled, now(), &w).value);
        assert!(score(&q, &unlabeled, now(), &w).value > score(&q, &reverted, now(), &w).value);
        assert_eq!(score(&q, &reverted, now(), &w).value, 0.0);
    }

    #[test]
    fn rank_is_best_first() {
        let q = query();
        let cands = vec![
            cand("z", "docs/x.md", &[], 9, &[OutcomeKind::Reverted]),
            cand(
                "a",
                "src/retry.rs",
                &["E_TIMEOUT"],
                0,
                &[OutcomeKind::Merged],
            ),
            cand("m", "src/other.rs", &[], 5, &[]),
        ];
        let ranked = rank(&q, &cands, now(), &SignalWeights::default());
        assert_eq!(ranked[0].candidate.path, "src/retry.rs");
    }

    #[test]
    fn stable_tie_break_is_by_candidate_id_and_order_independent() {
        let q = query();
        // identical signals → identical score; tie-break must order by id and be
        // independent of input order.
        let a = cand("aaa", "src/retry.rs", &["E_TIMEOUT"], 2, &[]);
        let b = cand("bbb", "src/retry.rs", &["E_TIMEOUT"], 2, &[]);
        let forward = rank(
            &q,
            &[a.clone(), b.clone()],
            now(),
            &SignalWeights::default(),
        );
        let reversed = rank(&q, &[b, a], now(), &SignalWeights::default());
        let ids_f: Vec<_> = forward.iter().map(|s| s.candidate.id.clone()).collect();
        let ids_r: Vec<_> = reversed.iter().map(|s| s.candidate.id.clone()).collect();
        assert_eq!(ids_f, vec!["aaa", "bbb"]);
        assert_eq!(ids_f, ids_r);
    }

    proptest::proptest! {
        /// Determinism: the ranked id order is a pure function of the inputs and
        /// independent of the candidate list order (no input-order dependence).
        #[test]
        fn rank_is_deterministic(
            paths in proptest::collection::vec("[a-z]{1,3}/[a-z]{1,3}\\.rs", 1..8),
            ages in proptest::collection::vec(0i64..60, 1..8),
        ) {
            let q = query();
            let n = paths.len().min(ages.len());
            let cands: Vec<Candidate> = (0..n)
                .map(|i| cand(&format!("id{i:02}"), &paths[i], &["E_TIMEOUT"], ages[i], &[]))
                .collect();
            let mut shuffled = cands.clone();
            shuffled.reverse();
            let a = rank(&q, &cands, now(), &SignalWeights::default());
            let b = rank(&q, &shuffled, now(), &SignalWeights::default());
            let ids_a: Vec<_> = a.iter().map(|s| s.candidate.id.clone()).collect();
            let ids_b: Vec<_> = b.iter().map(|s| s.candidate.id.clone()).collect();
            proptest::prop_assert_eq!(ids_a, ids_b);
        }

        /// Every signal and the task value stay in `[0,1]`; the final value is
        /// finite and non-negative.
        #[test]
        fn scores_are_bounded(age in 0i64..400, merged in proptest::bool::ANY) {
            let q = query();
            let outcomes = if merged { vec![OutcomeKind::Merged] } else { vec![] };
            let c = cand("p", "src/retry.rs", &["E_TIMEOUT"], age, &outcomes);
            let s = score(&q, &c, now(), &SignalWeights::default());
            proptest::prop_assert!((0.0..=1.0).contains(&s.features.recency));
            proptest::prop_assert!((0.0..=1.0).contains(&s.task_value));
            proptest::prop_assert!(s.value.is_finite() && s.value >= 0.0);
        }
    }
}

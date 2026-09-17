//! Delta-ledger metrics and the scope-containment model.
//!
//! The two numbers Rift instruments **before** it optimizes (so savings are the
//! customer's own measured numbers — measure before optimizing):
//!
//! * [`repeated_context_rate`] — input tokens spent re-reading artifacts whose
//!   content hash was *already known* ÷ total input tokens. This is the avoidable
//!   re-ingestion the recall plane targets.
//! * [`avoidable_context_rate`] — repeated reads **plus** reads of content
//!   identical to the current worktree (servable as a delta/summary).
//!
//! [`Scope`](rift_recall_types::Scope) is the default query containment (current repo/branch/developer,
//! last-N checkpoints); a query never returns rows outside its declared scope
//! (containment: a query never escapes its scope).
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        // small proptest loop indices cast to i64 ordinals/token counts
        clippy::cast_possible_wrap
    )
)]

pub mod blob_bridge;
pub mod blob_map;
pub mod candidates;
pub mod doctor;
pub mod grants;
pub mod graph;
pub mod hunks;
pub mod ingest;
pub mod metrics;
pub mod outcome;
pub mod production;
pub mod provenance;
pub mod savings;
pub mod savings_window;
pub mod scope_query;
pub mod tokens;
pub mod validity_store;

use std::collections::BTreeSet;

pub use savings::SAVINGS_TARGET_PCT;

/// The staged **bar**: the median savings the evaluation harness's paired
/// replays have measured, which is the most its go/no-go gate may demand.
///
/// It is read by `rift_recall_eval::GaThresholds::V1`, whose measure is the
/// P50 of `(baseline - delta) / baseline` over the harness's scripted
/// scenarios (`rift-memd eval`): a full re-read leg against a delta-served leg.
/// That is **not** the share `rift savings` publishes over a person's own
/// traffic, and not what `cargo run -p xtask -- replay-savings` reports over
/// recorded transcripts; those carry first reads and searches in their
/// denominator and are read against [`SAVINGS_TARGET_PCT`], never against this.
///
/// The rule, in one sentence: **this value is raised only after the harness
/// has measured a higher median, and never above what it last measured.** A
/// bar set to an aspiration fails builds for work nobody has done yet, and a
/// bar that drifts up with a hopeful edit stops being evidence of anything.
/// 15 is the original calibration seed, taken against the 33.7% avoidable
/// context the reread report measured on this repository's own sessions
/// (`docs/guide/08-cost-and-savings.md` §8.12). [`SAVINGS_TARGET_PCT`] is
/// where the ambition lives; this is where the measurement does, and the two
/// sit beside each other so neither is read alone.
pub const SAVINGS_BAR_PCT: f64 = 15.0;

/// A single file-read event captured from the session ledger.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileRead {
    /// Repo-relative path read.
    pub path: String,
    /// SHA-256 of the content at read time.
    pub content_sha256: String,
    /// Input tokens this read contributed.
    pub input_tokens: u32,
}

/// The fraction of input tokens spent re-reading content whose `(path, hash)`
/// was already seen earlier in the read stream. `0.0` if no reads.
#[must_use]
pub fn repeated_context_rate(reads: &[FileRead]) -> f64 {
    let total: u64 = reads.iter().map(|r| u64::from(r.input_tokens)).sum();
    if total == 0 {
        return 0.0;
    }
    let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut repeated: u64 = 0;
    for r in reads {
        if !seen.insert((r.path.as_str(), r.content_sha256.as_str())) {
            repeated += u64::from(r.input_tokens);
        }
    }
    repeated as f64 / total as f64
}

/// The input tokens that were avoidable: those spent on repeated reads, plus
/// reads of content identical to the current worktree (`worktree[path] == hash`)
/// that could have been served as a delta/handle instead of re-read in full.
///
/// This is the numerator behind [`avoidable_context_rate`]; the two share one
/// pass via this function. The plan's definition-of-done is `tokens_avoided > 0`
/// — measure the customer's own number before optimizing.
#[must_use]
pub fn tokens_avoided(
    reads: &[FileRead],
    worktree: &std::collections::BTreeMap<String, String>,
) -> u64 {
    let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut avoidable: u64 = 0;
    for r in reads {
        let repeated = !seen.insert((r.path.as_str(), r.content_sha256.as_str()));
        let unchanged_vs_worktree = worktree
            .get(&r.path)
            .is_some_and(|h| h == &r.content_sha256);
        if repeated || unchanged_vs_worktree {
            avoidable += u64::from(r.input_tokens);
        }
    }
    avoidable
}

/// The fraction of input tokens that were avoidable: repeated reads, plus reads
/// of content identical to the current worktree (`worktree[path] == hash`) that
/// could have been served as a delta instead of re-read in full.
#[must_use]
pub fn avoidable_context_rate(
    reads: &[FileRead],
    worktree: &std::collections::BTreeMap<String, String>,
) -> f64 {
    let total: u64 = reads.iter().map(|r| u64::from(r.input_tokens)).sum();
    savings_rate(tokens_avoided(reads, worktree), total)
}

/// The guarded savings ratio `avoided / total` with `0/0 ≡ 0` — the one
/// definition [`avoidable_context_rate`] and the dashboard's served-audit
/// savings % both use, so the two can never drift. `total` is the
/// would-be full context (served + avoided); `avoided` the part recall spared.
#[must_use]
pub fn savings_rate(avoided: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    avoided as f64 / total as f64
}

/// A compact summary of the delta-ledger metrics for one read stream against the
/// current worktree — what `rift-memd metrics` reports.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct RecallMetrics {
    /// Total input tokens across all reads.
    pub total_input_tokens: u64,
    /// Input tokens spent re-reading already-seen `(path, hash)` content.
    pub repeated_rate: f64,
    /// Fraction of input tokens that were avoidable (repeated + unchanged-vs-worktree).
    pub avoidable_rate: f64,
    /// Absolute avoidable input tokens (the headline number; DoD: `> 0`).
    pub tokens_avoided: u64,
}

impl RecallMetrics {
    /// Compute the metrics for `reads` against the current `worktree` hashes.
    #[must_use]
    pub fn compute(
        reads: &[FileRead],
        worktree: &std::collections::BTreeMap<String, String>,
    ) -> Self {
        let total_input_tokens: u64 = reads.iter().map(|r| u64::from(r.input_tokens)).sum();
        Self {
            total_input_tokens,
            repeated_rate: repeated_context_rate(reads),
            avoidable_rate: avoidable_context_rate(reads, worktree),
            tokens_avoided: tokens_avoided(reads, worktree),
        }
    }
}

// The scope *type* now lives in `rift-recall-types` (`Scope`/`CrossScopeGrant`,
// shared with the serve/pack consumers); the membership chokepoint that bounds
// every query to it is [`scope_query::in_scope_nodes`]. The graph-backed,
// scope-aware metrics are in [`metrics`].

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn rd(path: &str, hash: &str, tokens: u32) -> FileRead {
        FileRead {
            path: path.into(),
            content_sha256: hash.into(),
            input_tokens: tokens,
        }
    }

    #[test]
    fn repeated_rate_counts_rereads_only() {
        let reads = [rd("a", "h1", 100), rd("b", "h2", 100), rd("a", "h1", 100)];
        // third read repeats (a,h1) → 100 repeated / 300 total
        assert!((repeated_context_rate(&reads) - (100.0 / 300.0)).abs() < 1e-9);
    }

    #[test]
    fn repeated_rate_zero_when_all_distinct_or_empty() {
        assert_eq!(repeated_context_rate(&[]), 0.0);
        let reads = [rd("a", "h1", 50), rd("a", "h2", 50)]; // same path, changed content
        assert_eq!(repeated_context_rate(&reads), 0.0);
    }

    #[test]
    fn avoidable_includes_unchanged_vs_worktree() {
        let reads = [rd("a", "h1", 100), rd("b", "h2", 100)];
        let mut wt = BTreeMap::new();
        wt.insert("a".to_string(), "h1".to_string()); // a unchanged vs worktree
                                                      // 'a' (100) avoidable; 'b' not in worktree → 100/200
        assert!((avoidable_context_rate(&reads, &wt) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn tokens_avoided_counts_repeats_and_unchanged() {
        // (a,h1) read twice → second is a repeat (100); (b,h2) unchanged vs worktree (100).
        let reads = [rd("a", "h1", 100), rd("b", "h2", 100), rd("a", "h1", 100)];
        let mut wt = BTreeMap::new();
        wt.insert("b".to_string(), "h2".to_string());
        assert_eq!(tokens_avoided(&reads, &wt), 200);
    }

    #[test]
    fn recall_metrics_summarize_consistently() {
        let reads = [rd("a", "h1", 100), rd("a", "h1", 100)];
        let wt = BTreeMap::new();
        let m = RecallMetrics::compute(&reads, &wt);
        assert_eq!(m.total_input_tokens, 200);
        assert_eq!(m.tokens_avoided, 100);
        assert!((m.avoidable_rate - 0.5).abs() < 1e-9);
        assert!((m.repeated_rate - 0.5).abs() < 1e-9);
        assert!(
            m.tokens_avoided > 0,
            "definition-of-done: tokens_avoided > 0"
        );
    }

    proptest::proptest! {
        /// Rates are always within [0,1].
        #[test]
        fn rates_bounded(tokens in proptest::collection::vec(1u32..1000, 0..30)) {
            let reads: Vec<FileRead> = tokens.iter().enumerate()
                .map(|(i, t)| rd(if i % 2 == 0 { "a" } else { "b" }, "h", *t)).collect();
            let r = repeated_context_rate(&reads);
            proptest::prop_assert!((0.0..=1.0).contains(&r));
        }
    }
}

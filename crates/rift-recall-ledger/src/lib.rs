//! Delta-ledger metrics and the scope-containment model.
//!
//! The two numbers Rift instruments **before** it optimizes (so savings are the
//! customer's own measured numbers, `01-vision` principle 9):
//!
//! * [`repeated_context_rate`] — input tokens spent re-reading artifacts whose
//!   content hash was *already known* ÷ total input tokens. This is the avoidable
//!   re-ingestion the recall plane targets.
//! * [`avoidable_context_rate`] — repeated reads **plus** reads of content
//!   identical to the current worktree (servable as a delta/summary).
//!
//! [`Scope`] is the default query containment (current repo/branch/developer,
//! last-N checkpoints); a query never returns rows outside its declared scope
//! (W14, `05-security` §5.5).
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]
#![allow(
    clippy::module_name_repetitions,
    clippy::doc_markdown,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

pub mod candidates;
pub mod graph;
pub mod hunks;
pub mod ingest;
pub mod metrics;
pub mod outcome;
pub mod scope_query;
pub mod validity_store;

use std::collections::BTreeSet;

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
/// (`01-vision` principle 9 — measure the customer's own number before optimizing).
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
    if total == 0 {
        return 0.0;
    }
    tokens_avoided(reads, worktree) as f64 / total as f64
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
// shared with the MCP/pack consumers); the membership chokepoint that bounds
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

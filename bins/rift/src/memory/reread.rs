//! `rift memory reread-report` — the whole document.
//!
//! Four sources answer it, each named beside its figures: the recall
//! projection's metrics, the capture projection's repeat classes, the audit
//! relations' levers, and this member's own window journals — then the same
//! four again, cut once at the moment the gate began declining.

use std::collections::BTreeSet;
use std::path::Path;

use rift_core::Result;
use rift_recall_ledger::metrics::{compute_metrics, record_metric_snapshot, ContextMetrics};
use rift_recall_ledger::savings_window;
use rift_recall_substrate::worktree;
use rift_recall_types::{NoAncestry, Scope};
use rift_store::{CanonicalStore, SqliteStore};

use crate::cli::Outcome;

use super::classes::{family_repeat_classes, render_repeat_classes};
use super::journals::{member_window_journals, render_compaction, render_window_journals};
use super::levers::{family_levers, render_levers};
use super::panes::{family_enforce_split, render_enforce_split};
use super::savings::render_pull_claims;

/// The data behind one reread report (separated from rendering so the report
/// format is lockable by a golden test with fixed inputs).
pub(super) struct RereadReport {
    /// Captured sessions covered.
    pub(super) sessions: u64,
    /// The aggregate metrics.
    pub(super) metrics: ContextMetrics,
    /// `(session_id, input_tokens)` per session, descending.
    pub(super) per_session: Vec<(String, u64)>,
    /// The tokenizer that produced every number ("skeptical buyer" provenance).
    pub(super) tokenizer: &'static str,
}

/// Render the locked report format (golden-tested).
fn render_reread_report(r: &RereadReport) -> String {
    let m = &r.metrics;
    let unchanged_component = m.avoidable_tokens.saturating_sub(m.repeated_read_tokens);
    let mut out = String::new();
    out.push_str(&format!("reread report — {} session(s)\n", r.sessions));
    out.push_str(&format!("tokenizer:               {}\n", r.tokenizer));
    out.push_str(&format!(
        "total input tokens:      {}\n",
        m.total_input_tokens
    ));
    out.push_str(&format!(
        "repeated_context_rate:   {:.4}\n",
        m.repeated_context_rate
    ));
    out.push_str(&format!(
        "avoidable_context_rate:  {:.4}\n",
        m.avoidable_context_rate
    ));
    out.push_str("components:\n");
    out.push_str(&format!(
        "  repeated re-reads:     {}\n",
        m.repeated_read_tokens
    ));
    out.push_str(&format!("  unchanged-vs-worktree: {unchanged_component}\n"));
    out.push_str("per session:\n");
    if r.per_session.is_empty() {
        out.push_str("  (none)\n");
    }
    for (sid, tokens) in &r.per_session {
        let short: String = sid.chars().take(12).collect();
        out.push_str(&format!("  {short:<12}  {tokens} tok\n"));
    }
    out
}

/// The reread report's window note: empty unless this family's savings window
/// is narrower than the record the report covers.
fn family_window_note(repo: &Path) -> String {
    let Ok(family) = crate::family::resolve_at(repo) else {
        return String::new();
    };
    let window = savings_window::resolve(&family.common_dir, None);
    if !window.is_cut() {
        return String::new();
    }
    format!(
        "  the savings line above covers the whole record; `rift savings` reports\n  \
         this family's window ({})\n",
        window.label()
    )
}

/// Measured read tokens per session, heaviest first.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
async fn per_session_tokens(store: &SqliteStore) -> Result<Vec<(String, u64)>> {
    let rows = store
        .query(
            "SELECT e.from_node, CAST(SUM(r.input_tokens) AS INTEGER) FROM n_file_read r \
             JOIN graph_edge e ON e.to_node = r.checkpoint_id AND e.kind = 'created' \
             GROUP BY e.from_node ORDER BY SUM(r.input_tokens) DESC, e.from_node ASC",
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let sid = r.get_text(0).ok()?;
            let tok = u64::try_from(r.get_i64(1).ok()?).unwrap_or(0);
            Some((sid, tok))
        })
        .collect())
}

/// `rift memory reread-report`: the recall plane's savings evidence — honest,
/// explainable, appended immutably.
pub(super) async fn reread_report(
    store: &SqliteStore,
    repo: &Path,
    scope: &Scope,
    read_paths: &BTreeSet<String>,
) -> Result<Outcome> {
    let worktree_map = worktree::hash_paths(repo, read_paths);
    let metrics = compute_metrics(store, scope, &worktree_map, &NoAncestry).await?;
    record_metric_snapshot(store, &metrics, crate::hooks::now_ms()).await?;

    let report = RereadReport {
        sessions: u64::try_from(store.count("n_session").await?).unwrap_or(0),
        metrics: metrics.clone(),
        per_session: per_session_tokens(store).await?,
        tokenizer: rift_tokens::TOKENIZER,
    };
    // The pre-interception baseline, from the OTHER store: the recall
    // projection above knows revisions but not the order the reads happened
    // in, and the classes are an ordering fact. Both are family-keyed, so the
    // two halves of this report answer for the same set of checkouts.
    let classes = family_repeat_classes(repo).await;
    // The levers come from the recall projection's two audit relations, which
    // is a third store again: what was decided is not what was captured, and
    // the report says which relation every figure came out of.
    let levers = family_levers(repo).await;
    // And the fourth source: this member's own run state. The decisions the
    // gate took live in files, not in either projection, and a pool nobody
    // declined is only ever in the journal that recorded observing it.
    let windows = member_window_journals(repo);
    // And the same four sources again, cut once at the moment the gate began
    // declining: an observe half and an enforce half, so the two are compared
    // as measured windows rather than as one blended total.
    let split = family_enforce_split(repo).await;
    // What pulls claimed to hold, over the whole record like the rest of this
    // document, set beside how many of those revisions this family read.
    let claims = rift_recall_ledger::audit::pull_claim_totals(store, None)
        .await
        .ok();
    let human = format!(
        "{}{}{}{}{}{}{}{}",
        render_reread_report(&report),
        render_repeat_classes(&classes),
        render_levers(&levers, classes.total_tokens()),
        // This document covers the whole record; `rift savings` answers over
        // the family's window. When a boundary marker makes those two
        // different periods, the report says so here rather than let one
        // figure's name stand over two of them.
        family_window_note(repo),
        render_window_journals(&windows),
        // Appended after the block's own eviction lines rather than inside
        // it, so every line the block printed before is printed unchanged.
        render_compaction(&windows.compaction, windows.note.as_deref()),
        render_enforce_split(&split),
        // Last, so every line the report printed before is printed unchanged.
        render_pull_claims(claims.as_ref())
    );
    let db_path = crate::family::recall_db(repo)?;
    Ok(Outcome::ok(
        human,
        serde_json::json!({
            "sessions": report.sessions,
            "tokenizer": report.tokenizer,
            "metrics": metrics,
            "per_session": report.per_session,
            "repeat_classes": classes,
            "levers": levers,
            "window_journals": windows,
            "enforce_split": split,
            "pull_claims": claims,
            "snapshot": "appended",
            "recall_db": db_path.display().to_string(),
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::super::classes::{ClassTotals, RepeatClasses, SessionClasses};
    use super::super::journals::{RetryAfterDeny, SliceBucket, WindowJournals};
    use super::super::levers::{FamilyLevers, LeverKind, Levers, PullsAfterInjection};
    use super::*;

    /// The reread-report format is a locked artifact: this golden test and a
    /// snapshot test together pin it.
    #[test]
    fn reread_report_format_is_locked() {
        let report = RereadReport {
            sessions: 12,
            metrics: ContextMetrics {
                scope_hash: "abc".into(),
                total_input_tokens: 10_000,
                repeated_read_tokens: 1_500,
                avoidable_tokens: 2_200,
                repeated_context_rate: 0.15,
                avoidable_context_rate: 0.22,
            },
            per_session: vec![
                ("session-aaaa-1111".into(), 6_000),
                ("session-bbbb-2222".into(), 4_000),
            ],
            tokenizer: "tiktoken/o200k_base",
        };
        let rendered = render_reread_report(&report);
        let golden = "\
reread report — 12 session(s)
tokenizer:               tiktoken/o200k_base
total input tokens:      10000
repeated_context_rate:   0.1500
avoidable_context_rate:  0.2200
components:
  repeated re-reads:     1500
  unchanged-vs-worktree: 700
per session:
  session-aaaa  6000 tok
  session-bbbb  4000 tok
";
        assert_eq!(rendered, golden);
    }

    #[test]
    fn empty_report_renders_the_none_marker() {
        let report = RereadReport {
            sessions: 0,
            metrics: ContextMetrics {
                scope_hash: String::new(),
                total_input_tokens: 0,
                repeated_read_tokens: 0,
                avoidable_tokens: 0,
                repeated_context_rate: 0.0,
                avoidable_context_rate: 0.0,
            },
            per_session: Vec::new(),
            tokenizer: "tiktoken/o200k_base",
        };
        assert!(render_reread_report(&report).contains("(none)"));
    }

    /// The classes of the locked whole-report fixture.
    ///
    /// Every figure this report can carry appears in these three fixtures with
    /// a value, so the locks below are locks on the arithmetic as well as on
    /// the layout.
    fn locked_classes() -> RepeatClasses {
        RepeatClasses {
            projection: "/cache/fam/projection.db".to_owned(),
            sessions: 1,
            first_read: ClassTotals {
                reads: 3,
                tokens: 300,
            },
            reread_unchanged: ClassTotals {
                reads: 2,
                tokens: 600,
            },
            reread_changed: ClassTotals {
                reads: 1,
                tokens: 100,
            },
            reads_without_token_counts: 0,
            shell_reads: 0,
            reads_of_unknown_tools: 0,
            per_session: vec![SessionClasses {
                session_id: "session-aaaa-1111".to_owned(),
                first_read: ClassTotals {
                    reads: 3,
                    tokens: 300,
                },
                reread_unchanged: ClassTotals {
                    reads: 2,
                    tokens: 600,
                },
                reread_changed: ClassTotals {
                    reads: 1,
                    tokens: 100,
                },
            }],
            note: None,
        }
    }

    /// The levers of the locked whole-report fixture.
    fn locked_levers() -> Levers {
        Levers {
            recall_db: "/cache/fam/recall.db".to_owned(),
            note: None,
            kinds: vec![
                LeverKind {
                    verb: "memd.read_intercepted".to_owned(),
                    rows: 4,
                    tokens_returned: 160,
                    tokens_avoided: 900,
                },
                LeverKind {
                    verb: "memd.read_repeat_observed".to_owned(),
                    rows: 7,
                    tokens_returned: 0,
                    tokens_avoided: 0,
                },
            ],
            tokens_avoided: 900,
            tokens_returned: 160,
            families: vec![FamilyLevers {
                family: "claude-code".to_owned(),
                step: "read-gate".to_owned(),
                rows: 11,
                tokens_returned: 160,
                tokens_avoided: 900,
            }],
            pulls: vec![PullsAfterInjection {
                family: "claude-code".to_owned(),
                injections: 2,
                pulls: 1,
            }],
            pulls_without_injection: 1,
            held_revisions: 4,
            held_with_blob: 3,
        }
    }

    /// The window journals of the locked whole-report fixture.
    fn locked_windows() -> WindowJournals {
        WindowJournals {
            dir: "/repo/.rift/state/reads".to_owned(),
            note: None,
            windows: 2,
            lines: 12,
            reads: 6,
            read_tokens: 1000,
            reads_unmeasured: 1,
            reads_over_cap: 1,
            max_slice_bytes: 98_304,
            min_slice_lines: 24,
            pool_reads: 3,
            pool_tokens: 700,
            pool_unmeasured: 0,
            buckets: vec![
                SliceBucket {
                    label: "< 24 lines".to_owned(),
                    reads: 1,
                    tokens: 40,
                    unmeasured: 0,
                },
                SliceBucket {
                    label: "24–99 lines".to_owned(),
                    reads: 1,
                    tokens: 160,
                    unmeasured: 0,
                },
                SliceBucket {
                    label: "100–999 lines".to_owned(),
                    reads: 1,
                    tokens: 500,
                    unmeasured: 0,
                },
                SliceBucket {
                    label: "≥ 1000 lines".to_owned(),
                    reads: 0,
                    tokens: 0,
                    unmeasured: 0,
                },
            ],
            retries: vec![
                RetryAfterDeny {
                    frame: "delta".to_owned(),
                    denies: 2,
                    retried: 0,
                },
                RetryAfterDeny {
                    frame: "plain".to_owned(),
                    denies: 4,
                    retried: 1,
                },
            ],
            adjacent_reads: 2,
            evictions: 1,
            post_compaction_tokens: 250,
            compaction: super::super::journals::Compaction::default(),
        }
    }

    /// ★ The report's lever block, locked as an artifact.
    ///
    /// Two windows are compared line for line — the observe window and the
    /// enforce window that follows it — and a block that quietly changed
    /// shape between them would make that comparison a guess.
    #[test]
    fn the_lever_block_of_the_whole_report_is_locked() {
        let rendered = render_levers(&locked_levers(), locked_classes().total_tokens());
        let golden = "\
levers (audit relations, everything recorded since the reset):
  recall projection:     /cache/fam/recall.db
  verb                        rows    returned     avoided
  memd.read_intercepted          4         160         900
  memd.read_repeat_observed       7           0           0
  honesty (this relation): avoided 900 − returned 160 = 740
  savings: avoided ÷ (avoided + read) = 47.4% (900 ÷ 1900)
per family (the other relation, its own figures, never joined):
  claude-code:read-gate         11         160         900
pull after injection (a skill call within 600000 ms of one):
  claude-code                    2 injected       1 pulled
  pulls with no injection before them: 1
basis availability (recorded read revisions):
  held revisions:        4
  reachable as a blob:   3 (75.0%)
";
        assert_eq!(rendered, golden);
    }

    /// ★ The report's window-journal block, locked as an artifact, for the
    /// same reason its lever block is.
    #[test]
    fn the_window_journal_block_of_the_whole_report_is_locked() {
        let rendered = render_window_journals(&locked_windows());
        let golden = "\
window journals (this member's own run state):
  directory:             /repo/.rift/state/reads
  windows:               2
  lines:                 12
  reads:                 6 (1000 tok, 1 unmeasured)
  past the 98304 B cap:  1 (never eligible for a decline)
  would-have-declined:   3 reads, 700 tok (0 unmeasured), floor 24 lines
  by slice length        reads      tokens  unmeasured
    < 24 lines               1          40           0
    24–99 lines              1         160           0
    100–999 lines            1         500           0
    ≥ 1000 lines             0           0           0
  retry after decline   declines     retried
    delta                      2           0  0.0%
    plain                      4           1  25.0%
  range-walking:         2
  evictions:             1
  read within 3 turns of one: 250 of 1000 tok (25.0%)
";
        assert_eq!(rendered, golden);
    }
}

//! The read-stage summary `rift recall status --json` carries, and the one
//! line `rift doctor` and the human status print for reads that never became
//! queryable.
//!
//! Read-only end to end: the live journals are parsed in place and both
//! stores open read-only, only when they exist. Bounded to the most recent
//! [`RECENT_READS`] reads, so the cost does not grow with the family's history.

use std::path::Path;

use rift_recall_ledger::read_stages::{
    read_stage_facts, report, ArchiveStage, CallEmission, ReadReceipt, ReadStageFacts, StageFact,
    StageJoin, StageStores,
};
use serde::Serialize;

use crate::window_journal::{live_reads, LiveReads};

/// How many of the newest reads the summary covers.
pub(crate) const RECENT_READS: usize = 200;

/// A read older than this that is still not queryable is stuck: the recall
/// sweep runs every five minutes, so a whole interval without ingest is a
/// miss, not a wait.
pub(crate) const STUCK_AFTER_MS: i64 = 5 * 60 * 1000;

/// How many reads stopped at each archive stage.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Furthest {
    received: usize,
    captured: usize,
    queryable: usize,
}

/// The median and the 95th percentile of one step's lag.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Lag {
    p50_ms: Option<i64>,
    p95_ms: Option<i64>,
}

/// The three steps' lags, kept apart as the report keeps them.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Lags {
    archival: Lag,
    query: Lag,
    delivery: Lag,
}

/// How many matched stages rested on each join, and how many were ambiguous.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Joins {
    invocation_id: usize,
    path_hash: usize,
    ambiguous: usize,
}

/// The bounded summary of the newest reads.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct StageSummary {
    bound: usize,
    reads: usize,
    furthest: Furthest,
    delivered: usize,
    lag: Lags,
    joins: Joins,
    anomalies: usize,
    stuck_after_ms: i64,
    /// Reads whose capture or ingest could not be pinned to one checkpoint:
    /// the window read the same file at the same hash more than once, or
    /// several checkpoints hold it. Counted apart because such a read is not
    /// known to be stuck, only not attributable.
    ambiguous: usize,
    /// Reads past the stuck window that are not queryable, never counting an
    /// ambiguous one.
    never_queryable: usize,
}

/// The summary for the checkout at `repo`, or `None` when it has no live
/// reads or either store does not exist or cannot be read.
pub(crate) fn summary(repo: &Path, now_ms: i64) -> Option<StageSummary> {
    let live = live_reads(repo, RECENT_READS);
    if live.reads.is_empty() {
        return None;
    }
    let recall_db = crate::family::recall_db(repo)
        .ok()
        .filter(|db| db.exists())?;
    let recall = rift_store::SqliteStore::open_read_only(&recall_db).ok()?;
    let capture = crate::projection::open_projection_read_only(repo).ok()?;
    let rt = crate::projection::runtime().ok()?;
    let stores = StageStores {
        recall: &recall,
        capture: &capture,
    };
    let (receipts, calls) = stage_inputs(&live);
    let facts = rt
        .block_on(read_stage_facts(stores, &receipts, &calls))
        .ok()?;
    Some(summarize(&facts, now_ms))
}

/// The summary as `rift recall status --json` carries it: `null` when no
/// summary could be read, so a script never reads "nothing stuck" into it.
pub(crate) fn summary_json(summary: Option<&StageSummary>) -> serde_json::Value {
    summary
        .and_then(|s| serde_json::to_value(s).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// The one sentence for reads that never became queryable, or `None`.
pub(crate) fn stuck_sentence(summary: &StageSummary) -> Option<String> {
    (summary.never_queryable > 0).then(|| {
        format!(
            "{} of the last {} reads never became queryable within {} min",
            summary.never_queryable,
            summary.reads,
            summary.stuck_after_ms / 60_000
        )
    })
}

/// The doctor leg: one line naming the reads that never became queryable,
/// silent otherwise. Report-only; the JSON is the count, or `null` when no
/// summary could be read.
pub(crate) fn doctor_leg(repo: &Path) -> (String, serde_json::Value) {
    let Some(summary) = summary(repo, now_ms()) else {
        return (String::new(), serde_json::Value::Null);
    };
    let line = stuck_sentence(&summary)
        .map(|sentence| format!("\n  read stages:    {sentence}"))
        .unwrap_or_default();
    (
        line,
        serde_json::json!({ "never_queryable": summary.never_queryable }),
    )
}

/// The wall clock in epoch milliseconds, read once at the edge.
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// The receipts and call facts in the shape the store reader takes. The call
/// id is known only where the window's answer line and call sidecar tie a
/// read to one emitted call.
fn stage_inputs(live: &LiveReads) -> (Vec<ReadReceipt<'_>>, Vec<CallEmission<'_>>) {
    let receipts = live
        .reads
        .iter()
        .map(|r| ReadReceipt {
            window_key: &r.window_key,
            path: &r.path,
            content_sha256: &r.content_sha256,
            received_at: r.received_at,
            invocation_id: r.invocation_id.as_deref(),
        })
        .collect();
    let calls = live
        .calls
        .iter()
        .map(|c| CallEmission {
            invocation_id: &c.invocation_id,
            wrote: c.wrote,
            at_ms: c.at_ms,
        })
        .collect();
    (receipts, calls)
}

/// Fold the facts of the newest reads into the summary.
fn summarize(facts: &[ReadStageFacts], now_ms: i64) -> StageSummary {
    let mut summary = StageSummary {
        bound: RECENT_READS,
        reads: facts.len(),
        stuck_after_ms: STUCK_AFTER_MS,
        ..StageSummary::default()
    };
    let (mut archival, mut query, mut delivery) = (Vec::new(), Vec::new(), Vec::new());
    for fact in facts {
        let read = report(fact.times());
        match read.archived_to {
            ArchiveStage::Received => summary.furthest.received += 1,
            ArchiveStage::Captured => summary.furthest.captured += 1,
            ArchiveStage::Queryable => summary.furthest.queryable += 1,
        }
        let overdue = now_ms.saturating_sub(fact.received_at) >= STUCK_AFTER_MS;
        if is_ambiguous(fact) {
            summary.ambiguous += 1;
        } else if read.archived_to < ArchiveStage::Queryable && overdue {
            summary.never_queryable += 1;
        }
        summary.delivered += usize::from(read.delivered);
        summary.anomalies += read.anomalies.len();
        archival.extend(read.archival_lag_ms);
        query.extend(read.query_lag_ms);
        delivery.extend(read.delivery_latency_ms);
        for stage in [fact.captured, fact.queryable, fact.delivered] {
            count_join(&mut summary.joins, stage);
        }
    }
    summary.lag = Lags {
        archival: lag(archival),
        query: lag(query),
        delivery: lag(delivery),
    };
    summary
}

/// Whether the read's capture or ingest matched more than one candidate, so
/// its archive stage cannot be told apart from a stuck one.
fn is_ambiguous(fact: &ReadStageFacts) -> bool {
    [fact.captured, fact.queryable]
        .into_iter()
        .any(|stage| matches!(stage, StageFact::Ambiguous { .. }))
}

fn count_join(joins: &mut Joins, stage: StageFact) {
    match stage {
        StageFact::Absent => {}
        StageFact::At { join, .. } => match join {
            StageJoin::InvocationId => joins.invocation_id += 1,
            StageJoin::PathHash => joins.path_hash += 1,
        },
        StageFact::Ambiguous { .. } => joins.ambiguous += 1,
    }
}

fn lag(mut samples: Vec<i64>) -> Lag {
    samples.sort_unstable();
    Lag {
        p50_ms: percentile(&samples, 50),
        p95_ms: percentile(&samples, 95),
    }
}

/// The nearest-rank percentile of sorted samples.
fn percentile(sorted: &[i64], pct: usize) -> Option<i64> {
    let rank = (sorted.len() * pct).div_ceil(100);
    sorted.get(rank.checked_sub(1)?).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 10 * 60 * 1000;

    fn at(at_ms: i64) -> StageFact {
        StageFact::At {
            at_ms,
            join: StageJoin::PathHash,
        }
    }

    fn facts(received_at: i64, captured: StageFact, queryable: StageFact) -> ReadStageFacts {
        ReadStageFacts {
            received_at,
            captured,
            queryable,
            delivered: StageFact::Absent,
        }
    }

    fn mixed() -> Vec<ReadStageFacts> {
        let mut delivered = facts(100, at(150), at(400));
        delivered.delivered = StageFact::At {
            at_ms: 120,
            join: StageJoin::InvocationId,
        };
        vec![
            delivered,
            facts(200, at(300), StageFact::Absent),
            facts(NOW - 1000, StageFact::Absent, StageFact::Absent),
            facts(
                300,
                StageFact::Ambiguous {
                    join: StageJoin::PathHash,
                },
                StageFact::Absent,
            ),
        ]
    }

    #[test]
    fn read_stages_summary_counts_stages_lags_and_joins_on_mixed_reads() {
        let json = summary_json(Some(&summarize(&mixed(), NOW)));
        assert_eq!(
            json,
            serde_json::json!({
                "bound": RECENT_READS,
                "reads": 4,
                "furthest": { "received": 2, "captured": 1, "queryable": 1 },
                "delivered": 1,
                "lag": {
                    "archival": { "p50_ms": 50, "p95_ms": 100 },
                    "query": { "p50_ms": 250, "p95_ms": 250 },
                    "delivery": { "p50_ms": 20, "p95_ms": 20 },
                },
                "joins": { "invocation_id": 1, "path_hash": 3, "ambiguous": 1 },
                "anomalies": 0,
                "stuck_after_ms": STUCK_AFTER_MS,
                "ambiguous": 1,
                "never_queryable": 1,
            })
        );
    }

    #[test]
    fn read_stages_stuck_sentence_appears_only_when_a_read_never_became_queryable() {
        let stuck = summarize(&mixed(), NOW);
        assert_eq!(
            stuck_sentence(&stuck).as_deref(),
            Some("1 of the last 4 reads never became queryable within 5 min")
        );
        let fresh = summarize(
            &[facts(NOW - 1000, StageFact::Absent, StageFact::Absent)],
            NOW,
        );
        assert_eq!(stuck_sentence(&fresh), None);
        assert_eq!(summary_json(None), serde_json::Value::Null);
    }

    #[test]
    fn read_stages_ambiguous_read_is_counted_apart_and_never_called_stuck() {
        let ambiguous = StageFact::Ambiguous {
            join: StageJoin::PathHash,
        };
        let summary = summarize(&[facts(100, ambiguous, ambiguous)], NOW);
        let json = summary_json(Some(&summary));
        assert_eq!(json["ambiguous"], serde_json::json!(1));
        assert_eq!(json["never_queryable"], serde_json::json!(0));
        assert_eq!(stuck_sentence(&summary), None);
    }

    #[test]
    fn read_stages_summary_is_absent_and_writes_nothing_without_live_reads() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(summary(tmp.path(), NOW), None);
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }
}

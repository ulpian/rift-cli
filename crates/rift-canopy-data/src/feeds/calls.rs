//! The Recall card's call side: the per-channel serve-call series and the
//! count beside the headline.

use std::collections::BTreeMap;
use std::path::Path;

use rift_canopy::col3::{CallsData, ChannelCallSeries};
use rift_store::{recall_db_for_family_under, CanonicalStore, SqliteStore, Value};

use super::project_family;
use super::window::mix_window;
use crate::store::runtime;

/// The attributed-call read outcome. It is independent of the legacy savings
/// trail so internal-error-only calls and legacy-savings-only databases are
/// both represented honestly.
pub(super) enum CallDb {
    Absent,
    Quiet,
    Data(CallsData),
}

const CALL_BUCKET_MS: i64 = 60_000;
const CALL_BUCKETS: usize = 30;

/// Read a bounded, event-anchored per-channel serve-call window from
/// `serve_call_event`. The count beside the headline is the **card's window**;
/// only the final 30 minutes of rows enter memory for the chart.
///
/// Two different periods, and both say which they are. The count answers "how
/// many serves in the period this card is showing", because a count that
/// ignored the period buttons put an all-time figure on a row whose every
/// other number was a day old. The chart is event-anchored to the last thirty
/// minutes of activity and carries its own `−30m … last` axis, which is the
/// window it has always drawn and the one it labels.
pub(super) fn read_call_data(root: &Path, path: &str, cut_ms: Option<i64>) -> CallDb {
    let Some(family) = project_family(Path::new(path)) else {
        return CallDb::Absent;
    };
    let db = recall_db_for_family_under(root, &family);
    if !db.exists() {
        return CallDb::Absent;
    }
    let (Ok(store), Ok(rt)) = (SqliteStore::open_read_only(&db), runtime()) else {
        return CallDb::Quiet;
    };
    // Every unanswerable read below is the same answer — enabled, nothing to
    // draw from this window — so they are spelled as absence and become
    // `Quiet` here, once.
    match read_calls(&store, &rt, cut_ms) {
        Some(data) => CallDb::Data(data),
        None => CallDb::Quiet,
    }
}

/// The call figures themselves, or `None` for every shape that has none: a
/// pre-v11 database without the relation, a temporarily unreadable one, and a
/// repository that has never served.
fn read_calls(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    cut_ms: Option<i64>,
) -> Option<CallsData> {
    // Quiet-vs-data is decided over the WHOLE relation: a repository that has
    // served, on a period that holds nothing, is enabled-and-quiet in this
    // window rather than a pre-v11 store. Both queries are needed because they
    // answer different questions and the second is what the row prints.
    let ever = rt
        .block_on(store.query_opt_i64("SELECT COUNT(*) FROM serve_call_event", &[]))
        .ok()
        .flatten()?;
    if ever <= 0 {
        return None;
    }
    let (clause, params) = mix_window("recorded_at", cut_ms);
    let total_i64 = rt
        .block_on(store.query_opt_i64(
            &format!("SELECT COUNT(*) FROM serve_call_event{clause}"),
            &params,
        ))
        .ok()
        .flatten()?;
    let latest = rt
        .block_on(store.query_opt_i64("SELECT MAX(recorded_at) FROM serve_call_event", &[]))
        .ok()
        .flatten()?;
    let latest_minute = latest.div_euclid(CALL_BUCKET_MS) * CALL_BUCKET_MS;
    let window_end_ms = latest_minute.saturating_add(CALL_BUCKET_MS);
    let window_start_ms = window_end_ms
        .saturating_sub(CALL_BUCKET_MS.saturating_mul(i64::try_from(CALL_BUCKETS).unwrap_or(30)));
    // The chart's buckets stay where they are — the axis labels them −30m to
    // last — but it may not draw a call the card's window excludes: a bar
    // standing over a count of `0` would be the two halves of one row
    // describing two periods again.
    let rows_from_ms = cut_ms.map_or(window_start_ms, |cut| window_start_ms.max(cut));
    let rows = rt
        .block_on(store.query(
            "SELECT channel, recorded_at FROM serve_call_event \
             WHERE recorded_at >= ? AND recorded_at < ? \
             ORDER BY recorded_at, call_id",
            &[Value::Int(rows_from_ms), Value::Int(window_end_ms)],
        ))
        .ok()?;
    Some(CallsData {
        total: u64::try_from(total_i64).unwrap_or(u64::MAX),
        window_end_ms,
        series: channel_series(&rows, window_start_ms),
    })
}

/// Bucket the window's calls per channel, freshest channel first.
///
/// The grouping key is `channel`, whose CHECK constraint admits exactly the
/// closed `ServeChannel` pair, so the result set is bounded by the schema —
/// there is no visible-set truncation and therefore no hidden remainder to
/// report.
fn channel_series(rows: &[rift_store::Row], window_start_ms: i64) -> Vec<ChannelCallSeries> {
    let mut grouped: BTreeMap<String, (i64, Vec<f64>)> = BTreeMap::new();
    for row in rows {
        let (Ok(channel), Ok(recorded_at)) = (row.get_text(0), row.get_i64(1)) else {
            continue;
        };
        let offset = recorded_at.saturating_sub(window_start_ms) / CALL_BUCKET_MS;
        let Ok(bucket) = usize::try_from(offset) else {
            continue;
        };
        let entry = grouped
            .entry(channel)
            .or_insert_with(|| (recorded_at, vec![0.0; CALL_BUCKETS]));
        entry.0 = entry.0.max(recorded_at);
        if let Some(value) = entry.1.get_mut(bucket) {
            *value += 1.0;
        }
    }
    let mut series: Vec<ChannelCallSeries> = grouped
        .into_iter()
        .map(|(channel, (last_call_at, rate))| ChannelCallSeries {
            channel,
            last_call_at,
            rate,
        })
        .collect();
    series.sort_by(|a, b| {
        b.last_call_at
            .cmp(&a.last_call_at)
            .then_with(|| a.channel.cmp(&b.channel))
    });
    series
}

#[cfg(test)]
mod tests {
    use rift_canopy::col3::{RecallView, RecallWindow};
    use rift_canopy::FeedPayload;
    use rift_store::{recall_db_under, recall_repo_id};

    use super::*;
    use crate::feeds::compute_recall;
    use crate::feeds::fixtures::{make_project, USD_NOW};

    /// Seeds `serve_call_event` with an explicit per-row channel — the shape
    /// the Calls card reads. `seed_serve_call_events` below is the same table
    /// with the channel fixed, for the window queries that only count rows.
    fn seed_call_events(root: &Path, path: &str, events: &[(&str, &str, i64)]) {
        let repo = recall_repo_id(Path::new(path)).unwrap();
        let db = recall_db_under(root, &repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
        let rt = runtime().unwrap();
        for (call_id, channel, recorded_at) in events {
            rt.block_on(store.insert(
                "serve_call_event",
                &[
                    "call_id",
                    "channel",
                    "tool_name",
                    "terminal_verb",
                    "tokens_returned",
                    "tokens_avoided",
                    "recorded_at",
                    "schema_version",
                ],
                &[
                    Value::text(*call_id),
                    Value::text(*channel),
                    Value::text("rift_known_state"),
                    Value::text("memd.internal_error"),
                    Value::Int(0),
                    Value::Int(0),
                    Value::Int(*recorded_at),
                    Value::Int(1),
                ],
            ))
            .unwrap();
        }
    }
    #[test]
    fn legacy_serves_remain_savings_only_without_call_backfill() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "legacy");
        let repo = recall_repo_id(Path::new(&proj)).unwrap();
        let db = recall_db_under(root.path(), &repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
        let rt = runtime().unwrap();
        rt.block_on(store.insert(
            "memd_audit_event",
            &[
                "audit_id",
                "verb",
                "handle",
                "reason",
                "tokens_returned",
                "tokens_avoided",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text("legacy"),
                Value::text("memd.summary_served"),
                Value::text("summary:neutral"),
                Value::Null,
                Value::Int(10),
                Value::Int(90),
                Value::Int(1_000),
                Value::Int(1),
            ],
        ))
        .unwrap();
        let FeedPayload::Recall { savings, calls, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        assert!(matches!(savings, RecallView::Data(_)));
        assert_eq!(calls, RecallView::Quiet);
    }

    #[test]
    fn call_series_are_event_anchored_limited_and_stable_while_idle() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "calls");
        seed_call_events(
            root.path(),
            &proj,
            &[
                ("old", "hook_inject", 59_999),   // outside visible window
                ("a0", "hook_inject", 60_000),    // first visible bucket
                ("a1", "hook_inject", 1_800_000), // last visible bucket
                ("b", "skill_cli", 1_799_000),
            ],
        );
        let first = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime);
        let second = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime);
        assert_eq!(first, second, "idle recompute must be byte-identical");
        let FeedPayload::Recall { savings, calls, .. } = first.payload else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Quiet, "internal errors save nothing");
        let RecallView::Data(calls) = calls else {
            panic!("decoded internal failures are still attributed calls");
        };
        assert_eq!(calls.total, 4, "all-time total includes the old call");
        assert_eq!(calls.window_end_ms, 1_860_000);
        // The closed channel vocabulary bounds the series set by construction:
        // both channels served, so both are drawn and nothing is hidden.
        assert_eq!(calls.series.len(), 2);
        assert_eq!(calls.series[0].channel, "hook_inject", "freshest first");
        assert_eq!(calls.series[1].channel, "skill_cli");
        assert_eq!(calls.series[0].rate.len(), 30);
        assert_eq!(calls.series[0].rate[0], 1.0);
        assert_eq!(calls.series[0].rate[29], 1.0);
        // The pre-window `old` row shares `hook_inject`, so window exclusion
        // shows up as its absence from the buckets, not as a missing series.
        let hook_total: f64 = calls.series[0].rate.iter().sum();
        assert!(
            (hook_total - 2.0).abs() < f64::EPSILON,
            "only the two in-window hook calls are bucketed, got {hook_total}"
        );
    }
}

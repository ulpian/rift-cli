//! The column-3 feeds (ADR-0009's Tokens/Nic/Recall trio): scoped, read-only
//! computations from a project's own projections into the six section
//! view-models.
//!
//! Every feed emits **full section state** (never a delta): a dropped or
//! duplicated delivery therefore converges on the fresh-read truth by
//! construction — the delivery-robustness property is a property of the
//! payload shape, not of channel heroics. Feeds follow the **active**
//! project (the reducer's `SetActive` is the subscribe/unsubscribe request:
//! selecting a project subscribes it to all three kinds; switching away
//! unsubscribes — recorded as the M4-11 reading of ADR-0009's
//! subscribe/unsubscribe language), and the worker recomputes them on every
//! observed change of that project — a `Rebuilt` epoch bump re-bootstraps by
//! recomputation (user story 12).
//!
//! Scope: every query runs against the project's **own per-repo** databases
//! (resolved through `repo_id_for`), so cross-project spend cannot leak into
//! a panel (user story 17) — the scope property is tested over multi-repo
//! fixtures.

use std::path::Path;

use rift_canopy::col3::{
    BreakdownData, BreakdownView, CallsData, NicReading, NicView, RecallView, SavingsData,
    ThreadsView, TokensData, TokensView,
};
use rift_canopy::{FeedPayload, FeedUpdate, LiveSessions, SessionsRead};
use rift_git::repo_id_for;
use rift_nic_gauge::{NicGauge as _, Reading, ReferenceGauge};
#[cfg(test)]
use rift_store::Value;
use rift_store::{projection_db_under, recall_db_under, CanonicalStore, SqliteStore};

use crate::store::runtime;

/// The bucket-size ladder (ms) for the time-bucketed rate series, coarsest
/// last. Adaptive policy (M4-11 decision, deterministic in the data): the
/// smallest step where the captured span fits in [`TARGET_BUCKETS`] buckets —
/// so a short burst gets second-level detail and a month of history still
/// fits one plot.
const BUCKET_LADDER_MS: [i64; 9] = [
    1_000,      // 1s
    5_000,      // 5s
    15_000,     // 15s
    60_000,     // 1m
    300_000,    // 5m
    900_000,    // 15m
    3_600_000,  // 1h
    21_600_000, // 6h
    86_400_000, // 1d
];

/// How many rate buckets the sections display (≈ one per plot column).
const TARGET_BUCKETS: i64 = 48;

/// One token-usage fact: the model call's start time + the four classes.
struct TokenRow {
    t: i64,
    uncached_input: i64,
    cached_input: i64,
    cache_creation: i64,
    output: i64,
}

/// Compute the Tokens feed for the project at `path`: the token-usage section
/// (bucketed rate + cumulative total) and the input | read | output breakdown
/// ("read" = cache reads; cache writes are the secondary band). Total — any
/// unreadable projection yields the honest placeholders, never an error or a
/// fake zero.
#[must_use]
pub fn compute_tokens(root: &Path, path: &str) -> FeedUpdate {
    let payload = match read_token_rows(root, path) {
        Some(rows) if !rows.is_empty() => FeedPayload::Tokens {
            tokens: TokensView::Data(tokens_data(&rows)),
            breakdown: BreakdownView::Data(breakdown_data(&rows)),
        },
        // No facts in scope (or no readable projection): the placeholder.
        _ => FeedPayload::Tokens {
            tokens: TokensView::NoActivity,
            breakdown: BreakdownView::NoActivity,
        },
    };
    FeedUpdate {
        project: path.to_string(),
        payload,
    }
}

/// Read the project's token facts, scoped to its own projection. `None` when
/// the projection is absent/unreadable.
fn read_token_rows(root: &Path, path: &str) -> Option<Vec<TokenRow>> {
    let repo = repo_id_for(Path::new(path)).ok()?;
    let db = projection_db_under(root, &repo);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open(&db).ok()?;
    let rt = runtime().ok()?;
    let rows = rt
        .block_on(store.query(
            "SELECT mc.started_at, tu.uncached_input, tu.cached_input, \
                    tu.cache_creation, tu.output \
             FROM token_usage tu JOIN model_call mc ON mc.call_id = tu.call_id \
             ORDER BY mc.started_at, tu.call_id",
            &[],
        ))
        .ok()?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(TokenRow {
            t: r.get_i64(0).ok()?,
            uncached_input: r.get_i64(1).ok()?,
            cached_input: r.get_i64(2).ok()?,
            cache_creation: r.get_i64(3).ok()?,
            output: r.get_i64(4).ok()?,
        });
    }
    Some(out)
}

/// A row's total spend across the four classes (saturating; negatives from a
/// corrupt projection clamp to zero — validation, not trust).
fn row_total(r: &TokenRow) -> f64 {
    let clamp = |v: i64| -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let f = v.max(0) as f64;
        f
    };
    clamp(r.uncached_input) + clamp(r.cached_input) + clamp(r.cache_creation) + clamp(r.output)
}

/// The adaptive bucket size for a span: the smallest ladder step where the
/// span fits [`TARGET_BUCKETS`] buckets (the coarsest step otherwise).
fn bucket_ms(span_ms: i64) -> i64 {
    for step in BUCKET_LADDER_MS {
        if span_ms <= step.saturating_mul(TARGET_BUCKETS) {
            return step;
        }
    }
    *BUCKET_LADDER_MS.last().unwrap_or(&86_400_000)
}

/// Bucket rows into the trailing rate series: per-bucket total-token sums over
/// the last [`TARGET_BUCKETS`] buckets ending at the newest fact, zero-filled
/// where a bucket saw nothing (an honest quiet bucket inside an active span —
/// distinct from the placeholder, which means no facts at all). Deterministic
/// in the rows; replaying the same rows yields the same series (bucketing
/// idempotence).
fn rate_series(rows: &[TokenRow]) -> Vec<f64> {
    let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
        return Vec::new();
    };
    let step = bucket_ms(last.t.saturating_sub(first.t));
    let end_bucket = last.t.div_euclid(step);
    let start_bucket = end_bucket - (TARGET_BUCKETS - 1);
    let mut series = vec![0.0f64; usize::try_from(TARGET_BUCKETS).unwrap_or(48)];
    for row in rows {
        let b = row.t.div_euclid(step);
        if b < start_bucket {
            continue; // older than the visible window
        }
        let idx = usize::try_from(b - start_bucket).unwrap_or(0);
        if let Some(slot) = series.get_mut(idx) {
            *slot += row_total(row);
        }
    }
    // Trim the leading zero buckets before the first activity so a young
    // projection is not mostly empty margin.
    let first_active = series.iter().position(|v| *v > 0.0).unwrap_or(0);
    series.split_off(first_active)
}

/// The token-usage section's numbers.
fn tokens_data(rows: &[TokenRow]) -> TokensData {
    TokensData {
        rate: rate_series(rows),
        total: crate::series::saturating_sum(rows.iter().map(row_total)),
    }
}

/// The breakdown section's numbers: input = uncached input, read = cached
/// reads, output = output, cache writes as the secondary band; the read
/// fraction is read / (input + read), `0/0 ≡ 0` (user story 3).
fn breakdown_data(rows: &[TokenRow]) -> BreakdownData {
    let sum = |f: fn(&TokenRow) -> i64| -> f64 {
        crate::series::saturating_sum(rows.iter().map(|r| {
            #[allow(clippy::cast_precision_loss)]
            let v = f(r).max(0) as f64;
            v
        }))
    };
    let input = sum(|r| r.uncached_input);
    let read = sum(|r| r.cached_input);
    let output = sum(|r| r.output);
    let cache_write = sum(|r| r.cache_creation);
    let denominator = input + read;
    let read_fraction = if denominator > 0.0 {
        read / denominator
    } else {
        0.0
    };
    BreakdownData {
        input,
        read,
        output,
        cache_write,
        read_fraction,
    }
}

/// The active-sessions read the W4 feeds share: **one** call to the single
/// query per recomputation — the threads section and the gauge badge both
/// receive this same value, so they can never disagree (user story 6).
#[must_use]
pub fn read_active_sessions(path: &str, now_ms: i64) -> Option<LiveSessions> {
    let sessions = rift_git::discover(Path::new(path)).map_or(SessionsRead::Ok(None), |paths| {
        crate::session::active_sessions(&paths.git_dir, now_ms)
    });
    match sessions {
        SessionsRead::Ok(live) => live,
        // A corrupt cache is surfaced by the STATUS path; the feeds treat it
        // as no live sessions rather than inventing a count.
        SessionsRead::Corrupt => None,
    }
}

/// Compute the Nic feed: the freshest thread's context reading through the
/// [`NicGauge`] trait (ADR-0012 — the formula, bands, and reference table live
/// in rift-nic-gauge), with the thread badge fed by the shared
/// `active_sessions` value. An unknown model yields the explicit indicator,
/// never a defaulted percentage (user story 5).
#[must_use]
pub fn compute_nic(root: &Path, path: &str, live: Option<LiveSessions>) -> FeedUpdate {
    let threads = live.map_or(0, |l| l.active_sessions);
    let nic = match read_last_call(root, path) {
        None => NicView::NoData,
        Some(last) => {
            // Context occupancy after the freshest call: the full prompt
            // (uncached + cache reads + cache writes) plus its output.
            let used = last.used_tokens();
            match ReferenceGauge.reading(&last.model, used) {
                Reading::Known(k) => NicView::Reading(NicReading {
                    pct: k.pct,
                    band: k.band().label(),
                    model: last.model,
                    threads,
                }),
                Reading::UnknownModel { .. } => NicView::UnknownModel {
                    model: last.model,
                    threads,
                },
            }
        }
    };
    FeedUpdate {
        project: path.to_string(),
        payload: FeedPayload::Nic(nic),
    }
}

/// The freshest model call's identity + token classes.
struct LastCall {
    model: String,
    uncached_input: i64,
    cached_input: i64,
    cache_creation: i64,
    output: i64,
}

impl LastCall {
    /// Context tokens in use after this call (clamped, saturating).
    fn used_tokens(&self) -> u64 {
        let clamp = |v: i64| u64::try_from(v.max(0)).unwrap_or(0);
        clamp(self.uncached_input)
            .saturating_add(clamp(self.cached_input))
            .saturating_add(clamp(self.cache_creation))
            .saturating_add(clamp(self.output))
    }
}

/// Read the freshest usage-bearing model call, scoped to the projection.
fn read_last_call(root: &Path, path: &str) -> Option<LastCall> {
    let repo = repo_id_for(Path::new(path)).ok()?;
    let db = projection_db_under(root, &repo);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open(&db).ok()?;
    let rt = runtime().ok()?;
    let rows = rt
        .block_on(store.query(
            "SELECT mc.model, tu.uncached_input, tu.cached_input, \
                    tu.cache_creation, tu.output \
             FROM token_usage tu JOIN model_call mc ON mc.call_id = tu.call_id \
             ORDER BY mc.started_at DESC, tu.call_id DESC LIMIT 1",
            &[],
        ))
        .ok()?;
    let r = rows.first()?;
    Some(LastCall {
        model: r.get_text(0).ok()?,
        uncached_input: r.get_i64(1).ok()?,
        cached_input: r.get_i64(2).ok()?,
        cache_creation: r.get_i64(3).ok()?,
        output: r.get_i64(4).ok()?,
    })
}

/// One served `memd.*` audit fact.
struct AuditRow {
    t: i64,
    tokens_avoided: i64,
}

/// Compute the Recall feed: tokens saved (rate + cumulative) and MCP calls
/// per bucket from the served `memd.*` audit events in the project's own
/// recall.db, plus the threads count from the shared `active_sessions` value
/// (the Recall payload carries it — the M4-15 answer to "which feed kind").
///
/// Disabled-vs-quiet (M4-15 decision, recorded): **recall.db absent** means
/// recall was never enabled here → the enable-prompting placeholder;
/// recall.db present with no served events is enabled-but-quiet → the
/// visibly distinct honest-empty state. Neither is ever a zero line.
#[must_use]
pub fn compute_recall(root: &Path, path: &str, live: Option<LiveSessions>) -> FeedUpdate {
    let threads = live.map_or(ThreadsView::NoSessions, |l| {
        if l.active_sessions > 0 {
            ThreadsView::Count(l.active_sessions)
        } else {
            ThreadsView::NoSessions
        }
    });
    let (savings, calls) = match read_audit_rows(root, path) {
        RecallDb::Absent => (RecallView::Disabled, RecallView::Disabled),
        RecallDb::Quiet => (RecallView::Quiet, RecallView::Quiet),
        RecallDb::Rows(rows) => {
            let saved: Vec<(i64, f64)> = rows
                .iter()
                .map(|r| {
                    #[allow(clippy::cast_precision_loss)]
                    let v = r.tokens_avoided.max(0) as f64;
                    (r.t, v)
                })
                .collect();
            let calls_rows: Vec<(i64, f64)> = rows.iter().map(|r| (r.t, 1.0)).collect();
            (
                RecallView::Data(SavingsData {
                    rate: bucket_values(&saved),
                    total: crate::series::saturating_sum(saved.iter().map(|(_, v)| *v)),
                }),
                RecallView::Data(CallsData {
                    rate: bucket_values(&calls_rows),
                    total: crate::series::saturating_sum(calls_rows.iter().map(|(_, v)| *v)),
                }),
            )
        }
    };
    FeedUpdate {
        project: path.to_string(),
        payload: FeedPayload::Recall {
            savings,
            calls,
            threads,
        },
    }
}

/// The recall.db read outcome.
enum RecallDb {
    /// No recall.db: recall was never enabled for this repo.
    Absent,
    /// recall.db exists but has no served events yet.
    Quiet,
    /// The served audit facts, oldest first.
    Rows(Vec<AuditRow>),
}

/// Read the served `memd.*` audit events from the project's own recall.db.
fn read_audit_rows(root: &Path, path: &str) -> RecallDb {
    let Ok(repo) = repo_id_for(Path::new(path)) else {
        return RecallDb::Absent;
    };
    let db = recall_db_under(root, &repo);
    if !db.exists() {
        return RecallDb::Absent;
    }
    let Ok(store) = SqliteStore::open(&db) else {
        return RecallDb::Quiet; // present but unreadable: enabled, no data now
    };
    let Ok(rt) = runtime() else {
        return RecallDb::Quiet;
    };
    let Ok(rows) = rt.block_on(store.query(
        "SELECT recorded_at, tokens_avoided FROM memd_audit_event \
         ORDER BY recorded_at, audit_id",
        &[],
    )) else {
        return RecallDb::Quiet; // e.g. a pre-v4 recall.db without the table
    };
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let (Ok(t), Ok(tokens_avoided)) = (r.get_i64(0), r.get_i64(1)) else {
            continue;
        };
        out.push(AuditRow { t, tokens_avoided });
    }
    if out.is_empty() {
        return RecallDb::Quiet;
    }
    RecallDb::Rows(out)
}

/// Bucket arbitrary (t, value) facts with the same adaptive ladder the token
/// feed uses.
fn bucket_values(facts: &[(i64, f64)]) -> Vec<f64> {
    let (Some((first, _)), Some((last, _))) = (facts.first(), facts.last()) else {
        return Vec::new();
    };
    let step = bucket_ms(last.saturating_sub(*first));
    let end_bucket = last.div_euclid(step);
    let start_bucket = end_bucket - (TARGET_BUCKETS - 1);
    let mut series = vec![0.0f64; usize::try_from(TARGET_BUCKETS).unwrap_or(48)];
    for (t, v) in facts {
        let b = t.div_euclid(step);
        if b < start_bucket {
            continue;
        }
        let idx = usize::try_from(b - start_bucket).unwrap_or(0);
        if let Some(slot) = series.get_mut(idx) {
            *slot += v.max(0.0);
        }
    }
    let first_active = series.iter().position(|v| *v > 0.0).unwrap_or(0);
    series.split_off(first_active)
}

/// Seed one `(model_call, token_usage)` pair — the fixture helper the feed
/// tests (and the recall/nic feeds' tests) share.
#[cfg(test)]
pub(crate) fn seed_token_fact(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    call_id: &str,
    started_at: i64,
    classes: (i64, i64, i64, i64),
) {
    rt.block_on(store.insert(
        "model_call",
        &[
            "call_id",
            "turn_id",
            "model",
            "started_at",
            "schema_version",
        ],
        &[
            Value::text(call_id),
            Value::text("t1"),
            Value::text("claude-fable-5"),
            Value::Int(started_at),
            Value::Int(1),
        ],
    ))
    .unwrap();
    rt.block_on(store.insert(
        "token_usage",
        &[
            "call_id",
            "uncached_input",
            "cached_input",
            "cache_creation",
            "output",
            "counter_method",
            "schema_version",
        ],
        &[
            Value::text(call_id),
            Value::Int(classes.0),
            Value::Int(classes.1),
            Value::Int(classes.2),
            Value::Int(classes.3),
            Value::text("provider_reported"),
            Value::Int(1),
        ],
    ))
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rift_canopy::col3::{BreakdownView, TokensView};

    fn make_project(root: &Path, name: &str) -> String {
        let proj = root.join(name);
        std::fs::create_dir_all(&proj).unwrap();
        let repo = repo_id_for(&proj).unwrap();
        let db = projection_db_under(root, &repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = SqliteStore::open(&db).unwrap();
        store.create_canonical_schema().unwrap();
        proj.to_string_lossy().into_owned()
    }

    fn open_proj(root: &Path, path: &str) -> SqliteStore {
        let repo = repo_id_for(Path::new(path)).unwrap();
        SqliteStore::open(projection_db_under(root, &repo)).unwrap()
    }

    #[test]
    fn no_facts_yields_the_placeholder_never_zero() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "empty");
        let update = compute_tokens(root.path(), &proj);
        assert_eq!(
            update.payload,
            FeedPayload::Tokens {
                tokens: TokensView::NoActivity,
                breakdown: BreakdownView::NoActivity,
            }
        );
    }

    #[test]
    fn totals_and_breakdown_sum_the_four_classes() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_token_fact(&store, &rt, "c1", 1_000, (100, 400, 50, 200));
        seed_token_fact(&store, &rt, "c2", 2_000, (50, 600, 0, 100));

        let update = compute_tokens(root.path(), &proj);
        let FeedPayload::Tokens { tokens, breakdown } = update.payload else {
            panic!("expected a Tokens payload");
        };
        let TokensView::Data(t) = tokens else {
            panic!("expected token data");
        };
        assert!((t.total - 1_500.0).abs() < f64::EPSILON);
        assert!(!t.rate.is_empty());
        let BreakdownView::Data(b) = breakdown else {
            panic!("expected breakdown data");
        };
        assert!((b.input - 150.0).abs() < f64::EPSILON);
        assert!((b.read - 1_000.0).abs() < f64::EPSILON);
        assert!((b.output - 300.0).abs() < f64::EPSILON);
        assert!((b.cache_write - 50.0).abs() < f64::EPSILON);
        // read/(input+read) = 1000/1150
        assert!((b.read_fraction - 1_000.0 / 1_150.0).abs() < 1e-9);
    }

    #[test]
    fn read_fraction_zero_over_zero_is_zero() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        // Output-only usage: input + read == 0.
        seed_token_fact(&store, &rt, "c1", 1_000, (0, 0, 0, 500));
        let update = compute_tokens(root.path(), &proj);
        let FeedPayload::Tokens { breakdown, .. } = update.payload else {
            panic!("expected a Tokens payload");
        };
        let BreakdownView::Data(b) = breakdown else {
            panic!("expected breakdown data");
        };
        assert!((b.read_fraction - 0.0).abs() < f64::EPSILON, "0/0 ≡ 0");
    }

    #[test]
    fn bucketing_is_deterministic_and_idempotent_under_recompute() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        for i in 0..20i64 {
            seed_token_fact(&store, &rt, &format!("c{i}"), i * 7_000, (10, 20, 0, 5));
        }
        let a = compute_tokens(root.path(), &proj);
        let b = compute_tokens(root.path(), &proj);
        assert_eq!(a, b, "recomputing over the same rows is identical");
    }

    #[test]
    fn adaptive_bucket_ladder_scales_with_span() {
        assert_eq!(bucket_ms(0), 1_000, "tiny spans take the finest step");
        assert_eq!(bucket_ms(48_000), 1_000);
        assert_eq!(bucket_ms(49_000), 5_000, "one over → the next step");
        assert_eq!(bucket_ms(3_600_000), 300_000);
        assert!(
            bucket_ms(i64::MAX) == 86_400_000,
            "absurd spans clamp to the coarsest step"
        );
    }

    #[test]
    fn corrupt_negative_counts_clamp_to_zero() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_token_fact(&store, &rt, "c1", 1_000, (-50, 100, -3, 20));
        let update = compute_tokens(root.path(), &proj);
        let FeedPayload::Tokens { tokens, breakdown } = update.payload else {
            panic!("expected a Tokens payload");
        };
        let TokensView::Data(t) = tokens else {
            panic!("expected data");
        };
        assert!(
            (t.total - 120.0).abs() < f64::EPSILON,
            "negatives contribute 0"
        );
        let BreakdownView::Data(b) = breakdown else {
            panic!("expected data");
        };
        assert!((b.input - 0.0).abs() < f64::EPSILON);
    }

    // --- W4: the nic + recall feeds -----------------------------------------

    fn live(n: u16) -> Option<LiveSessions> {
        Some(LiveSessions {
            active_sessions: n,
            since_ms: 1,
        })
    }

    fn seed_recall_db(root: &Path, path: &str, events: &[(i64, i64)]) {
        let repo = repo_id_for(Path::new(path)).unwrap();
        let db = recall_db_under(root, &repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
        let rt = runtime().unwrap();
        for (i, (t, avoided)) in events.iter().enumerate() {
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
                    Value::text(format!("a{i}")),
                    Value::text("memd.summary_served"),
                    Value::text("h"),
                    Value::Null,
                    Value::Int(10),
                    Value::Int(*avoided),
                    Value::Int(*t),
                    Value::Int(1),
                ],
            ))
            .unwrap();
        }
    }

    #[test]
    fn nic_reads_the_freshest_call_through_the_gauge_crate() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        // Older small call, newer big call: the reading is the newest.
        seed_token_fact(&store, &rt, "c1", 1_000, (1_000, 0, 0, 100));
        seed_token_fact(&store, &rt, "c2", 9_000, (20_000, 100_000, 0, 4_000));

        let update = compute_nic(root.path(), &proj, live(2));
        let FeedPayload::Nic(NicView::Reading(r)) = update.payload else {
            panic!("expected a Reading");
        };
        // used = 124_000 over claude's 200k window → 62%.
        assert_eq!(r.pct, 62);
        assert_eq!(r.band, "Capable");
        assert_eq!(r.threads, 2, "the badge carries the shared query value");
        assert_eq!(r.model, "claude-fable-5");
    }

    #[test]
    fn nic_unknown_model_is_the_explicit_indicator_never_a_percentage() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        rt.block_on(store.insert(
            "model_call",
            &[
                "call_id",
                "turn_id",
                "model",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text("c1"),
                Value::text("t1"),
                Value::text("mystery-model-9000"),
                Value::Int(1_000),
                Value::Int(1),
            ],
        ))
        .unwrap();
        rt.block_on(store.insert(
            "token_usage",
            &[
                "call_id",
                "uncached_input",
                "cached_input",
                "cache_creation",
                "output",
                "counter_method",
                "schema_version",
            ],
            &[
                Value::text("c1"),
                Value::Int(100),
                Value::Int(0),
                Value::Int(0),
                Value::Int(10),
                Value::text("provider_reported"),
                Value::Int(1),
            ],
        ))
        .unwrap();

        let update = compute_nic(root.path(), &proj, live(1));
        match update.payload {
            FeedPayload::Nic(NicView::UnknownModel { model, threads }) => {
                assert_eq!(model, "mystery-model-9000");
                assert_eq!(threads, 1);
            }
            other => panic!("an unknown model must never read a percentage: {other:?}"),
        }
    }

    #[test]
    fn nic_no_calls_is_nodata() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let update = compute_nic(root.path(), &proj, None);
        assert!(matches!(update.payload, FeedPayload::Nic(NicView::NoData)));
    }

    #[test]
    fn recall_absent_db_is_disabled_present_empty_is_quiet() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        // No recall.db → Disabled (the enable prompt), never a zero line.
        let update = compute_recall(root.path(), &proj, None);
        let FeedPayload::Recall {
            savings,
            calls,
            threads,
        } = update.payload
        else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Disabled);
        assert_eq!(calls, RecallView::Disabled);
        assert_eq!(threads, ThreadsView::NoSessions);

        // recall.db exists but has no served events → Quiet (visibly distinct).
        seed_recall_db(root.path(), &proj, &[]);
        let update = compute_recall(root.path(), &proj, None);
        let FeedPayload::Recall { savings, calls, .. } = update.payload else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Quiet);
        assert_eq!(calls, RecallView::Quiet);
    }

    #[test]
    fn recall_sums_saved_tokens_and_counts_calls() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        seed_recall_db(
            root.path(),
            &proj,
            &[(1_000, 500), (2_000, 1_500), (3_000, 0)],
        );
        let update = compute_recall(root.path(), &proj, live(3));
        let FeedPayload::Recall {
            savings,
            calls,
            threads,
        } = update.payload
        else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        assert!((sd.total - 2_000.0).abs() < f64::EPSILON);
        let RecallView::Data(cd) = calls else {
            panic!("expected calls data");
        };
        assert!(
            (cd.total - 3.0).abs() < f64::EPSILON,
            "one call per served event"
        );
        assert_eq!(threads, ThreadsView::Count(3));
    }

    #[test]
    fn one_query_invariant_badge_equals_threads_by_construction() {
        // The bus reads active_sessions ONCE and hands the same value to both
        // feeds: the gauge badge and the threads section cannot disagree.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_token_fact(&store, &rt, "c1", 1_000, (10, 0, 0, 5));
        seed_recall_db(root.path(), &proj, &[(1_000, 100)]);

        let shared = live(4);
        let nic = compute_nic(root.path(), &proj, shared);
        let recall = compute_recall(root.path(), &proj, shared);
        let FeedPayload::Nic(NicView::Reading(r)) = nic.payload else {
            panic!("expected a Reading");
        };
        let FeedPayload::Recall { threads, .. } = recall.payload else {
            panic!("expected Recall");
        };
        assert_eq!(ThreadsView::Count(r.threads), threads);
    }

    #[test]
    fn recall_is_scope_filtered_across_projects() {
        let root = tempfile::tempdir().unwrap();
        let pa = make_project(root.path(), "a");
        let pb = make_project(root.path(), "b");
        seed_recall_db(root.path(), &pa, &[(1_000, 700)]);
        // b has NO recall.db: a's events must not leak into b's sections.
        let ub = compute_recall(root.path(), &pb, None);
        let FeedPayload::Recall { savings, .. } = ub.payload else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Disabled);
        let ua = compute_recall(root.path(), &pa, None);
        let FeedPayload::Recall {
            savings: RecallView::Data(sd),
            ..
        } = ua.payload
        else {
            panic!("expected a's data");
        };
        assert!((sd.total - 700.0).abs() < f64::EPSILON);
    }

    proptest! {
        // Real stores per case: keep the case count farm-friendly.
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(8))]

        /// Scope: two projects' feeds never mix, for arbitrary fixtures
        /// (user story 17).
        #[test]
        fn feeds_are_scope_filtered_across_projects(
            a_facts in proptest::collection::vec((0i64..100_000, 0i64..5_000), 1..12),
            b_facts in proptest::collection::vec((0i64..100_000, 0i64..5_000), 0..12),
        ) {
            let root = tempfile::tempdir().unwrap();
            let pa = make_project(root.path(), "a");
            let pb = make_project(root.path(), "b");
            let (sa, sb) = (open_proj(root.path(), &pa), open_proj(root.path(), &pb));
            let rt = runtime().unwrap();
            let mut a_total = 0.0f64;
            for (i, (t, v)) in a_facts.iter().enumerate() {
                seed_token_fact(&sa, &rt, &format!("a{i}"), *t, (*v, 0, 0, 0));
                #[allow(clippy::cast_precision_loss)]
                { a_total += *v as f64; }
            }
            for (i, (t, v)) in b_facts.iter().enumerate() {
                seed_token_fact(&sb, &rt, &format!("b{i}"), *t, (0, *v, 0, 0));
            }
            let ua = compute_tokens(root.path(), &pa);
            let FeedPayload::Tokens { tokens, breakdown } = ua.payload else {
                panic!("expected Tokens");
            };
            let TokensView::Data(t) = tokens else { panic!("a has facts") };
            prop_assert!((t.total - a_total).abs() < 1e-6, "a's total is exactly a's rows");
            // a seeded only uncached input: any cached-read leakage came from b.
            let BreakdownView::Data(bd) = breakdown else { panic!("a has facts") };
            prop_assert!(bd.read.abs() < f64::EPSILON, "project b's reads leaked into a");
        }

        /// The rate series is total and non-negative for arbitrary fact sets.
        #[test]
        fn rate_series_is_total(
            facts in proptest::collection::vec((0i64..10_000_000, -100i64..100_000), 0..40),
        ) {
            let mut rows: Vec<TokenRow> = facts
                .into_iter()
                .map(|(t, v)| TokenRow {
                    t,
                    uncached_input: v,
                    cached_input: 0,
                    cache_creation: 0,
                    output: 0,
                })
                .collect();
            rows.sort_by_key(|r| r.t);
            let series = rate_series(&rows);
            prop_assert!(series.len() <= usize::try_from(TARGET_BUCKETS).unwrap());
            for v in series {
                prop_assert!(v.is_finite() && v >= 0.0);
            }
        }
    }
}

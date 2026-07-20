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

use std::collections::BTreeMap;
use std::path::Path;

use rift_canopy::col3::{
    ActiveSessionsView, ActivityData, ActivityView, BreakdownData, BreakdownView, CallsData,
    DayBar, NicReading, NicView, PanelMetrics, RecallView, SavingsData, ThreadCallSeries,
    TokensData, TokensView,
};
use rift_canopy::{FeedPayload, FeedUpdate, LiveSessions, SessionsRead};
use rift_git::repo_id_for;
use rift_nic_gauge::{NicGauge as _, Reading, ReferenceGauge};
use rift_store::{
    projection_db_under, recall_db_under, recall_repo_id, CanonicalStore, SqliteStore, Value,
};

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

/// What the shared per-recompute session read yields: the badge value and all
/// native identities from the same cache pass, so scope and presentation
/// cannot disagree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActiveRead {
    /// The live-session badge value (thread count + running-since).
    pub live: Option<LiveSessions>,
    /// Complete identities retained from every live cache.
    pub identities: Vec<crate::session::LiveSessionIdentity>,
    /// Number of live caches whose identity is incomplete.
    pub unresolved: u16,
}

/// The active-sessions read the W4 feeds share: **one** call to the single
/// query per recomputation — every consumer (badge, context, activity scope)
/// reads this same pass, so they can never disagree (user story 6).
#[must_use]
pub fn read_active_sessions(path: &str, now_ms: i64) -> ActiveRead {
    let Ok(paths) = rift_git::discover(Path::new(path)) else {
        return ActiveRead::default();
    };
    let detail = crate::session::sessions_detail(&paths.git_dir, now_ms);
    match detail.read {
        SessionsRead::Ok(live) => ActiveRead {
            live,
            identities: detail.identities,
            unresolved: detail.unresolved,
        },
        // A corrupt cache is surfaced by the STATUS path; the feeds treat it
        // as no live sessions rather than inventing a count or an anchor.
        SessionsRead::Corrupt => ActiveRead::default(),
    }
}

/// Compute the entire System panel from one live-cache read. Context, active
/// sessions, explicit scope, recent counts, and storage are one payload, so a
/// reducer can never display values from different recomputations.
#[must_use]
pub fn compute_system(root: &Path, path: &str, active: &ActiveRead) -> FeedUpdate {
    let count = active.live.map_or(0, |live| live.active_sessions);
    let sessions = if count > 0 {
        ActiveSessionsView::Count(count)
    } else {
        ActiveSessionsView::NoSessions
    };
    let (context, metrics) = read_system(root, path, active, count);
    FeedUpdate {
        project: path.to_string(),
        payload: FeedPayload::System {
            context,
            metrics,
            active_sessions: sessions,
        },
    }
}

/// One separately-captured main-chain context observation.
struct ContextObservation {
    model: String,
    observed_at: i64,
    uncached_input: i64,
    cached_input: i64,
    cache_creation: i64,
    output: i64,
}

impl ContextObservation {
    /// Context tokens in use at this observation (clamped, saturating).
    fn used_tokens(&self) -> u64 {
        let clamp = |v: i64| u64::try_from(v.max(0)).unwrap_or(0);
        clamp(self.uncached_input)
            .saturating_add(clamp(self.cached_input))
            .saturating_add(clamp(self.cache_creation))
            .saturating_add(clamp(self.output))
    }
}

use rift_canopy::col3::SystemScope;

struct SessionSet {
    canonical: Vec<String>,
    native: Vec<String>,
    scope: SystemScope,
}

fn read_system(
    root: &Path,
    path: &str,
    active: &ActiveRead,
    active_count: u16,
) -> (NicView, PanelMetrics) {
    let Ok(repo) = repo_id_for(Path::new(path)) else {
        return (NicView::NoData, PanelMetrics::default());
    };
    let db = projection_db_under(root, &repo);
    let recall = recall_repo_id(Path::new(path))
        .ok()
        .map(|id| recall_db_under(root, &id));
    let projection_bytes = db.exists().then(|| {
        std::fs::metadata(&db)
            .map_or(0, |meta| meta.len())
            .saturating_add(
                recall
                    .as_ref()
                    .and_then(|path| std::fs::metadata(path).ok())
                    .map_or(0, |meta| meta.len()),
            )
    });
    if !db.exists() {
        return (NicView::NoData, PanelMetrics::default());
    }
    let (Ok(store), Ok(rt)) = (SqliteStore::open(&db), runtime()) else {
        return (
            NicView::NoData,
            PanelMetrics {
                projection_bytes,
                ..PanelMetrics::default()
            },
        );
    };

    let Some(session_set) = resolve_session_set(&store, &rt, active, active_count) else {
        return (
            NicView::NoData,
            PanelMetrics {
                projection_bytes,
                ..PanelMetrics::default()
            },
        );
    };
    let (events_5m, tokens_5m, mcp_calls_5m) =
        recent_activity(&store, &rt, recall.as_deref(), &session_set);

    let current_context = if session_set.scope == SystemScope::CurrentSessions {
        freshest_context(&store, &rt, &session_set.canonical)
            .map(|observation| (observation, SystemScope::CurrentSessions))
    } else {
        freshest_context(&store, &rt, &session_set.canonical)
            .map(|observation| (observation, SystemScope::LastCompletedSession))
    };
    let selected_context = current_context.or_else(|| {
        (session_set.scope == SystemScope::CurrentSessions)
            .then(|| freshest_completed_context(&store, &rt))
            .flatten()
            .map(|observation| (observation, SystemScope::LastCompletedSession))
    });

    let (context, context_used, context_window, context_scope) = match selected_context {
        Some((observation, scope)) => {
            let used = observation.used_tokens();
            let window = ReferenceGauge.window_for_model(&observation.model);
            let view = match ReferenceGauge.reading(&observation.model, used) {
                Reading::Known(known) => NicView::Reading(NicReading {
                    pct: known.pct,
                    band: known.band().label(),
                    model: observation.model,
                    active_sessions: active_count,
                }),
                Reading::UnknownModel { .. } => NicView::UnknownModel {
                    model: observation.model,
                    active_sessions: active_count,
                },
            };
            (view, Some(used), window, scope)
        }
        None => (NicView::NoData, None, None, SystemScope::Unavailable),
    };

    (
        context,
        PanelMetrics {
            events_5m,
            tokens_5m,
            mcp_calls_5m,
            projection_bytes,
            context_used,
            context_window,
            context_scope,
            activity_scope: session_set.scope,
        },
    )
}

fn resolve_session_set(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    active: &ActiveRead,
    active_count: u16,
) -> Option<SessionSet> {
    if active_count > 0 {
        if active.unresolved > 0 || usize::from(active_count) != active.identities.len() {
            return None;
        }
        let mut canonical = Vec::with_capacity(active.identities.len());
        let mut native = Vec::with_capacity(active.identities.len());
        for identity in &active.identities {
            let expected =
                rift_core::capture_session_row_id(&identity.agent, &identity.native_session_id);
            let rows = rt
                .block_on(store.query(
                    "SELECT session_id FROM capture_session_identity \
                     WHERE agent = ? AND native_session_id = ?",
                    &[
                        Value::text(identity.agent.clone()),
                        Value::text(identity.native_session_id.clone()),
                    ],
                ))
                .ok()?;
            let resolved = rows.first()?.get_text(0).ok()?;
            if resolved != expected {
                return None;
            }
            canonical.push(resolved);
            native.push(identity.native_session_id.clone());
        }
        return Some(SessionSet {
            canonical,
            native,
            scope: SystemScope::CurrentSessions,
        });
    }

    let rows = rt
        .block_on(store.query(
            "SELECT se.session_id, csi.native_session_id \
             FROM session_event se JOIN capture_session_identity csi \
               ON csi.session_id = se.session_id \
             WHERE se.kind = 'end' \
             ORDER BY se.occurred_at DESC, se.event_id DESC LIMIT 1",
            &[],
        ))
        .ok()?;
    let row = rows.first()?;
    Some(SessionSet {
        canonical: vec![row.get_text(0).ok()?],
        native: vec![row.get_text(1).ok()?],
        scope: SystemScope::LastCompletedSession,
    })
}

fn freshest_context(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    session_ids: &[String],
) -> Option<ContextObservation> {
    session_ids
        .iter()
        .filter_map(|session_id| {
            let rows = rt
                .block_on(store.query(
                    "SELECT model, observed_at, uncached_input, cached_input, \
                            cache_creation, output \
                     FROM main_context_observation WHERE session_id = ? \
                     ORDER BY observed_at DESC, observation_id DESC LIMIT 1",
                    &[Value::text(session_id.clone())],
                ))
                .ok()?;
            context_from_row(rows.first()?)
        })
        .max_by_key(|observation| observation.observed_at)
}

fn freshest_completed_context(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
) -> Option<ContextObservation> {
    let rows = rt
        .block_on(store.query(
            "SELECT o.model, o.observed_at, o.uncached_input, o.cached_input, \
                    o.cache_creation, o.output \
             FROM main_context_observation o JOIN session_event se \
               ON se.session_id = o.session_id AND se.kind = 'end' \
             ORDER BY se.occurred_at DESC, o.observed_at DESC, o.observation_id DESC LIMIT 1",
            &[],
        ))
        .ok()?;
    context_from_row(rows.first()?)
}

fn context_from_row(row: &rift_store::Row) -> Option<ContextObservation> {
    Some(ContextObservation {
        model: row.get_text(0).ok()?,
        observed_at: row.get_i64(1).ok()?,
        uncached_input: row.get_i64(2).ok()?,
        cached_input: row.get_i64(3).ok()?,
        cache_creation: row.get_i64(4).ok()?,
        output: row.get_i64(5).ok()?,
    })
}

fn recent_activity(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    recall: Option<&Path>,
    sessions: &SessionSet,
) -> (Option<u64>, Option<u64>, Option<u64>) {
    let end = sessions
        .native
        .iter()
        .filter_map(|native| {
            rt.block_on(store.query_opt_i64(
                "SELECT MAX(occurred_at_ms) FROM captured_event WHERE session_id = ?",
                &[Value::text(native.clone())],
            ))
            .ok()
            .flatten()
        })
        .max();
    let Some(end) = end else {
        return (Some(0), Some(0), recall.map(|_| 0));
    };
    let start = end.saturating_sub(5 * 60 * 1000);
    let mut events = 0u64;
    for native in &sessions.native {
        let count = rt
            .block_on(store.query_opt_i64(
                "SELECT COUNT(*) FROM captured_event \
                 WHERE session_id = ? AND occurred_at_ms >= ? AND occurred_at_ms <= ?",
                &[
                    Value::text(native.clone()),
                    Value::Int(start),
                    Value::Int(end),
                ],
            ))
            .ok()
            .flatten()
            .unwrap_or(0);
        events = events.saturating_add(u64::try_from(count.max(0)).unwrap_or(0));
    }
    let mut tokens = 0u64;
    for canonical in &sessions.canonical {
        let count = rt
            .block_on(store.query_opt_i64(
                "SELECT COALESCE(SUM(tu.uncached_input + tu.cached_input + \
                        tu.cache_creation + tu.output), 0) \
                 FROM token_usage tu JOIN model_call mc ON mc.call_id = tu.call_id \
                 JOIN turn t ON t.turn_id = mc.turn_id \
                 WHERE t.session_id = ? AND mc.started_at >= ? AND mc.started_at <= ?",
                &[
                    Value::text(canonical.clone()),
                    Value::Int(start),
                    Value::Int(end),
                ],
            ))
            .ok()
            .flatten()
            .unwrap_or(0);
        tokens = tokens.saturating_add(u64::try_from(count.max(0)).unwrap_or(0));
    }
    let calls = recall.filter(|path| path.exists()).and_then(|path| {
        let recall_store = SqliteStore::open(path).ok()?;
        let count = rt
            .block_on(recall_store.query_opt_i64(
                "SELECT COUNT(*) FROM memd_call_event \
                 WHERE recorded_at >= ? AND recorded_at <= ?",
                &[Value::Int(start), Value::Int(end)],
            ))
            .ok()
            .flatten()
            .unwrap_or(0);
        Some(u64::try_from(count.max(0)).unwrap_or(0))
    });
    (Some(events), Some(tokens), calls)
}

/// One served `memd.*` audit fact.
struct AuditRow {
    t: i64,
    tokens_avoided: i64,
    tokens_returned: i64,
}

/// Compute the Recall feed: tokens saved (rate + cumulative) and process-scoped
/// MCP call threads from the project's own recall.db.
///
/// Disabled-vs-quiet (M4-15 decision, recorded): **recall.db absent** means
/// recall was never enabled here → the enable-prompting placeholder;
/// recall.db present with no served events is enabled-but-quiet → the
/// visibly distinct honest-empty state. Neither is ever a zero line.
#[must_use]
pub fn compute_recall(root: &Path, path: &str) -> FeedUpdate {
    let savings = match read_audit_rows(root, path) {
        RecallDb::Absent => RecallView::Disabled,
        RecallDb::Quiet => RecallView::Quiet,
        RecallDb::Rows(rows) => {
            let saved: Vec<(i64, f64)> = rows
                .iter()
                .map(|r| {
                    #[allow(clippy::cast_precision_loss)]
                    let v = r.tokens_avoided.max(0) as f64;
                    (r.t, v)
                })
                .collect();
            // Savings % over the whole served audit trail: avoided / (avoided +
            // served), through the ledger's guarded ratio so the number can
            // never drift from `avoidable_context_rate` (M55-07).
            let sum_u64 = |f: fn(&AuditRow) -> i64| -> u64 {
                rows.iter()
                    .map(|r| u64::try_from(f(r).max(0)).unwrap_or(0))
                    .fold(0u64, u64::saturating_add)
            };
            let avoided = sum_u64(|r| r.tokens_avoided);
            let returned = sum_u64(|r| r.tokens_returned);
            let pct = rift_recall_ledger::savings_rate(avoided, avoided.saturating_add(returned));
            RecallView::Data(SavingsData {
                rate: bucket_values(&saved),
                total: crate::series::saturating_sum(saved.iter().map(|(_, v)| *v)),
                pct,
            })
        }
    };
    let calls = match read_call_data(root, path) {
        CallDb::Absent => RecallView::Disabled,
        CallDb::Quiet => RecallView::Quiet,
        CallDb::Data(data) => RecallView::Data(data),
    };
    FeedUpdate {
        project: path.to_string(),
        payload: FeedPayload::Recall { savings, calls },
    }
}

// --- W3: the activity feed + the panel-metrics feed (M55-06/07) --------------

/// One calendar day in milliseconds (UTC day boundaries — the fixture-review
/// decision, recorded in the map; deterministic and timezone-dependency-free).
const DAY_MS: i64 = 86_400_000;
/// The trailing window the activity card shows.
const ACTIVITY_DAYS: usize = 7;

/// Short weekday label for an epoch-day index (day 0 = 1970-01-01 = Thursday).
fn weekday_label(epoch_day: i64) -> &'static str {
    const LABELS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    // Monday-indexed: epoch-day 0 (Thu) → 3.
    let idx = (epoch_day.rem_euclid(7) + 3).rem_euclid(7);
    LABELS
        .get(usize::try_from(idx).unwrap_or(0))
        .copied()
        .unwrap_or("?")
}

/// Compute the Activity feed: per-calendar-day counts of events, sessions, and
/// checkpoints over the trailing seven days, from the project's own projection.
/// `now_ms` (the injected clock) defines "today" and the UTC day boundaries.
/// A **missing/unreadable projection** yields the placeholder; a readable one
/// with quiet days yields seven zero-count bars (a quiet week is a fact).
#[must_use]
pub fn compute_activity(root: &Path, path: &str, now_ms: i64) -> FeedUpdate {
    let payload = match read_day_counts(root, path, now_ms) {
        Some(days) => FeedPayload::Activity(ActivityView::Days(ActivityData { days })),
        None => FeedPayload::Activity(ActivityView::NoProjection),
    };
    FeedUpdate {
        project: path.to_string(),
        payload,
    }
}

/// Read the trailing seven days' capture counts, scoped to the projection.
/// `None` when the projection is absent/unreadable (the placeholder case).
fn read_day_counts(root: &Path, path: &str, now_ms: i64) -> Option<Vec<DayBar>> {
    let repo = repo_id_for(Path::new(path)).ok()?;
    let db = projection_db_under(root, &repo);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open(&db).ok()?;
    let rt = runtime().ok()?;

    let today = now_ms.div_euclid(DAY_MS);
    let days_span = i64::try_from(ACTIVITY_DAYS).unwrap_or(7) - 1;
    let start_day = today - days_span;
    let mut bars: Vec<DayBar> = (0..=days_span)
        .map(|d| {
            let day = start_day + d;
            DayBar {
                weekday: weekday_label(day),
                events: 0,
                sessions: 0,
                checkpoints: 0,
            }
        })
        .collect();

    // One grouped query per category; each returns (day, count) over the window.
    let window_start = start_day.saturating_mul(DAY_MS);
    let mut tally = |table: &str, col: &str, pick: &dyn Fn(&mut DayBar) -> &mut u64| {
        let sql = format!(
            "SELECT {col} / {DAY_MS} AS day, COUNT(*) FROM {table} \
             WHERE {col} >= {window_start} GROUP BY day"
        );
        if let Ok(rows) = rt.block_on(store.query(&sql, &[])) {
            for r in rows {
                if let (Ok(day), Ok(n)) = (r.get_i64(0), r.get_i64(1)) {
                    if day >= start_day && day <= today {
                        if let Some(bar) = usize::try_from(day - start_day)
                            .ok()
                            .and_then(|i| bars.get_mut(i))
                        {
                            *pick(bar) = u64::try_from(n).unwrap_or(0);
                        }
                    }
                }
            }
        }
    };
    tally("captured_event", "occurred_at_ms", &|b| &mut b.events);
    tally("session", "started_at", &|b| &mut b.sessions);
    tally("checkpoint", "created_at", &|b| &mut b.checkpoints);
    Some(bars)
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

/// The attributed-call read outcome. It is independent of the legacy savings
/// trail so internal-error-only calls and legacy-savings-only databases are
/// both represented honestly.
enum CallDb {
    Absent,
    Quiet,
    Data(CallsData),
}

const CALL_BUCKET_MS: i64 = 60_000;
const CALL_BUCKETS: usize = 30;
const VISIBLE_CALL_THREADS: usize = 4;

/// Read a bounded, event-anchored per-thread call window. The all-time total is
/// a separate aggregate; only the final 30 minutes of rows enter memory.
fn read_call_data(root: &Path, path: &str) -> CallDb {
    let Ok(repo) = recall_repo_id(Path::new(path)) else {
        return CallDb::Absent;
    };
    let db = recall_db_under(root, &repo);
    if !db.exists() {
        return CallDb::Absent;
    }
    let (Ok(store), Ok(rt)) = (SqliteStore::open(&db), runtime()) else {
        return CallDb::Quiet;
    };
    let Ok(Some(total_i64)) =
        rt.block_on(store.query_opt_i64("SELECT COUNT(*) FROM memd_call_event", &[]))
    else {
        return CallDb::Quiet; // pre-v7 database or temporarily unreadable
    };
    if total_i64 <= 0 {
        return CallDb::Quiet;
    }
    let Ok(Some(latest)) =
        rt.block_on(store.query_opt_i64("SELECT MAX(recorded_at) FROM memd_call_event", &[]))
    else {
        return CallDb::Quiet;
    };
    let latest_minute = latest.div_euclid(CALL_BUCKET_MS) * CALL_BUCKET_MS;
    let window_end_ms = latest_minute.saturating_add(CALL_BUCKET_MS);
    let window_start_ms = window_end_ms
        .saturating_sub(CALL_BUCKET_MS.saturating_mul(i64::try_from(CALL_BUCKETS).unwrap_or(30)));
    let Ok(rows) = rt.block_on(store.query(
        "SELECT thread_id, recorded_at FROM memd_call_event \
         WHERE recorded_at >= ? AND recorded_at < ? \
         ORDER BY recorded_at, call_id",
        &[Value::Int(window_start_ms), Value::Int(window_end_ms)],
    )) else {
        return CallDb::Quiet;
    };

    let mut grouped: BTreeMap<String, (i64, Vec<f64>)> = BTreeMap::new();
    for row in rows {
        let (Ok(thread_id), Ok(recorded_at)) = (row.get_text(0), row.get_i64(1)) else {
            continue;
        };
        let offset = recorded_at.saturating_sub(window_start_ms) / CALL_BUCKET_MS;
        let Ok(bucket) = usize::try_from(offset) else {
            continue;
        };
        let entry = grouped
            .entry(thread_id)
            .or_insert_with(|| (recorded_at, vec![0.0; CALL_BUCKETS]));
        entry.0 = entry.0.max(recorded_at);
        if let Some(value) = entry.1.get_mut(bucket) {
            *value += 1.0;
        }
    }
    let mut series: Vec<ThreadCallSeries> = grouped
        .into_iter()
        .map(|(thread_id, (last_call_at, rate))| ThreadCallSeries {
            thread_id,
            last_call_at,
            rate,
        })
        .collect();
    series.sort_by(|a, b| {
        b.last_call_at
            .cmp(&a.last_call_at)
            .then_with(|| a.thread_id.cmp(&b.thread_id))
    });
    let hidden_threads = series.len().saturating_sub(VISIBLE_CALL_THREADS);
    series.truncate(VISIBLE_CALL_THREADS);
    CallDb::Data(CallsData {
        total: u64::try_from(total_i64).unwrap_or(u64::MAX),
        window_end_ms,
        series,
        hidden_threads,
    })
}

/// Read the served `memd.*` audit events from the project's own recall.db.
fn read_audit_rows(root: &Path, path: &str) -> RecallDb {
    // recall.db is keyed by `recall_repo_id`, NOT the capture `repo_id_for`
    // (which keys projection.db). The serve/audit writer appends under
    // recall_repo_id; the reader must match or it opens a different, empty file.
    let Ok(repo) = recall_repo_id(Path::new(path)) else {
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
        "SELECT recorded_at, tokens_avoided, tokens_returned FROM memd_audit_event \
         ORDER BY recorded_at, audit_id",
        &[],
    )) else {
        return RecallDb::Quiet; // e.g. a pre-v4 recall.db without the table
    };
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let (Ok(t), Ok(tokens_avoided), Ok(tokens_returned)) =
            (r.get_i64(0), r.get_i64(1), r.get_i64(2))
        else {
            continue;
        };
        out.push(AuditRow {
            t,
            tokens_avoided,
            tokens_returned,
        });
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
        // Seed under `recall_repo_id` — the writer's authority the reader now
        // matches (recall.db is NOT keyed by the capture `repo_id_for`).
        let repo = recall_repo_id(Path::new(path)).unwrap();
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
            rt.block_on(store.insert(
                "memd_call_event",
                &[
                    "call_id",
                    "thread_id",
                    "tool_name",
                    "terminal_verb",
                    "tokens_returned",
                    "tokens_avoided",
                    "recorded_at",
                    "schema_version",
                ],
                &[
                    Value::text(format!("c{i}")),
                    Value::text(format!("thread-{}", i % 2)),
                    Value::text("rift_read_summary"),
                    Value::text("memd.summary_served"),
                    Value::Int(10),
                    Value::Int(*avoided),
                    Value::Int(*t),
                    Value::Int(1),
                ],
            ))
            .unwrap();
        }
    }

    fn seed_call_events(root: &Path, path: &str, events: &[(&str, &str, i64)]) {
        let repo = recall_repo_id(Path::new(path)).unwrap();
        let db = recall_db_under(root, &repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
        let rt = runtime().unwrap();
        for (call_id, thread_id, recorded_at) in events {
            rt.block_on(store.insert(
                "memd_call_event",
                &[
                    "call_id",
                    "thread_id",
                    "tool_name",
                    "terminal_verb",
                    "tokens_returned",
                    "tokens_avoided",
                    "recorded_at",
                    "schema_version",
                ],
                &[
                    Value::text(*call_id),
                    Value::text(*thread_id),
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

    fn active(native: &str) -> ActiveRead {
        ActiveRead {
            live: live(1),
            identities: vec![crate::session::LiveSessionIdentity {
                agent: "claude-code".into(),
                native_session_id: native.into(),
                updated_at_ms: 9_000,
            }],
            unresolved: 0,
        }
    }

    fn seed_context_observation(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        native: &str,
        canonical: &str,
        model: &str,
        context: (i64, i64, i64, i64),
    ) {
        rt.block_on(store.insert(
            "main_context_observation",
            &[
                "observation_id",
                "session_id",
                "model",
                "observed_at",
                "uncached_input",
                "cached_input",
                "cache_creation",
                "output",
                "counter_method",
                "schema_version",
            ],
            &[
                Value::text(format!("context-{native}")),
                Value::text(canonical),
                Value::text(model),
                Value::Int(8_500),
                Value::Int(context.0),
                Value::Int(context.1),
                Value::Int(context.2),
                Value::Int(context.3),
                Value::text("provider_reported"),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    fn seed_system_session(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        native: &str,
        model: &str,
        context: (i64, i64, i64, i64),
    ) -> String {
        let canonical = rift_core::capture_session_row_id("claude-code", native);
        rt.block_on(store.insert(
            "session",
            &[
                "session_id",
                "agent",
                "model_primary",
                "repo_id",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(canonical.clone()),
                Value::text("claude-code"),
                Value::text(model),
                Value::text("repo"),
                Value::Int(1_000),
                Value::Int(1),
            ],
        ))
        .unwrap();
        rt.block_on(store.insert(
            "capture_session_identity",
            &["session_id", "agent", "native_session_id", "schema_version"],
            &[
                Value::text(canonical.clone()),
                Value::text("claude-code"),
                Value::text(native),
                Value::Int(1),
            ],
        ))
        .unwrap();
        rt.block_on(store.insert(
            "turn",
            &[
                "turn_id",
                "session_id",
                "ordinal",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(format!("turn-{native}")),
                Value::text(canonical.clone()),
                Value::Int(1),
                Value::Int(8_000),
                Value::Int(1),
            ],
        ))
        .unwrap();
        rt.block_on(store.insert(
            "captured_event",
            &[
                "idempotency_key",
                "session_id",
                "turn_ordinal",
                "event_ordinal",
                "kind",
                "trust",
                "payload_sha256",
                "occurred_at_ms",
                "schema_version",
            ],
            &[
                Value::text(format!("event-{native}")),
                Value::text(native),
                Value::Int(1),
                Value::Int(0),
                Value::text("turn_end"),
                Value::text("transcript_untrusted"),
                Value::text("aa"),
                Value::Int(9_000),
                Value::Int(1),
            ],
        ))
        .unwrap();
        seed_context_observation(store, rt, native, &canonical, model, context);
        canonical
    }

    #[test]
    fn system_uses_main_context_not_huge_aggregate_spend() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "system");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        let canonical = seed_system_session(
            &store,
            &rt,
            "native-1",
            "claude-fable-5",
            (10_000, 30_000, 5_000, 5_000),
        );
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
                Value::text("huge"),
                Value::text("turn-native-1"),
                Value::text("claude-fable-5"),
                Value::Int(8_900),
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
                Value::text("huge"),
                Value::Int(1_000_000),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::text("provider_reported"),
                Value::Int(1),
            ],
        ))
        .unwrap();
        assert_eq!(
            canonical,
            rift_core::capture_session_row_id("claude-code", "native-1")
        );

        let first = compute_system(root.path(), &proj, &active("native-1"));
        assert_eq!(
            first,
            compute_system(root.path(), &proj, &active("native-1")),
            "unchanged canonical data yields a byte-identical System payload"
        );
        let FeedPayload::System {
            context: NicView::Reading(reading),
            metrics,
            active_sessions,
        } = first.payload
        else {
            panic!("expected atomic System reading");
        };
        assert_eq!(reading.pct, 25, "50k / 200k, never the 1M turn aggregate");
        assert_eq!(reading.band, "Clear");
        assert_eq!(metrics.context_used, Some(50_000));
        assert_eq!(metrics.tokens_5m, Some(1_000_000));
        assert_eq!(metrics.events_5m, Some(1));
        assert_eq!(metrics.activity_scope, SystemScope::CurrentSessions);
        assert_eq!(active_sessions, ActiveSessionsView::Count(1));
    }

    #[test]
    fn unresolved_live_identity_never_falls_back() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "unresolved");
        let active = ActiveRead {
            live: live(1),
            identities: Vec::new(),
            unresolved: 1,
        };
        let FeedPayload::System {
            context, metrics, ..
        } = compute_system(root.path(), &proj, &active).payload
        else {
            panic!("expected System");
        };
        assert_eq!(context, NicView::NoData);
        assert_eq!(metrics.activity_scope, SystemScope::Unavailable);
        assert!(metrics.events_5m.is_none());
    }

    #[test]
    fn system_aggregates_all_live_sessions_and_counts_terminal_invocations() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "multi");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_system_session(&store, &rt, "native-a", "claude-fable-5", (10, 0, 0, 0));
        seed_system_session(&store, &rt, "native-b", "claude-fable-5", (20, 0, 0, 0));
        seed_system_session(
            &store,
            &rt,
            "native-inactive",
            "claude-fable-5",
            (99_000, 0, 0, 0),
        );
        seed_event_for(&store, &rt, "old-active-event", "native-a", -400_000);
        let other_proj = make_project(root.path(), "other-project");
        let other_store = open_proj(root.path(), &other_proj);
        seed_system_session(
            &other_store,
            &rt,
            "native-other",
            "claude-fable-5",
            (199_000, 0, 0, 0),
        );
        seed_call_events(
            root.path(),
            &proj,
            &[
                ("in-a", "thread-a", 8_500),
                ("in-b", "thread-b", 9_000),
                ("old", "thread-a", -400_000),
            ],
        );
        let active = ActiveRead {
            live: live(2),
            identities: vec![
                crate::session::LiveSessionIdentity {
                    agent: "claude-code".into(),
                    native_session_id: "native-a".into(),
                    updated_at_ms: 8_000,
                },
                crate::session::LiveSessionIdentity {
                    agent: "claude-code".into(),
                    native_session_id: "native-b".into(),
                    updated_at_ms: 9_000,
                },
            ],
            unresolved: 0,
        };
        let FeedPayload::System {
            metrics,
            active_sessions,
            ..
        } = compute_system(root.path(), &proj, &active).payload
        else {
            panic!("expected System");
        };
        assert_eq!(active_sessions, ActiveSessionsView::Count(2));
        assert_eq!(metrics.events_5m, Some(2));
        assert_eq!(metrics.mcp_calls_5m, Some(2));
        assert_eq!(
            metrics.context_used,
            Some(20),
            "inactive sessions and other projections cannot leak"
        );
    }

    #[test]
    fn system_unknown_model_shows_used_without_percentage() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "unknown");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_system_session(&store, &rt, "native-1", "mystery-model", (100, 20, 0, 5));
        let FeedPayload::System {
            context, metrics, ..
        } = compute_system(root.path(), &proj, &active("native-1")).payload
        else {
            panic!("expected System");
        };
        assert!(matches!(context, NicView::UnknownModel { .. }));
        assert_eq!(metrics.context_used, Some(125));
        assert_eq!(metrics.context_window, None);
    }

    #[test]
    fn system_holds_completed_context_when_current_session_has_no_sample() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "held");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        let old = seed_system_session(
            &store,
            &rt,
            "old-native",
            "claude-fable-5",
            (10_000, 20_000, 0, 5_000),
        );
        rt.block_on(store.insert(
            "session_event",
            &[
                "event_id",
                "session_id",
                "kind",
                "occurred_at",
                "schema_version",
            ],
            &[
                Value::text("old-end"),
                Value::text(old),
                Value::text("end"),
                Value::Int(9_100),
                Value::Int(1),
            ],
        ))
        .unwrap();

        let native = "current-native";
        let current = rift_core::capture_session_row_id("claude-code", native);
        rt.block_on(store.insert(
            "session",
            &[
                "session_id",
                "agent",
                "model_primary",
                "repo_id",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(current.clone()),
                Value::text("claude-code"),
                Value::text("claude-fable-5"),
                Value::text("repo"),
                Value::Int(10_000),
                Value::Int(1),
            ],
        ))
        .unwrap();
        rt.block_on(store.insert(
            "capture_session_identity",
            &["session_id", "agent", "native_session_id", "schema_version"],
            &[
                Value::text(current),
                Value::text("claude-code"),
                Value::text(native),
                Value::Int(1),
            ],
        ))
        .unwrap();
        rt.block_on(store.insert(
            "captured_event",
            &[
                "idempotency_key",
                "session_id",
                "turn_ordinal",
                "event_ordinal",
                "kind",
                "trust",
                "payload_sha256",
                "occurred_at_ms",
                "schema_version",
            ],
            &[
                Value::text("current-event"),
                Value::text(native),
                Value::Int(1),
                Value::Int(0),
                Value::text("tool_call"),
                Value::text("transcript_untrusted"),
                Value::text("bb"),
                Value::Int(10_500),
                Value::Int(1),
            ],
        ))
        .unwrap();
        let FeedPayload::System {
            metrics, context, ..
        } = compute_system(root.path(), &proj, &active(native)).payload
        else {
            panic!("expected System");
        };
        assert!(matches!(context, NicView::Reading(_)));
        assert_eq!(metrics.context_scope, SystemScope::LastCompletedSession);
        assert_eq!(metrics.activity_scope, SystemScope::CurrentSessions);
        assert_eq!(metrics.context_used, Some(35_000));
    }

    #[test]
    fn recall_absent_db_is_disabled_present_empty_is_quiet() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        // No recall.db → Disabled (the enable prompt), never a zero line.
        let update = compute_recall(root.path(), &proj);
        let FeedPayload::Recall { savings, calls } = update.payload else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Disabled);
        assert_eq!(calls, RecallView::Disabled);

        // recall.db exists but has no served events → Quiet (visibly distinct).
        seed_recall_db(root.path(), &proj, &[]);
        let update = compute_recall(root.path(), &proj);
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
        let update = compute_recall(root.path(), &proj);
        let FeedPayload::Recall { savings, calls } = update.payload else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        assert!((sd.total - 2_000.0).abs() < f64::EPSILON);
        let RecallView::Data(cd) = calls else {
            panic!("expected calls data");
        };
        assert_eq!(cd.total, 3, "one attributed call per served event");
        assert_eq!(cd.series.len(), 2);
        assert!(cd.series.iter().all(|s| s.rate.len() == 30));
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
        let FeedPayload::Recall { savings, calls, .. } = compute_recall(root.path(), &proj).payload
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
                ("old", "thread-old", 59_999), // outside visible window
                ("a0", "thread-a", 60_000),    // first visible bucket
                ("a1", "thread-a", 1_800_000), // last visible bucket
                ("b", "thread-b", 1_799_000),
                ("c", "thread-c", 1_798_000),
                ("d", "thread-d", 1_797_000),
                ("e", "thread-e", 1_796_000),
            ],
        );
        let first = compute_recall(root.path(), &proj);
        let second = compute_recall(root.path(), &proj);
        assert_eq!(first, second, "idle recompute must be byte-identical");
        let FeedPayload::Recall { savings, calls, .. } = first.payload else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Quiet, "internal errors save nothing");
        let RecallView::Data(calls) = calls else {
            panic!("decoded internal failures are still attributed calls");
        };
        assert_eq!(calls.total, 7, "all-time total includes the old call");
        assert_eq!(calls.window_end_ms, 1_860_000);
        assert_eq!(calls.series.len(), 4);
        assert_eq!(calls.hidden_threads, 1);
        assert_eq!(calls.series[0].thread_id, "thread-a");
        assert_eq!(calls.series[0].rate.len(), 30);
        assert_eq!(calls.series[0].rate[0], 1.0);
        assert_eq!(calls.series[0].rate[29], 1.0);
        assert!(calls.series.iter().all(|s| s.thread_id != "thread-old"));
    }

    #[test]
    fn recall_reads_the_writers_recall_repo_id_not_the_capture_id() {
        // Regression: the serve/audit writer (`rift hooks memd` / `rift-memd`)
        // keys recall.db by `recall_repo_id`; the panel used the capture
        // `repo_id_for` and opened a different, empty recall.db — stranding it
        // on "no serves yet" even after real serves landed.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");

        // The two id spaces genuinely differ for the same path.
        let cap_id = repo_id_for(Path::new(&proj)).unwrap();
        let recall_id = recall_repo_id(Path::new(&proj)).unwrap();
        assert_ne!(
            cap_id.as_str(),
            recall_id.as_str(),
            "capture and recall repo-ids must differ, else the test proves nothing"
        );

        // A decoy EMPTY recall.db under the CAPTURE id: reading it would yield
        // Quiet. The reader must ignore it.
        let decoy = recall_db_under(root.path(), &cap_id);
        std::fs::create_dir_all(decoy.parent().unwrap()).unwrap();
        rift_store::open_migrated(&decoy, &rift_store::recall_migrations()).unwrap();

        // Real serves land under the recall id.
        seed_recall_db(root.path(), &proj, &[(1_000, 500), (2_000, 700)]);

        let FeedPayload::Recall { savings, calls, .. } = compute_recall(root.path(), &proj).payload
        else {
            panic!("expected Recall");
        };
        // If the reader used the capture id it would find the empty decoy and
        // report Quiet; matching the writer's id, it reports the served data.
        let RecallView::Data(sd) = savings else {
            panic!("must read the writer's recall.db, not the capture-id decoy");
        };
        assert!((sd.total - 1_200.0).abs() < f64::EPSILON);
        assert!(matches!(calls, RecallView::Data(_)));
    }

    #[test]
    fn atomic_system_badge_equals_context_count_by_construction() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_system_session(&store, &rt, "native-1", "claude-fable-5", (10, 0, 0, 5));
        let FeedPayload::System {
            context: NicView::Reading(reading),
            active_sessions,
            ..
        } = compute_system(root.path(), &proj, &active("native-1")).payload
        else {
            panic!("expected System");
        };
        assert_eq!(
            ActiveSessionsView::Count(reading.active_sessions),
            active_sessions
        );
    }

    #[test]
    fn recall_is_scope_filtered_across_projects() {
        let root = tempfile::tempdir().unwrap();
        let pa = make_project(root.path(), "a");
        let pb = make_project(root.path(), "b");
        seed_recall_db(root.path(), &pa, &[(1_000, 700)]);
        // b has NO recall.db: a's events must not leak into b's sections.
        let ub = compute_recall(root.path(), &pb);
        let FeedPayload::Recall { savings, .. } = ub.payload else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Disabled);
        let ua = compute_recall(root.path(), &pa);
        let FeedPayload::Recall {
            savings: RecallView::Data(sd),
            ..
        } = ua.payload
        else {
            panic!("expected a's data");
        };
        assert!((sd.total - 700.0).abs() < f64::EPSILON);
    }

    // --- M55-06: the activity feed ------------------------------------------

    use rift_canopy::col3::ActivityView;

    /// Seed a `session` row started at `t`.
    fn seed_session(store: &SqliteStore, rt: &tokio::runtime::Runtime, id: &str, t: i64) {
        rt.block_on(store.insert(
            "session",
            &[
                "session_id",
                "agent",
                "model_primary",
                "repo_id",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("claude-code"),
                Value::text("m"),
                Value::text("r"),
                Value::Int(t),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// Seed a `checkpoint` created at `t`.
    fn seed_checkpoint(store: &SqliteStore, rt: &tokio::runtime::Runtime, id: &str, t: i64) {
        rt.block_on(store.insert(
            "checkpoint",
            &[
                "checkpoint_id",
                "strategy",
                "created_at",
                "merkle_root",
                "prev_hash",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("manual"),
                Value::Int(t),
                Value::text("root"),
                Value::Null,
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// Seed a `captured_event` occurring at `t`.
    fn seed_event(store: &SqliteStore, rt: &tokio::runtime::Runtime, key: &str, t: i64) {
        seed_event_for(store, rt, key, "s", t);
    }

    /// Seed one captured event for an explicit native session identity.
    fn seed_event_for(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        key: &str,
        session: &str,
        t: i64,
    ) {
        rt.block_on(store.insert(
            "captured_event",
            &[
                "idempotency_key",
                "session_id",
                "turn_ordinal",
                "event_ordinal",
                "kind",
                "trust",
                "payload_sha256",
                "occurred_at_ms",
                "schema_version",
            ],
            &[
                Value::text(key),
                Value::text(session),
                Value::Int(0),
                Value::Int(0),
                Value::text("start"),
                Value::text("trusted"),
                Value::text("sha"),
                Value::Int(t),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// `now` fixed at day 20_000 (2024-10-04), the 8th hour, so "today" is stable.
    const NOW: i64 = 20_000 * DAY_MS + 8 * 3_600_000;

    #[test]
    fn activity_missing_projection_is_the_placeholder_never_seven_zeros() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("plain");
        std::fs::create_dir_all(&proj).unwrap();
        let update = compute_activity(root.path(), &proj.to_string_lossy(), NOW);
        assert_eq!(
            update.payload,
            FeedPayload::Activity(ActivityView::NoProjection),
            "no projection is the placeholder, not a zero week"
        );
    }

    #[test]
    fn activity_empty_projection_is_seven_zero_days() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "quiet");
        let FeedPayload::Activity(ActivityView::Days(d)) =
            compute_activity(root.path(), &proj, NOW).payload
        else {
            panic!("a readable projection yields Days, even when quiet");
        };
        assert_eq!(d.days.len(), 7);
        assert!(d
            .days
            .iter()
            .all(|b| b.events == 0 && b.sessions == 0 && b.checkpoints == 0));
    }

    #[test]
    fn activity_buckets_by_calendar_day_within_the_window() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        // Today (day 20_000): 2 events, 1 session, 1 checkpoint.
        seed_event(&store, &rt, "e1", 20_000 * DAY_MS + 1_000);
        seed_event(&store, &rt, "e2", 20_000 * DAY_MS + 2_000);
        seed_session(&store, &rt, "s-today", 20_000 * DAY_MS + 500);
        seed_checkpoint(&store, &rt, "k-today", 20_000 * DAY_MS + 3_000);
        // Yesterday (day 19_999): 1 event.
        seed_event(&store, &rt, "e3", 19_999 * DAY_MS + 100);
        // Eight days ago (day 19_992): OUTSIDE the 7-day window → dropped.
        seed_event(&store, &rt, "e-old", 19_992 * DAY_MS + 100);

        let FeedPayload::Activity(ActivityView::Days(d)) =
            compute_activity(root.path(), &proj, NOW).payload
        else {
            panic!("expected Days");
        };
        assert_eq!(d.days.len(), 7);
        let today = &d.days[6]; // oldest→newest, today is last
        assert_eq!((today.events, today.sessions, today.checkpoints), (2, 1, 1));
        let yesterday = &d.days[5];
        assert_eq!(yesterday.events, 1);
        // The eight-day-old event is outside the window → total is exactly 3.
        let total_events: u64 = d.days.iter().map(|b| b.events).sum();
        assert_eq!(total_events, 3, "the out-of-window event is dropped");
        // Weekday labels: day 20_000 is a Friday.
        assert_eq!(today.weekday, "Fri");
    }

    #[test]
    fn activity_bucketing_is_stable_around_the_day_boundary() {
        // A nudge within the same day never re-buckets (day-bucketing is total
        // and stable under intra-day clock movement).
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_event(&store, &rt, "e1", 20_000 * DAY_MS + 12 * 3_600_000);
        let a = compute_activity(root.path(), &proj, 20_000 * DAY_MS + 1);
        let b = compute_activity(root.path(), &proj, 20_000 * DAY_MS + DAY_MS - 1);
        assert_eq!(a, b, "any 'now' within the same day yields the same window");
        // Crossing into the next day shifts the window by one.
        let c = compute_activity(root.path(), &proj, 20_001 * DAY_MS + 1);
        assert_ne!(a, c, "a new calendar day shifts the trailing window");
    }

    #[test]
    fn weekday_labels_cover_the_week() {
        // Day 0 (1970-01-01) is a Thursday.
        assert_eq!(weekday_label(0), "Thu");
        assert_eq!(weekday_label(3), "Sun"); // 1970-01-04
        assert_eq!(weekday_label(20_000), "Fri");
        // Negative epoch-days (pre-1970) never panic.
        assert!(!weekday_label(-5).is_empty());
    }

    #[test]
    fn savings_pct_is_the_ledger_ratio_no_drift() {
        // The feed's savings % is provably `savings_rate(Σavoided, Σavoided+Σreturned)`
        // — the same guarded ratio the ledger uses, so no reimplementation drift.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        // seed_recall_db fixes tokens_returned = 10 per event.
        seed_recall_db(root.path(), &proj, &[(1_000, 90), (2_000, 40), (3_000, 20)]);
        let FeedPayload::Recall {
            savings: RecallView::Data(sd),
            ..
        } = compute_recall(root.path(), &proj).payload
        else {
            panic!("expected savings data");
        };
        let avoided = 90 + 40 + 20; // 150
        let returned = 10 * 3; // 30
        let expected = rift_recall_ledger::savings_rate(avoided, avoided + returned);
        assert!(
            (sd.pct - expected).abs() < 1e-12,
            "savings % == the ledger ratio"
        );
        assert!((sd.total - 150.0).abs() < f64::EPSILON);
    }

    #[test]
    fn savings_pct_zero_when_nothing_avoided() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        seed_recall_db(root.path(), &proj, &[(1_000, 0), (2_000, 0)]);
        let FeedPayload::Recall {
            savings: RecallView::Data(sd),
            ..
        } = compute_recall(root.path(), &proj).payload
        else {
            panic!("expected savings data");
        };
        assert!(
            (sd.pct - 0.0).abs() < f64::EPSILON,
            "0 avoided → 0% (never NaN)"
        );
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

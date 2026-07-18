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
    ActivityData, ActivityView, BreakdownData, BreakdownView, CallsData, DayBar, NicReading,
    NicView, PanelMetrics, RecallView, SavingsData, ThreadsView, TokensData, TokensView,
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

/// What the shared per-recompute session read yields: the badge value (the
/// one-query invariant — the threads section and the gauge badge receive
/// this same value, so they can never disagree, user story 6) plus the
/// newest live session's id — the anchor the session-scoped panel metrics
/// hang their event-anchored span on (ADR-0047).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActiveRead {
    /// The live-session badge value (thread count + running-since).
    pub live: Option<LiveSessions>,
    /// The newest live session's id (`None` = nothing live / no anchor).
    pub newest_live: Option<String>,
}

/// The active-sessions read the W4 feeds share: **one** call to the single
/// query per recomputation — every consumer (threads, badge, metrics anchor)
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
            newest_live: detail.newest_live,
        },
        // A corrupt cache is surfaced by the STATUS path; the feeds treat it
        // as no live sessions rather than inventing a count or an anchor.
        SessionsRead::Corrupt => ActiveRead::default(),
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
    tokens_returned: i64,
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
            (
                RecallView::Data(SavingsData {
                    rate: bucket_values(&saved),
                    total: crate::series::saturating_sum(saved.iter().map(|(_, v)| *v)),
                    pct,
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

/// Compute the Panel-metrics feed (M55-07, reshaped by ADR-0047): the system
/// panel's four domain metrics, rated over the **current session's** own
/// event-anchored span. Each is honest about absence — a missing projection
/// or disabled recall yields `None`, never a fake zero. `active_session` is
/// the newest live session's id from the shared session read (the anchor).
/// No wall clock enters the computation: with no new captured data the
/// output is byte-identical, so the values hold exactly while idle and the
/// reducer's dirty gate suppresses any redraw.
#[must_use]
pub fn compute_metrics(root: &Path, path: &str, active_session: Option<&str>) -> FeedUpdate {
    let metrics = read_metrics(root, path, active_session);
    FeedUpdate {
        project: path.to_string(),
        payload: FeedPayload::Metrics(metrics),
    }
}

/// The span the metric rates are computed over (ADR-0047): which session
/// scopes the counts, and the event-anchored `[start, end]` window the
/// denominators derive from. Both ends are **captured** times — never `now` —
/// so a rate moves only when the transcript moves.
struct SessionAnchor {
    /// s0 — the anchoring session's `started_at` (or, in the sessionless
    /// fallback, the earliest captured event).
    start: i64,
    /// e0 — the newest captured event in scope (clamped to ≥ `start`).
    end: i64,
    /// The anchoring session's id (`None` = the whole-projection fallback).
    session: Option<String>,
    /// Whether the anchor is the LIVE session (`false` = held from the most
    /// recent finished session — rendered dimmed).
    live: bool,
}

/// The anchor's span in `unit_ms` units, floored to one whole unit
/// (ADR-0047): a sub-unit span reports the count *as* the rate rather than
/// extrapolating a wild figure from a tiny sample — and the denominator can
/// never be zero.
#[allow(clippy::cast_precision_loss)]
fn span_units(a: &SessionAnchor, unit_ms: f64) -> f64 {
    ((a.end.saturating_sub(a.start)).max(0) as f64 / unit_ms).max(1.0)
}

/// Resolve the session anchor down the fallback ladder: the live session
/// once its `session` row has landed → the newest projection session (its
/// final rates HOLD when nothing is live, rendered dimmed) → the events
/// themselves (a projection captured before sessions existed) → `None` (a
/// readable but empty projection — the rate rows render their placeholder).
fn resolve_anchor(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    active_session: Option<&str>,
) -> Option<SessionAnchor> {
    if let Some(sid) = active_session {
        if let Some(start) = rt
            .block_on(store.query_opt_i64(
                "SELECT started_at FROM session WHERE session_id = ?",
                &[Value::text(sid)],
            ))
            .ok()
            .flatten()
        {
            return Some(anchor_for(store, rt, sid, start, true));
        }
    }
    let newest = rt
        .block_on(store.query(
            "SELECT session_id, started_at FROM session \
             ORDER BY started_at DESC, session_id DESC LIMIT 1",
            &[],
        ))
        .ok()
        .and_then(|rows| {
            let r = rows.first()?;
            Some((r.get_text(0).ok()?, r.get_i64(1).ok()?))
        });
    if let Some((sid, start)) = newest {
        return Some(anchor_for(store, rt, &sid, start, false));
    }
    // No session rows at all: anchor on the events themselves. MIN/MAX over
    // an empty table read as SQL NULL → `None` → no anchor (honest absence).
    let bound = |agg: &str| -> Option<i64> {
        rt.block_on(store.query_opt_i64(
            &format!("SELECT {agg}(occurred_at_ms) FROM captured_event"),
            &[],
        ))
        .ok()
        .flatten()
    };
    let start = bound("MIN")?;
    let end = bound("MAX")?.max(start);
    Some(SessionAnchor {
        start,
        end,
        session: None,
        live: false,
    })
}

/// Build a session's anchor: `[started_at, newest captured event]`. A
/// session with no events yet — or events somehow timed before its start —
/// spans zero (the per-unit floor keeps the rates finite).
fn anchor_for(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    sid: &str,
    start: i64,
    live: bool,
) -> SessionAnchor {
    let end = rt
        .block_on(store.query_opt_i64(
            "SELECT MAX(occurred_at_ms) FROM captured_event WHERE session_id = ?",
            &[Value::text(sid)],
        ))
        .ok()
        .flatten()
        .unwrap_or(start)
        .max(start);
    SessionAnchor {
        start,
        end,
        session: Some(sid.to_string()),
        live,
    }
}

/// The two projection-side rates over the anchor's span: captured events
/// (session-exact where a session anchors) and the four-class token sum over
/// `[start, end]` per minute of span. Tokens are time-window scoped —
/// `model_call` reaches a session only via `turn`, so the uniform
/// `[s0, e0]` window is the robust reading (ADR-0047).
fn session_rates(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    a: &SessionAnchor,
) -> (Option<f64>, Option<f64>) {
    let count = |sql: &str, params: &[Value]| -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let n = rt
            .block_on(store.query_opt_i64(sql, params))
            .ok()
            .flatten()
            .unwrap_or(0) as f64;
        n
    };
    let events = match &a.session {
        Some(sid) => count(
            "SELECT COUNT(*) FROM captured_event WHERE session_id = ?",
            &[Value::text(sid)],
        ),
        None => count("SELECT COUNT(*) FROM captured_event", &[]),
    };
    let tokens = count(
        &format!(
            "SELECT COALESCE(SUM(tu.uncached_input + tu.cached_input + \
                    tu.cache_creation + tu.output), 0) \
             FROM token_usage tu JOIN model_call mc ON mc.call_id = tu.call_id \
             WHERE mc.started_at >= {s0} AND mc.started_at <= {e0}",
            s0 = a.start,
            e0 = a.end,
        ),
        &[],
    );
    let span_min = span_units(a, 60_000.0);
    (Some(events / span_min), Some(tokens / span_min))
}

/// Read the four domain metrics, scoped to the project's own databases and
/// anchored to the active session's event-anchored span (ADR-0047).
fn read_metrics(root: &Path, path: &str, active_session: Option<&str>) -> PanelMetrics {
    let Ok(repo) = repo_id_for(Path::new(path)) else {
        return PanelMetrics::default();
    };
    let db = projection_db_under(root, &repo);
    // recall.db is keyed by `recall_repo_id` (the serve/audit writer's
    // authority — `rift hooks memd` / `rift-memd`), a DIFFERENT digest from the
    // capture `repo_id_for` that keys projection.db above. Resolving recall.db
    // with the capture id opened the wrong, empty file and stranded the panel
    // on "no serves yet" (paths.rs:120 — recall_repo_id is the single authority).
    let recall = match recall_repo_id(Path::new(path)) {
        Ok(rid) => recall_db_under(root, &rid),
        Err(_) => return PanelMetrics::default(),
    };

    // Projection size: the capture + recall projections' on-disk bytes
    // (instantaneous — the one row that never had a window).
    let projection_bytes = if db.exists() {
        let cap = std::fs::metadata(&db).map_or(0, |m| m.len());
        let rec = std::fs::metadata(&recall).map_or(0, |m| m.len());
        Some(cap.saturating_add(rec))
    } else {
        None
    };

    // The anchor + the two projection-side rates.
    let (anchor, capture_per_min, tokens_per_min) = if db.exists() {
        match (SqliteStore::open(&db).ok(), runtime().ok()) {
            (Some(store), Some(rt)) => match resolve_anchor(&store, &rt, active_session) {
                Some(a) => {
                    let (cap, tok) = session_rates(&store, &rt, &a);
                    (Some(a), cap, tok)
                }
                None => (None, None, None),
            },
            _ => (None, None, None),
        }
    } else {
        (None, None, None)
    };

    // Recall load: serves inside the anchored span, per hour of span. With
    // no span at all there is nothing to rate against — honest absence.
    let recall_per_hour = match (&anchor, recall.exists()) {
        (Some(a), true) => match (SqliteStore::open(&recall).ok(), runtime().ok()) {
            (Some(store), Some(rt)) => {
                #[allow(clippy::cast_precision_loss)]
                let serves = rt
                    .block_on(store.query_opt_i64(
                        &format!(
                            "SELECT COUNT(*) FROM memd_audit_event \
                             WHERE recorded_at >= {s0} AND recorded_at <= {e0}",
                            s0 = a.start,
                            e0 = a.end,
                        ),
                        &[],
                    ))
                    .ok()
                    .flatten()
                    .unwrap_or(0) as f64;
                Some(serves / span_units(a, 3_600_000.0))
            }
            _ => None,
        },
        _ => None,
    };

    PanelMetrics {
        capture_per_min,
        tokens_per_min,
        projection_bytes,
        recall_per_hour,
        session_live: anchor.is_some_and(|a| a.live),
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

        let FeedPayload::Recall { savings, calls, .. } =
            compute_recall(root.path(), &proj, None).payload
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

    // --- M55-06: the activity feed ------------------------------------------

    use rift_canopy::col3::{ActivityView, PanelMetrics};

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

    /// Seed one captured event for an explicit session — the session-scoped
    /// metrics tests (ADR-0047) need events in more than one session.
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

    // --- M55-07 / ADR-0047: the session-scoped panel-metrics feed -----------

    #[test]
    fn metrics_missing_projection_is_all_absent() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("plain");
        std::fs::create_dir_all(&proj).unwrap();
        let FeedPayload::Metrics(m) =
            compute_metrics(root.path(), &proj.to_string_lossy(), None).payload
        else {
            panic!("expected Metrics");
        };
        assert_eq!(
            m,
            PanelMetrics::default(),
            "no projection → every metric absent"
        );
        assert!(m.capture_per_min.is_none());
        assert!(m.projection_bytes.is_none());
        assert!(m.recall_per_hour.is_none());
        assert!(!m.session_live);
    }

    #[test]
    fn metrics_rates_are_scoped_to_the_active_sessions_span() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        // The active session "s": started at S0, events through S0+30min.
        const S0: i64 = 1_000_000;
        seed_session(&store, &rt, "s", S0);
        seed_event_for(&store, &rt, "e1", "s", S0 + 10 * 60_000);
        seed_event_for(&store, &rt, "e2", "s", S0 + 20 * 60_000);
        seed_event_for(&store, &rt, "e3", "s", S0 + 30 * 60_000);
        // Another session's event inside the same wall-time span: capture is
        // session-EXACT, so it must not count.
        seed_session(&store, &rt, "other", S0 - 3_600_000);
        seed_event_for(&store, &rt, "ex", "other", S0 + 5 * 60_000);
        // 600 tokens inside [S0, S0+30min] (tokens are span-scoped).
        seed_token_fact(&store, &rt, "c1", S0 + 5 * 60_000, (100, 200, 0, 300));
        // A model call BEFORE the session: outside the span, never counted.
        seed_token_fact(&store, &rt, "c0", S0 - 10 * 60_000, (999, 0, 0, 0));

        let FeedPayload::Metrics(m) = compute_metrics(root.path(), &proj, Some("s")).payload else {
            panic!("expected Metrics");
        };
        // Span = [S0, S0+30min] → 3 session events / 30 min = 0.1/min.
        let cap = m.capture_per_min.expect("readable projection → present");
        assert!(
            (cap - 3.0 / 30.0).abs() < 1e-9,
            "capture rate over the session span: {cap}"
        );
        // 600 span-scoped tokens / 30 min = 20/min (the pre-session 999 excluded).
        let burn = m.tokens_per_min.expect("present");
        assert!(
            (burn - 600.0 / 30.0).abs() < 1e-9,
            "burn rate over the session span: {burn}"
        );
        assert!(m.session_live, "anchored to the LIVE session");
        // A projection exists → its byte size is present and non-zero.
        assert!(m.projection_bytes.is_some_and(|b| b > 0));
        // No recall.db → recall load absent (never a fake zero).
        assert!(m.recall_per_hour.is_none(), "disabled recall → absent");
    }

    #[test]
    fn metrics_hold_when_only_the_clock_advances() {
        // The ADR-0047 key property: no wall clock enters the computation —
        // the same captured data yields byte-identical metrics no matter when
        // they are recomputed, so idle recomputes are no-ops by construction
        // (the reducer's dirty gate then suppresses any redraw).
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_session(&store, &rt, "s", 1_000_000);
        seed_event_for(&store, &rt, "e1", "s", 1_000_000 + 60_000);
        seed_token_fact(&store, &rt, "c1", 1_000_000 + 30_000, (10, 20, 0, 5));

        let a = compute_metrics(root.path(), &proj, Some("s"));
        let b = compute_metrics(root.path(), &proj, Some("s"));
        assert_eq!(a, b, "recomputing over unchanged data is identical");
        // And the value only moves when the transcript moves: a new event
        // advances the span, so the rate changes.
        seed_event_for(&store, &rt, "e2", "s", 1_000_000 + 10 * 60_000);
        let c = compute_metrics(root.path(), &proj, Some("s"));
        assert_ne!(a, c, "a new captured event re-anchors the span");
    }

    #[test]
    fn metrics_zero_span_floors_to_the_unit() {
        // A single event (span 0) reports the count AS the rate — the
        // per-unit floor — never NaN/inf and never a wild extrapolation.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        const S0: i64 = 5_000_000;
        seed_session(&store, &rt, "s", S0);
        seed_event_for(&store, &rt, "e1", "s", S0);
        seed_token_fact(&store, &rt, "c1", S0, (100, 0, 0, 0));
        let FeedPayload::Metrics(m) = compute_metrics(root.path(), &proj, Some("s")).payload else {
            panic!("expected Metrics");
        };
        let cap = m.capture_per_min.expect("present");
        assert!(cap.is_finite());
        assert!((cap - 1.0).abs() < 1e-9, "1 event in a zero span → 1.0/min");
        let burn = m.tokens_per_min.expect("present");
        assert!((burn - 100.0).abs() < 1e-9, "100 tokens, floored to /1min");
    }

    #[test]
    fn metrics_fall_back_to_the_newest_session_when_none_live() {
        // No live session: the most recent projection session anchors — its
        // final rates HOLD (rendered dimmed via `session_live = false`).
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_session(&store, &rt, "old", 1_000_000);
        seed_event_for(&store, &rt, "eo", "old", 1_000_000 + 60_000);
        seed_session(&store, &rt, "new", 9_000_000);
        seed_event_for(&store, &rt, "en1", "new", 9_000_000 + 5 * 60_000);
        seed_event_for(&store, &rt, "en2", "new", 9_000_000 + 10 * 60_000);
        let FeedPayload::Metrics(m) = compute_metrics(root.path(), &proj, None).payload else {
            panic!("expected Metrics");
        };
        // Anchored to "new": 2 events / 10 min.
        let cap = m.capture_per_min.expect("present");
        assert!((cap - 0.2).abs() < 1e-9, "newest session anchors: {cap}");
        assert!(!m.session_live, "a held fallback is not live");
    }

    #[test]
    fn metrics_active_session_wins_over_newer_rows() {
        // The LIVE session anchors even when another session started later —
        // liveness (the cache) beats recency (the table).
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_session(&store, &rt, "mine", 1_000_000);
        seed_event_for(&store, &rt, "e1", "mine", 1_000_000 + 60_000);
        seed_session(&store, &rt, "newer", 9_000_000);
        seed_event_for(&store, &rt, "e2", "newer", 9_000_000 + 60_000);
        let FeedPayload::Metrics(m) = compute_metrics(root.path(), &proj, Some("mine")).payload
        else {
            panic!("expected Metrics");
        };
        assert!((m.capture_per_min.unwrap() - 1.0).abs() < 1e-9);
        assert!(m.session_live);
    }

    #[test]
    fn metrics_empty_projection_rates_absent_bytes_present() {
        // Readable projection, zero sessions AND zero events: nothing to
        // anchor on → the rates are honestly absent; the size is a fact.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "quiet");
        let FeedPayload::Metrics(m) = compute_metrics(root.path(), &proj, None).payload else {
            panic!("expected Metrics");
        };
        assert!(m.capture_per_min.is_none());
        assert!(m.tokens_per_min.is_none());
        assert!(m.recall_per_hour.is_none());
        assert!(m.projection_bytes.is_some_and(|b| b > 0));
        assert!(!m.session_live);
    }

    #[test]
    fn metrics_recall_load_is_span_scoped() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        // Session span = [S0, S0 + 2h].
        const S0: i64 = 10_000_000;
        seed_session(&store, &rt, "s", S0);
        seed_event_for(&store, &rt, "e1", "s", S0 + 2 * 3_600_000);
        // One serve inside the span, one before it (excluded).
        seed_recall_db(root.path(), &proj, &[(S0 - 5_000, 50), (S0 + 1_000, 100)]);
        let FeedPayload::Metrics(m) = compute_metrics(root.path(), &proj, Some("s")).payload else {
            panic!("expected Metrics");
        };
        // 1 in-span serve / 2h of span = 0.5/hr.
        assert_eq!(m.recall_per_hour, Some(0.5));
        assert!(
            m.projection_bytes.is_some_and(|b| b > 0),
            "recall.db adds to size"
        );
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
        } = compute_recall(root.path(), &proj, None).payload
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
        } = compute_recall(root.path(), &proj, None).payload
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

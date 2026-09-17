//! The column-3 feeds (the data-plane split's Tokens/Nic/Recall trio): scoped, read-only
//! computations from a project's own projections into the six section
//! view-models.
//!
//! Every feed emits **full section state** (never a delta): a dropped or
//! duplicated delivery therefore converges on the fresh-read truth by
//! construction — the delivery-robustness property is a property of the
//! payload shape, not of channel heroics. Feeds follow the **active**
//! project (the reducer's `SetActive` is the subscribe/unsubscribe request:
//! selecting a project subscribes it to all three kinds; switching away
//! unsubscribes — the settled reading of the data plane's
//! subscribe/unsubscribe language), and the worker recomputes them on every
//! observed change of that project — a `Rebuilt` epoch bump re-bootstraps by
//! recomputation.
//!
//! Scope: every query runs against the project's **own** databases — its
//! repository family's, resolved through `project_family` (family identity), so
//! two checkouts of one repository read one store and two repositories never
//! read each other's. Cross-project spend cannot leak into a panel; the scope
//! property is tested over multi-repo fixtures.

use std::collections::BTreeMap;
use std::path::Path;

use rift_canopy::col3::{
    ActiveSessionsView, ActivityData, ActivityView, BreakdownData, BreakdownView, CallsData,
    ChannelCallSeries, DayBar, FamilyShareRow, NicReading, NicView, OffStrip, PanelMetrics,
    RecallView, RecallWindow, SavingsData, ShareData, TokensData, TokensView, UsdRow, WindowCut,
    WorktreeAxis, WorktreeSlice,
};
use rift_canopy::{FeedPayload, FeedUpdate, LiveSessions, SessionsRead};
use rift_git::{Family, FamilyState, MintPolicy};
use rift_nic_gauge::{NicGauge as _, Reading, ReferenceGauge};
use rift_shadow_cost::{
    avoided_tokens_micros, fmt_usd_estimate, fmt_usd_exact, partition_largest_remainder,
};
use rift_store::window::CHECKPOINT_TIME_CTE;
use rift_store::{
    effective_prices, projection_db_for_family_under, recall_db_for_family_under, CanonicalStore,
    SqliteStore, Value, CREATED_AT_UNKNOWN,
};

use crate::store::runtime;

/// The repository **family** a registered project belongs to (family identity).
///
/// The dashboard reads a project's stores through its family, so a card opened
/// on a linked worktree shows the repository's checkpoints, sessions and
/// savings instead of the empty pair the member's own path used to key — the
/// measured symptom family identity exists for (0 checkpoints from
/// `~/rift-worktrees/canopy` while the family's refs held 134).
///
/// Minting is allowed here, and the reason is that it changes nothing it does
/// not fix: the mint **adopts** the main work tree's historical ids, so a main
/// path resolves to byte-identically the file the dashboard already read, and a
/// vantage that cannot prove its main writes nothing at all. Refusing to mint
/// instead would blank every card until some other verb happened to run.
///
/// `None` on every failure — not a repository, no provable main, an unreadable
/// manifest. A card is a read, and an unresolvable identity is an absent fact,
/// never a broken panel.
pub(crate) fn project_family(path: &Path) -> Option<Family> {
    match rift_git::family_of(path, MintPolicy::Allow) {
        Ok((_, FamilyState::Minted(family))) => Some(family),
        _ => None,
    }
}

/// The bucket-size ladder (ms) for the time-bucketed rate series, coarsest
/// last. Adaptive policy, deterministic in the data: the
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

/// One token-usage fact: the model call's start time + the four classes, and
/// the worktree its session was last observed in (worktree provenance).
struct TokenRow {
    t: i64,
    uncached_input: i64,
    cached_input: i64,
    cache_creation: i64,
    output: i64,
    /// The member's name, or `None` for **unknown** — no session behind the
    /// call, or a session with no observation. Never `main` by default
    /// (worktree provenance).
    worktree: Option<String>,
}

/// Each session's CURRENT worktree: the observation with the greatest
/// `first_observed_at` (worktree provenance), keyed by session id.
///
/// Ties are broken by name so the map is deterministic — two observations
/// recorded in the same millisecond must not make a golden flap. Absence is
/// the answer for every session with no row, which is every session captured
/// before worktree provenance; sealed shards are never rewritten, so the
/// history that would have named them can never be back-filled.
///
/// Total: an unreadable table is an empty map, never an error. A projection
/// still on a pre-v14 schema has no `session_worktree` at all, and the token
/// panel must keep working there — it simply reads every fact as `unknown`.
fn latest_worktree_by_session(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
) -> BTreeMap<String, String> {
    let Ok(rows) = rt.block_on(store.query(
        "SELECT w.session_id, MIN(w.worktree_name) \
         FROM session_worktree w \
         JOIN (SELECT session_id, MAX(first_observed_at) AS latest \
               FROM session_worktree GROUP BY session_id) l \
           ON l.session_id = w.session_id AND l.latest = w.first_observed_at \
         GROUP BY w.session_id",
        &[],
    )) else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for r in rows {
        if let (Ok(session), Ok(name)) = (r.get_text(0), r.get_text(1)) {
            out.insert(session, name);
        }
    }
    out
}

/// Decompose the token rows by worktree (the family view).
///
/// Computed from the **same** rows [`tokens_data`] totals, so
/// [`WorktreeAxis::total`] is that total by construction and no surface can
/// pair one read's figure with another read's rows. `family_only` stays 0:
/// every token fact reaches a session (or fails to, and is `unknown`) — a
/// fact with no session key at all is the serve trail's case, not this one.
fn worktree_axis(rows: &[TokenRow]) -> WorktreeAxis {
    let mut by_name: BTreeMap<Option<&str>, u64> = BTreeMap::new();
    for row in rows {
        let tokens = u64::try_from(row_total(row).max(0.0) as i64).unwrap_or(0);
        let slot = by_name.entry(row.worktree.as_deref()).or_insert(0);
        *slot = slot.saturating_add(tokens);
    }
    let mut slices: Vec<WorktreeSlice> = by_name
        .into_iter()
        .map(|(name, tokens)| WorktreeSlice {
            name: name.map(ToOwned::to_owned),
            tokens,
        })
        .collect();
    // Largest first, ties by name, `unknown` always last: a stable order, so
    // the disclosure is a golden and not a hash walk.
    slices.sort_by(|a, b| {
        b.tokens
            .cmp(&a.tokens)
            .then_with(|| a.name.is_none().cmp(&b.name.is_none()))
            .then_with(|| a.name.cmp(&b.name))
    });
    WorktreeAxis {
        rows: slices,
        family_only: 0,
    }
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
///
/// The `turn` hop is a LEFT JOIN and so is the worktree lookup that follows
/// it: the row set is **exactly** what the panel's total is summed over, with
/// or without a session behind the call. That is what makes the by-worktree
/// axis sum to the family total rather than merely agree with it.
fn read_token_rows(root: &Path, path: &str) -> Option<Vec<TokenRow>> {
    let db = projection_db_for_family_under(root, &project_family(Path::new(path))?);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open(&db).ok()?;
    let rt = runtime().ok()?;
    let rows = rt
        .block_on(store.query(
            "SELECT mc.started_at, tu.uncached_input, tu.cached_input, \
                    tu.cache_creation, tu.output, tr.session_id \
             FROM token_usage tu JOIN model_call mc ON mc.call_id = tu.call_id \
             LEFT JOIN turn tr ON tr.turn_id = mc.turn_id \
             ORDER BY mc.started_at, tu.call_id",
            &[],
        ))
        .ok()?;
    // Separate query, deliberately: a pre-v14 projection has no
    // `session_worktree` table, and the token panel must not go blank because
    // the axis is unavailable. An unreadable map reads every fact `unknown`.
    let worktrees = latest_worktree_by_session(&store, &rt);
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let session = r.get_opt_text(5).ok()?;
        out.push(TokenRow {
            t: r.get_i64(0).ok()?,
            uncached_input: r.get_i64(1).ok()?,
            cached_input: r.get_i64(2).ok()?,
            cache_creation: r.get_i64(3).ok()?,
            output: r.get_i64(4).ok()?,
            worktree: session.and_then(|id| worktrees.get(&id).cloned()),
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
        by_worktree: worktree_axis(rows),
    }
}

/// The breakdown section's numbers: input = uncached input, read = cached
/// reads, output = output, cache writes as the secondary band; the read
/// fraction is read / (input + read), `0/0 ≡ 0`.
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

/// The active-sessions read the nic and recall feeds share: **one** call to the single
/// query per recomputation — every consumer (badge, context, activity scope)
/// reads this same pass, so they can never disagree.
///
/// The pass is over the whole repository **family**: the
/// badge keeps the truthful System panel's definition — fresh top-level capture sessions — now
/// counted across every member, each exactly once by the worktree discriminator.
/// Reading only the vantage's own gitdir made the panel disagree with the
/// project row the moment an agent ran in a linked worktree, which on a
/// repository developed in worktrees is most of the time.
#[must_use]
pub fn read_active_sessions(path: &str, now_ms: i64) -> ActiveRead {
    let Ok(paths) = rift_git::discover(Path::new(path)) else {
        return ActiveRead::default();
    };
    let roster = crate::session::roster(&paths);
    let detail = crate::session::family_sessions(&roster.members, now_ms).family;
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

/// Compute the whole System panel from one live-cache read. Context, active
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
    let Some(family) = project_family(Path::new(path)) else {
        return (NicView::NoData, PanelMetrics::default());
    };
    let db = projection_db_for_family_under(root, &family);
    let recall = Some(recall_db_for_family_under(root, &family));
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
    let (events_5m, tokens_5m, serve_calls_5m) =
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
            serve_calls_5m,
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
    // `serve_call_event` counts every serve over BOTH
    // surviving channels — hook injection and the skill CLI.
    let serves = recall.filter(|path| path.exists()).and_then(|path| {
        let recall_store = SqliteStore::open(path).ok()?;
        let count = rt
            .block_on(recall_store.query_opt_i64(
                "SELECT COUNT(*) FROM serve_call_event \
                 WHERE recorded_at >= ? AND recorded_at <= ?",
                &[Value::Int(start), Value::Int(end)],
            ))
            .ok()
            .flatten()
            .unwrap_or(0);
        Some(u64::try_from(count.max(0)).unwrap_or(0))
    });
    (Some(events), Some(tokens), serves)
}

/// The Recall card's window, resolved **once** for every figure on it.
///
/// One resolution is the whole point. The card draws a headline, two bars, a
/// dollar estimate, a serve count and a modal, and each of them used to reach
/// for the window in its own way: the audit sum took the button's cut, the
/// share took the family's boundary marker, the money took neither and the
/// serve count took nothing at all. On a repository carrying a marker the card
/// therefore showed one period's tokens beside another period's dollars. This
/// type exists so that cannot be spelled — it is resolved at the top of
/// [`compute_recall`] and handed to every read under it.
enum CardWindow {
    /// The cut every query takes (`None` = the family's whole record), and the
    /// sentence the card prints for it.
    Resolved {
        /// The composed boundary, in epoch milliseconds.
        cut_ms: Option<i64>,
        /// `Boundary::short_label`, when the window narrows the record.
        label: Option<String>,
    },
    /// A **trailing** window this machine's clock could not place.
    ///
    /// The bus reads the wall clock and answers `0` when the platform refuses
    /// it, and `now − 24h` clamped at the epoch is the whole record — so the
    /// card would draw every row it holds under a button labelled `24h`. An
    /// unplaceable window is drawn as the empty window it is, and the modal
    /// says why.
    Unplaceable,
}

impl CardWindow {
    /// The cut to hand a query. An unplaceable window cuts everything out:
    /// absence is the honest answer, never the whole record under a narrow
    /// label.
    fn cut_ms(&self) -> Option<i64> {
        match self {
            Self::Resolved { cut_ms, .. } => *cut_ms,
            Self::Unplaceable => Some(i64::MAX),
        }
    }

    /// The sentence the card and its modal print beside the figures.
    ///
    /// An unplaceable window has no sentence because it has no figures: every
    /// read under it answers empty, and the card draws its empty-window state
    /// rather than a period it cannot name.
    fn label(&self) -> Option<String> {
        match self {
            Self::Resolved { label, .. } => label.clone(),
            Self::Unplaceable => None,
        }
    }
}

/// Resolve the card's window: the button's trailing cut composed with the
/// family's boundary marker, through the same resolver `rift savings --last`
/// calls rather than around it.
///
/// Composition, not replacement, and the composed cut is the **later** of the
/// two. A marker says which rows are outside the evaluation period; a trailing
/// window says which are too old to be interesting. Both are exclusions, and
/// two exclusions compose to the tighter one — so no button can reach back
/// past a boundary somebody set, and `all` still means exactly what the marker
/// has always meant.
fn resolve_card_window(path: &str, now_ms: i64, window: RecallWindow) -> CardWindow {
    let trailing = match window.cut(now_ms) {
        WindowCut::NoClock => return CardWindow::Unplaceable,
        WindowCut::Marker => None,
        WindowCut::Since(ms) => Some(ms),
    };
    let Some(family) = project_family(Path::new(path)) else {
        // No family, no marker to compose with — and no stores to read under
        // one either, so this card is about to resolve to the placeholder.
        return CardWindow::Resolved {
            cut_ms: trailing,
            label: None,
        };
    };
    let boundary = rift_recall_ledger::savings_window::resolve_trailing(
        &family.common_dir,
        trailing,
        window.label(),
    );
    CardWindow::Resolved {
        cut_ms: boundary.cut_ms(),
        label: boundary.is_cut().then(|| boundary.short_label()),
    }
}

/// Compute the Recall feed: the savings share and what it is worth in dollars
/// (the priced savings estimate), plus per-channel serve-call rates, from the
/// project's own recall.db. `now_ms` is the injected clock (the
/// [`compute_activity`] precedent): the *current* prices value the window's
/// avoided total — per-serve historical re-pricing is explicitly out of scope.
///
/// Disabled-vs-quiet, decided deliberately: **recall.db absent** means
/// recall was never enabled here → the enable-prompting placeholder;
/// recall.db present with no served event in the window is enabled-but-quiet
/// → the visibly distinct honest-empty state. Neither is ever a zero line.
///
/// `window` is the viewer's chosen period, and it is resolved **once**
/// ([`CardWindow`]) into the cut every read below takes: the presence probe,
/// the share, the mix the money is priced through, and the serve count. The
/// headline is the share itself — `avoided ÷ (avoided + read)`, the figure
/// `rift savings` publishes — so the card and the command line cannot show two
/// arithmetics, and the two bars beside it are the two sides of that fraction.
#[must_use]
pub fn compute_recall(root: &Path, path: &str, now_ms: i64, window: RecallWindow) -> FeedUpdate {
    let card = resolve_card_window(path, now_ms, window);
    let cut_ms = card.cut_ms();
    let savings = match read_audit_presence(root, path, cut_ms) {
        RecallDb::Absent => RecallView::Disabled,
        RecallDb::Quiet => RecallView::Quiet,
        RecallDb::Live => {
            // The share is read FIRST because it carries BOTH sides of the
            // fraction, and the money values both of them through one mix at
            // one moment — two bars are comparable in dollars only if they
            // were partitioned across the same models at the same instant.
            let share = read_savings_share(root, path, &card);
            let priced = read_usd_savings(
                root,
                path,
                share.avoided_tokens,
                share.read_tokens,
                now_ms,
                cut_ms,
            );
            // Rung 4 (the priced savings estimate): no honest figure → no chip
            // at all; the token headline stands alone.
            let (usd_saved_micros, usd_label, usd_rows) = priced.as_ref().map_or_else(
                || (None, None, Vec::new()),
                |p| {
                    (
                        Some(p.saved_micros),
                        Some(fmt_usd_estimate(p.saved_micros)),
                        p.rows.clone(),
                    )
                },
            );
            let (usd_read_micros, usd_read_label) = priced
                .as_ref()
                .and_then(|p| p.read_micros)
                .map_or((None, None), |micros| {
                    (Some(micros), Some(fmt_usd_estimate(micros)))
                });
            RecallView::Data(SavingsData {
                usd_saved_micros,
                usd_label,
                usd_rows,
                usd_read_micros,
                usd_read_label,
                // the family view: a serve carries no session key, so it
                // cannot be attributed to a member. Not one relation in
                // recall.db holds one — `memd_audit_event`,
                // `serve_call_event` and `serve_session_event` are the whole
                // serve trail, and the last is one row per CHANNEL ever
                // ("refining to per-agent-session granularity is future work
                // ... declared rather than approximated", `schema.rs`). The
                // avoided total is therefore the family's, whole, and the
                // modal says so instead of splitting it on a guess.
                by_worktree: WorktreeAxis {
                    rows: Vec::new(),
                    family_only: share.avoided_tokens,
                },
                // The product's stated goal, from the ledger's one constant: a
                // target the card names, never a measurement it took.
                target_pct: Some(rift_recall_ledger::savings::SAVINGS_TARGET_PCT),
                // The owner's evaluation figure, computed by the ONE shared
                // function the CLI report, the `savings` verb and the recall
                // status table also call — so the card and the terminal can
                // never show two numbers for one repository.
                share: Box::new(share),
            })
        }
    };
    let calls = match read_call_data(root, path, cut_ms) {
        CallDb::Absent => RecallView::Disabled,
        CallDb::Quiet => RecallView::Quiet,
        CallDb::Data(data) => RecallView::Data(data),
    };
    FeedUpdate {
        project: path.to_string(),
        payload: FeedPayload::Recall { savings, calls },
    }
}

/// The savings share for this project's family: `avoided ÷ (avoided + read)`,
/// overall and per family, over the card's one resolved window.
///
/// The window arrives already composed ([`resolve_card_window`]) and this
/// function neither re-resolves it nor spells a sentence of its own: the cut
/// it hands `savings::compose` is the cut the headline, the bars, the money
/// and the serve count were all taken over, and the sentence it publishes is
/// that boundary's own.
///
/// Read-only on both stores, and empty on ANY failure — a share that cannot be
/// measured draws no chip and no modal rows, exactly as an unpriced project
/// draws no dollar chip. Identity is resolved the way every other reader here
/// resolves it: recall.db under the family's recall id, projection.db under
/// its capture id, never one under the other's key.
fn read_savings_share(root: &Path, path: &str, card: &CardWindow) -> ShareData {
    let Some(family) = project_family(Path::new(path)) else {
        return ShareData::default();
    };
    let (recall_db, projection_db) = (
        recall_db_for_family_under(root, &family),
        projection_db_for_family_under(root, &family),
    );
    if !recall_db.exists() || !projection_db.exists() {
        return ShareData::default();
    }
    let (Ok(recall), Ok(projection)) = (
        SqliteStore::open_read_only(&recall_db),
        SqliteStore::open_read_only(&projection_db),
    ) else {
        return ShareData::default();
    };
    let Ok(rt) = runtime() else {
        return ShareData::default();
    };
    let Ok(savings) = rt.block_on(rift_recall_ledger::savings::compose(
        &recall,
        &projection,
        card.cut_ms(),
    )) else {
        return ShareData::default();
    };
    ShareData {
        window: card.label(),
        overall: savings.share(),
        honesty: savings.honesty(),
        avoided_tokens: savings.tokens_avoided,
        read_tokens: savings.read_tokens,
        sessions_with_reads: savings.sessions_with_reads,
        reads_unmeasured: savings.reads_unmeasured,
        by_family: savings.families_share(),
        excluding: savings.share_excluding_undenominated(),
        excluded: savings.excluded().into_iter().map(str::to_owned).collect(),
        families: savings
            .families
            .iter()
            .map(|f| FamilyShareRow {
                family: f.family.clone(),
                share: f.share(),
                honesty: f.honesty(),
                sessions_with_reads: f.sessions_with_reads,
                decided_without_reads: f.decided_without_reads(),
            })
            .collect(),
    }
}

/// The window clause for one capture-projection column, and its parameter —
/// the shape `savings::window` uses on the serving side, spelled once here for
/// the two model-mix queries so the money is cut exactly where the tokens it
/// prices were. `None` is deliberately no clause at all, never `>= 0`.
///
/// The column name is this module's own constant; nothing a viewer types
/// reaches the SQL.
fn mix_window(column: &str, cut_ms: Option<i64>) -> (String, Vec<Value>) {
    cut_ms.map_or_else(
        || (String::new(), Vec::new()),
        |ms| (format!(" WHERE {column} >= ?1"), vec![Value::Int(ms)]),
    )
}

/// One model's share of the observed input-token mix, with its resolved
/// uncached-input rate (micro-USD per MTok; `None` = no seeded price).
struct MixShare {
    model: String,
    weight: u64,
    price_per_mtok: Option<i64>,
}

/// The pure pricing ladder over an already-resolved mix (rungs 1–3):
/// partition `avoided` across the shares (largest remainder — slices sum to
/// exactly `avoided`), price each slice at its model's rate, and sum the
/// priced subtotals into the headline.
///
/// `None` when there is no honest figure: no basis to divide on, or **no
/// share is priced** — a positive saving must never surface as a fabricated
/// `~$0.00`. Partially priced mixes (rung 2) succeed: unpriced rows keep
/// their token slice and contribute no dollars, and the modal discloses them.
fn usd_breakdown(avoided: u64, mix: &[MixShare]) -> Option<(i64, Vec<UsdRow>)> {
    let weights: Vec<u64> = mix.iter().map(|m| m.weight).collect();
    let slices = partition_largest_remainder(avoided, &weights)?;
    let mut rows = Vec::with_capacity(mix.len());
    let mut total: i64 = 0;
    let mut any_priced = false;
    for (share, tokens) in mix.iter().zip(slices) {
        // A rate of exactly zero is **not a price**. A seeded `0` values a
        // real saving at `~$0.00`, which reads as a measured nothing rather
        // than as the absent price it is — the one shape the degradation
        // ladder's rung 4 exists to prevent. Unpriced it is, and the modal
        // says so on its row.
        let rate = share.price_per_mtok.filter(|p| *p > 0);
        let subtotal = avoided_tokens_micros(tokens, rate);
        if let Some(s) = subtotal {
            any_priced = true;
            total = total.saturating_add(s);
        }
        rows.push(UsdRow {
            model: share.model.clone(),
            tokens,
            price_per_mtok_micros: rate,
            subtotal_micros: subtotal,
            rate_label: rate.map(|p| format!("{}/MTok", fmt_usd_exact(p))),
            subtotal_label: subtotal.map(fmt_usd_estimate),
        });
    }
    if !any_priced {
        return None;
    }
    Some((total, rows))
}

/// Resolve the observed model mix and current prices from the **capture**
/// projection and value the avoided total (the priced savings estimate). Read-only, and
/// `None` on *any* failure — a pricing problem degrades to "no chip", never a
/// broken panel.
///
/// Identity is one resolution per projection: this opens projection.db under
/// the family's capture id, while the avoided tokens came from recall.db under
/// the family's recall id — a split confirmed by measurement, now carried by the
/// family record rather than re-derived per path; resolving them the same way
/// is the recorded dashboard defect.
fn read_usd_savings(
    root: &Path,
    path: &str,
    avoided: u64,
    read: u64,
    now_ms: i64,
    cut_ms: Option<i64>,
) -> Option<UsdSavings> {
    let db = projection_db_for_family_under(root, &project_family(Path::new(path))?);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open_read_only(&db).ok()?;
    let rt = runtime().ok()?;
    // The observed mix: each model's share of *input* tokens (uncached +
    // cached) **over the card's window**, because the tokens it partitions are
    // that window's. A mix taken over all history against a window's avoided
    // total would attribute this week's savings to a model the project stopped
    // running in the spring, and the modal would disclose a basis that was
    // never this window's. Deterministic order.
    let (clause, params) = mix_window("m.started_at", cut_ms);
    let mix_rows = rt
        .block_on(store.query(
            &format!(
                "SELECT m.model, SUM(t.uncached_input + t.cached_input) AS w \
                 FROM model_call m JOIN token_usage t ON t.call_id = m.call_id{clause} \
                 GROUP BY m.model HAVING w > 0 \
                 ORDER BY w DESC, m.model ASC"
            ),
            &params,
        ))
        .ok()?;
    let mut mix: Vec<MixShare> = Vec::with_capacity(mix_rows.len());
    for r in mix_rows {
        let (Ok(model), Ok(w)) = (r.get_text(0), r.get_i64(1)) else {
            continue;
        };
        mix.push(MixShare {
            model,
            weight: u64::try_from(w.max(0)).unwrap_or(0),
            price_per_mtok: None,
        });
    }
    if mix.is_empty() {
        // Rung 3: no usable input-token mix — the most-frequent model captured
        // IN THE SAME WINDOW carries the whole total as a single disclosed
        // reference row. (`session.model_primary` is NOT a source: it was
        // measured empty.)
        let (clause, params) = mix_window("started_at", cut_ms);
        let model = rt
            .block_on(store.query(
                &format!(
                    "SELECT model, COUNT(*) AS c FROM model_call{clause} \
                     GROUP BY model ORDER BY c DESC, model ASC LIMIT 1"
                ),
                &params,
            ))
            .ok()?
            .first()
            .and_then(|r| r.get_text(0).ok())?;
        mix.push(MixShare {
            model,
            weight: 1,
            price_per_mtok: None,
        });
    }
    // Current prices (a deliberate choice: today's rate on the
    // cumulative total), through the production bitemporal resolver — never
    // a reimplementation of the price SQL, and never the dead in-memory
    // table. `None` unless all four classes are seeded, which is stricter
    // and honest.
    for share in &mut mix {
        share.price_per_mtok = rt
            .block_on(effective_prices(&store, &share.model, now_ms, now_ms))
            .ok()
            .flatten()
            .map(|p| p.uncached_input);
    }
    let (saved_micros, rows) = usd_breakdown(avoided, &mix)?;
    Some(UsdSavings {
        saved_micros,
        rows,
        // The read side, valued through the SAME mix and the SAME rates. It
        // is a second reading of one basis, not a second price table: the two
        // bars are comparable in dollars only because their tokens were
        // partitioned across the same models at the same moment.
        //
        // Uncached input for both, for the reason the avoided side already
        // uses it: a re-read is fresh input, and a bar priced as output would
        // overstate by about five times against a bar that is not.
        read_micros: usd_breakdown(read, &mix).map(|(micros, _)| micros),
    })
}

/// The priced side of the Recall card: the avoided total, its per-model rows,
/// and the read total valued on the same basis.
struct UsdSavings {
    saved_micros: i64,
    rows: Vec<UsdRow>,
    read_micros: Option<i64>,
}

// --- the activity feed + the panel-metrics feed ----------------------------

/// One calendar day in milliseconds (UTC day boundaries, chosen deliberately:
/// deterministic and free of any timezone dependency).
const DAY_MS: i64 = 86_400_000;
/// The trailing window the activity card shows.
const ACTIVITY_DAYS: usize = 30;

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
/// checkpoints over the trailing thirty days, from the project's own projection.
/// `now_ms` (the injected clock) defines "today" and the UTC day boundaries.
/// A **missing/unreadable projection** yields the placeholder; a readable one
/// with quiet days yields thirty zero-count days (a quiet month is a fact).
/// A readable projection additionally yields the [`OffStrip`] remainder — see
/// `read_day_counts`.
#[must_use]
pub fn compute_activity(root: &Path, path: &str, now_ms: i64) -> FeedUpdate {
    let payload = match read_day_counts(root, path, now_ms) {
        Some((days, off_strip)) => {
            FeedPayload::Activity(ActivityView::Days(ActivityData { days, off_strip }))
        }
        None => FeedPayload::Activity(ActivityView::NoProjection),
    };
    FeedUpdate {
        project: path.to_string(),
        payload,
    }
}

/// Read the trailing thirty days' capture counts **and the rows they do not
/// account for**, scoped to the projection. `None` when the projection is
/// absent/unreadable (the placeholder case).
///
/// # The strips are a filter, so they owe a remainder
///
/// Each strip is one grouped query with a `WHERE <time> >= window_start`
/// predicate, and a predicate drops what it does not match without saying so.
/// `checkpoint.created_at` carries an explicit unknown-time sentinel, which no
/// window predicate can ever match, so on the projection this repository
/// actually has, that query returns **nothing at all** for 103 checkpoints —
/// thirty zeros rendered as if the month had been quiet.
///
/// The remainder is therefore computed the only way that needs no knowledge of
/// how a time can be unreadable: `total rows − rows this strip placed`, per
/// category, from the same store. It is an identity, not a second reading of
/// the time column, so it stays true whatever the time column turns out to
/// hold — and it is why this function does **not** re-derive the sentinel rule
/// that `rift dispatch` owns. What it cannot do is say *why* a row is off the
/// strip; that needs the shared derivation named in the crate docs.
fn read_day_counts(root: &Path, path: &str, now_ms: i64) -> Option<(Vec<DayBar>, OffStrip)> {
    let db = projection_db_for_family_under(root, &project_family(Path::new(path))?);
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
    // Scoped, because `tally` borrows `bars` mutably and the remainder below
    // reads them.
    let window_start = start_day.saturating_mul(DAY_MS);
    {
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
    }

    // **The checkpoint bars do NOT use `tally`, and that is the point.**
    //
    // `checkpoint.created_at` carries `CREATED_AT_UNKNOWN` (-1) whenever a shard
    // records no time of its own — which is EVERY native shard. Bucketing that
    // column directly, as this function did until now, places -1 / 86_400_000
    // in the far past and therefore places nothing at all: on a repository of
    // native checkpoints the strip's checkpoint row was structurally zero and
    // the whole population fell into `OffStrip`, which reads as "older than the
    // window" and was really "undated". It is the same defect that once made
    // `rift dispatch` answer `checkpoints: 0` against 103 real checkpoints.
    //
    // The fix is not a second sentinel rule here. It is the ONE derivation,
    // linked: `rift_store::window::CHECKPOINT_TIME_CTE` resolves each
    // checkpoint to a recorded time, else to `MAX(session.started_at)` over the
    // sessions it archives — the tightest lower bound stored facts support —
    // else to NULL. A NULL is genuinely unplaceable and still becomes
    // `OffStrip`, so the strip's silence now means what it says.
    {
        let sql = format!(
            "{CHECKPOINT_TIME_CTE}\
             SELECT ct.effective_at / {DAY_MS} AS day, COUNT(*) FROM ct \
             WHERE ct.effective_at IS NOT NULL AND ct.effective_at >= {window_start} \
             GROUP BY day"
        );
        if let Ok(rows) = rt.block_on(store.query(&sql, &[Value::Int(CREATED_AT_UNKNOWN)])) {
            for r in rows {
                if let (Ok(day), Ok(n)) = (r.get_i64(0), r.get_i64(1)) {
                    if day >= start_day && day <= today {
                        if let Some(bar) = usize::try_from(day - start_day)
                            .ok()
                            .and_then(|i| bars.get_mut(i))
                        {
                            bar.checkpoints = u64::try_from(n).unwrap_or(0);
                        }
                    }
                }
            }
        }
    }

    // total − placed, per category. `count` is the table's own row count, so a
    // category whose every row landed on a day yields exactly zero and the card
    // is unchanged; saturation means a table that grew between the two reads
    // reports "nothing missing" rather than an underflowed number.
    let total = |table: &str| -> u64 {
        u64::try_from(rt.block_on(store.count(table)).unwrap_or(0)).unwrap_or(0)
    };
    let placed = |pick: &dyn Fn(&DayBar) -> u64| -> u64 {
        bars.iter().map(pick).fold(0u64, u64::saturating_add)
    };
    let off_strip = OffStrip {
        events: total("captured_event").saturating_sub(placed(&|b| b.events)),
        sessions: total("session").saturating_sub(placed(&|b| b.sessions)),
        checkpoints: total("checkpoint").saturating_sub(placed(&|b| b.checkpoints)),
    };
    Some((bars, off_strip))
}

/// The recall.db read outcome — a **presence** answer, not a figure.
///
/// Every number the card draws is composed by `savings::compose` over the same
/// window; what this read decides is which of the three honest states the card
/// is in. It used to pull every audit row into memory to sum them here, which
/// was both a second arithmetic beside the shared one and a full-table read
/// filtered in Rust.
enum RecallDb {
    /// No recall.db: recall was never enabled for this repo.
    Absent,
    /// recall.db exists but holds no served event in this window.
    Quiet,
    /// The window holds at least one served audit fact.
    Live,
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
///
/// The grouping key is `channel`, whose CHECK constraint admits exactly the
/// closed `ServeChannel` pair, so the result set is bounded by the schema —
/// there is no visible-set truncation and therefore no hidden remainder to
/// report.
fn read_call_data(root: &Path, path: &str, cut_ms: Option<i64>) -> CallDb {
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
    // Quiet-vs-data is decided over the WHOLE relation: a repository that has
    // served, on a period that holds nothing, is enabled-and-quiet in this
    // window rather than a pre-v11 store. Both queries are needed because they
    // answer different questions and the second is what the row prints.
    let Ok(Some(ever)) =
        rt.block_on(store.query_opt_i64("SELECT COUNT(*) FROM serve_call_event", &[]))
    else {
        return CallDb::Quiet; // pre-v11 database or temporarily unreadable
    };
    if ever <= 0 {
        return CallDb::Quiet;
    }
    let (clause, params) = mix_window("recorded_at", cut_ms);
    let Ok(Some(total_i64)) = rt.block_on(store.query_opt_i64(
        &format!("SELECT COUNT(*) FROM serve_call_event{clause}"),
        &params,
    )) else {
        return CallDb::Quiet;
    };
    let Ok(Some(latest)) =
        rt.block_on(store.query_opt_i64("SELECT MAX(recorded_at) FROM serve_call_event", &[]))
    else {
        return CallDb::Quiet;
    };
    let latest_minute = latest.div_euclid(CALL_BUCKET_MS) * CALL_BUCKET_MS;
    let window_end_ms = latest_minute.saturating_add(CALL_BUCKET_MS);
    let window_start_ms = window_end_ms
        .saturating_sub(CALL_BUCKET_MS.saturating_mul(i64::try_from(CALL_BUCKETS).unwrap_or(30)));
    // The chart's buckets stay where they are — the axis labels them −30m to
    // last — but it may not draw a call the card's window excludes: a bar
    // standing over a count of `0` would be the two halves of one row
    // describing two periods again.
    let rows_from_ms = cut_ms.map_or(window_start_ms, |cut| window_start_ms.max(cut));
    let Ok(rows) = rt.block_on(store.query(
        "SELECT channel, recorded_at FROM serve_call_event \
         WHERE recorded_at >= ? AND recorded_at < ? \
         ORDER BY recorded_at, call_id",
        &[Value::Int(rows_from_ms), Value::Int(window_end_ms)],
    )) else {
        return CallDb::Quiet;
    };

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
    CallDb::Data(CallsData {
        total: u64::try_from(total_i64).unwrap_or(u64::MAX),
        window_end_ms,
        series,
    })
}

/// Does this project have a recall plane, and did it decide anything inside
/// the card's window?
///
/// The window is pushed into SQL rather than filtered in Rust after selecting
/// every row: a projection with a long serve history made the dashboard read
/// all of it thirty times a minute to answer a question `COUNT(*)` answers in
/// the store, and the rows it materialised were then used for a series nothing
/// drew.
fn read_audit_presence(root: &Path, path: &str, cut_ms: Option<i64>) -> RecallDb {
    // recall.db is keyed by the family's RECALL id, not its capture id (which
    // keys projection.db). The serve/audit writer resolves the same family, so
    // reader and writer cannot land on different files — the defect that
    // stranded this panel on "no serves yet" while real serves were being made.
    let Some(family) = project_family(Path::new(path)) else {
        return RecallDb::Absent;
    };
    let db = recall_db_for_family_under(root, &family);
    if !db.exists() {
        return RecallDb::Absent;
    }
    // Read-only: a measurement never migrates the projection it measures, and
    // a dashboard that bootstrapped a store by looking at it would change the
    // Disabled/Quiet distinction as a side effect of being drawn.
    let Ok(store) = SqliteStore::open_read_only(&db) else {
        return RecallDb::Quiet; // present but unreadable: enabled, no data now
    };
    let Ok(rt) = runtime() else {
        return RecallDb::Quiet;
    };
    // The cut, on the same column and with the same `>=` the shared
    // computation's own clause uses (`savings::audit_totals`), so this probe
    // and the share beside it are answering over one set of rows.
    let (clause, params) = mix_window("recorded_at", cut_ms);
    let count = rt.block_on(store.query_opt_i64(
        &format!("SELECT COUNT(*) FROM memd_audit_event{clause}"),
        &params,
    ));
    let Ok(Some(count)) = count else {
        return RecallDb::Quiet; // e.g. a pre-v4 recall.db without the table
    };
    // A window that excludes every row is **quiet, not absent**: the plane is
    // enabled here and has served, just not lately. Absent means "recall was
    // never enabled", and answering a narrow window with the enable-prompt
    // would name a door the operator has already walked through.
    if count <= 0 {
        return RecallDb::Quiet;
    }
    RecallDb::Live
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
    seed_model_fact(store, rt, call_id, "claude-fable-5", started_at, classes);
}

/// [`seed_token_fact`] with the model made explicit — the USD-savings tests
/// need a real multi-model mix.
#[cfg(test)]
pub(crate) fn seed_model_fact(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    call_id: &str,
    model: &str,
    started_at: i64,
    classes: (i64, i64, i64, i64),
) {
    seed_model_fact_in_turn(store, rt, call_id, "t1", model, started_at, classes);
}

/// [`seed_model_fact`] with the turn made explicit — the by-worktree axis
/// needs calls that reach **different sessions**, and a call reaches its
/// session only through its turn.
#[cfg(test)]
pub(crate) fn seed_model_fact_in_turn(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    call_id: &str,
    turn_id: &str,
    model: &str,
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
            Value::text(turn_id),
            Value::text(model),
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
    // The **legacy** per-path derivations, used here as the seeding oracle: a
    // main work tree's family adopts exactly these ids (family identity), so a
    // fixture seeded under them is the family's store, and the readers above
    // must find it having resolved the family instead.
    use rift_git::repo_id_for;
    use rift_store::{projection_db_under, recall_db_under, recall_repo_id};

    /// The injected clock for recall-feed tests: 2026-07-20T00:00:00Z. Any
    /// value ≥ the test price rows' `recorded_at` works; pinned for
    /// determinism (no test reads the wall clock).
    const USD_NOW: i64 = 1_784_505_600_000;

    /// Seed a model's full four-class price row set (the priced savings estimate: a partial
    /// catalog prices nothing) — cached at 0.1×, write at 1.25×, output at
    /// 5×, the shipped seeds' derivation.
    fn seed_price(store: &SqliteStore, rt: &tokio::runtime::Runtime, model: &str, uncached: i64) {
        for (class, price) in [
            ("uncached_input", uncached),
            ("cached_input", uncached / 10),
            ("cache_creation", uncached + uncached / 4),
            ("output", uncached * 5),
        ] {
            rt.block_on(rift_store::record_price(
                store,
                &rift_store::PriceTableRow {
                    price_id: format!("test-{model}-{class}"),
                    model: model.to_owned(),
                    token_class: class.to_owned(),
                    unit_price_micros: price,
                    currency: "USD".to_owned(),
                    valid_from: 0,
                    valid_to: None,
                    recorded_at: 1,
                    supersedes_id: None,
                    schema_version: 1,
                },
            ))
            .unwrap();
        }
    }

    fn make_project(root: &Path, name: &str) -> String {
        let proj = root.join(name);
        std::fs::create_dir_all(&proj).unwrap();
        // A repository, because every store a feed opens is its family's.
        rift_git::test_support::fake_repo(&proj).unwrap();
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

    // ---- the family view / worktree provenance: the by-worktree axis ------------

    /// Seed a session and one turn belonging to it.
    fn seed_session_turn(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        session: &str,
        turn: &str,
    ) {
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
                Value::text(session),
                Value::text("claude-code"),
                Value::text("claude-fable-5"),
                Value::text("repo"),
                Value::Int(1),
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
                Value::text(turn),
                Value::text(session),
                Value::Int(1),
                Value::Int(1),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// Seed one `session_worktree` OBSERVATION (worktree provenance). The relation is
    /// append-only and keyed by observed state, so a session that moved has
    /// two rows and the greatest `first_observed_at` is its current worktree.
    fn seed_observation(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        observation: &str,
        session: &str,
        name: &str,
        first_observed_at: i64,
    ) {
        rt.block_on(store.insert(
            "session_worktree",
            &[
                "observation_id",
                "session_id",
                "worktree_token",
                "worktree_name",
                "is_linked",
                "work_tree",
                "basis",
                "first_observed_at",
                "schema_version",
            ],
            &[
                Value::text(observation),
                Value::text(session),
                Value::text("0123456789ab"),
                Value::text(name),
                Value::Int(i64::from(name != "main")),
                Value::text(format!("/w/{name}")),
                Value::text("payload_cwd"),
                Value::Int(first_observed_at),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// ★ the family view: the rows are per `worktree_name` (plus `unknown`)
    /// from each session's LATEST observation, and they **sum to the family
    /// total** — the panel's Σ and the axis come out of one pass over one set
    /// of rows, so they cannot disagree.
    ///
    /// The fixture carries every shape the join can meet: a session observed
    /// in the main work tree, a session observed twice (moved into a linked
    /// member — the later observation wins), a session with no observation at
    /// all, and a call whose turn row does not exist. The last two are
    /// `unknown`, never `main` (worktree provenance).
    #[test]
    fn the_breakdown_sums_to_the_family_total() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();

        seed_session_turn(&store, &rt, "s-main", "t-main");
        seed_session_turn(&store, &rt, "s-moved", "t-moved");
        seed_session_turn(&store, &rt, "s-unseen", "t-unseen");
        seed_observation(&store, &rt, "o1", "s-main", "main", 1_000);
        // The moved session: observed in main first, then in the member. The
        // greatest `first_observed_at` is its current worktree.
        seed_observation(&store, &rt, "o2", "s-moved", "main", 1_000);
        seed_observation(&store, &rt, "o3", "s-moved", "swt-d", 2_000);
        // 100 + 400 + 50 + 200 = 750 under `main`
        seed_model_fact_in_turn(&store, &rt, "c1", "t-main", "m", 1_000, (100, 400, 50, 200));
        // 300 under `swt-d`
        seed_model_fact_in_turn(&store, &rt, "c2", "t-moved", "m", 2_000, (100, 100, 0, 100));
        // 60 under `unknown` (a session that was never observed)
        seed_model_fact_in_turn(&store, &rt, "c3", "t-unseen", "m", 3_000, (10, 20, 0, 30));
        // 5 under `unknown` (a call whose turn row is missing altogether —
        // the LEFT JOIN keeps it, which is why the sum holds)
        seed_model_fact_in_turn(&store, &rt, "c4", "t-missing", "m", 4_000, (5, 0, 0, 0));

        let FeedPayload::Tokens { tokens, .. } = compute_tokens(root.path(), &proj).payload else {
            panic!("expected a Tokens payload");
        };
        let TokensView::Data(t) = tokens else {
            panic!("expected token data");
        };
        assert!((t.total - 1_115.0).abs() < f64::EPSILON, "{}", t.total);
        assert_eq!(
            t.by_worktree.total(),
            1_115,
            "the breakdown sums to the family total"
        );
        assert_eq!(
            t.by_worktree.rows,
            vec![
                WorktreeSlice {
                    name: Some("main".to_owned()),
                    tokens: 750,
                },
                WorktreeSlice {
                    name: Some("swt-d".to_owned()),
                    tokens: 300,
                },
                WorktreeSlice {
                    name: None,
                    tokens: 65,
                },
            ],
            "largest first; the moved session counts under its LATEST \
             observation; both unattributable calls land in ONE unknown row"
        );
        assert_eq!(
            t.by_worktree.family_only, 0,
            "every token fact reaches a session or fails to — none is \
             family-only, which is the serve trail's case"
        );
    }

    /// worktree provenance, the reader's half: a projection that records no
    /// observation reads `unknown` — never `main` — and the total still
    /// decomposes. This is also the pre-v14 shape: the table is empty (or, on
    /// a projection that has not migrated, absent), and the panel keeps
    /// working.
    #[test]
    fn a_projection_with_no_observations_reads_unknown_never_main() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_session_turn(&store, &rt, "s1", "t1");
        seed_token_fact(&store, &rt, "c1", 1_000, (10, 0, 0, 0));

        let FeedPayload::Tokens { tokens, .. } = compute_tokens(root.path(), &proj).payload else {
            panic!("expected a Tokens payload");
        };
        let TokensView::Data(t) = tokens else {
            panic!("expected token data");
        };
        assert_eq!(
            t.by_worktree.rows,
            vec![WorktreeSlice {
                name: None,
                tokens: 10,
            }],
            "no observation ⇒ unknown, and unknown is None — never `main`"
        );
        assert_eq!(t.by_worktree.total(), 10);
    }

    /// The map query is defensive on purpose: an unreadable
    /// `session_worktree` (a pre-v14 projection has no such table) must cost
    /// the axis, never the panel. Proven by dropping the table under the
    /// reader.
    #[test]
    fn a_projection_without_the_worktree_table_still_reads_its_tokens() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_token_fact(&store, &rt, "c1", 1_000, (7, 0, 0, 0));
        assert!(
            latest_worktree_by_session(&store, &rt).is_empty(),
            "no observations yet"
        );
        rt.block_on(async {
            let conn = rift_store::Store::conn(&store).await.unwrap();
            conn.execute("DROP TABLE session_worktree", &[]).await
        })
        .unwrap();

        assert!(
            latest_worktree_by_session(&store, &rt).is_empty(),
            "an unreadable relation is an empty map, not an error"
        );
        let FeedPayload::Tokens { tokens, .. } = compute_tokens(root.path(), &proj).payload else {
            panic!("expected a Tokens payload");
        };
        let TokensView::Data(t) = tokens else {
            panic!("the panel keeps its facts when the axis cannot be read");
        };
        assert!((t.total - 7.0).abs() < f64::EPSILON);
        assert_eq!(t.by_worktree.total(), 7, "and still decomposes, as unknown");
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

    // --- the nic + recall feeds ---------------------------------------------

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
            // The pull-side call sibling of that audit row, alternating over
            // the closed serve-channel pair so the card gets both series.
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
                    Value::text(format!("c{i}")),
                    Value::text(if i % 2 == 0 {
                        "hook_inject"
                    } else {
                        "skill_cli"
                    }),
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

    /// Append one **injected**-manifest audit row: the push tier's spend
    /// (hook-and-skill serving), which has its own verb and — unlike every pull-side
    /// serve — no `serve_call_event` sibling, because nobody called anything.
    fn seed_injection_row(root: &Path, path: &str, id: &str, t: i64, returned: i64) {
        let repo = recall_repo_id(Path::new(path)).unwrap();
        let db = recall_db_under(root, &repo);
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
                Value::text(id),
                Value::text("memd.manifest_injected"),
                Value::Null,
                Value::Null,
                Value::Int(returned),
                Value::Int(0),
                Value::Int(t),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

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

    /// Seeds `serve_call_event` — the channel-attributed
    /// relation `recent_activity` counts for `PanelMetrics::serve_calls_5m`.
    /// The channel value is irrelevant to a plain `COUNT(*)` window query, so
    /// every row is tagged `skill_cli`.
    fn seed_serve_call_events(root: &Path, path: &str, events: &[(&str, i64)]) {
        let repo = recall_repo_id(Path::new(path)).unwrap();
        let db = recall_db_under(root, &repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();
        let rt = runtime().unwrap();
        for (call_id, recorded_at) in events {
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
                    Value::text("skill_cli"),
                    Value::text("known-state"),
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
        seed_serve_call_events(
            root.path(),
            &proj,
            &[("in-a", 8_500), ("in-b", 9_000), ("old", -400_000)],
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
        assert_eq!(metrics.serve_calls_5m, Some(2));
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
        let update = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime);
        let FeedPayload::Recall { savings, calls } = update.payload else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Disabled);
        assert_eq!(calls, RecallView::Disabled);

        // recall.db exists but has no served events → Quiet (visibly distinct).
        seed_recall_db(root.path(), &proj, &[]);
        let update = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime);
        let FeedPayload::Recall { savings, calls, .. } = update.payload else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Quiet);
        assert_eq!(calls, RecallView::Quiet);
    }

    /// Seed one captured read of `tokens` tokens by `agent` into the project's
    /// capture projection: the six rows the measurement query joins.
    fn seed_one_read(root: &Path, path: &str, agent: &str, tokens: i64) {
        let store = open_proj(root, path);
        let rt = runtime().unwrap();
        let insert = |table: &str, cols: &[&str], vals: &[Value]| {
            rt.block_on(store.insert(table, cols, vals)).unwrap();
        };
        insert(
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
                Value::text("s1"),
                Value::text(agent.to_owned()),
                Value::text("m"),
                Value::text("r"),
                Value::Int(1),
                Value::Int(1),
            ],
        );
        insert(
            "turn",
            &[
                "turn_id",
                "session_id",
                "ordinal",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text("t1"),
                Value::text("s1"),
                Value::Int(0),
                Value::Int(1),
                Value::Int(1),
            ],
        );
        insert(
            "tool_call",
            &[
                "tool_call_id",
                "turn_id",
                "tool_name",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text("tc1"),
                Value::text("t1"),
                Value::text("Read"),
                Value::Int(2),
                Value::Int(1),
            ],
        );
        insert(
            "file_revision",
            &[
                "file_revision_id",
                "path",
                "blob_oid",
                "content_sha256",
                "schema_version",
            ],
            &[
                Value::text("fr1"),
                Value::text("src/lib.rs"),
                Value::text("b1"),
                Value::text("c".repeat(64)),
                Value::Int(1),
            ],
        );
        insert(
            "tool_reads_file",
            &["tool_call_id", "file_revision_id"],
            &[Value::text("tc1"), Value::text("fr1")],
        );
        insert(
            "file_read_tokens",
            &[
                "tool_call_id",
                "input_tokens",
                "tokenizer",
                "schema_version",
            ],
            &[
                Value::text("tc1"),
                Value::Int(tokens),
                Value::text("cl100k_base"),
                Value::Int(1),
            ],
        );
    }

    /// The card's savings share is the shared computation's own figure, over
    /// the two stores this feed resolves — not a second arithmetic that could
    /// drift from the terminal's.
    #[test]
    fn the_card_publishes_the_shared_savings_share() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        seed_recall_db(root.path(), &proj, &[(1_000, 600), (2_000, 400)]);
        seed_one_read(root.path(), &proj, "claude-code", 9_000);

        let update = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime);
        let FeedPayload::Recall { savings, .. } = update.payload else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        let expected = rift_recall_ledger::savings::share(1_000, 9_000);
        assert_eq!(sd.share.overall, expected);
        assert_eq!(sd.share.read_tokens, 9_000);
        assert_eq!(sd.share.sessions_with_reads, 1);
        assert!(
            sd.share.families.iter().any(|f| f.family == "claude-code"),
            "the family that read has a row: {:?}",
            sd.share.families
        );
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
        seed_one_read(root.path(), &proj, "claude-code", 9_000);
        let update = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime);
        let FeedPayload::Recall { savings, calls } = update.payload else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        assert_eq!(sd.share.avoided_tokens, 2_000);
        assert_eq!(
            sd.by_worktree.family_only, 2_000,
            "the axis decomposes the same total"
        );
        let RecallView::Data(cd) = calls else {
            panic!("expected calls data");
        };
        assert_eq!(cd.total, 3, "one attributed call per served event");
        assert_eq!(cd.series.len(), 2);
        assert!(cd.series.iter().all(|s| s.rate.len() == 30));
    }

    /// Rung 1 (the priced savings estimate): a fully priced mix composes the headline — the
    /// rows' tokens sum to exactly the avoided total, the priced subtotals
    /// sum to exactly the headline, and every label carries its marker.
    #[test]
    fn usd_full_mix_composes_the_headline() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        // The observed mix: 3M input tokens on opus, 1M on fable (input =
        // uncached + cached; output never weighs the split).
        seed_model_fact(
            &store,
            &rt,
            "c1",
            "claude-opus-4-8",
            1_000,
            (2_000_000, 1_000_000, 0, 999),
        );
        seed_model_fact(
            &store,
            &rt,
            "c2",
            "claude-fable-5",
            2_000,
            (600_000, 400_000, 0, 999),
        );
        seed_price(&store, &rt, "claude-opus-4-8", 5_000_000);
        seed_price(&store, &rt, "claude-fable-5", 10_000_000);
        seed_recall_db(root.path(), &proj, &[(1_000, 600_000), (2_000, 400_000)]);

        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        // 1M avoided split 3:1 → 750k @ $5/MTok + 250k @ $10/MTok = $6.25.
        assert_eq!(sd.usd_saved_micros, Some(6_250_000));
        assert_eq!(sd.usd_label.as_deref(), Some("~$6.25"));
        assert_eq!(sd.usd_rows.len(), 2);
        assert_eq!(sd.usd_rows.iter().map(|r| r.tokens).sum::<u64>(), 1_000_000);
        let priced: i64 = sd.usd_rows.iter().filter_map(|r| r.subtotal_micros).sum();
        assert_eq!(priced, 6_250_000, "subtotals compose the headline exactly");
        // Deterministic order: heaviest share first.
        assert_eq!(sd.usd_rows[0].model, "claude-opus-4-8");
        assert_eq!(sd.usd_rows[0].tokens, 750_000);
        assert_eq!(sd.usd_rows[0].rate_label.as_deref(), Some("$5.00/MTok"));
        assert_eq!(sd.usd_rows[0].subtotal_label.as_deref(), Some("~$3.75"));
        assert_eq!(sd.usd_rows[1].model, "claude-fable-5");
        assert_eq!(sd.usd_rows[1].subtotal_label.as_deref(), Some("~$2.50"));
    }

    /// The read side is valued through the **same mix and the same rates** as
    /// the avoided side — one basis read twice, never a second price table.
    ///
    /// The check is a ratio: the two bars are comparable in dollars only if
    /// their token counts went through one partition at one moment, so the
    /// dollar ratio has to equal the token ratio for a fully-priced mix.
    #[test]
    fn the_read_side_is_priced_through_the_same_mix_as_the_avoided_side() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "priced");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_model_fact(
            &store,
            &rt,
            "c1",
            "claude-opus-4-8",
            1_000,
            (1_000_000, 0, 0, 0),
        );
        seed_price(&store, &rt, "claude-opus-4-8", 5_000_000);
        seed_recall_db(root.path(), &proj, &[(1_000, 1_000_000)]);

        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        // 1M avoided at $5/MTok.
        assert_eq!(sd.usd_saved_micros, Some(5_000_000));
        // This fixture captures no reads, so the read side has no tokens to
        // value and honestly carries `0` rather than a figure.
        assert_eq!(sd.share.read_tokens, 0);
        assert_eq!(sd.usd_read_micros, Some(0));
        assert_eq!(sd.usd_read_label.as_deref(), Some("~$0.00"));
        // The label is the money authority's, never this crate's: no surface
        // here formats a currency.
        assert_eq!(
            sd.usd_read_label,
            sd.usd_read_micros.map(rift_shadow_cost::fmt_usd_estimate)
        );
    }

    /// Rung 2: an unpriced model keeps its token slice, contributes no
    /// dollars, and the headline honestly covers only the priced share.
    #[test]
    fn usd_partial_coverage_prices_what_it_can() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_model_fact(
            &store,
            &rt,
            "c1",
            "claude-opus-4-8",
            1_000,
            (1_000_000, 0, 0, 0),
        );
        seed_model_fact(
            &store,
            &rt,
            "c2",
            "mystery-model",
            2_000,
            (1_000_000, 0, 0, 0),
        );
        seed_price(&store, &rt, "claude-opus-4-8", 5_000_000);
        // mystery-model: no seeded price.
        seed_recall_db(root.path(), &proj, &[(1_000, 1_000_000)]);

        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        // Even split (1M:1M): only opus's 500k slice is priced → $2.50.
        assert_eq!(sd.usd_saved_micros, Some(2_500_000));
        let unpriced: Vec<_> = sd
            .usd_rows
            .iter()
            .filter(|r| r.subtotal_micros.is_none())
            .collect();
        assert_eq!(unpriced.len(), 1);
        assert_eq!(unpriced[0].model, "mystery-model");
        assert_eq!(unpriced[0].tokens, 500_000, "the slice is kept, undollared");
        assert_eq!(unpriced[0].rate_label, None);
        assert_eq!(unpriced[0].subtotal_label, None);
    }

    /// Rung 3: no input-token mix at all — the most-frequent captured model
    /// carries the whole total as a single disclosed reference row.
    #[test]
    fn usd_no_mix_falls_back_to_most_frequent_model() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        // model_call rows without token_usage: calls observed, no mix.
        for (id, model) in [
            ("c1", "claude-opus-4-8"),
            ("c2", "claude-opus-4-8"),
            ("c3", "claude-fable-5"),
        ] {
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
                    Value::text(id),
                    Value::text("t1"),
                    Value::text(model),
                    Value::Int(1_000),
                    Value::Int(1),
                ],
            ))
            .unwrap();
        }
        seed_price(&store, &rt, "claude-opus-4-8", 5_000_000);
        seed_recall_db(root.path(), &proj, &[(1_000, 2_000_000)]);

        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        assert_eq!(sd.usd_rows.len(), 1, "one reference row");
        assert_eq!(sd.usd_rows[0].model, "claude-opus-4-8", "most frequent");
        assert_eq!(sd.usd_rows[0].tokens, 2_000_000, "carries the whole total");
        assert_eq!(sd.usd_saved_micros, Some(10_000_000)); // 2M @ $5/MTok
    }

    /// Rung 4: honest absence — no capture projection, or nothing priced,
    /// yields no figure at all while the token headline stands untouched.
    /// Never `Some(0)` for a real saving.
    #[test]
    fn usd_absent_when_nothing_is_priced_or_no_capture() {
        // (a) recall data exists but there is no capture projection at all.
        let root = tempfile::tempdir().unwrap();
        let proj_dir = root.path().join("bare");
        std::fs::create_dir_all(&proj_dir).unwrap();
        // A repository with no projection — the state under test. Without the
        // gitdir there would be no family either, and the panel would be
        // reporting the absence of an identity rather than of a projection.
        rift_git::test_support::fake_repo(&proj_dir).unwrap();
        let proj = proj_dir.to_string_lossy().into_owned();
        seed_recall_db(root.path(), &proj, &[(1_000, 500)]);
        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        assert_eq!(
            sd.share,
            Box::default(),
            "no capture projection: the share is unanswerable, never a 100 % \
             over a denominator nobody observed"
        );
        assert_eq!(sd.usd_saved_micros, None);
        assert_eq!(sd.usd_label, None);
        assert!(sd.usd_rows.is_empty());

        // (b) a mix exists but no model has a seeded price → still None.
        let proj2 = make_project(root.path(), "unpriced");
        let store = open_proj(root.path(), &proj2);
        let rt = runtime().unwrap();
        seed_model_fact(
            &store,
            &rt,
            "c1",
            "mystery-model",
            1_000,
            (1_000_000, 0, 0, 0),
        );
        seed_recall_db(root.path(), &proj2, &[(1_000, 500)]);
        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj2, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        assert_eq!(sd.usd_saved_micros, None, "never a fabricated ~$0.00");
        assert!(sd.usd_rows.is_empty());
    }

    /// The money multiplies the NET avoided figure (the serve accounting seam) — the same
    /// value the percentage uses — never gross (net + returned).
    #[test]
    fn usd_values_net_avoided_not_gross() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_model_fact(
            &store,
            &rt,
            "c1",
            "claude-opus-4-8",
            1_000,
            (1_000_000, 0, 0, 0),
        );
        seed_price(&store, &rt, "claude-opus-4-8", 5_000_000);
        // Two serves, each returned=10 (the fixture's fixed cost): net
        // avoided is 1_000_000; gross would be 1_000_020.
        seed_recall_db(root.path(), &proj, &[(1_000, 600_000), (2_000, 400_000)]);

        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        let RecallView::Data(sd) = savings else {
            panic!("expected savings data");
        };
        // net 1M @ $5/MTok = $5.00 exactly; gross would price 20 more tokens.
        assert_eq!(sd.usd_saved_micros, Some(5_000_000));
    }

    /// The model mix is the **window's own**.
    ///
    /// The avoided tokens a period holds are partitioned across the models
    /// that period ran, not across every model the project ever ran: a mix
    /// taken over all history against a window's tokens attributes this
    /// week's savings to a model the project stopped using in the spring, and
    /// the modal then discloses a basis that was never this window's.
    #[test]
    fn the_model_mix_is_cut_where_the_tokens_it_prices_are() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "mixed");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        let day = 24 * 60 * 60 * 1000;
        // An old model, long out of use, and a current one.
        seed_model_fact(
            &store,
            &rt,
            "old",
            "claude-opus-4-8",
            USD_NOW - 10 * day,
            (3_000_000, 0, 0, 0),
        );
        seed_model_fact(
            &store,
            &rt,
            "new",
            "claude-fable-5",
            USD_NOW - 60_000,
            (1_000_000, 0, 0, 0),
        );
        seed_price(&store, &rt, "claude-opus-4-8", 5_000_000);
        seed_price(&store, &rt, "claude-fable-5", 10_000_000);
        seed_recall_db(root.path(), &proj, &[(USD_NOW - 60_000, 1_000_000)]);

        let models = |window: RecallWindow| -> Vec<String> {
            let FeedPayload::Recall { savings, .. } =
                compute_recall(root.path(), &proj, USD_NOW, window).payload
            else {
                panic!("expected Recall");
            };
            let RecallView::Data(sd) = savings else {
                panic!("expected savings data for {window:?}");
            };
            sd.usd_rows.iter().map(|r| r.model.clone()).collect()
        };
        assert_eq!(
            models(RecallWindow::Today),
            vec!["claude-fable-5".to_owned()],
            "only the model this window actually ran"
        );
        assert_eq!(
            models(RecallWindow::AllTime).len(),
            2,
            "the whole record is still the whole mix"
        );
    }

    /// The pure ladder: no basis and all-unpriced are both absence, and a
    /// partially priced mix reports exactly its priced sum.
    #[test]
    fn usd_breakdown_ladder_edges() {
        // No shares at all → no basis.
        assert!(usd_breakdown(1_000, &[]).is_none());
        // Shares exist but nothing is priced → absence, never Some(0).
        let unpriced = [
            MixShare {
                model: "a".into(),
                weight: 1,
                price_per_mtok: None,
            },
            MixShare {
                model: "b".into(),
                weight: 1,
                price_per_mtok: None,
            },
        ];
        assert!(usd_breakdown(1_000, &unpriced).is_none());
        // A negative price is nonsense input, treated as unpriced — and so is
        // a price of exactly ZERO. A seeded `0` used to value a real saving at
        // `~$0.00`, which reads as a measured nothing rather than as the
        // missing price it is; the ladder's bottom rung is "no chip at all".
        for nonsense in [-5, 0] {
            let mix = [MixShare {
                model: "a".into(),
                weight: 1,
                price_per_mtok: Some(nonsense),
            }];
            assert!(
                usd_breakdown(1_000, &mix).is_none(),
                "a rate of {nonsense} is not a price"
            );
        }
        // Zero avoided with a priced share is a real zero: "~$0.00" is honest.
        let priced = [MixShare {
            model: "a".into(),
            weight: 1,
            price_per_mtok: Some(5_000_000),
        }];
        let (total, rows) = usd_breakdown(0, &priced).unwrap();
        assert_eq!(total, 0);
        assert_eq!(rows[0].tokens, 0);
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

    /// The chosen period is a **cut on the audit relation**, and the same cut
    /// on both halves of the card.
    ///
    /// The rows are placed either side of a 24-hour boundary from `USD_NOW`,
    /// so each window's headline is arithmetic anyone can check by hand rather
    /// than whatever the fixture happened to contain.
    #[test]
    fn a_narrower_window_sums_only_the_rows_inside_it() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "windowed");
        let day = 24 * 60 * 60 * 1000;
        seed_recall_db(
            root.path(),
            &proj,
            &[
                (USD_NOW - 40 * day, 5_000), // outside every narrow window
                (USD_NOW - 10 * day, 700),   // inside 30d only
                (USD_NOW - 3 * day, 200),    // inside 30d and 7d
                (USD_NOW - 60_000, 90),      // inside all three
            ],
        );
        seed_one_read(root.path(), &proj, "claude-code", 9_000);
        let avoided = |window: RecallWindow| -> u64 {
            let FeedPayload::Recall { savings, .. } =
                compute_recall(root.path(), &proj, USD_NOW, window).payload
            else {
                panic!("expected Recall");
            };
            let RecallView::Data(sd) = savings else {
                panic!("expected savings data for {window:?}");
            };
            // The headline, the axis and the priced rows are all this figure:
            // one window resolution reaches every one of them.
            assert_eq!(sd.by_worktree.family_only, sd.share.avoided_tokens);
            sd.share.avoided_tokens
        };
        assert_eq!(avoided(RecallWindow::Today), 90);
        assert_eq!(avoided(RecallWindow::Last7Days), 290);
        assert_eq!(avoided(RecallWindow::Last30Days), 990);
        assert_eq!(
            avoided(RecallWindow::AllTime),
            5_990,
            "all-time keeps the pre-window rows the boundary marker never cut"
        );
    }

    /// A window that excludes every served row is **quiet**, not absent.
    ///
    /// Absent is the enable-prompt — "recall off — open Project Settings" —
    /// and a repository that served yesterday has already been through that
    /// door. Naming it again because the viewer asked about the last hour
    /// would be the card telling an operator to fix something that is working.
    #[test]
    fn an_empty_window_over_a_live_ledger_is_quiet_not_disabled() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "stale");
        let day = 24 * 60 * 60 * 1000;
        seed_recall_db(root.path(), &proj, &[(USD_NOW - 40 * day, 4_000)]);
        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::Today).payload
        else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Quiet);
        // …and the same ledger over the whole record still carries its data,
        // so "quiet" is a fact about the window and not about the store.
        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        assert!(matches!(savings, RecallView::Data(_)));
    }

    /// The card's window sentence is the **resolved boundary's own**, naming
    /// the instant it cut at and the button that asked for it.
    ///
    /// One resolution, one sentence: the card used to print a phrase of its
    /// own ("since 24 hours ago") that could not say which instant it meant
    /// and could not say when a boundary marker had moved the cut somewhere
    /// else. No flag was typed here, so nothing may claim one was.
    #[test]
    fn the_share_block_names_the_boundary_it_actually_cut_at() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "labelled");
        seed_recall_db(root.path(), &proj, &[(USD_NOW - 60_000, 90)]);
        for (window, spelling) in [
            (RecallWindow::Today, Some("(24h)")),
            (RecallWindow::Last7Days, Some("(7d)")),
            (RecallWindow::Last30Days, Some("(30d)")),
            // No marker in this fixture, so all-time is the whole record and
            // says nothing — exactly what it said before the buttons existed.
            (RecallWindow::AllTime, None),
        ] {
            let FeedPayload::Recall { savings, .. } =
                compute_recall(root.path(), &proj, USD_NOW, window).payload
            else {
                panic!("expected Recall");
            };
            let RecallView::Data(sd) = savings else {
                panic!("expected savings data for {window:?}");
            };
            let sentence = sd.share.window.clone();
            match spelling {
                Some(s) => {
                    let sentence = sentence.unwrap_or_default();
                    assert!(sentence.ends_with(s), "{window:?}: {sentence}");
                    assert!(sentence.starts_with("since 20"), "{window:?}: {sentence}");
                    assert!(
                        !sentence.contains("--since"),
                        "no flag was typed: {sentence}"
                    );
                }
                None => assert_eq!(sentence, None, "{window:?}"),
            }
        }
    }

    /// A period button never reaches back past the family's boundary marker.
    ///
    /// The two are exclusions and they compose to the tighter one, so `[30d]`
    /// on a repository whose window was reset an hour ago covers the hour —
    /// and says the marker is what cut it. Anything else would draw rows on
    /// the card that `rift savings` refuses to count.
    #[test]
    fn a_period_button_is_composed_with_the_boundary_marker() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "marked");
        let day = 24 * 60 * 60 * 1000;
        seed_recall_db(
            root.path(),
            &proj,
            &[
                (USD_NOW - 10 * day, 5_000), // before the marker
                (USD_NOW - 60_000, 90),      // after it
            ],
        );
        seed_one_read(root.path(), &proj, "claude-code", 9_000);
        let family = project_family(Path::new(&proj)).expect("family");
        rift_recall_ledger::savings_window::write_marker(
            &family.common_dir,
            USD_NOW - 2 * day,
            USD_NOW,
        )
        .expect("marker");

        let card = |window: RecallWindow| {
            let FeedPayload::Recall { savings, .. } =
                compute_recall(root.path(), &proj, USD_NOW, window).payload
            else {
                panic!("expected Recall");
            };
            match savings {
                RecallView::Data(sd) => sd,
                other => panic!("expected savings data for {window:?}: {other:?}"),
            }
        };
        // 30d asked for more than the marker allows: the marker wins, and the
        // sentence says which of the two did.
        let wide = card(RecallWindow::Last30Days);
        assert_eq!(wide.share.avoided_tokens, 90);
        assert!(
            wide.share
                .window
                .as_deref()
                .unwrap_or_default()
                .contains("was reset"),
            "{:?}",
            wide.share.window
        );
        // 24h is narrower than the marker: its own cut stands.
        let narrow = card(RecallWindow::Today);
        assert_eq!(narrow.share.avoided_tokens, 90);
        assert!(narrow
            .share
            .window
            .as_deref()
            .unwrap_or_default()
            .ends_with("(24h)"));
        // `all` is the marker, as it always was.
        assert_eq!(card(RecallWindow::AllTime).share.avoided_tokens, 90);
    }

    /// The serve count beside the headline covers the card's period, and the
    /// chart draws nothing from outside it.
    #[test]
    fn the_serve_count_follows_the_card_window() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "counted");
        let day = 24 * 60 * 60 * 1000;
        seed_recall_db(
            root.path(),
            &proj,
            &[
                (USD_NOW - 10 * day, 100),
                (USD_NOW - 3 * day, 100),
                (USD_NOW - 60_000, 100),
            ],
        );
        let total = |window: RecallWindow| -> u64 {
            let FeedPayload::Recall { calls, .. } =
                compute_recall(root.path(), &proj, USD_NOW, window).payload
            else {
                panic!("expected Recall");
            };
            match calls {
                RecallView::Data(cd) => cd.total,
                other => panic!("expected call data for {window:?}: {other:?}"),
            }
        };
        assert_eq!(total(RecallWindow::Today), 1);
        assert_eq!(total(RecallWindow::Last7Days), 2);
        assert_eq!(total(RecallWindow::AllTime), 3);
    }

    /// A trailing window a broken clock cannot place is an **empty window**,
    /// never the whole record under a narrow label.
    ///
    /// The bus answers `0` when the platform refuses the wall clock; the old
    /// arithmetic clamped `0 − 24h` to the epoch and the card drew every row
    /// it had under `[24h]`.
    #[test]
    fn an_unplaceable_window_draws_nothing_rather_than_everything() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "clockless");
        seed_recall_db(root.path(), &proj, &[(1_000, 5_000)]);
        seed_one_read(root.path(), &proj, "claude-code", 9_000);
        let FeedPayload::Recall { savings, calls } =
            compute_recall(root.path(), &proj, 0, RecallWindow::Today).payload
        else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Quiet, "no figures under a dead clock");
        match calls {
            RecallView::Data(cd) => assert_eq!(cd.total, 0),
            other => panic!("expected an empty call window: {other:?}"),
        }
        // The same clock with `all` selected still answers: that window needs
        // no clock at all.
        let FeedPayload::Recall { savings, .. } =
            compute_recall(root.path(), &proj, 0, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        assert!(matches!(savings, RecallView::Data(_)));
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
        seed_one_read(root.path(), &proj, "claude-code", 9_000);

        let FeedPayload::Recall { savings, calls, .. } =
            compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected Recall");
        };
        // If the reader used the capture id it would find the empty decoy and
        // report Quiet; matching the writer's id, it reports the served data.
        let RecallView::Data(sd) = savings else {
            panic!("must read the writer's recall.db, not the capture-id decoy");
        };
        assert_eq!(sd.share.avoided_tokens, 1_200);
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
        seed_one_read(root.path(), &pa, "claude-code", 9_000);
        // b has NO recall.db: a's events must not leak into b's sections.
        let ub = compute_recall(root.path(), &pb, USD_NOW, RecallWindow::AllTime);
        let FeedPayload::Recall { savings, .. } = ub.payload else {
            panic!("expected Recall");
        };
        assert_eq!(savings, RecallView::Disabled);
        let ua = compute_recall(root.path(), &pa, USD_NOW, RecallWindow::AllTime);
        let FeedPayload::Recall {
            savings: RecallView::Data(sd),
            ..
        } = ua.payload
        else {
            panic!("expected a's data");
        };
        assert_eq!(sd.share.avoided_tokens, 700);
    }

    // --- the activity feed ------------------------------------------

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
    fn activity_missing_projection_is_the_placeholder_never_thirty_zeros() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("plain");
        std::fs::create_dir_all(&proj).unwrap();
        let update = compute_activity(root.path(), &proj.to_string_lossy(), NOW);
        assert_eq!(
            update.payload,
            FeedPayload::Activity(ActivityView::NoProjection),
            "no projection is the placeholder, not a zero month"
        );
    }

    #[test]
    fn activity_empty_projection_is_thirty_zero_days() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "quiet");
        let FeedPayload::Activity(ActivityView::Days(d)) =
            compute_activity(root.path(), &proj, NOW).payload
        else {
            panic!("a readable projection yields Days, even when quiet");
        };
        assert_eq!(d.days.len(), 30);
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
        // The oldest included day (29 days ago) remains visible.
        seed_event(&store, &rt, "e-start", 19_971 * DAY_MS + 100);
        // Thirty days ago is outside the fixed window.
        seed_event(&store, &rt, "e-old", 19_970 * DAY_MS + 100);
        // Future facts are never pulled into today's final bucket.
        seed_event(&store, &rt, "e-future", 20_001 * DAY_MS + 100);

        let FeedPayload::Activity(ActivityView::Days(d)) =
            compute_activity(root.path(), &proj, NOW).payload
        else {
            panic!("expected Days");
        };
        assert_eq!(d.days.len(), 30);
        let today = &d.days[29]; // oldest→newest, today is last
        assert_eq!((today.events, today.sessions, today.checkpoints), (2, 1, 1));
        let yesterday = &d.days[28];
        assert_eq!(yesterday.events, 1);
        assert_eq!(d.days[0].events, 1, "the inclusive oldest day is retained");
        // Outside/future events are dropped → total is exactly four.
        let total_events: u64 = d.days.iter().map(|b| b.events).sum();
        assert_eq!(total_events, 4, "outside and future events are dropped");
        // …and "dropped" is now said out loud: the two that fell outside the
        // window are the remainder, not a silence.
        assert_eq!(
            d.off_strip.events, 2,
            "e-old and e-future are accounted for"
        );
        assert_eq!(d.off_strip.sessions, 0);
        assert_eq!(d.off_strip.checkpoints, 0);
        // Weekday labels: day 20_000 is a Friday.
        assert_eq!(today.weekday, "Fri");
    }

    /// **The sentinel guard, and it now pins the RESIDUAL case.**
    ///
    /// `checkpoint.created_at` carries an explicit unknown-time sentinel.
    /// Measured on the projection for this repository (`4d9d222…`, 2026-08-18):
    /// 0 of 103 checkpoints placed beside 15,887 placed events, rendered as a
    /// quiet month.
    ///
    /// **These three checkpoints archive no session**, which is why they are
    /// still unplaceable after the strip moved onto
    /// `rift_store::window::CHECKPOINT_TIME_CTE`: the derivation resolves a
    /// sentinel row through `session_in_checkpoint`, and there is nothing here
    /// to resolve it to. That is the honest floor — a row no stored fact can
    /// place. The population the fix actually rescues is the one with sessions,
    /// pinned by
    /// `a_shard_that_records_no_time_is_placed_by_the_sessions_it_archives`
    /// below; keeping BOTH is what stops "the strip got busier" from hiding
    /// "the strip now guesses".
    ///
    /// What the strip may never do is stay silent about what it could not
    /// place, and this is the assertion that reddens if it does.
    #[test]
    fn checkpoints_whose_time_is_the_unknown_sentinel_are_counted_not_silently_dropped() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        // The shape of the real projection: events and sessions carry real
        // times inside the window; every checkpoint carries the sentinel —
        // read from the crate that declares it, now that this crate can link
        // to it. It could not when this test was written, and the comment that
        // said so has been removed rather than left to age.
        const SENTINEL: i64 = CREATED_AT_UNKNOWN;
        seed_event(&store, &rt, "e1", 20_000 * DAY_MS + 1_000);
        seed_session(&store, &rt, "s1", 20_000 * DAY_MS + 500);
        for i in 0..3 {
            seed_checkpoint(&store, &rt, &format!("k{i}"), SENTINEL);
        }

        let FeedPayload::Activity(ActivityView::Days(d)) =
            compute_activity(root.path(), &proj, NOW).payload
        else {
            panic!("expected Days");
        };
        assert!(
            d.days.iter().all(|b| b.checkpoints == 0),
            "an undated checkpoint has no day to sit on"
        );
        assert_eq!(
            d.off_strip.checkpoints, 3,
            "…and every one of them is counted where the reader cannot miss it"
        );
        // The other two categories were placed, so the remainder is specific to
        // the column that lied and not a blanket "everything is missing".
        assert_eq!(d.days[29].events, 1);
        assert_eq!(d.days[29].sessions, 1);
        assert_eq!((d.off_strip.events, d.off_strip.sessions), (0, 0));
    }

    /// **The defect this crate carried until 2026-08-19, pinned from the side
    /// that can see it.**
    ///
    /// A native shard records no time of its own, so `created_at` is the
    /// sentinel — and the strip used to bucket that column directly, putting
    /// `-1 / 86_400_000` in 1969 and therefore placing NOTHING. On a repository
    /// of native checkpoints the checkpoint row was structurally zero and the
    /// legacy population fell into `OffStrip`, which a reader takes to mean
    /// "older than the window" when it really meant "undated".
    ///
    /// The strip now prefixes `rift_store::window::CHECKPOINT_TIME_CTE`, the
    /// same derivation `rift dispatch` uses, so a shard is placed at
    /// `MAX(session.started_at)` over the sessions it archives — the tightest
    /// lower bound stored facts support, and never a guess: the session cannot
    /// postdate the shard that covers it.
    ///
    /// Written as a SEPARATE test rather than by editing the one above, because
    /// the two cases are different facts: with a session it is placed, without
    /// one it is counted off-strip, and a single test asserting only the happy
    /// half would let the fix regress into placing everything at day zero.
    #[test]
    fn a_shard_that_records_no_time_is_placed_by_the_sessions_it_archives() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();

        // Two undated shards. `k-linked` archives a session that started
        // yesterday; `k-orphan` archives nothing at all.
        seed_session(&store, &rt, "s-yesterday", 19_999 * DAY_MS + 500);
        seed_checkpoint(&store, &rt, "k-linked", CREATED_AT_UNKNOWN);
        seed_checkpoint(&store, &rt, "k-orphan", CREATED_AT_UNKNOWN);
        rt.block_on(store.insert(
            "session_in_checkpoint",
            &["checkpoint_id", "session_id", "ordinal", "schema_version"],
            &[
                Value::text("k-linked"),
                Value::text("s-yesterday"),
                Value::Int(0),
                Value::Int(1),
            ],
        ))
        .unwrap();

        let FeedPayload::Activity(ActivityView::Days(d)) =
            compute_activity(root.path(), &proj, NOW).payload
        else {
            panic!("expected Days");
        };
        assert_eq!(
            d.days[28].checkpoints, 1,
            "the linked shard sits on its session's day, not on no day at all"
        );
        assert_eq!(d.days[29].checkpoints, 0, "and not on today");
        assert_eq!(
            d.off_strip.checkpoints, 1,
            "the orphan is still unplaceable, and is still counted rather than hidden"
        );
    }

    #[test]
    fn a_fully_placed_month_leaves_nothing_off_the_strip() {
        // The other half of the guard: the remainder is zero when the strips
        // account for everything, so the card is unchanged where nothing is
        // wrong. Without this, "always report 3 missing" would pass the test
        // above.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        let store = open_proj(root.path(), &proj);
        let rt = runtime().unwrap();
        seed_event(&store, &rt, "e1", 20_000 * DAY_MS + 1_000);
        seed_session(&store, &rt, "s1", 19_990 * DAY_MS + 500);
        seed_checkpoint(&store, &rt, "k1", 19_971 * DAY_MS + 3_000);

        let FeedPayload::Activity(ActivityView::Days(d)) =
            compute_activity(root.path(), &proj, NOW).payload
        else {
            panic!("expected Days");
        };
        assert_eq!(d.off_strip, OffStrip::default());
        assert!(!d.off_strip.any());
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

    /// The card's headline is the **shared share** and nothing of its own.
    ///
    /// It used to be a second ratio — `avoided ÷ (avoided + returned)` over
    /// the audit trail — computed here, with a different denominator from the
    /// one `rift savings` publishes and the same shape on screen. There is one
    /// figure now, and this test pins it to the ledger's own function over the
    /// same two sides.
    #[test]
    fn the_headline_is_the_shared_share_and_never_a_second_ratio() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        // seed_recall_db fixes tokens_returned = 10 per event.
        seed_recall_db(root.path(), &proj, &[(1_000, 90), (2_000, 40), (3_000, 20)]);
        seed_one_read(root.path(), &proj, "claude-code", 850);
        let FeedPayload::Recall {
            savings: RecallView::Data(sd),
            ..
        } = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected savings data");
        };
        let avoided = 90 + 40 + 20; // 150
        assert_eq!(sd.share.avoided_tokens, avoided);
        assert_eq!(sd.share.read_tokens, 850);
        assert_eq!(
            sd.share.overall,
            rift_recall_ledger::savings::share(avoided, 850),
            "the headline is the ledger's share, denominator and all"
        );
        // …and the honesty figure carries what the serves cost: 150 − 30.
        assert_eq!(sd.share.honesty, 120);
    }

    /// The verb split must not make the panel lie in the other direction.
    ///
    /// The savings feed reads `memd_audit_event` with no verb predicate, so a
    /// newly minted verb is neither dropped nor bucketed as credit — it lands
    /// in `Σreturned`, which is what the **honesty** figure charges it
    /// against, exactly as a push-tier cost should. This test is the pin for
    /// that: without it, adding a verb predicate here (or filtering unknown
    /// verbs out "defensively") would silently delete injected spend from the
    /// card and leave the avoided total looking free.
    #[test]
    fn an_injected_manifest_is_read_as_spend_and_never_as_credit() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        // One pull-side serve: 90 avoided, 10 returned (seed_recall_db fixes 10).
        seed_recall_db(root.path(), &proj, &[(1_000, 90)]);
        seed_one_read(root.path(), &proj, "claude-code", 910);
        let FeedPayload::Recall {
            savings: RecallView::Data(before),
            ..
        } = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected savings data");
        };

        // Then one injected manifest: 40 tokens of spend, no credit, no call row.
        seed_injection_row(root.path(), &proj, "inj-1", 2_000, 40);
        let FeedPayload::Recall {
            savings: RecallView::Data(after),
            ..
        } = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected savings data");
        };

        // Credit is unchanged: an injection avoided nothing, so neither the
        // headline share nor the token figure under it moves.
        assert_eq!(
            after.share.avoided_tokens, before.share.avoided_tokens,
            "an injection must add no credit"
        );
        assert_eq!(after.share.overall, before.share.overall);
        // Spend is charged where spend belongs: 90 avoided − (10 + 40)
        // returned, down by exactly the injection.
        assert_eq!(before.share.honesty, 80);
        assert_eq!(
            after.share.honesty, 40,
            "unrequested spend must be charged, not vanish"
        );
    }

    /// Nothing avoided over a window that read is **0 %**, not absence and
    /// never a NaN: the question was asked and the answer is zero.
    #[test]
    fn a_window_that_avoided_nothing_reads_zero_rather_than_absent() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p");
        seed_recall_db(root.path(), &proj, &[(1_000, 0), (2_000, 0)]);
        seed_one_read(root.path(), &proj, "claude-code", 4_000);
        let FeedPayload::Recall {
            savings: RecallView::Data(sd),
            ..
        } = compute_recall(root.path(), &proj, USD_NOW, RecallWindow::AllTime).payload
        else {
            panic!("expected savings data");
        };
        assert_eq!(sd.share.overall, Some(0.0));
        assert_eq!(sd.share.avoided_tokens, 0);
    }

    proptest! {
        // Real stores per case: keep the case count farm-friendly.
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(8))]

        /// Scope: two projects' feeds never mix, for arbitrary fixtures.
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
                    worktree: None,
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

    fn token_row(t: i64, uncached: i64, cached: i64, creation: i64, output: i64) -> TokenRow {
        TokenRow {
            t,
            uncached_input: uncached,
            cached_input: cached,
            cache_creation: creation,
            output,
            worktree: None,
        }
    }

    /// A row's total is the sum of all four classes, and a negative class
    /// contributes zero rather than subtracting — a bad row must not be able
    /// to pull a displayed total downwards.
    #[test]
    fn a_negative_token_class_contributes_zero_not_a_subtraction() {
        assert_eq!(row_total(&token_row(0, 1, 2, 3, 4)), 10.0);
        assert_eq!(row_total(&token_row(0, 0, 0, 0, 0)), 0.0);
        assert_eq!(
            row_total(&token_row(0, 10, -5, 3, -1)),
            13.0,
            "negatives floor at zero"
        );
    }

    /// The bucket ladder: the smallest step where the span fits the display
    /// budget, and the coarsest step for anything past the ladder's end. A
    /// span at the exact boundary still fits the smaller step.
    #[test]
    fn the_bucket_ladder_picks_the_smallest_step_that_fits() {
        assert_eq!(bucket_ms(0), 1_000);
        assert_eq!(bucket_ms(1_000 * TARGET_BUCKETS), 1_000, "exactly fits");
        assert_eq!(bucket_ms(1_000 * TARGET_BUCKETS + 1), 5_000);
        assert_eq!(bucket_ms(60_000 * TARGET_BUCKETS), 60_000);
        assert_eq!(bucket_ms(60_000 * TARGET_BUCKETS + 1), 300_000);
        assert_eq!(
            bucket_ms(86_400_000 * TARGET_BUCKETS + 1),
            86_400_000,
            "past the ladder, the coarsest step"
        );
        assert_eq!(bucket_ms(i64::MAX), 86_400_000, "no overflow off the end");
    }

    /// The rate series is a bucketed sum: no facts is an empty series (the
    /// placeholder's job, not a row of zeros), a quiet bucket inside an active
    /// span is an honest zero, leading empty margin is trimmed, and replaying
    /// the same rows yields the same series.
    #[test]
    fn the_rate_series_is_bucketed_trimmed_and_replayable() {
        assert!(rate_series(&[]).is_empty());

        // Two facts one second apart: step 1s, so they land in adjacent
        // buckets at the END of the window, with the empty margin trimmed.
        let rows = vec![token_row(0, 1, 0, 0, 0), token_row(1_000, 0, 0, 0, 2)];
        let series = rate_series(&rows);
        assert_eq!(series, vec![1.0, 2.0]);
        assert_eq!(
            rate_series(&rows),
            series,
            "bucketing is idempotent in the rows"
        );

        // A quiet bucket between two active ones is a zero, not a gap.
        let quiet = rate_series(&[token_row(0, 3, 0, 0, 0), token_row(2_000, 0, 0, 0, 5)]);
        assert_eq!(quiet, vec![3.0, 0.0, 5.0]);

        // Facts older than the visible window are dropped rather than folded
        // into the first bucket, which would invent a spike.
        let old = rate_series(&[
            token_row(0, 100, 0, 0, 0),
            token_row(86_400_000 * TARGET_BUCKETS * 2, 0, 0, 0, 7),
        ]);
        assert_eq!(
            crate::series::saturating_sum(old.iter().copied()),
            7.0,
            "the out-of-window fact is dropped, not relocated"
        );
    }

    /// The breakdown's read fraction is `read / (input + read)` with `0/0 ≡ 0`
    /// — an unpriced-style division that must never produce NaN in a drawn
    /// bar — and every class floors at zero.
    #[test]
    fn the_read_fraction_is_zero_when_there_is_nothing_to_divide() {
        let empty = breakdown_data(&[]);
        assert_eq!(empty.read_fraction, 0.0);
        assert!(empty.read_fraction.is_finite(), "never NaN");

        let output_only = breakdown_data(&[token_row(0, 0, 0, 0, 9)]);
        assert_eq!(output_only.output, 9.0);
        assert_eq!(
            output_only.read_fraction, 0.0,
            "output alone does not make a read fraction"
        );

        let mixed = breakdown_data(&[token_row(0, 3, 1, 5, 2), token_row(1, -4, 0, 0, 0)]);
        assert_eq!((mixed.input, mixed.read), (3.0, 1.0));
        assert_eq!((mixed.output, mixed.cache_write), (2.0, 5.0));
        assert!((mixed.read_fraction - 0.25).abs() < 1e-12);
    }

    /// The tokens section's total is the sum of the same row totals its rate
    /// series buckets, so the headline number and the plot cannot disagree.
    #[test]
    fn the_tokens_total_is_the_same_arithmetic_as_the_plot() {
        let rows = vec![
            token_row(0, 1, 2, 3, 4),
            token_row(1_000, 5, 0, 0, 0),
            token_row(2_000, 0, 0, 0, 6),
        ];
        let data = tokens_data(&rows);
        assert_eq!(data.total, 21.0);
        assert_eq!(
            crate::series::saturating_sum(data.rate.iter().copied()),
            data.total,
            "the plot sums to the headline"
        );
        let empty = tokens_data(&[]);
        assert_eq!(empty.total, 0.0);
        assert!(empty.rate.is_empty());
    }

    /// Weekday labels are Monday-indexed off an epoch-day count whose day 0 is
    /// a Thursday, and negative days (pre-1970) wrap the same way rather than
    /// falling off the table.
    #[test]
    fn weekday_labels_are_monday_indexed_from_a_thursday_epoch() {
        assert_eq!(weekday_label(0), "Thu");
        for (day, expected) in [
            (1, "Fri"),
            (2, "Sat"),
            (3, "Sun"),
            (4, "Mon"),
            (5, "Tue"),
            (6, "Wed"),
            (7, "Thu"),
        ] {
            assert_eq!(weekday_label(day), expected, "day {day}");
        }
        assert_eq!(weekday_label(-1), "Wed");
        assert_eq!(weekday_label(-7), "Thu");
    }
}

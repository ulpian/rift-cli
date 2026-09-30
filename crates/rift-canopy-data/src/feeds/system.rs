//! The System card: the live-session badge, the context gauge, and the
//! five-minute activity figures — all composed from ONE live-cache read so a
//! reducer can never pair one recomputation's badge with another's context.

use std::path::Path;

use rift_canopy::col3::{ActiveSessionsView, NicReading, NicView, PanelMetrics, SystemScope};
use rift_canopy::{FeedPayload, FeedUpdate, LiveSessions, SessionsRead};
use rift_nic_gauge::{NicGauge as _, Reading, ReferenceGauge};
use rift_store::{
    projection_db_for_family_under, recall_db_for_family_under, CanonicalStore, SqliteStore, Value,
};

use super::project_family;
use crate::store::runtime;

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
    if !db.exists() {
        return (NicView::NoData, PanelMetrics::default());
    }
    let recall = recall_db_for_family_under(root, &family);
    let bytes = Some(file_len(&db).saturating_add(file_len(&recall)));
    let (Ok(store), Ok(rt)) = (SqliteStore::open(&db), runtime()) else {
        return storage_only(bytes);
    };
    let Some(sessions) = resolve_session_set(&store, &rt, active, active_count) else {
        return storage_only(bytes);
    };
    let (events_5m, tokens_5m, serve_calls_5m) =
        recent_activity(&store, &rt, Some(recall.as_path()), &sessions);

    let (context, context_used, context_window, context_scope) =
        match observed_context(&store, &rt, &sessions) {
            Some((observation, scope)) => {
                let (view, used, window) = gauge_reading(observation, active_count);
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
            projection_bytes: bytes,
            context_used,
            context_window,
            context_scope,
            activity_scope: sessions.scope,
        },
    )
}

/// The panel when the stores are on disk but nothing under them could be read:
/// the size is a fact and is kept; every figure that needs a query is absent
/// rather than zero.
fn storage_only(projection_bytes: Option<u64>) -> (NicView, PanelMetrics) {
    (
        NicView::NoData,
        PanelMetrics {
            projection_bytes,
            ..PanelMetrics::default()
        },
    )
}

/// A file's size, or zero when it cannot be stated — the panel reports the
/// space it can see, never an error.
fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |meta| meta.len())
}

/// The context sample the gauge shows, and the scope it was taken under.
///
/// A live session that has not sampled its context yet **holds** the last
/// completed session's sample rather than blanking the gauge, and says so by
/// returning that scope: a gauge that went dark between two turns would read
/// as a fault rather than as the quiet moment it is.
fn observed_context(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    sessions: &SessionSet,
) -> Option<(ContextObservation, SystemScope)> {
    match freshest_context(store, rt, &sessions.canonical) {
        Some(observation) => Some((observation, sessions.scope)),
        None if sessions.scope == SystemScope::CurrentSessions => {
            let held = freshest_completed_context(store, rt)?;
            Some((held, SystemScope::LastCompletedSession))
        }
        None => None,
    }
}

/// The gauge's view of one observation, with the two figures the panel prints
/// beside it: the tokens in use and the model's window.
fn gauge_reading(
    observation: ContextObservation,
    active_sessions: u16,
) -> (NicView, u64, Option<u64>) {
    let used = observation.used_tokens();
    let window = ReferenceGauge.window_for_model(&observation.model);
    let view = match ReferenceGauge.reading(&observation.model, used) {
        Reading::Known(known) => NicView::Reading(NicReading {
            pct: known.pct,
            band: known.band().label(),
            model: observation.model,
            active_sessions,
        }),
        Reading::UnknownModel { .. } => NicView::UnknownModel {
            model: observation.model,
            active_sessions,
        },
    };
    (view, used, window)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feeds::fixtures::{make_project, open_proj, seed_event_for, seed_serve_call_events};

    fn live(n: u16) -> Option<LiveSessions> {
        Some(LiveSessions {
            active_sessions: n,
            since_ms: 1,
        })
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

    /// Seed the `session` + `capture_session_identity` pair a live cache
    /// resolves through, and answer with the canonical row id.
    ///
    /// Alone, this is a session the panel can name but has no context sample
    /// for — the state the held-context test needs.
    fn seed_identified_session(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        native: &str,
        model: &str,
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
        canonical
    }

    /// Seed the session's one turn.
    fn seed_turn(store: &SqliteStore, rt: &tokio::runtime::Runtime, native: &str, canonical: &str) {
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
                Value::text(canonical),
                Value::Int(1),
                Value::Int(8_000),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// Seed one captured event keyed by the NATIVE session id, the way the
    /// capture writer records it — the row `events_5m` counts.
    fn seed_native_event(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        key: &str,
        native: &str,
        kind: &str,
        occurred_at: i64,
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
                Value::text(native),
                Value::Int(1),
                Value::Int(0),
                Value::text(kind),
                Value::text("transcript_untrusted"),
                Value::text("aa"),
                Value::Int(occurred_at),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// A whole System-panel session: identity, one turn, one captured event
    /// and the context sample the gauge reads.
    fn seed_system_session(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        native: &str,
        model: &str,
        context: (i64, i64, i64, i64),
    ) -> String {
        let canonical = seed_identified_session(store, rt, native, model);
        seed_turn(store, rt, native, &canonical);
        seed_native_event(
            store,
            rt,
            &format!("event-{native}"),
            native,
            "turn_end",
            9_000,
        );
        seed_context_observation(store, rt, native, &canonical, model, context);
        canonical
    }

    /// Seed the `end` event that closes a session — what makes it the *last
    /// completed* one.
    fn seed_session_end(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        canonical: &str,
        occurred_at: i64,
    ) {
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
                Value::text(canonical),
                Value::text("end"),
                Value::Int(occurred_at),
                Value::Int(1),
            ],
        ))
        .unwrap();
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
        assert_eq!(reading.pct, 5, "50k / 1M, never the 1M turn aggregate");
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
        // A completed session that DID sample its context…
        let old = seed_system_session(
            &store,
            &rt,
            "old-native",
            "claude-fable-5",
            (10_000, 20_000, 0, 5_000),
        );
        seed_session_end(&store, &rt, &old, 9_100);

        // …and the live one, which has captured an event but no sample yet.
        let native = "current-native";
        seed_identified_session(&store, &rt, native, "claude-fable-5");
        seed_native_event(&store, &rt, "current-event", native, "tool_call", 10_500);

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
}

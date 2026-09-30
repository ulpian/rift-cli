//! Detector (b): tool events arriving while the lifecycle hooks do not.
//!
//! An agent whose client runs Rift's tool hooks but skips its session and
//! prompt hooks still fills the ledger with tool calls, so nothing looks
//! empty; what is lost is every session boundary and every prompt. The
//! signal is the agent's newest recent session: tool events, and neither a
//! native session-opening nor a prompt of its own.
//!
//! Antigravity has no session-start hook, and the capture path writes one
//! canonical `session_start` for it, stamped with the reserved event ordinal
//! `u32::MAX`; that ordinal reaches `captured_event.event_ordinal` unchanged,
//! so the row is excluded by it here rather than by reading receipts, and a
//! start the capture path invented never counts as one the agent sent.

use std::collections::BTreeMap;

use rift_shadow_hooks::session_cache::SessionCache;
use rift_shadow_hooks::{AdapterRegistry, AgentAdapter, AgentId, EventKind};
use rift_store::{CanonicalStore as _, SqliteStore, Value};

use super::{Finding, Reason};

/// What one session's captured events hold, counted.
struct Holdings {
    tools: i64,
    native_openings: i64,
    prompts: i64,
}

/// At most one finding per agent, about its newest session whose last event
/// is inside the alarm window. Older sessions are not judged: the question is
/// whether the hooks arrive now, and a session from before a repair would
/// otherwise keep the alarm on.
pub(super) fn lifecycle_findings(
    projection: &SqliteStore,
    sessions: &[SessionCache],
    now_ms: i64,
) -> Vec<Finding> {
    let Some(runtime) = super::runtime() else {
        return Vec::new();
    };
    let recent = runtime.block_on(last_event_by_session(
        projection,
        super::window_start(now_ms),
    ));
    let registry = AdapterRegistry::with_builtin();
    let mut findings = Vec::new();
    for cache in newest_per_agent(sessions, &recent).into_values() {
        let Some(adapter) = registry.get(&AgentId::new(cache.agent.clone())) else {
            continue;
        };
        let Some(holdings) = runtime.block_on(holdings_of(projection, &cache.session_id)) else {
            continue;
        };
        if holdings.tools > 0 && holdings.native_openings == 0 && holdings.prompts == 0 {
            findings.push(Finding::new(
                Reason::LifecycleHooksMissing,
                &cache.agent,
                Some(&cache.session_id),
                format!(
                    "no {}; {} tool events",
                    opening_verbs(adapter).join(", "),
                    holdings.tools
                ),
            ));
        }
    }
    findings
}

/// Per agent, the cached session with the latest event in `recent`. Ties go
/// to the greater session id, so the choice does not depend on cache order.
fn newest_per_agent<'a>(
    sessions: &'a [SessionCache],
    recent: &BTreeMap<String, i64>,
) -> BTreeMap<&'a str, &'a SessionCache> {
    let mut newest: BTreeMap<&str, (i64, &SessionCache)> = BTreeMap::new();
    for cache in sessions {
        let Some(&last) = recent.get(&cache.session_id) else {
            continue;
        };
        let entry = newest.entry(cache.agent.as_str()).or_insert((last, cache));
        if (last, &cache.session_id) > (entry.0, &entry.1.session_id) {
            *entry = (last, cache);
        }
    }
    newest
        .into_iter()
        .map(|(agent, (_, cache))| (agent, cache))
        .collect()
}

/// The verbs of `adapter` that carry a session-opening or a prompt, found by
/// asking the adapter's own mapping rather than keeping a second table. The
/// probe payload names a first model invocation, which is how Antigravity's
/// opening verb is told from its later invocations; every other adapter
/// ignores the field.
fn opening_verbs(adapter: &dyn AgentAdapter) -> Vec<&'static str> {
    let probe = serde_json::json!({ "invocationNum": 0 });
    adapter
        .verbs()
        .iter()
        .copied()
        .filter(|verb| {
            adapter.map(verb, &probe).is_ok_and(|events| {
                events.iter().any(|event| {
                    matches!(
                        event.kind,
                        EventKind::SessionStart | EventKind::PromptSubmit
                    )
                })
            })
        })
        .collect()
}

/// Every session with an event at or after `since`, with its latest event
/// time. Empty on any read failure: a detector that cannot read reports
/// nothing rather than a guess.
async fn last_event_by_session(store: &SqliteStore, since: i64) -> BTreeMap<String, i64> {
    let rows = store
        .query(
            "SELECT session_id, max(occurred_at_ms) FROM captured_event \
             WHERE occurred_at_ms >= ? GROUP BY session_id",
            &[Value::Int(since)],
        )
        .await
        .unwrap_or_default();
    rows.iter()
        .filter_map(|row| Some((row.get_text(0).ok()?, row.get_i64(1).ok()?)))
        .collect()
}

/// What `session`'s captured events hold, over its whole history: an opening
/// sent a week ago still opened the session.
async fn holdings_of(store: &SqliteStore, session: &str) -> Option<Holdings> {
    let synthesized = i64::from(u32::MAX);
    let rows = store
        .query(
            "SELECT coalesce(sum(kind IN ('tool_call', 'tool_result', 'file_edit')), 0), \
             coalesce(sum(kind = 'session_start' AND event_ordinal != ?), 0), \
             coalesce(sum(kind = 'prompt_submit'), 0) \
             FROM captured_event WHERE session_id = ?",
            &[Value::Int(synthesized), Value::text(session)],
        )
        .await
        .ok()?;
    let row = rows.first()?;
    Some(Holdings {
        tools: row.get_i64(0).ok()?,
        native_openings: row.get_i64(1).ok()?,
        prompts: row.get_i64(2).ok()?,
    })
}

//! Detector (d): a session whose recorded phase is behind its own events.
//!
//! Every event returns a session to work, so this should never be found; it
//! is the alarm if it ever is, and it is Rift's own defect rather than a thing
//! the developer did. `rift doctor`'s session leg reads it from here, so the
//! leg and every other surface count the same sessions.

use rift_shadow_hooks::session_cache::SessionCache;
use rift_store::{CanonicalStore as _, SqliteStore, Value};

use super::{Finding, Reason};

/// Whether `cache` could be behind its events at all: only a session recorded
/// `ended` or `idle` can have events its phase does not account for.
///
/// Public so a caller can skip opening the projection when no session
/// qualifies.
#[must_use]
pub fn is_behind_candidate(cache: &SessionCache) -> bool {
    matches!(cache.state.as_str(), "ended" | "idle")
}

/// The sessions among `sessions`, in the order given, cached `ended` or
/// `idle` with a captured event other than their own end that arrived after
/// the session's last recorded transition and inside the alarm window.
///
/// The window bound is what keeps a session that died before the phase repair
/// from reading as a fault forever: its late events age out after a day, and
/// a fault that is still happening keeps producing new ones. Read-only over
/// the projection the caller opened.
#[must_use]
pub fn sessions_behind_events(
    projection: &SqliteStore,
    sessions: &[SessionCache],
    now_ms: i64,
) -> Vec<Finding> {
    let candidates: Vec<&SessionCache> = sessions
        .iter()
        .filter(|cache| is_behind_candidate(cache))
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }
    let Some(runtime) = super::runtime() else {
        return Vec::new();
    };
    let since = super::window_start(now_ms);
    let mut findings = Vec::new();
    for cache in candidates {
        let late = runtime.block_on(events_after_last_transition(
            projection,
            &cache.session_id,
            since,
        ));
        if late > 0 {
            findings.push(Finding::new(
                Reason::SessionBehindEvents,
                &cache.agent,
                Some(&cache.session_id),
                format!("{late} events after the last transition"),
            ));
        }
    }
    findings
}

/// How many of `session`'s own events, its end excepted, postdate its last
/// recorded transition and arrived at or after `since`. A session with no
/// transition at all has every such event after it.
async fn events_after_last_transition(store: &SqliteStore, session: &str, since: i64) -> i64 {
    store
        .query_opt_i64(
            "SELECT count(*) FROM captured_event WHERE session_id = ? AND kind != 'session_end' \
             AND occurred_at_ms > (SELECT coalesce(max(occurred_at), 0) \
             FROM session_transition WHERE session_id = ?) \
             AND occurred_at_ms >= ?",
            &[
                Value::text(session),
                Value::text(session),
                Value::Int(since),
            ],
        )
        .await
        .ok()
        .flatten()
        .unwrap_or(0)
}

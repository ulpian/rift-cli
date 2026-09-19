//! The session state machine's drivers: the trigger a canonical event
//! carries, the cached phase token, and the one place a transition is
//! persisted — so an ingest, the idle sweep and the condense driver all
//! advance the same machine through the same append-only row. The transient
//! cache is refreshed here too, because it mirrors the phase that row holds.

use std::path::Path;

use rift_core::{AgentId, Result, Trust};
use rift_shadow_hooks::session_cache::{self, SessionCache};
use rift_shadow_hooks::{
    step, CanonicalEvent, Effect, EventKind, SessionState, Transition, Trigger,
};
use rift_store::{record_session_transition, CanonicalStore, SessionTransitionRow, Value};

use super::turns::TurnStamp;
use super::IngestCtx;
use crate::sha256_hex;

/// Map a canonical event to the state-machine trigger it drives, if any. Turn /
/// tool / file / prompt events keep the session ACTIVE (no transition).
#[must_use]
pub fn trigger_for(kind: EventKind) -> Option<Trigger> {
    match kind {
        EventKind::SessionStart => Some(Trigger::Start),
        EventKind::SessionCompaction => Some(Trigger::Compaction),
        EventKind::SessionEnd => Some(Trigger::End),
        _ => None,
    }
}

/// Drive the `Ending → Ended` transition after a condense completes, refreshing
/// the cache. Convenience for the executor/bin.
///
/// # Errors
/// [`rift_core::RiftError`] on a storage failure.
pub async fn mark_condense_done(
    store: &impl CanonicalStore,
    git_dir: &Path,
    session: &str,
    agent_label: &str,
    now_ms: i64,
) -> Result<SessionState> {
    let from = session_cache::read_cache(git_dir, session)?
        .and_then(|c| state_from_token(&c.state))
        .unwrap_or(SessionState::Ending);
    let (state, _fx) = apply_trigger(store, session, from, Trigger::CondenseDone, now_ms).await?;
    if let Some(mut cache) = session_cache::read_cache(git_dir, session)? {
        cache.state = String::from(state.as_str());
        cache.updated_at_ms = now_ms;
        session_cache::write_cache(git_dir, &cache)?;
    }
    let _ = agent_label;
    Ok(state)
}

/// Drive [`Trigger::IdleTimeout`] for one session: `Active → Ending` with a
/// `RequestCondense` effect, recorded in the append-only `session_transition`
/// relation and mirrored into the cache.
///
/// This is the crashed-session driver. Until now `IdleTimeout` was a legal
/// transition that nothing in the product ever produced — the machine could
/// describe an abandoned session but had no way to reach it, so a session whose
/// agent died stayed `active` in the cache forever, was never condensed, and
/// kept being offered to every subsequent archive as the live session to
/// attribute against. The caller decides *which* sessions are stale (see
/// `session_cache::is_idle_past`) and runs the condense the returned effects
/// ask for; this function owns only the transition.
///
/// An illegal transition — a session already `ending` or `ended` — is swallowed
/// by `apply_trigger`, so a repeated sweep is a no-op rather than a second
/// condense.
///
/// Only the phase is written back. `updated_at_ms` keeps meaning *when the
/// agent last touched this session*, which is why it survives: see the comment
/// at the write.
///
/// # Errors
/// [`RiftError`](rift_core::RiftError) if the transition cannot be recorded or
/// the cache cannot be rewritten.
pub async fn timeout_idle_session(
    store: &impl CanonicalStore,
    git_dir: &Path,
    session: &str,
    now_ms: i64,
) -> Result<(SessionState, Vec<Effect>)> {
    let from = session_cache::read_cache(git_dir, session)?
        .and_then(|c| state_from_token(&c.state))
        .unwrap_or(SessionState::Idle);
    let (state, effects) =
        apply_trigger(store, session, from, Trigger::IdleTimeout, now_ms).await?;
    if state != from {
        if let Some(mut cache) = session_cache::read_cache(git_dir, session)? {
            // The phase moves; the last-touch stamp does NOT. `updated_at_ms`
            // answers "when did the agent last touch this session", and it is
            // read twice: it is the input to the idleness decision, and it is
            // the key `live_sessions` sorts by — and `live_sessions` counts
            // `ending` as live. Stamping it here would make the session we
            // just declared DEAD the most recently touched one in the repo, so
            // the next post-commit, commit-msg trailer or PR archive would take
            // the dead session as its attribution target and write that
            // durably. The transition is already recorded, append-only, in
            // `session_transition`; the cache only mirrors the phase.
            // Idempotence does not depend on the stamp either — `Ending` has no
            // `IdleTimeout` edge, so a second sweep is refused by the machine.
            // Leaving the stamp stale is also what lets a later sweep see that
            // this session is still idle and retry a condense that never landed.
            cache.state = String::from(state.as_str());
            session_cache::write_cache(git_dir, &cache)?;
        }
    }
    Ok((state, effects))
}

/// The session start an adapter without a start contract cannot send.
///
/// Antigravity exposes no explicit SessionStart contract. The first known
/// hook for a conversation therefore gains one deterministic synthetic
/// start. Canonical history is queried rather than the transient cache
/// (which may be deleted), and MAX is reserved as its ordinal so
/// retransmitting the native hook keeps every native event's idempotency key
/// unchanged.
pub(super) async fn synthesize_missing_start(
    store: &impl CanonicalStore,
    ctx: &IngestCtx<'_>,
    events: &mut Vec<CanonicalEvent>,
) -> Result<()> {
    let has_start = events
        .iter()
        .any(|event| event.kind == EventKind::SessionStart);
    if ctx.raw.agent != AgentId::ANTIGRAVITY || events.is_empty() || has_start {
        return Ok(());
    }
    let session_row = crate::ids::session_row_id(ctx.owner, &ctx.raw.session);
    let already_started = !store
        .query(
            "SELECT 1 FROM session_event WHERE session_id = ? AND kind = 'start' LIMIT 1",
            &[Value::text(session_row)],
        )
        .await?
        .is_empty();
    if already_started {
        return Ok(());
    }
    let Some(mut start) = events.first().cloned() else {
        return Ok(());
    };
    start.kind = EventKind::SessionStart;
    start.trust = Trust::GitFact;
    start.event_ordinal = u32::MAX;
    events.insert(0, start);
    Ok(())
}

/// Advance the machine one trigger per state-changing event, from the cached
/// phase (Idle if no cache yet), collecting the effects for the caller's
/// executor.
pub(super) async fn advance(
    store: &impl CanonicalStore,
    ctx: &IngestCtx<'_>,
    existing: Option<&SessionCache>,
    events: &[CanonicalEvent],
) -> Result<(SessionState, Vec<Effect>)> {
    let session = &ctx.raw.session;
    let mut state = existing
        .and_then(|c| state_from_token(&c.state))
        .unwrap_or(SessionState::Idle);
    let mut effects = Vec::new();
    for ev in events {
        let trig = trigger_for(ev.kind);
        // A non-compaction event arriving mid-compaction means compaction is done.
        if state == SessionState::Compacting && trig != Some(Trigger::Compaction) {
            let (next, fx) =
                apply_trigger(store, session, state, Trigger::CompactionDone, ctx.now_ms).await?;
            state = next;
            effects.extend(fx);
        }
        if let Some(trigger) = trig {
            let (next, fx) = apply_trigger(store, session, state, trigger, ctx.now_ms).await?;
            state = next;
            effects.extend(fx);
        }
    }
    Ok((state, effects))
}

/// Refresh the transient cache (structural only; last-write-wins).
pub(super) fn refresh_cache(
    ctx: &IngestCtx<'_>,
    existing: Option<SessionCache>,
    state: SessionState,
    stamp: &TurnStamp,
) -> Result<()> {
    let mut cache = existing.unwrap_or_else(|| {
        SessionCache::new(
            ctx.raw.session.clone(),
            ctx.owner,
            state.as_str(),
            ctx.now_ms,
        )
    });
    cache.state = String::from(state.as_str());
    cache.agent = String::from(ctx.owner);
    cache.current_turn_ordinal = stamp.ordinal;
    cache.turn_open = stamp.open;
    cache.updated_at_ms = ctx.now_ms;
    // The transcript's ADDRESS, when this hook's payload names one — the
    // post-commit archive has no payload and reads it from here. Last
    // report wins (agents relocate transcripts across resumes); a hook without
    // the field keeps the previous address rather than forgetting it.
    let named = ctx
        .raw
        .payload
        .get("transcript_path")
        .and_then(|v| v.as_str());
    match named {
        Some(path) if !path.is_empty() => cache.transcript_path = Some(path.to_owned()),
        _ => (),
    }
    session_cache::write_cache(ctx.git_dir, &cache)
}

/// Parse a cached state token back into a [`SessionState`].
pub(super) fn state_from_token(s: &str) -> Option<SessionState> {
    Some(match s {
        "idle" => SessionState::Idle,
        "active" => SessionState::Active,
        "compacting" => SessionState::Compacting,
        "ending" => SessionState::Ending,
        "ended" => SessionState::Ended,
        _ => return None,
    })
}

/// Apply one trigger to the state machine, persisting the transition on success.
/// An illegal transition returns the unchanged state with no effects (swallowed).
pub(super) async fn apply_trigger(
    store: &impl CanonicalStore,
    session: &str,
    from: SessionState,
    trigger: Trigger,
    now_ms: i64,
) -> Result<(SessionState, Vec<Effect>)> {
    let Ok(Transition { next, effects }) = step(from, trigger) else {
        return Ok((from, Vec::new()));
    };
    let row = SessionTransitionRow {
        transition_id: sha256_hex(&[
            session.as_bytes(),
            b"\0",
            from.as_str().as_bytes(),
            b"\0",
            next.as_str().as_bytes(),
            b"\0",
            trigger.as_str().as_bytes(),
            b"\0",
            now_ms.to_string().as_bytes(),
        ]),
        session_id: session.to_owned(),
        from_state: from.as_str().to_owned(),
        to_state: next.as_str().to_owned(),
        trigger_kind: trigger.as_str().to_owned(),
        occurred_at: now_ms,
        schema_version: 1,
    };
    record_session_transition(store, &row).await?;
    Ok((next, effects))
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::store;
    use super::*;
    use rift_shadow_hooks::session_cache::SessionCache;

    #[test]
    fn trigger_mapping() {
        assert_eq!(trigger_for(EventKind::SessionStart), Some(Trigger::Start));
        assert_eq!(trigger_for(EventKind::SessionEnd), Some(Trigger::End));
        assert_eq!(trigger_for(EventKind::ToolCall), None);
    }

    /// The "idle timeout" trigger the machine has always
    /// accepted and nothing ever produced. A session left `active` by a dead
    /// agent is finalized on demand — one recorded transition, the condense
    /// request the machine owes, and a cache that agrees.
    #[tokio::test]
    async fn an_idle_session_is_finalized_and_the_sweep_is_idempotent() {
        let s = store();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        session_cache::write_cache(
            git_dir,
            &SessionCache::new("01JSESSION", "claude-code", "active", 1_000),
        )
        .unwrap();

        let (state, effects) = timeout_idle_session(&s, git_dir, "01JSESSION", 9_000)
            .await
            .unwrap();
        assert_eq!(state, SessionState::Ending);
        assert!(effects.contains(&Effect::RequestCondense));
        assert_eq!(s.count("session_transition").await.unwrap(), 1);
        let cache = session_cache::read_cache(git_dir, "01JSESSION")
            .unwrap()
            .unwrap();
        assert_eq!(cache.state, "ending");
        // The last-touch stamp is the agent's, not the sweep's: it stays at the
        // moment the dead agent really did touch this session. Bumping it here
        // would both hide the session from the next sweep's idle check and — via
        // `live_sessions`, which sorts most-recently-updated first and counts
        // `ending` as live — promote a dead session to the repo's archive target.
        assert_eq!(cache.updated_at_ms, 1_000);

        // A second sweep before the condense lands must not re-request one:
        // `Ending` has no `IdleTimeout` edge, so the illegal transition is
        // swallowed and nothing new is recorded.
        let (again, effects) = timeout_idle_session(&s, git_dir, "01JSESSION", 10_000)
            .await
            .unwrap();
        assert_eq!(again, SessionState::Ending);
        assert!(effects.is_empty());
        assert_eq!(s.count("session_transition").await.unwrap(), 1);

        // And it closes normally from there.
        let ended = mark_condense_done(&s, git_dir, "01JSESSION", "claude-code", 11_000)
            .await
            .unwrap();
        assert_eq!(ended, SessionState::Ended);
    }

    /// The regression the first cut of the sweep shipped, pinned at its source.
    ///
    /// `live_sessions` returns the repo's live sessions most-recently-updated
    /// first — and it counts `ending` as live — so its first element is the
    /// session every later archive attributes against. Stamping the cache on an
    /// idle timeout therefore made the session the sweep had just declared DEAD
    /// the top of that list, and the next post-commit wrote a checkpoint against
    /// a crashed session while the developer's real one sat behind it.
    #[tokio::test]
    async fn timing_out_a_dead_session_never_makes_it_the_repo_archive_target() {
        let s = store();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        session_cache::write_cache(
            git_dir,
            &SessionCache::new("01JDEAD", "claude-code", "active", 1_000),
        )
        .unwrap();
        session_cache::write_cache(
            git_dir,
            &SessionCache::new("01JLIVE", "claude-code", "active", 5_000),
        )
        .unwrap();

        let (state, _) = timeout_idle_session(&s, git_dir, "01JDEAD", 9_000)
            .await
            .unwrap();
        assert_eq!(state, SessionState::Ending);

        let live = session_cache::live_sessions(git_dir).unwrap();
        let order: Vec<&str> = live.iter().map(|c| c.session_id.as_str()).collect();
        assert_eq!(
            order,
            vec!["01JLIVE", "01JDEAD"],
            "the developer's live session stays the archive target"
        );
        // And the dead one is still visibly idle, so a later sweep can retry the
        // condense this transition asked for.
        let dead = session_cache::read_cache(git_dir, "01JDEAD")
            .unwrap()
            .unwrap();
        assert!(session_cache::is_idle_past(&dead, 9_000, 1_000));
    }

    /// A session that has *never* been seen (no cache file) is not resurrected
    /// into a lifecycle by a sweep: `Idle` has no `IdleTimeout` edge.
    #[tokio::test]
    async fn a_session_with_no_cache_is_not_invented_by_the_sweep() {
        let s = store();
        let tmp = tempfile::tempdir().unwrap();
        let (state, effects) = timeout_idle_session(&s, tmp.path(), "01JGHOST", 9_000)
            .await
            .unwrap();
        assert_eq!(state, SessionState::Idle);
        assert!(effects.is_empty());
        assert_eq!(s.count("session_transition").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn compaction_round_trip_and_illegal_is_swallowed() {
        let s = store();
        let (st1, _) = apply_trigger(&s, "sess", SessionState::Active, Trigger::Compaction, 1)
            .await
            .unwrap();
        assert_eq!(st1, SessionState::Compacting);
        let (st2, _) = apply_trigger(
            &s,
            "sess",
            SessionState::Compacting,
            Trigger::CompactionDone,
            2,
        )
        .await
        .unwrap();
        assert_eq!(st2, SessionState::Active);
        // an illegal transition is swallowed: state unchanged, no row, no effects
        let (st3, fx3) = apply_trigger(&s, "sess", SessionState::Idle, Trigger::End, 3)
            .await
            .unwrap();
        assert_eq!(st3, SessionState::Idle);
        assert!(fx3.is_empty());
        assert_eq!(
            s.count("session_transition").await.unwrap(),
            2,
            "illegal transition records nothing"
        );
    }
}

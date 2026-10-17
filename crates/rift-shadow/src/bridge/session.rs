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

/// The `trigger_kind` of a transition that opened a session no `SessionStart`
/// opened.
pub const SYNTHESIZED_START: &str = "synthesized-start";

/// What [`advance`] left: the phase, the effects owed, and the triggers the
/// machine refused.
#[derive(Debug)]
pub(super) struct Advanced {
    pub(super) state: SessionState,
    pub(super) effects: Vec<Effect>,
    pub(super) refused: Vec<Trigger>,
}

/// Map a canonical event to the lifecycle trigger its hook names, if any.
///
/// Turn, tool, file and prompt events name none; what they drive depends on
/// the phase they arrive in, which `plan_triggers` decides.
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
/// A session already `ending` or `ended`, or never opened, is not asked at
/// all, so a repeated sweep is a no-op rather than a second condense, and it
/// leaves no refusal row behind.
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
    // Only a working phase can time out. Asking from any other would be
    // refused, and a refusal is recorded; a sweep that re-finds a session it
    // already moved to `ending` owes nothing new and records nothing.
    if !matches!(from, SessionState::Active | SessionState::Compacting) {
        return Ok((from, Vec::new()));
    }
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

/// The canonical session start an adapter without a start contract cannot
/// send.
///
/// Antigravity exposes no explicit SessionStart contract. The first known
/// hook for a conversation therefore gains one deterministic synthetic
/// start event. Canonical history is queried rather than the transient cache
/// (which may be deleted), and MAX is reserved as its ordinal so
/// retransmitting the native hook keeps every native event's idempotency key
/// unchanged. The check and the write agree: both address the owner's
/// session row and the `start` kind, and a start row exists for every
/// native start too (Antigravity's zeroth invocation of each prompt maps to
/// one), which is why a session carries one start row per prompt.
///
/// This is the only agent whose start is written into canonical history.
/// For every other agent a missing start is not invented as an event: the
/// state machine opens the session on its first captured activity and the
/// transition row says the start was synthesized (see [`plan_triggers`]), so
/// the event stream holds only what the agent sent.
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

/// Advance the machine through every trigger each event drives, from the
/// cached phase (Idle if no cache yet), collecting the effects for the
/// caller's executor.
///
/// The steps come from [`plan_triggers`], so after any event but a
/// `SessionEnd` the phase is `active` or `compacting`, whatever it was
/// before. A trigger the machine still refuses is recorded as a refusal and
/// returned for the caller to log.
pub(super) async fn advance(
    store: &impl CanonicalStore,
    ctx: &IngestCtx<'_>,
    existing: Option<&SessionCache>,
    events: &[CanonicalEvent],
) -> Result<Advanced> {
    let session = &ctx.raw.session;
    let mut advanced = Advanced {
        state: existing
            .and_then(|c| state_from_token(&c.state))
            .unwrap_or(SessionState::Idle),
        effects: Vec::new(),
        refused: Vec::new(),
    };
    for ev in events {
        // Antigravity's canonical start (ordinal MAX) was synthesized, not sent.
        let synthesized = ev.kind == EventKind::SessionStart && ev.event_ordinal == u32::MAX;
        for trigger in plan_triggers(advanced.state, ev.kind) {
            let from = advanced.state;
            let kind = audit_kind(from, trigger, synthesized);
            let (next, fx) = apply_as(store, session, from, trigger, kind, ctx.now_ms).await?;
            // The machine has no self-loop, so a phase that did not move is a
            // trigger it refused.
            if next == from {
                advanced.refused.push(trigger);
            }
            advanced.state = next;
            advanced.effects.extend(fx);
        }
    }
    Ok(advanced)
}

/// The triggers one event drives from `state`, in order.
///
/// A `Compacting` session that sees anything but another compaction has
/// finished compacting. A session that is not working (`idle`, `ending`,
/// `ended`) is returned to work first, by the event's own `Start` or by
/// `Activity`, and then the event's lifecycle trigger applies: a `SessionEnd`
/// to a session never opened opens and ends it in one pass. A `Start` for a
/// session already working, a compaction for one already compacting, and an
/// end for one already ending or ended change nothing and ask the machine
/// nothing: the last is a redelivered `SessionEnd`, whose condense is owed or
/// done already, and re-opening the session for it would ask for a second.
/// Every pair this returns is legal, which the liveness property test holds
/// it to.
#[must_use]
pub(super) fn plan_triggers(state: SessionState, kind: EventKind) -> Vec<Trigger> {
    let hook = trigger_for(kind);
    let mut plan = Vec::new();
    let mut at = state;
    if at == SessionState::Compacting && hook != Some(Trigger::Compaction) {
        plan.push(Trigger::CompactionDone);
        at = SessionState::Active;
    }
    let working = matches!(at, SessionState::Active | SessionState::Compacting);
    let already = match hook {
        Some(Trigger::Start) => working,
        Some(Trigger::Compaction) => at == SessionState::Compacting,
        Some(Trigger::End) => matches!(at, SessionState::Ending | SessionState::Ended),
        _ => false,
    };
    if already {
        return plan;
    }
    if !working && hook != Some(Trigger::Start) {
        plan.push(Trigger::Activity);
    }
    plan.extend(hook);
    plan
}

/// The token a transition row is filed under.
///
/// A session opened without a `SessionStart` from its agent is filed as
/// `synthesized-start`, never as `start` or `activity`: the machine opened it
/// exactly as a start would, and the audit says no start arrived.
fn audit_kind(from: SessionState, trigger: Trigger, synthesized: bool) -> &'static str {
    match (from, trigger) {
        (SessionState::Idle, Trigger::Activity) => SYNTHESIZED_START,
        (_, Trigger::Start) if synthesized => SYNTHESIZED_START,
        _ => trigger.as_str(),
    }
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
    if let Some(name) = &stamp.native_turn {
        cache.native_turn = Some(name.clone());
    }
    cache.updated_at_ms = ctx.now_ms;
    // The transcript's ADDRESS, when this hook's payload names one — the
    // post-commit archive has no payload and reads it from here. Last
    // report wins (agents relocate transcripts across resumes); a hook without
    // the field keeps the previous address rather than forgetting it. Only the
    // address is copied: the hook path never opens the file it names, and the
    // archive, off the decision path, is what reads and seals it.
    if let Some(path) = named_transcript_path(&ctx.raw.payload) {
        cache.transcript_path = Some(path.to_owned());
    }
    session_cache::write_cache(ctx.git_dir, &cache)
}

/// The non-empty transcript address a hook payload names, under either
/// spelling: Claude Code's `transcript_path`, else Antigravity's
/// `transcriptPath`. Snake case wins when a payload carries both, because
/// only Antigravity sends the camelCase key and it sends nothing else.
fn named_transcript_path(payload: &serde_json::Value) -> Option<&str> {
    ["transcript_path", "transcriptPath"]
        .iter()
        .find_map(|key| payload.get(key).and_then(serde_json::Value::as_str))
        .filter(|path| !path.is_empty())
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

/// Apply one trigger to the state machine and persist what happened: the
/// transition, or the refusal.
pub(super) async fn apply_trigger(
    store: &impl CanonicalStore,
    session: &str,
    from: SessionState,
    trigger: Trigger,
    now_ms: i64,
) -> Result<(SessionState, Vec<Effect>)> {
    apply_as(store, session, from, trigger, trigger.as_str(), now_ms).await
}

/// [`apply_trigger`], filing the row under `kind`.
///
/// **A refused trigger leaves a row.** It used to be swallowed with nothing
/// written, after the hook's receipt had already been recorded as captured,
/// so a refusal that hid a working session for days left no trace anywhere.
/// Now it is a row of its own, `refused-<trigger>`, whose from and to phases
/// are the phase that refused it: appended like every transition, never an
/// update, and the caller still carries on unchanged.
async fn apply_as(
    store: &impl CanonicalStore,
    session: &str,
    from: SessionState,
    trigger: Trigger,
    kind: &str,
    now_ms: i64,
) -> Result<(SessionState, Vec<Effect>)> {
    let (next, effects, kind) = match step(from, trigger) {
        Ok(Transition { next, effects }) => (next, effects, kind.to_owned()),
        Err(_) => (from, Vec::new(), format!("refused-{}", trigger.as_str())),
    };
    let row = SessionTransitionRow {
        transition_id: sha256_hex(&[
            session.as_bytes(),
            b"\0",
            from.as_str().as_bytes(),
            b"\0",
            next.as_str().as_bytes(),
            b"\0",
            kind.as_bytes(),
            b"\0",
            now_ms.to_string().as_bytes(),
        ]),
        session_id: session.to_owned(),
        from_state: from.as_str().to_owned(),
        to_state: next.as_str().to_owned(),
        trigger_kind: kind,
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
    async fn compaction_round_trips_and_an_illegal_trigger_leaves_a_refusal_row() {
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
        // An illegal transition changes nothing and asks for nothing, and it
        // is never silent: it is a row of its own, appended like any other.
        let (st3, fx3) = apply_trigger(&s, "sess", SessionState::Idle, Trigger::End, 3)
            .await
            .unwrap();
        assert_eq!(st3, SessionState::Idle);
        assert!(fx3.is_empty());
        let rows = s
            .query(
                "SELECT from_state, to_state FROM session_transition \
                 WHERE trigger_kind = 'refused-end'",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "the refusal is recorded");
        assert_eq!(s.count("session_transition").await.unwrap(), 3);
    }

    /// Every state meets every event kind, and the plan is always legal and
    /// always leaves the session working, unless the event was the session's
    /// own end, which leaves it `ending` with a condense owed (or, redelivered
    /// to a session already ending or ended, where it was).
    #[test]
    fn every_planned_step_is_legal_and_leaves_the_session_working() {
        use SessionState::{Active, Compacting, Ended, Ending, Idle};
        for state in [Idle, Active, Compacting, Ending, Ended] {
            for kind in EventKind::ALL {
                let mut at = state;
                let mut effects = Vec::new();
                for trigger in plan_triggers(state, *kind) {
                    let tr = step(at, trigger).unwrap_or_else(|e| {
                        panic!("{state:?} + {kind:?}: {trigger:?} refused: {e}")
                    });
                    at = tr.next;
                    effects.extend(tr.effects);
                }
                if *kind == EventKind::SessionEnd && matches!(state, Ending | Ended) {
                    assert_eq!(at, state, "a redelivered end changes nothing");
                    assert!(effects.is_empty());
                } else if *kind == EventKind::SessionEnd {
                    assert_eq!(at, Ending, "{state:?} + {kind:?}");
                    assert!(effects.contains(&Effect::RequestCondense));
                } else {
                    assert!(matches!(at, Active | Compacting), "{state:?} + {kind:?}");
                }
            }
        }
    }

    /// A session opened by activity is filed as a synthesized start, a real
    /// start as a start, and a return from `ended` as activity.
    #[test]
    fn the_audit_names_a_synthesized_start_as_such() {
        use SessionState::{Ended, Idle};
        assert_eq!(
            audit_kind(Idle, Trigger::Activity, false),
            SYNTHESIZED_START
        );
        assert_eq!(audit_kind(Idle, Trigger::Start, true), SYNTHESIZED_START);
        assert_eq!(audit_kind(Idle, Trigger::Start, false), "start");
        assert_eq!(audit_kind(Ended, Trigger::Activity, false), "activity");
    }
}

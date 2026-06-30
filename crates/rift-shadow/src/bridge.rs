//! P3 S2/S3 — the bridge from agent hooks to the durable session lifecycle.
//!
//! [`ingest_hook`] takes one raw agent hook, dispatches it to canonical events,
//! records the append-only hook receipt, advances the **pure** session state
//! machine ([`step`](rift_shadow_hooks::step)) one transition per state-changing
//! event, persists each transition as an append-only `session_transition` row
//! (the source of truth), and refreshes the out-of-tree transient cache. Effects
//! (`RequestCondense`, …) are returned for the caller's executor to run. Illegal
//! transitions are swallowed (the caller logs them) — never panic, never fail the
//! agent.

use std::path::Path;

use rift_core::Result;
use rift_shadow_hooks::session_cache::{self, SessionCache};
use rift_shadow_hooks::{
    dispatch, idempotency_key, step, AdapterRegistry, Effect, EventKind, RawHook, SessionState,
    Transition, Trigger,
};
use rift_store::{
    record_agent_hook_event, record_captured_event, record_session_transition, AgentHookEventRow,
    CanonicalStore, CapturedEventRow, SessionTransitionRow,
};

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

/// Parse a cached state token back into a [`SessionState`].
fn state_from_token(s: &str) -> Option<SessionState> {
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
async fn apply_trigger(
    store: &impl CanonicalStore,
    session: &str,
    from: SessionState,
    trigger: Trigger,
    now_ms: i64,
) -> Result<(SessionState, Vec<Effect>)> {
    match step(from, trigger) {
        Ok(Transition { next, effects }) => {
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
        Err(_) => Ok((from, Vec::new())),
    }
}

/// Ingest one raw agent hook end-to-end. Returns the resulting session phase and
/// any effects the caller's executor should run (e.g. condense).
///
/// # Errors
/// [`rift_core::RiftError`] only on a storage failure or a malformed payload that
/// cannot be content-addressed; the caller swallows it at the hook boundary.
pub async fn ingest_hook(
    raw: &RawHook,
    registry: &AdapterRegistry,
    store: &impl CanonicalStore,
    git_dir: &Path,
    agent_label: &str,
    now_ms: i64,
) -> Result<(SessionState, Vec<Effect>)> {
    let events = dispatch(raw, registry)?;

    // Append-only hook receipt (idempotent on its content address).
    let payload_hash = sha256_hex(&[serde_json::to_vec(&raw.payload)
        .unwrap_or_default()
        .as_slice()]);
    let outcome = if events.is_empty() {
        "skipped-unknown-verb"
    } else {
        "recorded"
    };
    record_agent_hook_event(
        store,
        &AgentHookEventRow {
            hook_event_id: sha256_hex(&[
                agent_label.as_bytes(),
                b"\0",
                raw.verb.as_bytes(),
                b"\0",
                raw.session.as_bytes(),
                b"\0",
                raw.turn.to_string().as_bytes(),
                b"\0",
                payload_hash.as_bytes(),
            ]),
            agent: agent_label.to_owned(),
            verb: raw.verb.clone(),
            session_id: Some(raw.session.clone()),
            received_at: now_ms,
            outcome: outcome.to_owned(),
            schema_version: 1,
        },
    )
    .await?;

    // Per-canonical-event idempotency index (P8 S2): one append-only row per
    // *logical* event, keyed by a content/identity-derived key. A duplicate
    // delivery of the same hook re-derives the same keys and no-ops here, so the
    // chain never gets a doubled leaf and cost/attribution are never double-counted.
    for ev in &events {
        let key = idempotency_key(
            &raw.agent,
            &raw.session,
            &raw.verb,
            ev.turn_ordinal,
            ev.event_ordinal,
            &ev.payload_sha256,
        );
        record_captured_event(
            store,
            &CapturedEventRow {
                idempotency_key: key.to_hex(),
                session_id: ev.session.clone(),
                turn_ordinal: i64::from(ev.turn_ordinal),
                event_ordinal: i64::from(ev.event_ordinal),
                kind: ev.kind.as_str().to_owned(),
                trust: ev.trust.as_str().to_owned(),
                payload_sha256: ev.payload_sha256.to_hex(),
                occurred_at_ms: ev.occurred_at_ms,
                schema_version: i64::from(ev.schema_version),
            },
        )
        .await?;
    }

    // Current phase (Idle if no cache yet).
    let existing = session_cache::read_cache(git_dir, &raw.session)?;
    let mut state = existing
        .as_ref()
        .and_then(|c| state_from_token(&c.state))
        .unwrap_or(SessionState::Idle);

    let mut effects = Vec::new();
    for ev in &events {
        let trig = trigger_for(ev.kind);
        // A non-compaction event arriving mid-compaction means compaction is done.
        if state == SessionState::Compacting && trig != Some(Trigger::Compaction) {
            let (next, fx) =
                apply_trigger(store, &raw.session, state, Trigger::CompactionDone, now_ms).await?;
            state = next;
            effects.extend(fx);
        }
        if let Some(trigger) = trig {
            let (next, fx) = apply_trigger(store, &raw.session, state, trigger, now_ms).await?;
            state = next;
            effects.extend(fx);
        }
    }

    // Refresh the transient cache (structural only; last-write-wins).
    let mut cache = existing.unwrap_or_else(|| {
        SessionCache::new(raw.session.clone(), agent_label, state.as_str(), now_ms)
    });
    cache.state = String::from(state.as_str());
    cache.agent = String::from(agent_label);
    cache.current_turn_ordinal = raw.turn;
    cache.updated_at_ms = now_ms;
    session_cache::write_cache(git_dir, &cache)?;

    Ok((state, effects))
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

#[cfg(test)]
mod tests {
    use super::*;
    use rift_shadow_hooks::AgentId;
    use rift_store::SqliteStore;

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    fn raw(verb: &str, turn: u32) -> RawHook {
        RawHook {
            agent: AgentId::CLAUDE_CODE,
            verb: verb.into(),
            session: "01JSESSION".into(),
            turn,
            occurred_at_ms: 1,
            payload: serde_json::json!({"tool_name": "Bash"}),
        }
    }

    #[test]
    fn trigger_mapping() {
        assert_eq!(trigger_for(EventKind::SessionStart), Some(Trigger::Start));
        assert_eq!(trigger_for(EventKind::SessionEnd), Some(Trigger::End));
        assert_eq!(trigger_for(EventKind::ToolCall), None);
    }

    #[tokio::test]
    async fn session_start_then_end_persists_transitions_and_cache() {
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();

        let (st1, _) = ingest_hook(
            &raw("SessionStart", 0),
            &reg,
            &s,
            git_dir,
            "claude-code",
            10,
        )
        .await
        .unwrap();
        assert_eq!(st1, SessionState::Active);

        // a tool event keeps ACTIVE (no transition) but records the hook receipt
        let (st2, fx2) = ingest_hook(&raw("PreToolUse", 1), &reg, &s, git_dir, "claude-code", 11)
            .await
            .unwrap();
        assert_eq!(st2, SessionState::Active);
        assert!(fx2.is_empty());

        // `SessionEnd` (not `Stop`, which is a turn boundary) drives the End transition.
        let (st3, fx3) = ingest_hook(&raw("SessionEnd", 2), &reg, &s, git_dir, "claude-code", 12)
            .await
            .unwrap();
        assert_eq!(st3, SessionState::Ending);
        assert!(fx3.contains(&Effect::RequestCondense));

        // two real transitions recorded (Idle→Active, Active→Ending)
        assert_eq!(s.count("session_transition").await.unwrap(), 2);
        // hook receipts recorded for all three invocations
        assert_eq!(s.count("agent_hook_event").await.unwrap(), 3);
        // cache reflects the latest phase
        let cache = session_cache::read_cache(git_dir, "01JSESSION")
            .unwrap()
            .unwrap();
        assert_eq!(cache.state, "ending");
        assert_eq!(cache.current_turn_ordinal, 2);

        // condense completes → Ended
        let ended = mark_condense_done(&s, git_dir, "01JSESSION", "claude-code", 13)
            .await
            .unwrap();
        assert_eq!(ended, SessionState::Ended);
        assert_eq!(s.count("session_transition").await.unwrap(), 3);
    }

    #[tokio::test]
    async fn duplicate_hook_delivery_captures_each_event_once() {
        // P8 S2: re-delivering the same hook is a true no-op — the per-event
        // idempotency index dedups, so the chain never gets a doubled leaf.
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();

        // PreToolUse(Edit) fans out to two canonical events (ToolCall + FileEdit).
        let mut edit = raw("PreToolUse", 1);
        edit.payload = serde_json::json!({"tool_name": "Edit"});

        ingest_hook(&edit, &reg, &s, git_dir, "claude-code", 10)
            .await
            .unwrap();
        assert_eq!(s.count("captured_event").await.unwrap(), 2);

        // Re-deliver the identical hook: same idempotency keys → no new rows.
        ingest_hook(&edit, &reg, &s, git_dir, "claude-code", 99)
            .await
            .unwrap();
        assert_eq!(
            s.count("captured_event").await.unwrap(),
            2,
            "duplicate delivery is a no-op"
        );

        // A distinct turn is a distinct logical event → new rows.
        let mut edit2 = edit.clone();
        edit2.turn = 2;
        ingest_hook(&edit2, &reg, &s, git_dir, "claude-code", 100)
            .await
            .unwrap();
        assert_eq!(s.count("captured_event").await.unwrap(), 4);
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

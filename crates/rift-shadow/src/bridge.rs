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

/// The injected worktree read-hasher (M5-06): a read-shaped payload path in,
/// the resolved repo-relative path + content identity out (`None` when the
/// path is outside the repo, vanished, or unsafe). Injected so the ingest
/// layer never touches the filesystem itself.
pub type ReadHasher<'a> = &'a dyn Fn(&str) -> Option<crate::project::ReadContent>;

/// What one hook ingest produced: the session phase, the effects the caller's
/// executor should run, and the turn boundaries this hook closed (the caller
/// runs the working-tree snapshotter for each — it needs the worktree, which
/// the ingest layer deliberately does not touch).
#[derive(Debug)]
pub struct IngestOutcome {
    /// The session phase after the state machine advanced.
    pub state: SessionState,
    /// Effects for the caller's executor (e.g. `RequestCondense`).
    pub effects: Vec<Effect>,
    /// Turn boundaries closed by this hook: `(turn_ordinal, boundary)`.
    pub boundaries: Vec<(u32, crate::turn_boundary::Boundary)>,
}

/// Ingest one raw agent hook end-to-end: the append-only receipt, the per-event
/// idempotency index, the **normalized projection** (session / turn / model-call
/// / tool-call / token-usage / cost — the same projector the artifact bridge
/// replays, ADR-0002), and the session state machine. Returns the resulting
/// session phase, effects, and any turn boundaries the caller must snapshot.
///
/// # Errors
/// [`rift_core::RiftError`] only on a storage failure or a malformed payload that
/// cannot be content-addressed; the caller swallows it at the hook boundary.
#[allow(clippy::too_many_lines)] // one linear ingest pass; each write is one appended fact
pub async fn ingest_hook(
    raw: &RawHook,
    registry: &AdapterRegistry,
    store: &impl CanonicalStore,
    git_dir: &Path,
    agent_label: &str,
    repo_id: &str,
    now_ms: i64,
    read_hasher: Option<ReadHasher<'_>>,
) -> Result<IngestOutcome> {
    let mut events = dispatch(raw, registry)?;

    // The current cache, read once: it feeds both turn derivation and the
    // state machine's starting phase.
    let existing = session_cache::read_cache(git_dir, &raw.session)?;

    // The effective turn ordinal (M2-15): an envelope-carried ordinal is
    // trusted verbatim; a wire with none (native agent hooks) derives it from
    // the event stream — a prompt/turn-start on a closed turn opens the next
    // ordinal, a turn-end closes the current one. Every event of this hook is
    // re-stamped with the result.
    let (turn, turn_open) = effective_turn(raw.turn, &events, existing.as_ref());
    for ev in &mut events {
        ev.turn_ordinal = turn;
    }

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
                turn.to_string().as_bytes(),
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

    // Normalized projection (P2 S3): the same pure mapping the artifact bridge
    // replays over re-derived events, so both sources' rows union to a no-op.
    let mut facts = crate::project::payload_facts(&raw.payload);
    // Resolve the read-time content identity through the caller's worktree
    // hasher (M5-06): the ingest layer never touches the filesystem itself,
    // and an unresolvable path (outside the repo, vanished, unsafe) simply
    // records no read fact.
    if let (Some(path), Some(hasher)) = (&facts.read_path, read_hasher) {
        facts.read_content = hasher(path);
    }
    let facts = facts;
    let ctx = crate::project::ProjectionCtx {
        agent: agent_label,
        repo_id,
    };
    for ev in &events {
        crate::project::project_event(store, ev, &facts, ctx).await?;
    }
    // Provider-reported token usage (never fabricated): attaches to the LAST
    // event of this hook's fan-out — the result/turn-end fact that carried it.
    if let Some(usage) = registry
        .get(&raw.agent)
        .and_then(|a| a.usage(&raw.verb, &raw.payload))
    {
        if let Some(anchor) = events.last() {
            crate::project::project_usage(store, anchor, &facts, ctx, &usage, now_ms).await?;
        }
    }
    if let Some(usage) = registry
        .get(&raw.agent)
        .and_then(|a| a.main_context_usage(&raw.verb, &raw.payload))
    {
        if let Some(anchor) = events.last() {
            crate::project::project_main_context(store, anchor, &raw.verb, &facts, ctx, &usage)
                .await?;
        }
    }

    // Current phase (Idle if no cache yet).
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
    cache.current_turn_ordinal = turn;
    cache.turn_open = turn_open;
    cache.updated_at_ms = now_ms;
    session_cache::write_cache(git_dir, &cache)?;

    // The turn boundaries this hook closed (P4 S2): a prompt submission /
    // turn-start closes the human's H_i window; a turn-end closes the agent's
    // A_i window. Deduped per (ordinal, boundary).
    let mut boundaries: Vec<(u32, crate::turn_boundary::Boundary)> = Vec::new();
    for ev in &events {
        let b = match ev.kind {
            EventKind::TurnStart | EventKind::PromptSubmit => {
                Some(crate::turn_boundary::Boundary::Start)
            }
            EventKind::TurnEnd => Some(crate::turn_boundary::Boundary::End),
            _ => None,
        };
        if let Some(b) = b {
            if !boundaries.contains(&(ev.turn_ordinal, b)) {
                boundaries.push((ev.turn_ordinal, b));
            }
        }
    }

    Ok(IngestOutcome {
        state,
        effects,
        boundaries,
    })
}

/// The effective turn ordinal for one hook, and whether a turn is open after
/// it (M2-15). An envelope-carried ordinal is trusted verbatim. A wire with
/// none (native agent hooks carry no turn) derives it: a prompt/turn-start on
/// a closed turn opens ordinal N+1; further events ride the open turn; a
/// turn-end closes it. A prompt queued mid-turn stays in the open turn.
fn effective_turn(
    envelope_turn: Option<u32>,
    events: &[rift_shadow_hooks::CanonicalEvent],
    cache: Option<&SessionCache>,
) -> (u32, bool) {
    let opens = events
        .iter()
        .any(|e| matches!(e.kind, EventKind::PromptSubmit | EventKind::TurnStart));
    let ends = events.iter().any(|e| e.kind == EventKind::TurnEnd);
    let (current, was_open) = cache.map_or((0, false), |c| (c.current_turn_ordinal, c.turn_open));
    let turn = match envelope_turn {
        Some(t) => t,
        None if opens && !was_open => current.saturating_add(1),
        None => current,
    };
    (turn, (was_open || opens) && !ends)
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
            turn: Some(turn),
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

        let o1 = ingest_hook(
            &raw("SessionStart", 0),
            &reg,
            &s,
            git_dir,
            "claude-code",
            "repo-1",
            10,
            None,
        )
        .await
        .unwrap();
        assert_eq!(o1.state, SessionState::Active);

        // a tool event keeps ACTIVE (no transition) but records the hook receipt
        let o2 = ingest_hook(
            &raw("PreToolUse", 1),
            &reg,
            &s,
            git_dir,
            "claude-code",
            "repo-1",
            11,
            None,
        )
        .await
        .unwrap();
        assert_eq!(o2.state, SessionState::Active);
        assert!(o2.effects.is_empty());
        assert!(o2.boundaries.is_empty(), "a tool event is not a boundary");

        // `SessionEnd` (not `Stop`, which is a turn boundary) drives the End transition.
        let o3 = ingest_hook(
            &raw("SessionEnd", 2),
            &reg,
            &s,
            git_dir,
            "claude-code",
            "repo-1",
            12,
            None,
        )
        .await
        .unwrap();
        assert_eq!(o3.state, SessionState::Ending);
        assert!(o3.effects.contains(&Effect::RequestCondense));

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

        ingest_hook(&edit, &reg, &s, git_dir, "claude-code", "repo-1", 10, None)
            .await
            .unwrap();
        assert_eq!(s.count("captured_event").await.unwrap(), 2);

        // Re-deliver the identical hook: same idempotency keys → no new rows.
        ingest_hook(&edit, &reg, &s, git_dir, "claude-code", "repo-1", 99, None)
            .await
            .unwrap();
        assert_eq!(
            s.count("captured_event").await.unwrap(),
            2,
            "duplicate delivery is a no-op"
        );

        // A distinct turn is a distinct logical event → new rows.
        let mut edit2 = edit.clone();
        edit2.turn = Some(2);
        ingest_hook(
            &edit2,
            &reg,
            &s,
            git_dir,
            "claude-code",
            "repo-1",
            100,
            None,
        )
        .await
        .unwrap();
        assert_eq!(s.count("captured_event").await.unwrap(), 4);
    }

    #[tokio::test]
    async fn native_hooks_derive_turn_ordinals_from_the_event_stream() {
        // M2-15: Claude Code's native stdin carries no turn. Ordinals derive:
        // a prompt opens turn 1, tool events ride it, Stop closes it; the next
        // prompt opens turn 2.
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        let native = |verb: &str, at: i64| RawHook {
            agent: AgentId::CLAUDE_CODE,
            verb: verb.into(),
            session: "native-sess".into(),
            turn: None,
            occurred_at_ms: at,
            payload: serde_json::json!({"session_id": "native-sess", "tool_name": "Bash"}),
        };
        let ingest = |raw: RawHook, at: i64| {
            let (s, reg) = (&s, &reg);
            async move {
                ingest_hook(&raw, reg, s, git_dir, "claude-code", "repo-1", at, None)
                    .await
                    .unwrap()
            }
        };

        ingest(native("SessionStart", 10), 10).await;
        let o = ingest(native("UserPromptSubmit", 20), 20).await;
        assert_eq!(
            o.boundaries,
            vec![(1, crate::turn_boundary::Boundary::Start)],
            "the first prompt opens turn 1"
        );
        ingest(native("PreToolUse", 30), 30).await;
        let cache = session_cache::read_cache(git_dir, "native-sess")
            .unwrap()
            .unwrap();
        assert_eq!(cache.current_turn_ordinal, 1);
        assert!(cache.turn_open, "tool events ride the open turn");

        let o = ingest(native("Stop", 40), 40).await;
        assert_eq!(o.boundaries, vec![(1, crate::turn_boundary::Boundary::End)]);
        let o = ingest(native("UserPromptSubmit", 50), 50).await;
        assert_eq!(
            o.boundaries,
            vec![(2, crate::turn_boundary::Boundary::Start)],
            "the next prompt opens turn 2"
        );
        let o = ingest(native("Stop", 60), 60).await;
        assert_eq!(o.boundaries, vec![(2, crate::turn_boundary::Boundary::End)]);
        let cache = session_cache::read_cache(git_dir, "native-sess")
            .unwrap()
            .unwrap();
        assert_eq!(cache.current_turn_ordinal, 2);
        assert!(!cache.turn_open);
    }

    #[test]
    fn effective_turn_trusts_envelope_and_handles_queued_prompts() {
        use rift_shadow_hooks::{CanonicalEvent, Trust};
        let ev = |kind: EventKind| CanonicalEvent {
            schema_version: 1,
            session: String::new(),
            turn_ordinal: 0,
            event_ordinal: 0,
            kind,
            trust: Trust::GitFact,
            occurred_at_ms: 0,
            payload_sha256: rift_core::Sha256Id::from_bytes([0u8; 32]),
        };
        let mut cache = SessionCache::new("s", "claude-code", "active", 0);
        cache.current_turn_ordinal = 4;
        cache.turn_open = true;
        // Envelope ordinal wins verbatim, even against a divergent cache.
        let (t, _) = effective_turn(Some(9), &[ev(EventKind::ToolCall)], Some(&cache));
        assert_eq!(t, 9);
        // A prompt queued while the turn is open stays in the open turn.
        let (t, open) = effective_turn(None, &[ev(EventKind::PromptSubmit)], Some(&cache));
        assert_eq!((t, open), (4, true));
        // No cache at all: the first prompt opens turn 1.
        let (t, open) = effective_turn(None, &[ev(EventKind::PromptSubmit)], None);
        assert_eq!((t, open), (1, true));
        // A stray turn-end with nothing open stays honest at 0, closed.
        let (t, open) = effective_turn(None, &[ev(EventKind::TurnEnd)], None);
        assert_eq!((t, open), (0, false));
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

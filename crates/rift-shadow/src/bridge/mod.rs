//! The bridge from agent hooks to the durable session lifecycle.
//!
//! [`ingest_hook`] takes one raw agent hook, dispatches it to canonical events,
//! records the append-only hook receipt, advances the **pure** session state
//! machine ([`step`](rift_shadow_hooks::step)) one transition per state-changing
//! event, persists each transition as an append-only `session_transition` row
//! (the source of truth), and refreshes the out-of-tree transient cache. Effects
//! (`RequestCondense`, …) are returned for the caller's executor to run. Illegal
//! transitions are swallowed (the caller logs them) — never panic, never fail the
//! agent.
//!
//! The ingest is one linear pass whose every write is one appended fact, in
//! the order the chain hashes: the receipt, the per-event idempotency index,
//! the normalized projection, the session transitions, then the cache. This
//! module orders the steps; each lives beside the concern it serves — the
//! `session` module drives the state machine, `reads` names the read facts,
//! `turns` derives ordinals and settles a closed turn's spend.

#[cfg(test)]
mod fixtures;
mod reads;
mod session;
#[cfg(test)]
mod settle_tests;
mod turns;

use std::path::Path;

use rift_core::Result;
use rift_shadow_hooks::claim;
use rift_shadow_hooks::session_cache;
use rift_shadow_hooks::{
    dispatch, idempotency_key, AdapterRegistry, CanonicalEvent, Effect, EventKind, HookFacts,
    RawHook, SessionState,
};
use rift_store::{
    record_agent_hook_event, record_captured_event, AgentHookEventRow, CanonicalStore,
    CapturedEventRow,
};

use crate::project::{PayloadFacts, ProjectionCtx};
use crate::sha256_hex;

pub use reads::RecordedRead;
pub use session::{mark_condense_done, timeout_idle_session, trigger_for};
pub use turns::{settle_closed_turn_usage, SettleMode, Settled, SETTLE_WINDOW_BYTES};

/// The injected worktree read-hasher: a read-shaped payload path in,
/// the resolved repo-relative path + content identity out (`None` when the
/// path is outside the repo, vanished, or unsafe). Injected so the ingest
/// layer never touches the filesystem itself.
pub type ReadHasher<'a> = &'a dyn Fn(&str) -> Option<crate::project::ReadContent>;

/// What the append-only `agent_hook_event` receipt recorded for one ingest.
///
/// The receipt is written for **every** invocation, including the ones that
/// capture nothing: a hook Rift cannot use is a capture loss, and a loss that
/// leaves no row is indistinguishable from a hook that never fired.
///
/// **The receipt's `agent` column names the session's *owner*, not the agent
/// whose process fired the hook**. The two differ whenever one
/// conversation drives two hook systems — Cursor forwarding to Claude Code is
/// the motivating case — and the owner is the label every other row for that
/// session is addressed by, so a receipt keyed to the firing agent would not
/// join its own session. The firing agent is not lost: it chose the adapter
/// that mapped the payload, and it is what the per-event idempotency key is
/// derived from, so redelivery still dedups against the delivering agent.
/// [`DeferredNoSession`](Self::DeferredNoSession) is the one case with no
/// owner at all — there is no session to own — and its receipt therefore names
/// the firing agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookReceipt {
    /// The adapter mapped the verb and the canonical events were recorded.
    Recorded,
    /// The adapter does not know this verb. Not an error (a new agent version
    /// may add verbs before Rift does), but recorded so the gap is countable.
    SkippedUnknownVerb,
    /// The hook carried no session id Rift can attribute to (absent, or not
    /// usable as a name — see
    /// [`is_safe_session_id`](rift_shadow_hooks::is_safe_session_id)). Nothing
    /// downstream can be written, because every durable row for a hook is
    /// addressed by its session; the receipt is the only trace, and it exists
    /// precisely so the loss is visible rather than silent.
    DeferredNoSession,
}

impl HookReceipt {
    /// The token stored in `agent_hook_event.outcome`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::SkippedUnknownVerb => "skipped-unknown-verb",
            Self::DeferredNoSession => "deferred-no-session",
        }
    }
}

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
    /// The agent label this session is attributed to — the first agent to
    /// claim the session id, which is not necessarily the agent that fired
    /// this hook (the Cursor double-config case). Every post-ingest
    /// step that addresses the session row — the turn-boundary snapshots, the
    /// condense drivers — must use this label, or it will write rows the
    /// session cannot see.
    pub owner_agent: String,
    /// What the append-only receipt recorded. A
    /// [`DeferredNoSession`](HookReceipt::DeferredNoSession) ingest wrote the
    /// receipt and nothing else, so the caller must skip every post-ingest
    /// step: they all address a session that does not exist.
    pub receipt: HookReceipt,
    /// The read facts this hook recorded (one per `ToolResult` event that
    /// resolved a read-shaped payload through the caller's hasher), addressed
    /// by the `tool_call` row each landed on — so the caller can owe the
    /// read's token count to a later, off-path writer at the same identity.
    pub read_calls: Vec<RecordedRead>,
}

/// What every step of one ingest addresses its rows by.
///
/// Built only once the session is known to be usable and its owner settled,
/// so no step downstream can address a row by the firing agent by mistake.
struct IngestCtx<'a> {
    raw: &'a RawHook,
    registry: &'a AdapterRegistry,
    git_dir: &'a Path,
    /// The session's owner: the first agent to claim the id, which is not
    /// necessarily the agent that fired this hook.
    owner: &'a str,
    repo_id: &'a str,
    now_ms: i64,
    /// The content address of this delivery, shared by the receipt on both
    /// the deferred and the recorded path so one recipe keys them all.
    payload_hash: String,
}

/// Ingest one raw agent hook end-to-end: the append-only receipt, the per-event
/// idempotency index, the **normalized projection** (session / turn / model-call
/// / tool-call / token-usage / cost — the same projector the artifact bridge
/// replays), and the session state machine. Returns the resulting
/// session phase, effects, and any turn boundaries the caller must snapshot.
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
    repo_id: &str,
    now_ms: i64,
    read_hasher: Option<ReadHasher<'_>>,
) -> Result<IngestOutcome> {
    let mut events = dispatch(raw, registry)?;
    let payload_hash = sha256_hex(&[serde_json::to_vec(&raw.payload)
        .unwrap_or_default()
        .as_slice()]);
    if !rift_shadow_hooks::is_safe_session_id(&raw.session) {
        return defer_without_session(store, raw, agent_label, &payload_hash, now_ms).await;
    }

    // One conversation can fire hooks as two agents (Cursor runs cursor-agent
    // *and* forwards the session to Claude Code's hook system), and a session
    // row is derived from `(agent, session_id)` — so without an owner the one
    // conversation splits into two sessions, each holding half the turns.
    // First-writer-wins settles it: the *firing* agent still chose the adapter
    // above and still keys the idempotency index (redelivery stays a no-op),
    // while the *owner* decides which session these events join.
    let claim = claim::claim_session_agent(git_dir, &raw.session, agent_label);
    let ctx = IngestCtx {
        raw,
        registry,
        git_dir,
        owner: claim.owner(agent_label),
        repo_id,
        now_ms,
        payload_hash,
    };

    // The current cache, read once: it feeds both turn derivation and the
    // state machine's starting phase.
    let existing = session_cache::read_cache(git_dir, &raw.session)?;
    session::synthesize_missing_start(store, &ctx, &mut events).await?;
    let stamp = turns::stamp_turn(&ctx, &mut events, existing.as_ref());

    let receipt = record_receipt(store, &ctx, stamp.ordinal, &events).await?;
    record_captured_events(store, &ctx, &events).await?;
    let read_calls = project_hook(store, &ctx, &events, read_hasher).await?;
    let (state, effects) = session::advance(store, &ctx, existing.as_ref(), &events).await?;
    session::refresh_cache(&ctx, existing, state, &stamp)?;
    Ok(IngestOutcome {
        state,
        effects,
        boundaries: turns::closed_boundaries(&events, &stamp),
        owner_agent: ctx.owner.to_owned(),
        receipt,
        read_calls,
    })
}

/// The receipt, and nothing else, for a hook that names no usable session.
///
/// Every durable write of an ingest is addressed by session id, and the
/// transient cache refuses to turn an unusable id into a filename, so the
/// first of those writes used to abort the whole ingest and the hook vanished
/// leaving nothing at all. On a path whose contract is to swallow every error
/// in silence, that made a capture loss indistinguishable from an agent that
/// never fired. Distinct payloads still get distinct receipts (the id hashes
/// the payload), and a redelivery of the same one still collapses to one row.
async fn defer_without_session(
    store: &impl CanonicalStore,
    raw: &RawHook,
    agent_label: &str,
    payload_hash: &str,
    now_ms: i64,
) -> Result<IngestOutcome> {
    let row = AgentHookEventRow {
        hook_event_id: receipt_id(agent_label, raw, "0", payload_hash),
        agent: agent_label.to_owned(),
        verb: raw.verb.clone(),
        // Deliberately NULL rather than the empty string: there is no
        // session here, and an empty id is a value every session-less
        // hook would share.
        session_id: None,
        received_at: now_ms,
        outcome: HookReceipt::DeferredNoSession.as_str().to_owned(),
        schema_version: 1,
    };
    record_agent_hook_event(store, &row).await?;
    Ok(IngestOutcome {
        state: SessionState::Idle,
        effects: Vec::new(),
        boundaries: Vec::new(),
        owner_agent: agent_label.to_owned(),
        receipt: HookReceipt::DeferredNoSession,
        read_calls: Vec::new(),
    })
}

/// The append-only hook receipt, idempotent on its content address.
async fn record_receipt(
    store: &impl CanonicalStore,
    ctx: &IngestCtx<'_>,
    turn: u32,
    events: &[CanonicalEvent],
) -> Result<HookReceipt> {
    let receipt = if events.is_empty() {
        HookReceipt::SkippedUnknownVerb
    } else {
        HookReceipt::Recorded
    };
    let row = AgentHookEventRow {
        hook_event_id: receipt_id(ctx.owner, ctx.raw, &turn.to_string(), &ctx.payload_hash),
        agent: ctx.owner.to_owned(),
        verb: ctx.raw.verb.clone(),
        session_id: Some(ctx.raw.session.clone()),
        received_at: ctx.now_ms,
        outcome: receipt.as_str().to_owned(),
        schema_version: 1,
    };
    record_agent_hook_event(store, &row).await?;
    Ok(receipt)
}

/// The receipt's content address: the agent it is filed under, the verb, the
/// session, the turn and the payload's hash, so a redelivery collapses to one
/// row and a distinct payload gets its own. A session-less receipt is filed
/// at turn `"0"`.
fn receipt_id(agent: &str, raw: &RawHook, turn: &str, payload_hash: &str) -> String {
    sha256_hex(&[
        agent.as_bytes(),
        b"\0",
        raw.verb.as_bytes(),
        b"\0",
        raw.session.as_bytes(),
        b"\0",
        turn.as_bytes(),
        b"\0",
        payload_hash.as_bytes(),
    ])
}

/// The per-canonical-event idempotency index: one append-only row per
/// *logical* event, keyed by a content/identity-derived key. A duplicate
/// delivery of the same hook re-derives the same keys and no-ops here, so the
/// chain never gets a doubled leaf and cost/attribution are never double-counted.
async fn record_captured_events(
    store: &impl CanonicalStore,
    ctx: &IngestCtx<'_>,
    events: &[CanonicalEvent],
) -> Result<()> {
    for ev in events {
        let key = idempotency_key(
            &ctx.raw.agent,
            &ctx.raw.session,
            &ctx.raw.verb,
            ev.turn_ordinal,
            ev.event_ordinal,
            &ev.payload_sha256,
        );
        let row = CapturedEventRow {
            idempotency_key: key.to_hex(),
            session_id: ev.session.clone(),
            turn_ordinal: i64::from(ev.turn_ordinal),
            event_ordinal: i64::from(ev.event_ordinal),
            kind: ev.kind.as_str().to_owned(),
            trust: ev.trust.as_str().to_owned(),
            payload_sha256: ev.payload_sha256.to_hex(),
            occurred_at_ms: ev.occurred_at_ms,
            schema_version: i64::from(ev.schema_version),
        };
        record_captured_event(store, &row).await?;
    }
    Ok(())
}

/// The normalized projection: the same pure mapping the artifact bridge
/// replays over re-derived events, so both sources' rows union to a no-op.
/// Returns the read facts that landed, at the rows they live on.
async fn project_hook(
    store: &impl CanonicalStore,
    ctx: &IngestCtx<'_>,
    events: &[CanonicalEvent],
    read_hasher: Option<ReadHasher<'_>>,
) -> Result<Vec<RecordedRead>> {
    let normalized = ctx
        .registry
        .get(&ctx.raw.agent)
        .map(|adapter| adapter.facts(&ctx.raw.verb, &ctx.raw.payload))
        .unwrap_or_default();
    let commanded = reads::commanded_read(ctx, &normalized, events);
    let read_lines = commanded.as_ref().map(|&(_, lines)| lines);
    let facts = payload_facts(normalized, commanded.map(|(path, _)| path), read_hasher);
    let projection = ProjectionCtx {
        agent: ctx.owner,
        repo_id: ctx.repo_id,
    };
    let mut read_calls = Vec::new();
    for ev in events {
        crate::project::project_event(store, ev, &facts, projection).await?;
        if let (EventKind::ToolResult, Some(content)) = (ev.kind, &facts.read_content) {
            read_calls.push(reads::recorded_read(ctx.owner, ev, content, read_lines));
        }
    }
    record_usage(store, ctx, events.last(), &facts, projection).await?;
    Ok(read_calls)
}

/// The projector's view of this payload: the adapter's structured facts, with
/// a commanded read standing in where the family has no read tool.
///
/// The read-time content identity is resolved through the caller's worktree
/// hasher: the ingest layer never touches the filesystem itself, and an
/// unresolvable path (outside the repo, vanished, unsafe) simply records no
/// read fact.
fn payload_facts(
    normalized: HookFacts,
    commanded_path: Option<String>,
    read_hasher: Option<ReadHasher<'_>>,
) -> PayloadFacts {
    let read_path = normalized.read_path.or(commanded_path);
    let read_content = match (&read_path, read_hasher) {
        (Some(path), Some(hasher)) => hasher(path),
        _ => None,
    };
    PayloadFacts {
        model: normalized.model.as_ref().map(|fact| fact.model.clone()),
        model_source: normalized
            .model
            .as_ref()
            .map(|fact| fact.source.as_str().to_owned()),
        tool_name: normalized.tool_name,
        child_session: normalized.child_session,
        read_path,
        read_content,
    }
}

/// Provider-reported token usage (never fabricated): attaches to the LAST
/// event of this hook's fan-out — the result/turn-end fact that carried it.
async fn record_usage(
    store: &impl CanonicalStore,
    ctx: &IngestCtx<'_>,
    anchor: Option<&CanonicalEvent>,
    facts: &PayloadFacts,
    projection: ProjectionCtx<'_>,
) -> Result<()> {
    let Some(anchor) = anchor else {
        return Ok(());
    };
    let Some(adapter) = ctx.registry.get(&ctx.raw.agent) else {
        return Ok(());
    };
    if let Some(usage) = adapter.usage(&ctx.raw.verb, &ctx.raw.payload) {
        crate::project::project_usage(store, anchor, facts, projection, &usage, ctx.now_ms).await?;
    }
    if let Some(usage) = adapter.main_context_usage(&ctx.raw.verb, &ctx.raw.payload) {
        crate::project::project_main_context(
            store,
            anchor,
            &ctx.raw.verb,
            facts,
            projection,
            &usage,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::fixtures::{raw, store};
    use super::*;
    use rift_shadow_hooks::AgentId;
    use rift_store::SqliteStore;

    /// The transcript's ADDRESS rides the cache — last report wins, and a
    /// hook without the field keeps the previous address rather than
    /// forgetting it (the post-commit worker reads it when no payload exists).
    #[tokio::test]
    async fn a_payload_transcript_path_is_remembered_and_survives_hooks_without_one() {
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();

        let mut with_path = raw("SessionStart", 0);
        with_path.payload = serde_json::json!({"transcript_path": "/tmp/agent/s1.jsonl"});
        ingest_hook(
            &with_path,
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
        let cache = session_cache::read_cache(git_dir, "01JSESSION")
            .unwrap()
            .unwrap();
        assert_eq!(
            cache.transcript_path.as_deref(),
            Some("/tmp/agent/s1.jsonl")
        );

        // A later hook with no transcript_path keeps the remembered address…
        ingest_hook(
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
        let cache = session_cache::read_cache(git_dir, "01JSESSION")
            .unwrap()
            .unwrap();
        assert_eq!(
            cache.transcript_path.as_deref(),
            Some("/tmp/agent/s1.jsonl")
        );

        // …and a relocated transcript (a resume) replaces it.
        let mut moved = raw("UserPromptSubmit", 2);
        moved.payload = serde_json::json!({"transcript_path": "/tmp/agent/s1-resumed.jsonl"});
        ingest_hook(&moved, &reg, &s, git_dir, "claude-code", "repo-1", 12, None)
            .await
            .unwrap();
        let cache = session_cache::read_cache(git_dir, "01JSESSION")
            .unwrap()
            .unwrap();
        assert_eq!(
            cache.transcript_path.as_deref(),
            Some("/tmp/agent/s1-resumed.jsonl")
        );
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
        // Re-delivering the same hook is a true no-op — the per-event
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

    /// A hook whose payload names no session used to write **nothing**:
    /// the session cache refuses an unusable id, and that refusal aborted the
    /// ingest before the receipt row — so on a path contractually obliged to
    /// swallow errors, a real capture loss left no evidence it had happened.
    /// The receipt now records it with its reason, and records nothing else.
    #[tokio::test]
    async fn a_hook_with_no_session_leaves_a_receipt_that_names_the_loss() {
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        let sessionless = sessionless_hook(serde_json::json!({"tool_name": "Bash"}));

        let out = ingest_hook(
            &sessionless,
            &reg,
            &s,
            git_dir,
            "claude-code",
            "repo-1",
            5,
            None,
        )
        .await
        .expect("a session-less hook is deferred, not an error");
        assert_eq!(out.receipt, HookReceipt::DeferredNoSession);
        assert_eq!(out.state, SessionState::Idle);
        assert!(out.effects.is_empty() && out.boundaries.is_empty());

        // The receipt exists, names the reason, and carries a NULL session.
        let rows = s
            .query(
                "SELECT outcome, session_id, agent, verb FROM agent_hook_event",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "exactly one receipt");
        assert_eq!(rows[0].get_text(0).unwrap(), "deferred-no-session");
        assert_eq!(
            rows[0].get_opt_text(1).unwrap(),
            None,
            "no session means NULL, not an empty id every such hook would share"
        );
        assert_eq!(rows[0].get_text(2).unwrap(), "claude-code");
        assert_eq!(rows[0].get_text(3).unwrap(), "PreToolUse");

        // …and nothing else was written: no events, no session, no transition.
        for table in ["captured_event", "session", "session_transition", "turn"] {
            assert_eq!(
                s.count(table).await.unwrap(),
                0,
                "{table} must stay empty — the hook named no session to write under"
            );
        }
    }

    /// Replay of a session-less hook stays a no-op (the receipt is
    /// content-addressed), while a different session-less payload is its own
    /// visible loss.
    #[tokio::test]
    async fn a_session_less_receipt_is_addressed_by_its_payload() {
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        let sessionless = sessionless_hook(serde_json::json!({"tool_name": "Bash"}));
        for at in [5, 6] {
            ingest_hook(
                &sessionless,
                &reg,
                &s,
                git_dir,
                "claude-code",
                "repo-1",
                at,
                None,
            )
            .await
            .unwrap();
        }
        assert_eq!(s.count("agent_hook_event").await.unwrap(), 1);
        let other = sessionless_hook(serde_json::json!({"tool_name": "Read"}));
        ingest_hook(&other, &reg, &s, git_dir, "claude-code", "repo-1", 7, None)
            .await
            .unwrap();
        assert_eq!(s.count("agent_hook_event").await.unwrap(), 2);
    }

    /// A `PreToolUse` whose payload names no session at all.
    fn sessionless_hook(payload: serde_json::Value) -> RawHook {
        RawHook {
            agent: AgentId::CLAUDE_CODE,
            verb: "PreToolUse".into(),
            session: String::new(),
            turn: None,
            occurred_at_ms: 5,
            payload,
        }
    }

    /// The same guard covers an id that is present but cannot name a file. It
    /// cannot arrive over the wire (`build_raw_hook` defers it first), so this
    /// pins the sink's behaviour for every other caller.
    #[tokio::test]
    async fn an_unusable_session_id_defers_with_a_receipt_too() {
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let out = ingest_hook(
            &RawHook {
                agent: AgentId::CLAUDE_CODE,
                verb: "Stop".into(),
                session: "../evil".into(),
                turn: None,
                occurred_at_ms: 5,
                payload: serde_json::json!({}),
            },
            &reg,
            &s,
            tmp.path(),
            "claude-code",
            "repo-1",
            5,
            None,
        )
        .await
        .unwrap();
        assert_eq!(out.receipt, HookReceipt::DeferredNoSession);
        assert_eq!(s.count("agent_hook_event").await.unwrap(), 1);
        assert_eq!(s.count("captured_event").await.unwrap(), 0);
        assert!(
            !tmp.path().parent().unwrap().join("evil.json").exists(),
            "a deferred hook writes no file outside the sessions directory"
        );
    }

    /// The Copilot pair, end to end. Copilot's session key **is**
    /// stated at the pinned tag (`upstream: copilotcli/compat.go:74`
    /// reads `firstString(raw, "sessionId", "session_id")`), so binding it is
    /// the fix — but Copilot also fires those same *session* lifecycle hooks
    /// for subagent turns, reusing the subagent's tool-use id as the
    /// `sessionId` (`lifecycle.go:17`). Bound, each subagent turn would leave
    /// an ownership marker and a session-cache file that never receive an end.
    /// Binding without refusing would therefore have traded one visible loss
    /// for an invisible leak, which is why the two land together.
    #[tokio::test]
    async fn a_copilot_subagent_lifecycle_hook_opens_no_session() {
        let s = store();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        copilot_session_and_subagent_turn(&s, git_dir).await;

        // The phantom leaves no marker, no cache and no session row, while
        // the real one keeps all three.
        let names: Vec<String> = std::fs::read_dir(git_dir.join("rift-sessions"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|n| !n.starts_with("toolu_")),
            "a subagent's tool-use id must not name a session file: {names:?}"
        );
        assert!(names.contains(&format!("{COPILOT_SESSION}.agent")));
        assert!(names.contains(&format!("{COPILOT_SESSION}.json")));
        assert_eq!(
            s.count("session").await.unwrap(),
            1,
            "one real session, no phantom"
        );
    }

    /// The refusal is still countable: two receipts, and the deferred one
    /// hashes this subagent's own payload rather than a shared null, so a
    /// second refused turn is a second row.
    #[tokio::test]
    async fn a_refused_copilot_subagent_turn_is_still_countable() {
        let s = store();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        copilot_session_and_subagent_turn(&s, git_dir).await;
        let deferred = s
            .query(
                "SELECT COUNT(*) FROM agent_hook_event WHERE outcome = 'deferred-no-session'",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(deferred[0].get_i64(0).unwrap(), 1);

        let other = copilot_start("toolu_bdrk_99other", 7);
        let reg = AdapterRegistry::with_builtin();
        ingest_hook(&other, &reg, &s, git_dir, "copilot-cli", "repo-1", 7, None)
            .await
            .unwrap();
        assert_eq!(s.count("agent_hook_event").await.unwrap(), 3);
    }

    /// A real Copilot session id.
    const COPILOT_SESSION: &str = "b0ff98c0-8e01-4b73-bf92-9649b139931b";

    /// A Copilot `session_start` for `session_id`, built the way the wire
    /// builds it.
    fn copilot_start(session_id: &str, at: i64) -> RawHook {
        rift_shadow_hooks::build_raw_hook(
            "copilot-cli",
            "session_start",
            format!(r#"{{"sessionId":"{session_id}","cwd":"/repo","source":"new"}}"#).as_bytes(),
            at,
        )
        .unwrap()
    }

    /// A real Copilot session binds and captures — the row the previous
    /// cycle called unverifiable and shipped session-less — and the identical
    /// hook fired for a subagent turn binds nothing.
    async fn copilot_session_and_subagent_turn(s: &SqliteStore, git_dir: &Path) {
        let reg = AdapterRegistry::with_builtin();
        let real = copilot_start(COPILOT_SESSION, 5);
        assert_eq!(real.session, COPILOT_SESSION);
        let out = ingest_hook(&real, &reg, s, git_dir, "copilot-cli", "repo-1", 5, None)
            .await
            .unwrap();
        assert_eq!(out.receipt, HookReceipt::Recorded);

        let subagent = copilot_start("toolu_bdrk_01KqZ8mR3fN2", 6);
        assert_eq!(subagent.session, "");
        let out = ingest_hook(
            &subagent,
            &reg,
            s,
            git_dir,
            "copilot-cli",
            "repo-1",
            6,
            None,
        )
        .await
        .unwrap();
        assert_eq!(out.receipt, HookReceipt::DeferredNoSession);
    }

    /// The receipt's three outcomes are the whole vocabulary, and each ingest
    /// reports the one it wrote.
    #[tokio::test]
    async fn every_ingest_reports_the_receipt_it_wrote() {
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        let recorded = ingest_hook(
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
        assert_eq!(recorded.receipt, HookReceipt::Recorded);
        let skipped = ingest_hook(
            &raw("NoSuchVerb", 1),
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
        assert_eq!(skipped.receipt, HookReceipt::SkippedUnknownVerb);
        let rows = s
            .query(
                "SELECT outcome FROM agent_hook_event ORDER BY received_at",
                &[],
            )
            .await
            .unwrap();
        let outcomes: Vec<String> = rows.iter().map(|r| r.get_text(0).unwrap()).collect();
        assert_eq!(outcomes, vec!["recorded", "skipped-unknown-verb"]);
    }

    /// The Cursor double-config case: the IDE runs `cursor-agent` and
    /// also forwards the session to Claude Code's hook system, so two agents
    /// fire hooks carrying one session id. A session row is keyed by
    /// `(agent, session_id)`, so before the ownership claim the conversation
    /// split in two — half the turns and half the spend under each.
    #[tokio::test]
    async fn two_agents_on_one_session_id_capture_a_single_session() {
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        let hook = |agent: AgentId, verb: &str, at: i64| RawHook {
            agent,
            verb: verb.into(),
            session: "shared-session".into(),
            turn: None,
            occurred_at_ms: at,
            payload: serde_json::json!({"session_id": "shared-session"}),
        };

        // Cursor's hook lands first and claims the session.
        ingest_hook(
            &hook(AgentId::CURSOR, "session_start", 10),
            &reg,
            &s,
            git_dir,
            "cursor",
            "repo-1",
            10,
            None,
        )
        .await
        .unwrap();
        // Claude Code's forwarded hook for the *same* conversation follows.
        let forwarded = ingest_hook(
            &hook(AgentId::CLAUDE_CODE, "UserPromptSubmit", 20),
            &reg,
            &s,
            git_dir,
            "claude-code",
            "repo-1",
            20,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            forwarded.owner_agent, "cursor",
            "the caller's post-ingest steps must address the owner's session row"
        );

        assert_eq!(
            s.count("session").await.unwrap(),
            1,
            "one conversation is one session"
        );
        let owner = rift_shadow_hooks::session_owner(git_dir, "shared-session").unwrap();
        assert_eq!(owner, "cursor", "first writer wins");
        let cache = session_cache::read_cache(git_dir, "shared-session")
            .unwrap()
            .unwrap();
        assert_eq!(cache.agent, "cursor", "the cache follows the owner");
        assert_eq!(cache.current_turn_ordinal, 1, "the turn stream is shared");
        // Both hooks were still captured, each under the adapter that fired it.
        assert_eq!(s.count("agent_hook_event").await.unwrap(), 2);
        assert_eq!(s.count("captured_event").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn antigravity_first_known_hook_synthesizes_one_durable_start() {
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        let first = RawHook {
            agent: AgentId::ANTIGRAVITY,
            verb: "PreToolUse".into(),
            session: "conversation-1".into(),
            turn: None,
            occurred_at_ms: 10,
            payload: serde_json::json!({
                "conversationId": "conversation-1",
                "toolCall": {"name": "view_file", "args": {"AbsolutePath": "/repo/a.rs"}}
            }),
        };
        let outcome = ingest_hook(&first, &reg, &s, git_dir, "antigravity", "repo-1", 10, None)
            .await
            .unwrap();
        assert_eq!(outcome.state, SessionState::Active);
        assert_eq!(s.count("session_event").await.unwrap(), 1);
        assert_eq!(s.count("captured_event").await.unwrap(), 2);

        // Even if transient cache state disappears, canonical history prevents
        // a second synthetic start.
        let cache_dir = git_dir.join("rift-sessions");
        for entry in std::fs::read_dir(cache_dir).unwrap().flatten() {
            std::fs::remove_file(entry.path()).unwrap();
        }
        let mut second = first;
        second.verb = "PostToolUse".into();
        second.occurred_at_ms = 20;
        ingest_hook(
            &second,
            &reg,
            &s,
            git_dir,
            "antigravity",
            "repo-1",
            20,
            None,
        )
        .await
        .unwrap();
        assert_eq!(s.count("session_event").await.unwrap(), 1);
        assert_eq!(s.count("captured_event").await.unwrap(), 3);
    }

    #[tokio::test]
    async fn native_hooks_derive_turn_ordinals_from_the_event_stream() {
        // Claude Code's native stdin carries no turn. Ordinals derive:
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
}

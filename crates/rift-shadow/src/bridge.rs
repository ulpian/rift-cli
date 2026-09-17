//! The bridge from agent hooks to the durable session lifecycle.
//!
//! [`ingest_hook`] takes one raw agent hook, dispatches it to canonical events,
//! records the append-only hook receipt, advances the **pure** session state
//! machine ([`step`]) one transition per state-changing
//! event, persists each transition as an append-only `session_transition` row
//! (the source of truth), and refreshes the out-of-tree transient cache. Effects
//! (`RequestCondense`, …) are returned for the caller's executor to run. Illegal
//! transitions are swallowed (the caller logs them) — never panic, never fail the
//! agent.

use std::path::Path;

use rift_core::{AgentId, Result, Trust};
use rift_shadow_hooks::claim;
use rift_shadow_hooks::session_cache::{self, SessionCache};
use rift_shadow_hooks::{
    dispatch, idempotency_key, step, AdapterRegistry, Effect, EventKind, RawHook, SessionState,
    Transition, Trigger, TurnSignals,
};
use rift_store::{
    record_agent_hook_event, record_captured_event, record_session_transition, AgentHookEventRow,
    CanonicalStore, CapturedEventRow, SessionTransitionRow, Value,
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

/// A command line's path, as an address the read hasher can resolve.
///
/// A shell read spells its path the way the agent's shell read it: relative to
/// the agent's working directory, which is not necessarily the work-tree root
/// the hasher resolves a relative path against. Joining the two here is what
/// stops a `cat mod.rs` run in a subdirectory from being recorded against a
/// same-named file at the root — a fact about a file nobody read, which is
/// worse than no fact at all. An absolute path is already an address, and a
/// payload that names no directory leaves the path exactly as it was rather
/// than having one guessed for it.
fn shell_read_address(path: String, cwd: Option<&str>) -> String {
    let relative = std::path::Path::new(&path);
    if relative.is_absolute() {
        return path;
    }
    // Joined textually with `/`, not with `Path::join`: the address goes to
    // the read hasher, which keys the ledger with `/` on every platform, and
    // the platform joiner would put a `\` into it on Windows.
    match cwd {
        Some(dir) => format!("{}/{path}", dir.trim_end_matches(['/', '\\'])),
        None => path,
    }
}

/// One read fact a hook ingest recorded, at the row it lives on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRead {
    /// The read's content-addressed `tool_call.tool_call_id`
    /// (`ids::read_call_row_id`).
    pub tool_call_id: String,
    /// Repo-relative path of the bytes read.
    pub rel_path: String,
    /// Git blob oid of the bytes at read time.
    pub blob_oid: String,
    /// SHA-256 (hex) of the bytes at read time.
    pub content_sha256: String,
    /// Whether a `file_read_tokens` row landed with the fact (the hasher
    /// measured inline) — `false` means the count is still owed.
    pub tokens_recorded: bool,
    /// The 1-based inclusive line range the call asked for, when the call
    /// *named* one — a command line does, a read tool's structured arguments
    /// are left alone. `None` means the whole file, which is what the count
    /// has always been for a structured read.
    pub lines: Option<(u32, u32)>,
    /// The event schema version the fact was recorded under.
    pub schema_version: i64,
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

    // The content address of this delivery, shared by the receipt on both the
    // deferred and the recorded path so one recipe keys them all.
    let payload_hash = sha256_hex(&[serde_json::to_vec(&raw.payload)
        .unwrap_or_default()
        .as_slice()]);

    // A hook that names no usable session cannot be attributed to one, and
    // every durable write below is addressed by session id — the transient
    // cache refuses to turn an unusable id into a filename, so the
    // first of those writes used to abort the whole ingest and the hook
    // vanished leaving nothing at all. On a path whose contract is to swallow
    // every error in silence, that made a capture loss indistinguishable from
    // an agent that never fired. Write the receipt with the reason, then
    // defer. Distinct payloads still get distinct receipts (the id hashes the
    // payload), and a redelivery of the same one still collapses to one row.
    if !rift_shadow_hooks::is_safe_session_id(&raw.session) {
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
                    b"0",
                    b"\0",
                    payload_hash.as_bytes(),
                ]),
                agent: agent_label.to_owned(),
                verb: raw.verb.clone(),
                // Deliberately NULL rather than the empty string: there is no
                // session here, and an empty id is a value every session-less
                // hook would share.
                session_id: None,
                received_at: now_ms,
                outcome: HookReceipt::DeferredNoSession.as_str().to_owned(),
                schema_version: 1,
            },
        )
        .await?;
        return Ok(IngestOutcome {
            state: SessionState::Idle,
            effects: Vec::new(),
            boundaries: Vec::new(),
            owner_agent: agent_label.to_owned(),
            receipt: HookReceipt::DeferredNoSession,
            read_calls: Vec::new(),
        });
    }

    // One conversation can fire hooks as two agents (Cursor runs cursor-agent
    // *and* forwards the session to Claude Code's hook system), and a session
    // row is derived from `(agent, session_id)` — so without an owner the one
    // conversation splits into two sessions, each holding half the turns.
    // First-writer-wins settles it: the *firing* agent still chose the adapter
    // above and still keys the idempotency index (redelivery stays a no-op),
    // while the *owner* decides which session these events join.
    let claim = claim::claim_session_agent(git_dir, &raw.session, agent_label);
    let agent_label = claim.owner(agent_label);

    // The current cache, read once: it feeds both turn derivation and the
    // state machine's starting phase.
    let existing = session_cache::read_cache(git_dir, &raw.session)?;

    // Antigravity exposes no explicit SessionStart contract. The first known
    // hook for a conversation therefore gains one deterministic synthetic
    // start. Query canonical history rather than relying on the transient cache
    // (which may be deleted), and reserve MAX as its ordinal so retransmitting
    // the native hook keeps every native event's idempotency key unchanged.
    if raw.agent == AgentId::ANTIGRAVITY
        && !events.is_empty()
        && !events
            .iter()
            .any(|event| event.kind == EventKind::SessionStart)
    {
        let session_row = crate::ids::session_row_id(agent_label, &raw.session);
        let already_started = !store
            .query(
                "SELECT 1 FROM session_event WHERE session_id = ? AND kind = 'start' LIMIT 1",
                &[Value::text(session_row)],
            )
            .await?
            .is_empty();
        if !already_started {
            if let Some(mut start) = events.first().cloned() {
                start.kind = EventKind::SessionStart;
                start.trust = Trust::GitFact;
                start.event_ordinal = u32::MAX;
                events.insert(0, start);
            }
        }
    }

    // The effective turn ordinal: an envelope-carried ordinal is
    // trusted verbatim; a wire with none (native agent hooks) derives it from
    // the event stream — a prompt/turn-start on a closed turn opens the next
    // ordinal, a turn-end closes the current one. Every event of this hook is
    // re-stamped with the result.
    // An End-only adapter (Antigravity: `Stop` closes a turn nothing opened)
    // declares so, and the first activity of a closed turn opens the next
    // ordinal with a SYNTHESIZED Start — otherwise its ordinal never advanced
    // and `record_turn_boundary_snapshot` (insert-or-ignore on
    // `sha256(session ‖ ordinal ‖ boundary)`) dropped every Stop after the
    // first.
    let signals = match registry.get(&raw.agent) {
        Some(adapter) => adapter.turn_signals(),
        None => TurnSignals::BOTH,
    };
    let (turn, turn_open, synthesized_start) =
        effective_turn(raw.turn, &events, existing.as_ref(), signals);
    for ev in &mut events {
        ev.turn_ordinal = turn;
    }

    // Append-only hook receipt (idempotent on its content address).
    let outcome = if events.is_empty() {
        HookReceipt::SkippedUnknownVerb
    } else {
        HookReceipt::Recorded
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
            outcome: outcome.as_str().to_owned(),
            schema_version: 1,
        },
    )
    .await?;

    // Per-canonical-event idempotency index: one append-only row per
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

    // Normalized projection: the same pure mapping the artifact bridge
    // replays over re-derived events, so both sources' rows union to a no-op.
    let normalized = registry
        .get(&raw.agent)
        .map(|adapter| adapter.facts(&raw.verb, &raw.payload))
        .unwrap_or_default();
    // A family whose read is a *command line* leaves `facts()` nothing to
    // report: Codex has no read tool at all, so `cat`, `head -n N` and
    // `sed -n 'A,Bp'` are how its context window fills, and every one of those
    // reads was invisible to the projection — which is the denominator every
    // savings share divides by. The authority consulted here is the same
    // recogniser that already classified this exact call for the read gate,
    // and it is consulted only where the structured fact is absent, so a read
    // tool's fact keeps the whole-file count it has always had.
    //
    // Asked only where a read fact could land — a `tool_result` in this hook's
    // fan-out — because resolving one costs the caller a file read and a hash,
    // and the call that is only *about* to run has nothing to record yet.
    let commanded = (normalized.read_path.is_none()
        && events.iter().any(|ev| ev.kind == EventKind::ToolResult))
    .then(|| {
        registry
            .get(&raw.agent)
            .and_then(|adapter| adapter.read_request(&raw.verb, &raw.payload))
    })
    .flatten()
    // A recognised command whose lines are not derivable *from the request
    // alone* — a `grep`, a decorated whole-file print, a `head` or `tail`
    // relying on a default nothing pins, and a counted `tail`, whose line
    // numbers are a function of a length this layer never learns — is a read
    // whose range nobody here can name, and an unnameable range is not booked
    // as a read at all. The read gate resolves the counted `tail` because it
    // has the bytes in hand for its own hash; this path has only a hashing
    // callback that answers with an identity and not a line count, so it
    // keeps the answer it always gave. The direction is the recogniser's own:
    // a missed read costs a saving, an invented one costs a claim.
    //
    // The first read the line performs, likewise, and not all of them: this
    // fact is one path and one range on one tool call, and a second would
    // need a second row.
    .and_then(|request| Some((request.path.clone(), request.conservative_lines()?)))
    // …and spelled as an address the caller's hasher can resolve, which is
    // not the same thing as the address the shell read it as.
    .map(|(path, lines)| {
        (
            shell_read_address(path, normalized.cwd.as_ref().map(|fact| fact.path.as_str())),
            lines,
        )
    });
    let read_lines = commanded.as_ref().map(|&(_, lines)| lines);
    let mut facts = crate::project::PayloadFacts {
        model: normalized.model.as_ref().map(|fact| fact.model.clone()),
        model_source: normalized
            .model
            .as_ref()
            .map(|fact| fact.source.as_str().to_owned()),
        tool_name: normalized.tool_name,
        child_session: normalized.child_session,
        read_path: normalized
            .read_path
            .or_else(|| commanded.map(|(path, _)| path)),
        read_content: None,
    };
    // Resolve the read-time content identity through the caller's worktree
    // hasher: the ingest layer never touches the filesystem itself,
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
    let mut read_calls = Vec::new();
    for ev in &events {
        crate::project::project_event(store, ev, &facts, ctx).await?;
        if let (EventKind::ToolResult, Some(rc)) = (ev.kind, &facts.read_content) {
            read_calls.push(RecordedRead {
                tool_call_id: crate::ids::read_call_row_id(
                    &crate::ids::session_row_id(agent_label, &ev.session),
                    ev.turn_ordinal,
                    &rc.rel_path,
                    &rc.content_sha256,
                ),
                rel_path: rc.rel_path.clone(),
                blob_oid: rc.blob_oid.clone(),
                content_sha256: rc.content_sha256.clone(),
                tokens_recorded: rc.tokens.is_some(),
                lines: read_lines,
                schema_version: i64::from(ev.schema_version),
            });
        }
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
    // The transcript's ADDRESS, when this hook's payload names one — the
    // post-commit archive has no payload and reads it from here (2c). Last
    // report wins (agents relocate transcripts across resumes); a hook without
    // the field keeps the previous address rather than forgetting it.
    if let Some(path) = raw.payload.get("transcript_path").and_then(|v| v.as_str()) {
        if !path.is_empty() {
            cache.transcript_path = Some(path.to_owned());
        }
    }
    session_cache::write_cache(git_dir, &cache)?;

    // The turn boundaries this hook closed: a prompt submission /
    // turn-start closes the human's H_i window; a turn-end closes the agent's
    // A_i window. Deduped per (ordinal, boundary).
    let mut boundaries: Vec<(u32, crate::turn_boundary::Boundary)> = Vec::new();
    if synthesized_start {
        // The Start an End-only adapter never sends: pushed FIRST, so the
        // human window H_i closes before anything this hook records.
        boundaries.push((turn, crate::turn_boundary::Boundary::Start));
    }
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
        owner_agent: agent_label.to_owned(),
        receipt: outcome,
        read_calls,
    })
}

/// The effective turn ordinal for one hook, and whether a turn is open after
/// it. An envelope-carried ordinal is trusted verbatim. A wire with
/// none (native agent hooks carry no turn) derives it: a prompt/turn-start on
/// a closed turn opens ordinal N+1; further events ride the open turn; a
/// turn-end closes it. A prompt queued mid-turn stays in the open turn.
/// The adapter's declared [`TurnSignals`] decide the opener. For an End-only
/// adapter (`end && !start`) a closed turn is opened by the first event that
/// is not itself a `TurnEnd`/`SessionEnd` — the Start the adapter cannot send
/// is synthesized here, and the third field says so, so the caller records a
/// `Start` boundary first. A Start-capable adapter never gets a synthesized
/// Start: its own prompt/turn-start is the opener, exactly as before.
fn effective_turn(
    envelope_turn: Option<u32>,
    events: &[rift_shadow_hooks::CanonicalEvent],
    cache: Option<&SessionCache>,
    signals: TurnSignals,
) -> (u32, bool, bool) {
    let real_open = events
        .iter()
        .any(|e| matches!(e.kind, EventKind::PromptSubmit | EventKind::TurnStart));
    let ends = events.iter().any(|e| e.kind == EventKind::TurnEnd);
    let (current, was_open) = cache.map_or((0, false), |c| (c.current_turn_ordinal, c.turn_open));
    let end_only = signals.end && !signals.start;
    let synthesized = end_only
        && !was_open
        && envelope_turn.is_none()
        && events
            .iter()
            .any(|e| !matches!(e.kind, EventKind::TurnEnd | EventKind::SessionEnd));
    let opens = real_open || synthesized;
    let turn = match envelope_turn {
        Some(t) => t,
        None if opens && !was_open => current.saturating_add(1),
        None => current,
    };
    (turn, (was_open || opens) && !ends, synthesized)
}

/// When a closed turn's spend is settled from the transcript — the deferred
/// settle and the hook-stop budget share this one fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleMode {
    /// After an ordinary agent hook for the session. `turn_closed_here` says
    /// whether *this* hook closed a turn (its boundaries carry an `End`); then
    /// nothing is settled, because the agent is still flushing the transcript
    /// the Stop fired from — reading it now is the truncated-turn defect the
    /// old inline settle paid 500 ms to avoid.
    AfterHook {
        /// This hook's ingest closed a turn.
        turn_closed_here: bool,
    },
    /// The session will fire no more hooks (the doctor sweep, `session
    /// stop`): the current turn is settled on its last event whether or not
    /// it ever closed.
    Final,
}

/// What one settle did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// Nothing to do: no cache, no transcript address, no closed turn past the
    /// last settled one, or this hook closed the turn itself.
    Skipped,
    /// The window for `turn` was consumed and the offset moved.
    Recorded {
        /// The turn whose window was consumed.
        turn: u32,
        /// Whether provider-reported usage was found and projected.
        usage_recorded: bool,
        /// The transcript offset the next window starts at.
        next_offset: u64,
    },
}

/// The most a settle reads in one window. A turn longer than this under-sums
/// honestly: the offset still advances past what was read, so nothing is ever
/// counted twice. Same figure as the retired tail read.
pub const SETTLE_WINDOW_BYTES: u64 = 8 * 1024 * 1024;

/// Settle a closed turn's provider-reported spend from the session's
/// transcript, reading forward from the hook-owned `.usage` sidecar's offset
/// (`session_cache::UsageProgress`).
///
/// **No new table, no new column.** The spend lands through the same
/// `project_usage` / `project_main_context` the Stop-time enrichment used to
/// drive, at the same content-addressed ids: it is anchored on the turn's own
/// `captured_event` row (`turn_end` first, else the turn's last event), so
/// every consumer that reads `turn → model_call → token_usage` sees exactly
/// what it saw before — one hook later. INSERT OR IGNORE plus `settled_turn`
/// make redelivery, a double settle and the sweep idempotent.
///
/// Which turn: after an ordinary hook the closed turn is the one before the
/// open turn (or the current one when no turn is open); under
/// [`SettleMode::Final`] it is the current turn, open or not. A turn at or
/// below `settled_turn`, or turn 0, settles nothing.
///
/// Offsets: no sidecar yet → seed `byte_offset` at the transcript's current
/// length (a late start never back-fills); offset past the file's length (a
/// relocated or rewritten transcript) → reset to the length and count
/// nothing; the read is capped at [`SETTLE_WINDOW_BYTES`].
///
/// # Errors
/// [`rift_core::RiftError`] on a storage failure or an unreadable sidecar;
/// the hook path swallows it.
pub async fn settle_closed_turn_usage(
    store: &impl CanonicalStore,
    git_dir: &Path,
    session: &str,
    owner: &str,
    repo_id: &str,
    mode: SettleMode,
    now_ms: i64,
) -> Result<Settled> {
    use rift_shadow_hooks::session_cache::{
        read_usage_progress, write_usage_progress, UsageProgress, SCHEMA,
    };
    use rift_shadow_hooks::transcript::turn_window_since_offset;

    let Some(cache) = session_cache::read_cache(git_dir, session)? else {
        return Ok(Settled::Skipped);
    };
    let Some(path) = cache.transcript_path.as_deref() else {
        return Ok(Settled::Skipped);
    };
    let path = Path::new(path);
    let len = std::fs::metadata(path).map_or(0, |m| m.len());
    let (mut progress, seeded) = match read_usage_progress(git_dir, session)? {
        Some(p) => (p, false),
        None => (
            UsageProgress {
                schema: SCHEMA,
                session_id: session.to_owned(),
                byte_offset: len,
                settled_turn: 0,
                updated_at_ms: now_ms,
            },
            true,
        ),
    };

    let turn = match mode {
        SettleMode::AfterHook {
            turn_closed_here: true,
        } => None,
        SettleMode::AfterHook { .. } if cache.turn_open => {
            cache.current_turn_ordinal.checked_sub(1)
        }
        SettleMode::AfterHook { .. } | SettleMode::Final => Some(cache.current_turn_ordinal),
    }
    .filter(|t| *t > 0 && *t > progress.settled_turn);
    let Some(turn) = turn else {
        if seeded {
            write_usage_progress(git_dir, &progress)?;
        }
        return Ok(Settled::Skipped);
    };

    // A relocated or rewritten transcript: never re-count, start over at
    // its end.
    let offset = progress.byte_offset.min(len);
    let bytes = read_span(
        path,
        offset,
        len.saturating_sub(offset).min(SETTLE_WINDOW_BYTES),
    );
    let window = turn_window_since_offset(&bytes, 0);
    let next_offset = offset.saturating_add(window.next_offset);

    let mut usage_recorded = false;
    if window.usage.is_present() || window.main_context.is_some() {
        if let Some(anchor) = turn_anchor(store, session, turn).await? {
            let facts = crate::project::PayloadFacts {
                model: window.model.clone(),
                model_source: window.model.is_some().then(|| {
                    rift_shadow_hooks::ModelSource::RuntimeHook
                        .as_str()
                        .to_owned()
                }),
                tool_name: None,
                child_session: None,
                read_path: None,
                read_content: None,
            };
            let ctx = crate::project::ProjectionCtx {
                agent: owner,
                repo_id,
            };
            if window.usage.is_present() {
                crate::project::project_usage(store, &anchor, &facts, ctx, &window.usage, now_ms)
                    .await?;
                usage_recorded = true;
            }
            if let Some(context) = &window.main_context {
                crate::project::project_main_context(
                    store,
                    &anchor,
                    "Stop",
                    &facts,
                    ctx,
                    &context.usage,
                )
                .await?;
            }
        }
    }

    progress.byte_offset = next_offset;
    progress.settled_turn = turn;
    progress.updated_at_ms = now_ms;
    write_usage_progress(git_dir, &progress)?;
    Ok(Settled::Recorded {
        turn,
        usage_recorded,
        next_offset,
    })
}

/// `count` bytes of `path` from `offset`; empty on any I/O failure (a settle
/// that cannot read counts nothing and says so through the offset it leaves).
fn read_span(path: &Path, offset: u64, count: u64) -> Vec<u8> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    if f.seek(SeekFrom::Start(offset)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if f.take(count).read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    buf
}

/// The event a turn's settled spend anchors on: its `turn_end` row when one
/// was captured, else the turn's last event — rebuilt as a synthetic
/// [`CanonicalEvent`] so the projection derives the same ids the Stop-time
/// path would have. The synthetic session-start an End-only adapter gets
/// (ordinal `u32::MAX`) is not a spend anchor.
async fn turn_anchor(
    store: &impl CanonicalStore,
    session: &str,
    turn: u32,
) -> Result<Option<rift_shadow_hooks::CanonicalEvent>> {
    let rows = store
        .query(
            "SELECT event_ordinal, occurred_at_ms, payload_sha256, schema_version \
             FROM captured_event \
             WHERE session_id = ? AND turn_ordinal = ? AND kind != 'session_start' \
             ORDER BY (kind = 'turn_end') DESC, event_ordinal DESC LIMIT 1",
            &[Value::text(session.to_owned()), Value::Int(i64::from(turn))],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let ordinal = u32::try_from(row.get_i64(0)?).unwrap_or(u32::MAX);
    let occurred_at_ms = row.get_i64(1)?;
    let payload_sha256 = rift_core::Sha256Id::parse_hex(&row.get_text(2)?)?;
    let schema_version = u16::try_from(row.get_i64(3)?).unwrap_or(1);
    Ok(Some(rift_shadow_hooks::CanonicalEvent {
        schema_version,
        session: session.to_owned(),
        turn_ordinal: turn,
        event_ordinal: ordinal,
        kind: EventKind::TurnEnd,
        trust: Trust::TranscriptUntrusted,
        occurred_at_ms,
        payload_sha256,
    }))
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

    /// The outcome names every read fact this hook recorded, at the row the
    /// projector put it on, and says whether its count is still owed — what
    /// the hook path journals for the detached worker.
    #[tokio::test]
    async fn the_outcome_carries_the_reads_row_id_and_whether_a_count_is_owed() {
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let mut read = raw("PostToolUse", 1);
        read.payload =
            serde_json::json!({"tool_name": "Read", "tool_input": {"file_path": "src/lib.rs"}});
        let hasher = |path: &str| {
            Some(crate::project::ReadContent {
                rel_path: path.to_owned(),
                blob_oid: "oid-1".into(),
                content_sha256: "aa11".into(),
                tokens: None,
            })
        };
        let out = ingest_hook(
            &read,
            &reg,
            &s,
            tmp.path(),
            "claude-code",
            "repo-1",
            10,
            Some(&hasher),
        )
        .await
        .unwrap();
        let session_row = crate::ids::session_row_id("claude-code", "01JSESSION");
        assert_eq!(
            out.read_calls,
            vec![RecordedRead {
                tool_call_id: crate::ids::read_call_row_id(&session_row, 1, "src/lib.rs", "aa11"),
                rel_path: "src/lib.rs".into(),
                blob_oid: "oid-1".into(),
                content_sha256: "aa11".into(),
                tokens_recorded: false,
                lines: None,
                schema_version: 1,
            }]
        );
        let rows = s
            .query("SELECT tool_call_id FROM tool_call", &[])
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_text(0).unwrap(), out.read_calls[0].tool_call_id);

        // A hook with no read fact owes nothing.
        let out = ingest_hook(
            &raw("PreToolUse", 1),
            &reg,
            &s,
            tmp.path(),
            "claude-code",
            "repo-1",
            11,
            Some(&hasher),
        )
        .await
        .unwrap();
        assert!(out.read_calls.is_empty());
    }

    /// A Codex session's reads are command lines: the family has no read tool
    /// at all, so every `cat` it ran was a file's worth of tokens the
    /// projection never saw and the savings denominator never counted. A
    /// command the pinned grammar recognises lands the same read fact a read
    /// tool's does — same row shape, same owed count — and the tool call it
    /// hangs on names the shell, which is what tells the two apart later
    /// without a column that does not exist.
    #[tokio::test]
    async fn a_recognised_shell_read_lands_as_a_read_fact_with_its_tokens() {
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let mut read = raw("PostToolUse", 1);
        read.agent = AgentId::CODEX;
        read.payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": "cat src/lib.rs"},
        });
        let hasher = |path: &str| {
            Some(crate::project::ReadContent {
                rel_path: path.to_owned(),
                blob_oid: "oid-7".into(),
                content_sha256: "cc77".into(),
                tokens: None,
            })
        };
        let out = ingest_hook(
            &read,
            &reg,
            &s,
            tmp.path(),
            "codex",
            "repo-1",
            10,
            Some(&hasher),
        )
        .await
        .unwrap();
        let session_row = crate::ids::session_row_id("codex", "01JSESSION");
        assert_eq!(
            out.read_calls,
            vec![RecordedRead {
                tool_call_id: crate::ids::read_call_row_id(&session_row, 1, "src/lib.rs", "cc77"),
                rel_path: "src/lib.rs".into(),
                blob_oid: "oid-7".into(),
                content_sha256: "cc77".into(),
                tokens_recorded: false,
                // A bare `cat` reaches the end of the file, so the range the
                // worker prices clamps to the file itself (`HiKind::END_OF_FILE`).
                lines: Some((1, u32::MAX)),
                schema_version: 1,
            }]
        );
        let rows = s
            .query(
                "SELECT tc.tool_name FROM tool_call tc \
                 JOIN tool_reads_file trf ON trf.tool_call_id = tc.tool_call_id",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "the read fact links to its own tool call");
        assert_eq!(
            rows[0].get_text(0).unwrap(),
            "Bash",
            "the existing tool_name column is what separates a shell read from a read tool"
        );
    }

    /// The range the command named travels with the fact, so the count the
    /// worker owes is the file's tokens over *those* lines. `sed -n '40,80p'`
    /// asked for forty-one lines; pricing it at the whole file would put
    /// tokens in the denominator that never reached a context window.
    #[tokio::test]
    async fn a_ranged_shell_read_carries_its_lines_to_the_count() {
        let s = store();
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let mut read = raw("PostToolUse", 1);
        read.agent = AgentId::CODEX;
        read.payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": "sed -n '40,80p' src/lib.rs"},
        });
        let hasher = |path: &str| {
            Some(crate::project::ReadContent {
                rel_path: path.to_owned(),
                blob_oid: "oid-8".into(),
                content_sha256: "dd88".into(),
                tokens: None,
            })
        };
        let out = ingest_hook(
            &read,
            &reg,
            &s,
            tmp.path(),
            "codex",
            "repo-1",
            10,
            Some(&hasher),
        )
        .await
        .unwrap();
        assert_eq!(out.read_calls.len(), 1);
        assert_eq!(out.read_calls[0].lines, Some((40, 80)));
    }

    /// A command line names its path the way its shell read it — relative to
    /// the agent's directory, which need not be the work-tree root the
    /// hasher resolves against. The payload's own directory is what closes
    /// that gap; without one the path is left alone rather than guessed at.
    #[tokio::test]
    async fn a_shell_read_is_addressed_from_the_directory_the_payload_names() {
        let reg = AdapterRegistry::with_builtin();
        let seen = std::sync::Mutex::new(Vec::<String>::new());
        let hasher = |path: &str| {
            seen.lock().ok()?.push(path.to_owned());
            Some(crate::project::ReadContent {
                rel_path: "crates/a/src/mod.rs".into(),
                blob_oid: "oid-3".into(),
                content_sha256: "ff33".into(),
                tokens: None,
            })
        };
        let s = store();
        let tmp = tempfile::tempdir().unwrap();
        let mut read = raw("PostToolUse", 1);
        read.agent = AgentId::CODEX;
        read.payload = serde_json::json!({
            "tool_name": "Bash",
            "cwd": "/repo/crates/a/src",
            "tool_input": {"command": "cat mod.rs"},
        });
        ingest_hook(
            &read,
            &reg,
            &s,
            tmp.path(),
            "codex",
            "repo-1",
            10,
            Some(&hasher),
        )
        .await
        .unwrap();
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["/repo/crates/a/src/mod.rs"],
            "a bare name in a subdirectory is not a file at the root"
        );
    }

    /// Everything the grammar does not spell leaves no fact at all, and so
    /// does everything it spells as a read whose lines this layer cannot name.
    /// A member that is not a read makes the whole line a small program whose
    /// effect nobody here can read; a `grep` prints what matched; a bare
    /// `head` relies on a default nothing pins; and a counted `tail` names its
    /// lines only against a length this layer never learns, because the
    /// hashing callback it has answers with an identity and not a line count.
    /// The direction is deliberate: an uncounted read makes the savings share
    /// look larger than it is, which is a shortfall someone can find, where an
    /// invented one is a claim nobody can check.
    #[tokio::test]
    async fn a_command_whose_lines_are_not_derivable_leaves_no_read_fact() {
        for command in [
            "cat src/lib.rs && touch done",
            "cat src/lib.rs | grep needle",
            "tail -n 5 src/lib.rs",
            "head src/lib.rs",
            "grep needle src/lib.rs",
            // A line that only labels its own output read nothing to book.
            "echo done",
        ] {
            let (store, out) = ingest_command(command).await;
            assert!(
                out.read_calls.is_empty(),
                "`{command}` is outside the grammar or names no derivable range"
            );
            assert_eq!(
                store.count("tool_reads_file").await.unwrap(),
                0,
                "{command}"
            );
        }
    }

    /// A command line that reads several files leaves **one** fact, for the
    /// first of them.
    ///
    /// This row is one path and one range on one tool call, and a second read
    /// would need a second row — which is a schema question, not a recognition
    /// one. The read gate is where every member of such a line is accounted
    /// for; here the first is recorded and the rest cost a saving, which is
    /// the same direction every other absence on this path takes.
    #[tokio::test]
    async fn a_command_line_of_several_reads_books_the_first_of_them() {
        for (command, path, lines) in [
            ("cat src/lib.rs | head -n 20", "src/lib.rs", (1, 20)),
            ("cat src/a.rs src/b.rs", "src/a.rs", (1, u32::MAX)),
            (
                "cd crates && sed -n '4,9p' a.rs && cat b.rs",
                "crates/a.rs",
                (4, 9),
            ),
            // A label between the reads is not one of them.
            (
                "cd crates && echo \"=== a ===\" && sed -n '4,9p' a.rs && echo && cat b.rs",
                "crates/a.rs",
                (4, 9),
            ),
        ] {
            let (store, out) = ingest_command(command).await;
            assert_eq!(out.read_calls.len(), 1, "`{command}` books one read");
            assert_eq!(out.read_calls[0].rel_path, path, "{command}");
            assert_eq!(out.read_calls[0].lines, Some(lines), "{command}");
            assert_eq!(
                store.count("tool_reads_file").await.unwrap(),
                1,
                "{command}"
            );
        }
    }

    /// One Codex `Bash` result through the ingest, with a hasher that resolves
    /// whatever path it is handed.
    async fn ingest_command(command: &str) -> (impl CanonicalStore, IngestOutcome) {
        let reg = AdapterRegistry::with_builtin();
        let hasher = |path: &str| {
            Some(crate::project::ReadContent {
                rel_path: path.to_owned(),
                blob_oid: "oid-9".into(),
                content_sha256: "ee99".into(),
                tokens: None,
            })
        };
        let s = store();
        let tmp = tempfile::tempdir().unwrap();
        let mut read = raw("PostToolUse", 1);
        read.agent = AgentId::CODEX;
        read.payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": command},
        });
        let out = ingest_hook(
            &read,
            &reg,
            &s,
            tmp.path(),
            "codex",
            "repo-1",
            10,
            Some(&hasher),
        )
        .await
        .unwrap();
        (s, out)
    }

    #[test]
    fn trigger_mapping() {
        assert_eq!(trigger_for(EventKind::SessionStart), Some(Trigger::Start));
        assert_eq!(trigger_for(EventKind::SessionEnd), Some(Trigger::End));
        assert_eq!(trigger_for(EventKind::ToolCall), None);
    }

    /// 2c: the transcript's ADDRESS rides the cache — last report wins, and a
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
        let sessionless = RawHook {
            agent: AgentId::CLAUDE_CODE,
            verb: "PreToolUse".into(),
            session: String::new(),
            turn: None,
            occurred_at_ms: 5,
            payload: serde_json::json!({"tool_name": "Bash"}),
        };

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

        // Replay stays a no-op (the receipt is content-addressed), while a
        // different session-less payload is its own visible loss.
        ingest_hook(
            &sessionless,
            &reg,
            &s,
            git_dir,
            "claude-code",
            "repo-1",
            6,
            None,
        )
        .await
        .unwrap();
        assert_eq!(s.count("agent_hook_event").await.unwrap(), 1);
        let other = RawHook {
            payload: serde_json::json!({"tool_name": "Read"}),
            ..sessionless
        };
        ingest_hook(&other, &reg, &s, git_dir, "claude-code", "repo-1", 7, None)
            .await
            .unwrap();
        assert_eq!(s.count("agent_hook_event").await.unwrap(), 2);
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
        let reg = AdapterRegistry::with_builtin();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        let real_id = "b0ff98c0-8e01-4b73-bf92-9649b139931b";

        // A real Copilot session binds and captures — the row the previous
        // cycle called unverifiable and shipped session-less.
        let real = rift_shadow_hooks::build_raw_hook(
            "copilot-cli",
            "session_start",
            format!(r#"{{"sessionId":"{real_id}","cwd":"/repo","source":"new"}}"#).as_bytes(),
            5,
        )
        .unwrap();
        assert_eq!(real.session, real_id);
        let out = ingest_hook(&real, &reg, &s, git_dir, "copilot-cli", "repo-1", 5, None)
            .await
            .unwrap();
        assert_eq!(out.receipt, HookReceipt::Recorded);

        // The identical hook fired for a subagent turn binds nothing…
        let subagent = rift_shadow_hooks::build_raw_hook(
            "copilot-cli",
            "session_start",
            br#"{"sessionId":"toolu_bdrk_01KqZ8mR3fN2","cwd":"/repo","source":"new"}"#,
            6,
        )
        .unwrap();
        assert_eq!(subagent.session, "");
        let out = ingest_hook(
            &subagent,
            &reg,
            &s,
            git_dir,
            "copilot-cli",
            "repo-1",
            6,
            None,
        )
        .await
        .unwrap();
        assert_eq!(out.receipt, HookReceipt::DeferredNoSession);

        // …so the phantom leaves no marker, no cache and no session row, while
        // the real one keeps all three.
        let names: Vec<String> = std::fs::read_dir(git_dir.join("rift-sessions"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().all(|n| !n.starts_with("toolu_")),
            "a subagent's tool-use id must not name a session file: {names:?}"
        );
        assert!(names.contains(&format!("{real_id}.agent")));
        assert!(names.contains(&format!("{real_id}.json")));
        assert_eq!(
            s.count("session").await.unwrap(),
            1,
            "one real session, no phantom"
        );

        // The refusal is still countable: two receipts, and the deferred one
        // hashes this subagent's own payload rather than a shared null, so a
        // second refused turn is a second row.
        let deferred = s
            .query(
                "SELECT COUNT(*) FROM agent_hook_event WHERE outcome = 'deferred-no-session'",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(deferred[0].get_i64(0).unwrap(), 1);
        let other = rift_shadow_hooks::build_raw_hook(
            "copilot-cli",
            "session_start",
            br#"{"sessionId":"toolu_bdrk_99other","cwd":"/repo","source":"new"}"#,
            7,
        )
        .unwrap();
        ingest_hook(&other, &reg, &s, git_dir, "copilot-cli", "repo-1", 7, None)
            .await
            .unwrap();
        assert_eq!(s.count("agent_hook_event").await.unwrap(), 3);
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
        let both = TurnSignals::BOTH;
        let (t, _, _) = effective_turn(Some(9), &[ev(EventKind::ToolCall)], Some(&cache), both);
        assert_eq!(t, 9);
        // A prompt queued while the turn is open stays in the open turn.
        let (t, open, _) = effective_turn(None, &[ev(EventKind::PromptSubmit)], Some(&cache), both);
        assert_eq!((t, open), (4, true));
        // No cache at all: the first prompt opens turn 1.
        let (t, open, _) = effective_turn(None, &[ev(EventKind::PromptSubmit)], None, both);
        assert_eq!((t, open), (1, true));
        // A stray turn-end with nothing open stays honest at 0, closed.
        let (t, open, _) = effective_turn(None, &[ev(EventKind::TurnEnd)], None, both);
        assert_eq!((t, open), (0, false));
    }

    /// **An End-only adapter gets a synthesized Start at session start and at
    /// the next activity after each End** (2026-08-29). Antigravity's `Stop`
    /// closes a turn nothing opened; before this, its ordinal never advanced,
    /// so `record_turn_boundary_snapshot` (insert-or-ignore on
    /// `sha256(session ‖ ordinal ‖ boundary)`) dropped every Stop after the
    /// first. Now the first non-End event of a closed turn opens the next
    /// ordinal and reports the synthesized Start.
    #[test]
    fn an_end_only_adapter_gets_a_synthesized_start_at_session_start_and_next_activity() {
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
        let end_only = TurnSignals {
            start: false,
            end: true,
        };
        // Session start, no cache: opens turn 1 with a synthesized Start.
        let got = effective_turn(None, &[ev(EventKind::SessionStart)], None, end_only);
        assert_eq!(got, (1, true, true));
        // Activity on the open turn rides it — nothing synthesized.
        let mut cache = SessionCache::new("s", "antigravity", "active", 0);
        cache.current_turn_ordinal = 1;
        cache.turn_open = true;
        let got = effective_turn(None, &[ev(EventKind::ToolCall)], Some(&cache), end_only);
        assert_eq!(got, (1, true, false));
        // The End closes it and is never an opener.
        let got = effective_turn(None, &[ev(EventKind::TurnEnd)], Some(&cache), end_only);
        assert_eq!(got, (1, false, false));
        // The next activity after the End opens turn 2 with a synthesized Start…
        cache.turn_open = false;
        let got = effective_turn(None, &[ev(EventKind::ToolCall)], Some(&cache), end_only);
        assert_eq!(got, (2, true, true));
        // …but a lone End on a closed turn, or a SessionEnd, opens nothing.
        let got = effective_turn(None, &[ev(EventKind::TurnEnd)], Some(&cache), end_only);
        assert_eq!(got, (1, false, false));
        let got = effective_turn(None, &[ev(EventKind::SessionEnd)], Some(&cache), end_only);
        assert_eq!(got, (1, false, false));
        // An envelope-carried ordinal is trusted verbatim and never synthesized
        // over: nothing opens (no prompt rode it), nothing is synthesized.
        let got = effective_turn(Some(7), &[ev(EventKind::ToolCall)], Some(&cache), end_only);
        assert_eq!(got, (7, false, false));
    }

    /// **A Start-capable adapter never gets a synthesized Start**: with
    /// `start: true` the only opener is its own prompt/turn-start, exactly as
    /// before — activity on a closed turn stays on the closed ordinal.
    #[test]
    fn a_start_capable_adapter_never_gets_a_synthesized_start() {
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
        for signals in [
            TurnSignals::BOTH,
            TurnSignals {
                start: true,
                end: false,
            },
            TurnSignals {
                start: false,
                end: false,
            },
        ] {
            let got = effective_turn(None, &[ev(EventKind::SessionStart)], None, signals);
            assert_eq!(got, (0, false, false), "{signals:?}");
            let got = effective_turn(None, &[ev(EventKind::ToolCall)], None, signals);
            assert_eq!(got, (0, false, false), "{signals:?}");
            let got = effective_turn(None, &[ev(EventKind::PromptSubmit)], None, signals);
            assert_eq!(got, (1, true, false), "{signals:?}");
        }
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

#[cfg(test)]
mod settle_tests {
    use super::*;
    use rift_shadow_hooks::AgentId;
    use rift_store::SqliteStore;

    const SESSION: &str = "01JSETTLE";

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    async fn hook(store: &SqliteStore, git_dir: &Path, verb: &str, payload: serde_json::Value) {
        let raw = RawHook {
            agent: AgentId::CLAUDE_CODE,
            verb: verb.into(),
            session: SESSION.into(),
            turn: None,
            occurred_at_ms: 1,
            payload,
        };
        ingest_hook(
            &raw,
            &AdapterRegistry::with_builtin(),
            store,
            git_dir,
            "claude-code",
            "repo-1",
            10,
            None,
        )
        .await
        .unwrap();
    }

    fn line(kind: &str, input: u64) -> String {
        match kind {
            "prompt" => serde_json::json!({"type": "user", "message": {"content": "go"}}),
            _ => serde_json::json!({
                "type": "assistant",
                "isSidechain": kind == "side",
                "message": {"model": "claude-fable-5", "usage": {"input_tokens": input, "output_tokens": 1}}
            }),
        }
        .to_string()
    }

    fn write_lines(path: &Path, lines: &[String]) {
        let mut body = lines.join("\n");
        body.push('\n');
        std::fs::write(path, body).unwrap();
    }

    async fn usage_rows(store: &SqliteStore) -> Vec<(String, i64)> {
        store
            .query(
                "SELECT call_id, uncached_input FROM token_usage ORDER BY call_id",
                &[],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get_text(0).unwrap(), r.get_i64(1).unwrap()))
            .collect()
    }

    async fn event_ordinal(store: &SqliteStore, turn: u32, kind: &str) -> u32 {
        let rows = store
            .query(
                "SELECT event_ordinal FROM captured_event WHERE session_id = ? AND turn_ordinal = ? \
                 AND kind = ? ORDER BY event_ordinal DESC LIMIT 1",
                &[
                    Value::text(SESSION.to_owned()),
                    Value::Int(i64::from(turn)),
                    Value::text(kind.to_owned()),
                ],
            )
            .await
            .unwrap();
        u32::try_from(rows[0].get_i64(0).unwrap()).unwrap()
    }

    async fn settle(store: &SqliteStore, git_dir: &Path, mode: SettleMode) -> Settled {
        settle_closed_turn_usage(store, git_dir, SESSION, "claude-code", "repo-1", mode, 99)
            .await
            .unwrap()
    }

    /// A whole turn, ingested the way the hooks deliver it: a session start
    /// that seeds the sidecar, a prompt, a tool call, and the Stop. The
    /// transcript is written AFTER the Stop returned — the deferred contract.
    async fn one_turn(store: &SqliteStore, git_dir: &Path, transcript: &Path) {
        let path = transcript.display().to_string();
        hook(
            store,
            git_dir,
            "SessionStart",
            serde_json::json!({"transcript_path": path}),
        )
        .await;
        settle(
            store,
            git_dir,
            SettleMode::AfterHook {
                turn_closed_here: false,
            },
        )
        .await;
        hook(
            store,
            git_dir,
            "UserPromptSubmit",
            serde_json::json!({"prompt": "go"}),
        )
        .await;
        hook(
            store,
            git_dir,
            "PreToolUse",
            serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}),
        )
        .await;
        hook(
            store,
            git_dir,
            "Stop",
            serde_json::json!({"stop_hook_active": false}),
        )
        .await;
        write_lines(
            transcript,
            &[line("prompt", 0), line("side", 100), line("main", 20)],
        );
    }

    /// The load-bearing claim: the spend lands at the Stop event's OWN ids —
    /// `model_call_id(session_row, turn, event_ordinal-of-turn_end)` — so
    /// every reader of `turn → model_call → token_usage` sees what the
    /// inline settle used to write, one hook later.
    #[tokio::test]
    async fn spend_lands_at_the_stop_events_own_ids() {
        let s = store();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join("git");
        let transcript = tmp.path().join("t.jsonl");
        one_turn(&s, &git_dir, &transcript).await;
        assert!(
            usage_rows(&s).await.is_empty(),
            "the Stop itself records no spend"
        );

        // The next hook for the session settles turn 1.
        hook(
            &s,
            &git_dir,
            "UserPromptSubmit",
            serde_json::json!({"prompt": "again"}),
        )
        .await;
        let done = settle(
            &s,
            &git_dir,
            SettleMode::AfterHook {
                turn_closed_here: false,
            },
        )
        .await;
        let transcript_len = std::fs::metadata(&transcript).unwrap().len();
        assert_eq!(
            done,
            Settled::Recorded {
                turn: 1,
                usage_recorded: true,
                next_offset: transcript_len
            }
        );
        let stop = event_ordinal(&s, 1, "turn_end").await;
        let session_row = crate::ids::session_row_id("claude-code", SESSION);
        let call = crate::ids::model_call_id(&session_row, 1, stop);
        assert_eq!(usage_rows(&s).await, vec![(call.clone(), 120)]);
        let turns = s
            .query(
                "SELECT turn_id, model FROM model_call WHERE call_id = ?",
                &[Value::text(call)],
            )
            .await
            .unwrap();
        assert_eq!(
            turns[0].get_text(0).unwrap(),
            crate::ids::turn_row_id(&session_row, 1)
        );
        assert_eq!(turns[0].get_text(1).unwrap(), "claude-fable-5");
        let context = s
            .query("SELECT uncached_input FROM main_context_observation", &[])
            .await
            .unwrap();
        assert_eq!(context.len(), 1, "one context sample rides the same anchor");
        assert_eq!(context[0].get_i64(0).unwrap(), 20);
        let progress = session_cache::read_usage_progress(&git_dir, SESSION)
            .unwrap()
            .unwrap();
        assert_eq!(
            (progress.byte_offset, progress.settled_turn),
            (transcript_len, 1)
        );
    }

    /// The Stop is the one hook that must NOT settle the turn it closed: the
    /// agent is still writing the transcript it fired from.
    #[tokio::test]
    async fn a_hook_that_closes_a_turn_never_settles_it() {
        let s = store();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join("git");
        let transcript = tmp.path().join("t.jsonl");
        one_turn(&s, &git_dir, &transcript).await;
        let done = settle(
            &s,
            &git_dir,
            SettleMode::AfterHook {
                turn_closed_here: true,
            },
        )
        .await;
        assert_eq!(done, Settled::Skipped);
        assert!(usage_rows(&s).await.is_empty());
        let progress = session_cache::read_usage_progress(&git_dir, SESSION)
            .unwrap()
            .unwrap();
        assert_eq!(
            progress.settled_turn, 0,
            "nothing settled, offset untouched"
        );
        assert_eq!(progress.byte_offset, 0);
    }

    /// Redelivery, a retried hook, the sweep after a hook: the second settle
    /// of the same turn is a no-op and the offset advanced exactly once.
    #[tokio::test]
    async fn a_second_settle_is_a_no_op_and_the_offset_advances_once() {
        let s = store();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join("git");
        let transcript = tmp.path().join("t.jsonl");
        one_turn(&s, &git_dir, &transcript).await;
        let after = SettleMode::AfterHook {
            turn_closed_here: false,
        };
        let first = settle(&s, &git_dir, after).await;
        assert!(
            matches!(first, Settled::Recorded { turn: 1, .. }),
            "{first:?}"
        );
        let rows = usage_rows(&s).await;
        assert_eq!(rows.len(), 1);
        let offset = session_cache::read_usage_progress(&git_dir, SESSION)
            .unwrap()
            .unwrap()
            .byte_offset;

        assert_eq!(settle(&s, &git_dir, after).await, Settled::Skipped);
        assert_eq!(
            settle(&s, &git_dir, SettleMode::Final).await,
            Settled::Skipped
        );
        assert_eq!(usage_rows(&s).await, rows);
        let progress = session_cache::read_usage_progress(&git_dir, SESSION)
            .unwrap()
            .unwrap();
        assert_eq!(progress.byte_offset, offset);
        assert_eq!(progress.settled_turn, 1);
    }

    /// A crashed session: the turn never closed, so there is no `turn_end`
    /// row. The sweep's Final settle anchors the spend on the turn's last
    /// captured event instead of losing it.
    #[tokio::test]
    async fn final_anchors_an_open_turn_on_its_last_event() {
        let s = store();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join("git");
        let transcript = tmp.path().join("t.jsonl");
        let path = transcript.display().to_string();
        hook(
            &s,
            &git_dir,
            "SessionStart",
            serde_json::json!({"transcript_path": path}),
        )
        .await;
        settle(
            &s,
            &git_dir,
            SettleMode::AfterHook {
                turn_closed_here: false,
            },
        )
        .await;
        hook(
            &s,
            &git_dir,
            "UserPromptSubmit",
            serde_json::json!({"prompt": "go"}),
        )
        .await;
        hook(
            &s,
            &git_dir,
            "PreToolUse",
            serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}),
        )
        .await;
        write_lines(&transcript, &[line("prompt", 0), line("main", 7)]);

        // An ordinary hook settles nothing: the turn is still open.
        assert_eq!(
            settle(
                &s,
                &git_dir,
                SettleMode::AfterHook {
                    turn_closed_here: false
                }
            )
            .await,
            Settled::Skipped
        );
        let done = settle(&s, &git_dir, SettleMode::Final).await;
        assert!(
            matches!(
                done,
                Settled::Recorded {
                    turn: 1,
                    usage_recorded: true,
                    ..
                }
            ),
            "{done:?}"
        );
        let last = event_ordinal(&s, 1, "tool_call").await;
        let session_row = crate::ids::session_row_id("claude-code", SESSION);
        assert_eq!(
            usage_rows(&s).await,
            vec![(crate::ids::model_call_id(&session_row, 1, last), 7)]
        );
    }

    /// Two ways an offset can be wrong, and neither re-counts anything: a
    /// late start seeds at the transcript's current length (no back-fill),
    /// and a transcript shorter than the recorded offset (relocated,
    /// rewritten) resets to its length and counts nothing.
    #[tokio::test]
    async fn a_transcript_shorter_than_the_offset_counts_nothing() {
        let s = store();
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join("git");
        let transcript = tmp.path().join("t.jsonl");
        // Content that predates capture: never attributed.
        write_lines(&transcript, &[line("prompt", 0), line("main", 5000)]);
        let before = std::fs::metadata(&transcript).unwrap().len();
        let path = transcript.display().to_string();
        hook(
            &s,
            &git_dir,
            "SessionStart",
            serde_json::json!({"transcript_path": path}),
        )
        .await;
        assert_eq!(
            settle(
                &s,
                &git_dir,
                SettleMode::AfterHook {
                    turn_closed_here: false
                }
            )
            .await,
            Settled::Skipped
        );
        let seeded = session_cache::read_usage_progress(&git_dir, SESSION)
            .unwrap()
            .unwrap();
        assert_eq!(seeded.byte_offset, before, "seeded at the current length");

        hook(
            &s,
            &git_dir,
            "UserPromptSubmit",
            serde_json::json!({"prompt": "go"}),
        )
        .await;
        hook(
            &s,
            &git_dir,
            "Stop",
            serde_json::json!({"stop_hook_active": false}),
        )
        .await;
        // The transcript was replaced by something shorter than the offset.
        write_lines(&transcript, &[line("main", 9)]);
        let shorter = std::fs::metadata(&transcript).unwrap().len();
        assert!(shorter < before);
        let done = settle(
            &s,
            &git_dir,
            SettleMode::AfterHook {
                turn_closed_here: false,
            },
        )
        .await;
        assert_eq!(
            done,
            Settled::Recorded {
                turn: 1,
                usage_recorded: false,
                next_offset: shorter
            }
        );
        assert!(usage_rows(&s).await.is_empty(), "nothing was re-counted");
    }
}

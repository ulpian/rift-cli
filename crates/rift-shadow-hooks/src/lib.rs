//! Hook lifecycle capture: canonical events, agent adapters, the session state
//! machine, and checkpoint strategies.
//!
//! `rift hooks <agent> <verb>` is short-lived: it reads one JSON object on stdin,
//! normalizes it through the agent's [`AgentAdapter`] into zero-or-more
//! [`CanonicalEvent`]s, and exits — never failing the agent. The canonical event
//! vocabulary is agent-independent; each first-party agent maps its native verbs
//! onto it. Events are content-addressed (so a retried hook is idempotent) and
//! their canonical bytes feed the tamper chain via `rift-hashchain`.
//!
//! The [`step`] function is a **pure** session state machine: `(state, trigger)
//! → (next, effects)`. Effects are *requests* (archive/condense) handed to the
//! checkpoint strategy, never side effects inside the transition. Illegal
//! transitions are refused, never panic; the hook boundary records a refusal
//! as its own audit row, so a refused trigger is never silent.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::collections::BTreeMap;

use rift_core::{Result, RiftError, Sha256Id};
use sha2::{Digest, Sha256};

pub mod claim;
pub mod install;
pub mod push;
pub mod run_state;
pub mod session_cache;
pub mod session_id;
pub mod strategy;
pub mod trailers;
pub mod wire;

pub use claim::{claim_session_agent, session_owner, Claim};
pub use install::{
    binary_of_record_path, install_hooks_with, is_executable_file, is_managed_hook,
    is_retired_hook, is_transient, read_binary_of_record, record_binary_of_record, shim_text,
    BinaryOfRecord, BinaryRecord, FamilySites, HookInstall, HookInstallPolicy, HookInstallReport,
    RecordedBy, RefusedBinary, TransientReason, HOOK_NAMES,
};
pub use push::{screen_push, PushDecision, PushRef};
pub use session_cache::{
    cache_path, read_cache, read_cache_or_repair, read_transcript_progress, read_usage_progress,
    torn_caches, write_cache, write_transcript_progress, write_usage_progress, CacheRead,
    SessionCache, TranscriptProgress, UsageProgress, WorktreeObservation, STALE_WRITER_MS,
};
pub use session_id::{is_safe_session_id, MAX_SESSION_ID_BYTES};
pub use strategy::{decide, ArchiveDecision, ArchiveReason, GitEvent, StrategyKind, TriggerCtx};
pub use trailers::{AttributionSummary, Trailers};
pub use wire::{
    build_raw_hook, is_tool_use_id, parse_bounded, schema_json, MAX_DEPTH, MAX_STDIN_BYTES,
    SESSION_ID_FIELDS, TOOL_USE_ID_PREFIX,
};

// Claude Code's transcript-tail usage reader, re-exported so the bin's
// enrichment step reaches it through the existing hooks surface (no new
// bin→adapter crate-graph edge; this crate already depends on the adapter).
pub use rift_agent_claude_code::transcript;
// Codex's session-record reader, re-exported on the same terms: the installer
// reads which client ran a Codex session, and which version, through the
// crate it already depends on rather than a new edge to the adapter.
pub use rift_agent_codex::rollout as codex_rollout;
// Re-exported so the bridge (`rift-shadow`) reads an adapter's declared turn
// signals through this crate rather than growing a `rift-shadow → rift-agent`
// edge (the crate-graph snapshot stays still). The trait itself needs no
// re-export: `AdapterRegistry::get` hands out `&dyn AgentAdapter`, and a
// method call on a trait object resolves without the trait in scope.
pub use rift_agent::TurnSignals;

// The canonical event schema is the stable agent↔ledger contract and lives in
// `rift-core` so the plugin SDK and this ingest layer share one authority.
// Re-exported here so existing `rift_shadow::hooks::…` imports keep resolving.
pub use rift_core::{AgentId, CanonicalEvent, EventKind, Trust};
// `event.leaf()` is derived in the hash layer (kept out of `rift-core`).
pub use rift_hashchain::EventLeaf;

fn sha256(bytes: &[u8]) -> Sha256Id {
    let mut h = Sha256::new();
    h.update(bytes);
    let mut b = [0u8; 32];
    b.copy_from_slice(&h.finalize());
    Sha256Id::from_bytes(b)
}

/// A raw hook invocation: one agent verb + its single JSON payload (read from
/// stdin, never argv).
#[derive(Clone, Debug)]
pub struct RawHook {
    /// The agent that fired.
    pub agent: AgentId,
    /// The agent-native verb.
    pub verb: String,
    /// Session id.
    pub session: String,
    /// Turn ordinal, when the wire carried one. `None` means the agent's
    /// native hook shape carried no turn — the ingest layer derives
    /// the ordinal from the event stream and the session cache.
    pub turn: Option<u32>,
    /// Transaction-time (epoch ms).
    pub occurred_at_ms: i64,
    /// The agent-native payload (already redacted before durable write).
    pub payload: serde_json::Value,
}

// The plugin SDK trait and its companion types live in `rift-agent`.
// Re-exported so callers build the registry from one import surface; the SDK
// adapters have **no** redaction/chain authority — this ingest layer owns it.
/// Alias distinguishing the adapter-facing optional usage report from the
/// four-class `rift_shadow_cost::TokenUsage` (both are in scope in `rift-shadow`).
pub use rift_agent::TokenUsage as AdapterTokenUsage;
pub use rift_agent::{
    idempotency_key, AdapterError, AgentAdapter, CompatNotes, HookFacts, ModelFact, ModelSource,
    TokenUsage,
};

fn payload_hash(payload: &serde_json::Value) -> Sha256Id {
    sha256(&serde_json::to_vec(payload).unwrap_or_default())
}

/// The agents this alpha actually captures from.
///
/// Every adapter stays REGISTERED — all nine keep appearing in listings and in
/// Project Settings, because hiding an integration is a worse answer than
/// naming it as not-yet-supported. What the six outside this list do not get is
/// a written descriptor: `rift_agent_install::native::enable` refuses them.
///
/// **TWO DIFFERENT REASONS PUT AN AGENT OUTSIDE THIS LIST, and conflating them
/// makes one of the sentences false.** Until 2026-08-24 this block said "the
/// reason is a measurement" without qualification. That was true of five rows
/// and is not true of the sixth.
///
/// **1. Five are outside by MEASUREMENT.** The completeness sweep
/// (`tests/adapter_sweep.rs`) established that Cursor, Copilot CLI, OpenCode
/// and Factory Droid each declare a verb surface the real agent never sends —
/// `cursor.verb_surface`, `copilot-cli.verb_surface`, `opencode.verb_surface`,
/// `factory-droid.verb_surface` — so an install that appeared to succeed would
/// capture nothing and report nothing. Ollama is a local model runner rather
/// than a coding assistant and publishes no agent lifecycle at
/// all (`ollama.no_agent_lifecycle`). For these five the refusal FOLLOWS FROM A
/// FACT ABOUT THE AGENT.
///
/// **2. Gemini CLI is outside because the owner narrowed the scope** of this
/// alpha to {claude-code, codex, antigravity}. Gemini's two open gaps —
/// `gemini.model_fact_is_dropped` and `gemini.missing_verbs` — are as real as
/// they were and are **not** the reason: they are ordinary defects that would
/// be fixed if the agent were in scope. Its gap rows are RE-WORDED, never
/// closed, because a gap is not fixed by declaring the agent unsupported.
///
/// It lives here, beside [`AdapterRegistry`], rather than in the installer that
/// enforces it, so the sweep can bind its unsupported-adapter rows to the same
/// list the product refuses on. Two copies of this set would drift, and the
/// first symptom of the drift would be a table describing an agent as
/// not-yet-supported while `enable` cheerfully wired it.
pub const ALPHA_AGENTS: &[&str] = &["claude-code", "codex", "antigravity"];

/// Whether `agent_token` is captured by this alpha ([`ALPHA_AGENTS`]).
#[must_use]
pub fn is_alpha_supported(agent_token: &str) -> bool {
    ALPHA_AGENTS.contains(&agent_token)
}

/// A registry of agent adapters, keyed by the agent's string token so that
/// third-party adapters (open [`AgentId`]) can register alongside first-party ones.
#[derive(Default)]
pub struct AdapterRegistry {
    map: BTreeMap<String, Box<dyn AgentAdapter>>,
    order: Vec<String>,
}

impl AdapterRegistry {
    /// A registry with the built-in first-party adapters (including Antigravity).
    #[must_use]
    pub fn with_builtin() -> Self {
        let mut r = Self::default();
        r.register(Box::new(rift_agent_claude_code::ClaudeCodeAdapter));
        r.register(Box::new(rift_agent_codex::CodexAdapter));
        r.register(Box::new(rift_agent_gemini::GeminiAdapter));
        r.register(Box::new(rift_agent_antigravity::AntigravityAdapter));
        r.register(Box::new(rift_agent_cursor::CursorAdapter));
        r.register(Box::new(rift_agent_copilot::CopilotAdapter));
        r.register(Box::new(rift_agent_opencode::OpenCodeAdapter));
        r.register(Box::new(rift_agent_factory_droid::FactoryDroidAdapter));
        r.register(Box::new(rift_agent_ollama::OllamaAdapter));
        r
    }

    /// Register an adapter (keyed by its [`AgentId`] token).
    pub fn register(&mut self, adapter: Box<dyn AgentAdapter>) {
        let key = adapter.agent_id().as_str().to_owned();
        if !self.map.contains_key(&key) {
            self.order.push(key.clone());
        }
        self.map.insert(key, adapter);
    }

    /// Look up an adapter by agent.
    #[must_use]
    pub fn get(&self, agent: &AgentId) -> Option<&dyn AgentAdapter> {
        self.map.get(agent.as_str()).map(AsRef::as_ref)
    }

    /// Whether an adapter is registered for `agent`.
    #[must_use]
    pub fn contains(&self, agent: &AgentId) -> bool {
        self.map.contains_key(agent.as_str())
    }

    /// The agents this registry can capture, in stable registration/display
    /// order. The built-in order is the Project Settings integrations order;
    /// adding a built-in adapter automatically adds a row.
    #[must_use]
    pub fn agents(&self) -> Vec<AgentId> {
        self.order.iter().map(|k| AgentId::new(k.clone())).collect()
    }
}

/// Dispatch a raw hook through the registry: map the agent's native verb to
/// canonical events via the SDK adapter, then **stamp** the envelope-derived
/// fields (session, ordinals, time, redacted-payload hash) that the SDK has no
/// authority to set. An unknown verb is *not* an error here — it yields an empty
/// event set so the ingest layer records it as a skipped hook receipt (the
/// adapter still errors on it, which the conformance kit asserts).
///
/// # Errors
/// [`RiftError::NotFound`] if no adapter is registered for the agent;
/// [`RiftError::Validity`] if the adapter rejects the payload as malformed.
pub fn dispatch(raw: &RawHook, reg: &AdapterRegistry) -> Result<Vec<CanonicalEvent>> {
    let adapter = reg
        .get(&raw.agent)
        .ok_or_else(|| RiftError::NotFound("unknown agent".into()))?;
    let mut events = match adapter.map(&raw.verb, &raw.payload) {
        Ok(events) => events,
        // Unknown verb → recorded upstream as "skipped-unknown-verb" (never a hard fail).
        Err(AdapterError::UnknownVerb(_)) => Vec::new(),
        Err(e @ AdapterError::MalformedPayload(_)) => {
            return Err(RiftError::validity(format!("adapter: {e}")))
        }
    };
    let hash = payload_hash(&raw.payload);
    for (i, ev) in events.iter_mut().enumerate() {
        ev.session.clone_from(&raw.session);
        // A wire with no turn stamps 0 here; the ingest layer re-stamps with
        // the derived ordinal (it owns the session cache this needs).
        ev.turn_ordinal = raw.turn.unwrap_or(0);
        ev.event_ordinal = u32::try_from(i).unwrap_or(0);
        ev.occurred_at_ms = raw.occurred_at_ms;
        ev.payload_sha256 = hash;
    }
    Ok(events)
}

// --- session state machine --------------------------------------------------

/// The session lifecycle states.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionState {
    /// No active session.
    Idle,
    /// A session is active.
    Active,
    /// A context compaction is in progress.
    Compacting,
    /// The session is flushing/condensing.
    Ending,
    /// Terminal: checkpoint condensed.
    Ended,
}

impl SessionState {
    /// The canonical lowercase token used in the cache + `session_transition` audit.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Active => "active",
            Self::Compacting => "compacting",
            Self::Ending => "ending",
            Self::Ended => "ended",
        }
    }
}

/// Triggers that drive the state machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
    /// A session began (`rift_core::EventKind::SessionStart`, `session_start`).
    Start,
    /// A captured hook arrived that is none of the other triggers: a prompt,
    /// a tool call, a turn end. It is what proves an agent is working in a
    /// session whose recorded phase says otherwise.
    Activity,
    /// A context compaction began (`session_compaction`).
    Compaction,
    /// Compaction finished.
    CompactionDone,
    /// A session ended (`session_end`) — the agent's `Stop`/`SessionEnd` verb.
    End,
    /// Idle timeout.
    IdleTimeout,
    /// Condense finished.
    CondenseDone,
}

impl Trigger {
    /// The canonical lowercase token used in the `session_transition` audit
    /// (`trigger_kind` column).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Activity => "activity",
            Self::Compaction => "compaction",
            Self::CompactionDone => "compaction-done",
            Self::End => "end",
            Self::IdleTimeout => "idle-timeout",
            Self::CondenseDone => "condense-done",
        }
    }
}

/// A requested side effect (handed to the checkpoint strategy, not run here).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    /// Open the out-of-tree state file.
    OpenStateFile,
    /// Mark a compaction boundary.
    MarkCompactionBoundary,
    /// Request a checkpoint archive.
    RequestArchive,
    /// Request a condense to the permanent branch.
    RequestCondense,
    /// Close the state file.
    CloseStateFile,
}

/// The result of a transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    /// The next state.
    pub next: SessionState,
    /// Effects to request.
    pub effects: Vec<Effect>,
}

/// The pure session state machine: `(state, trigger) → (next, effects)`.
///
/// **No state is a trap.** `Idle`, `Ending` and `Ended` are all left by the
/// next sign of life: a `Start` or an `Activity` returns the session to
/// `Active`. An agent resumes a session id it already ended (an editor
/// restart ends every tab and resumes each under the same id), launches
/// before capture existed, or has its start hook skipped by the client, and
/// in every one of those cases the events keep arriving. A phase that refused
/// them would describe a busy session as over, and everything derived from
/// the phase (the dashboard, the trailer's session pick, the condense) would
/// be wrong for as long as the agent kept working. `Ended` is therefore not
/// terminal: a session id can outlive its first checkpoint, and a re-opened
/// session condenses again at its next end. A `Start`, or the first activity
/// of a session never opened, asks for the state file; activity that merely
/// returns an ending or ended session to work asks for nothing.
///
/// `(Ending, IdleTimeout)` stays illegal, which is what keeps a repeated sweep
/// from asking for a second condense, and nothing but `CondenseDone` leads
/// into `Ended`, so a session claims to be ended only once a checkpoint exists.
///
/// # Errors
/// [`RiftError::Validity`] for an illegal transition (recorded as a refusal at
/// the hook boundary; never panics, never aborts the agent).
pub fn step(state: SessionState, trigger: Trigger) -> Result<Transition> {
    use SessionState::{Active, Compacting, Ended, Ending, Idle};
    use Trigger::{Activity, Compaction, CompactionDone, CondenseDone, End, IdleTimeout, Start};
    let t = |next, effects: &[Effect]| Transition {
        next,
        effects: effects.to_vec(),
    };
    Ok(match (state, trigger) {
        (Idle | Ending | Ended, Start) | (Idle, Activity) => t(Active, &[Effect::OpenStateFile]),
        (Ending | Ended, Activity) | (Compacting, CompactionDone) => t(Active, &[]),
        (Active, Compaction) => t(Compacting, &[Effect::MarkCompactionBoundary]),
        // A crash does not wait for a compaction to finish. `IdleTimeout` means
        // the agent is gone, which is exactly as true mid-compaction as it is
        // mid-turn — and without the `Compacting` arm a session whose agent
        // died between `PreCompact` and its completion could never be finalized
        // at all: it stays `compacting`, which `live_sessions` counts as live,
        // so every later sweep would keep finding it and being refused. `End`
        // is deliberately NOT admitted from `Compacting`: that trigger comes
        // from a real hook, and a compaction that never reports done is a
        // different defect. `(Ending, IdleTimeout)` stays illegal — that is
        // what makes a repeated sweep a no-op instead of a second condense.
        (Active, End | IdleTimeout) | (Compacting, IdleTimeout) => {
            t(Ending, &[Effect::RequestCondense])
        }
        (Ending, CondenseDone) => t(Ended, &[Effect::CloseStateFile]),
        (s, trigger) => {
            return Err(RiftError::validity(format!(
                "illegal session transition from {s:?} on {trigger:?}"
            )))
        }
    })
}

// Checkpoint strategies live in [`strategy`] (the four-strategy pure `decide`).

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(verb: &str, payload: serde_json::Value) -> RawHook {
        RawHook {
            agent: AgentId::CLAUDE_CODE,
            verb: verb.into(),
            session: "01JSESSION".into(),
            turn: Some(1),
            occurred_at_ms: 1000,
            payload,
        }
    }

    #[test]
    fn claude_code_pretooluse_maps_to_tool_call_and_file_edit() {
        let reg = AdapterRegistry::with_builtin();
        let events = dispatch(
            &raw("PreToolUse", serde_json::json!({"tool_name": "Edit"})),
            &reg,
        )
        .unwrap();
        let kinds: Vec<_> = events.iter().map(|e| e.kind).collect();
        assert_eq!(kinds, vec![EventKind::ToolCall, EventKind::FileEdit]);
    }

    #[test]
    fn session_start_turn_end_session_end_map() {
        let reg = AdapterRegistry::with_builtin();
        assert_eq!(
            dispatch(&raw("SessionStart", serde_json::json!({})), &reg).unwrap()[0].kind,
            EventKind::SessionStart
        );
        // `Stop` ends a turn; `SessionEnd` ends the session (distinct canonical kinds).
        assert_eq!(
            dispatch(&raw("Stop", serde_json::json!({})), &reg).unwrap()[0].kind,
            EventKind::TurnEnd
        );
        assert_eq!(
            dispatch(&raw("SessionEnd", serde_json::json!({})), &reg).unwrap()[0].kind,
            EventKind::SessionEnd
        );
    }

    /// The Rift half of replay-safety: the conformance kit proves each
    /// adapter maps deterministically; this proves the stamping *around* it
    /// turns that into stable, distinct idempotency keys — for every verb of
    /// every registered adapter, so a new agent inherits the assertion by being
    /// registered.
    ///
    /// Re-delivery must derive the identical key set (dedup swallows it), and
    /// the events of one hook must never share a key (a `PreToolUse` fanning
    /// out to `ToolCall` + `FileEdit` would otherwise dedup one of them away).
    #[test]
    fn every_registered_adapter_is_replay_safe_through_dispatch() {
        let reg = AdapterRegistry::with_builtin();
        let payload = serde_json::json!({
            "session_id": "replay-session",
            "conversationId": "replay-session",
            "tool_name": "Edit",
            "tool_input": {"file_path": "src/main.rs"},
            "usage": {"input_tokens": 10, "output_tokens": 2},
        });
        let mut mapped_verbs = 0usize;
        for agent in reg.agents() {
            let verbs = reg.get(&agent).map(AgentAdapter::verbs).unwrap_or_default();
            assert!(!verbs.is_empty(), "{agent:?} declares no verbs");
            for verb in verbs {
                let hook = RawHook {
                    agent: agent.clone(),
                    verb: (*verb).to_owned(),
                    session: "replay-session".into(),
                    turn: Some(2),
                    occurred_at_ms: 7,
                    payload: payload.clone(),
                };
                let key_set = |hook: &RawHook| {
                    dispatch(hook, &reg)
                        .unwrap()
                        .iter()
                        .map(|e| {
                            idempotency_key(
                                &hook.agent,
                                &hook.session,
                                &hook.verb,
                                e.turn_ordinal,
                                e.event_ordinal,
                                &e.payload_sha256,
                            )
                            .to_hex()
                        })
                        .collect::<Vec<_>>()
                };
                let keys = key_set(&hook);
                assert_eq!(
                    keys,
                    key_set(&hook),
                    "{}/{verb}: re-delivery moved its keys",
                    agent.as_str()
                );
                let mut unique = keys.clone();
                unique.sort();
                unique.dedup();
                assert_eq!(
                    unique.len(),
                    keys.len(),
                    "{}/{verb}: two events of one hook share a key",
                    agent.as_str()
                );
                mapped_verbs += usize::from(!keys.is_empty());
            }
        }
        assert!(
            mapped_verbs >= reg.agents().len(),
            "every adapter should map at least one of its own verbs"
        );
    }

    #[test]
    fn unknown_verb_is_skipped() {
        let reg = AdapterRegistry::with_builtin();
        assert!(dispatch(&raw("MysteryVerb", serde_json::json!({})), &reg)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn unknown_agent_is_refused() {
        let reg = AdapterRegistry::default(); // no adapters registered
        let r = raw("SessionStart", serde_json::json!({}));
        assert!(matches!(dispatch(&r, &reg), Err(RiftError::NotFound(_))));
    }

    #[test]
    fn tool_output_is_trust_tagged_untrusted() {
        let reg = AdapterRegistry::with_builtin();
        let e = dispatch(
            &raw("PostToolUse", serde_json::json!({"tool_name": "Bash"})),
            &reg,
        )
        .unwrap();
        assert_eq!(e[0].trust, Trust::ToolOutputUntrusted);
    }

    #[test]
    fn retried_hook_is_idempotent_by_content_address() {
        let reg = AdapterRegistry::with_builtin();
        let a = dispatch(
            &raw("UserPromptSubmit", serde_json::json!({"text": "hi"})),
            &reg,
        )
        .unwrap();
        let b = dispatch(
            &raw("UserPromptSubmit", serde_json::json!({"text": "hi"})),
            &reg,
        )
        .unwrap();
        assert_eq!(a[0].payload_sha256, b[0].payload_sha256);
        assert_eq!(a[0].leaf(), b[0].leaf());
    }

    #[test]
    fn canonical_bytes_are_deterministic() {
        let reg = AdapterRegistry::with_builtin();
        let e = &dispatch(&raw("SessionStart", serde_json::json!({})), &reg).unwrap()[0];
        assert_eq!(e.canonical_bytes(), e.canonical_bytes());
    }

    #[test]
    fn legal_transitions() {
        assert_eq!(
            step(SessionState::Idle, Trigger::Start).unwrap().next,
            SessionState::Active
        );
        assert_eq!(
            step(SessionState::Active, Trigger::Compaction)
                .unwrap()
                .next,
            SessionState::Compacting
        );
        assert_eq!(
            step(SessionState::Compacting, Trigger::CompactionDone)
                .unwrap()
                .next,
            SessionState::Active
        );
        let ending = step(SessionState::Active, Trigger::End).unwrap();
        assert_eq!(ending.next, SessionState::Ending);
        assert!(ending.effects.contains(&Effect::RequestCondense));
        assert_eq!(
            step(SessionState::Ending, Trigger::CondenseDone)
                .unwrap()
                .next,
            SessionState::Ended
        );
    }

    #[test]
    fn illegal_transition_is_refused_not_panic() {
        assert!(matches!(
            step(SessionState::Idle, Trigger::End),
            Err(RiftError::Validity(_))
        ));
        assert!(matches!(
            step(SessionState::Ended, Trigger::CondenseDone),
            Err(RiftError::Validity(_))
        ));
    }

    const STATES: [SessionState; 5] = [
        SessionState::Idle,
        SessionState::Active,
        SessionState::Compacting,
        SessionState::Ending,
        SessionState::Ended,
    ];

    const TRIGGERS: [Trigger; 7] = [
        Trigger::Start,
        Trigger::Activity,
        Trigger::Compaction,
        Trigger::CompactionDone,
        Trigger::End,
        Trigger::IdleTimeout,
        Trigger::CondenseDone,
    ];

    /// Every one of the 35 pairs, spelled out: the twelve legal ones with their
    /// exact target and effects, and every other pair refused. A new edge has
    /// to be written here to exist, so the table is the machine's whole
    /// contract. Three of the refusals carry weight elsewhere and are named:
    /// `(Ending, IdleTimeout)` keeps a repeated sweep from a second condense,
    /// `(Compacting, End)` makes a compaction that never reports done look like
    /// the defect it is, and nothing but a condense leads into `Ended`.
    #[test]
    fn every_state_and_trigger_pair_is_either_a_listed_edge_or_refused() {
        use Effect::{CloseStateFile, MarkCompactionBoundary, OpenStateFile, RequestCondense};
        use SessionState::{Active, Compacting, Ended, Ending, Idle};
        use Trigger::{
            Activity, Compaction, CompactionDone, CondenseDone, End, IdleTimeout, Start,
        };
        let legal: [(SessionState, Trigger, SessionState, &[Effect]); 12] = [
            (Idle, Start, Active, &[OpenStateFile]),
            (Idle, Activity, Active, &[OpenStateFile]),
            (Active, Compaction, Compacting, &[MarkCompactionBoundary]),
            (Active, End, Ending, &[RequestCondense]),
            (Active, IdleTimeout, Ending, &[RequestCondense]),
            (Compacting, CompactionDone, Active, &[]),
            (Compacting, IdleTimeout, Ending, &[RequestCondense]),
            (Ending, Start, Active, &[OpenStateFile]),
            (Ending, Activity, Active, &[]),
            (Ending, CondenseDone, Ended, &[CloseStateFile]),
            (Ended, Start, Active, &[OpenStateFile]),
            (Ended, Activity, Active, &[]),
        ];
        for state in STATES {
            for trigger in TRIGGERS {
                let listed = legal
                    .iter()
                    .find(|(s, t, _, _)| *s == state && *t == trigger);
                match (listed, step(state, trigger)) {
                    (Some((_, _, next, effects)), Ok(tr)) => {
                        assert_eq!(tr.next, *next, "{state:?} on {trigger:?}");
                        assert_eq!(tr.effects, effects.to_vec(), "{state:?} on {trigger:?}");
                    }
                    (None, Err(RiftError::Validity(_))) => (),
                    (listed, got) => {
                        panic!("{state:?} on {trigger:?}: listed {listed:?}, got {got:?}")
                    }
                }
            }
        }
        for (state, trigger) in [
            (Ending, IdleTimeout),
            (Compacting, End),
            (Ended, CondenseDone),
        ] {
            assert!(step(state, trigger).is_err(), "{state:?} on {trigger:?}");
        }
        let into_ended = legal.iter().filter(|(_, _, next, _)| *next == Ended);
        assert!(into_ended.map(|(_, t, _, _)| *t).eq([CondenseDone]));
    }

    /// From every state, the triggers a hook can produce reach `ended` through
    /// a condense: no phase is a trap the agent's own `SessionEnd` cannot leave.
    #[test]
    fn ended_is_reachable_from_every_state() {
        for state in STATES {
            let mut at = state;
            for trigger in [
                Trigger::CompactionDone,
                Trigger::Activity,
                Trigger::End,
                Trigger::CondenseDone,
            ] {
                if let Ok(tr) = step(at, trigger) {
                    at = tr.next;
                }
            }
            assert_eq!(at, SessionState::Ended, "from {state:?}");
        }
    }

    /// An agent that died mid-compaction is exactly as dead as one that died
    /// mid-turn, and without this edge the sweep could never finalize it: the
    /// cache stays `compacting`, which counts as live, so every later pass
    /// would find it and refuse it again. `End` stays illegal there — that
    /// trigger comes from a real hook, and a compaction that never reports done
    /// is a different defect.
    #[test]
    fn an_idle_timeout_finalizes_a_session_stuck_mid_compaction() {
        let out = step(SessionState::Compacting, Trigger::IdleTimeout).unwrap();
        assert_eq!(out.next, SessionState::Ending);
        assert!(out.effects.contains(&Effect::RequestCondense));
        assert!(matches!(
            step(SessionState::Compacting, Trigger::End),
            Err(RiftError::Validity(_))
        ));
        // And the sweep stays idempotent: `Ending` has no idle-timeout edge.
        assert!(matches!(
            step(SessionState::Ending, Trigger::IdleTimeout),
            Err(RiftError::Validity(_))
        ));
    }

    #[test]
    fn state_and_trigger_audit_tokens() {
        assert_eq!(SessionState::Active.as_str(), "active");
        assert_eq!(SessionState::Ended.as_str(), "ended");
        assert_eq!(Trigger::CondenseDone.as_str(), "condense-done");
        assert_eq!(Trigger::Start.as_str(), "start");
        assert_eq!(Trigger::Activity.as_str(), "activity");
    }

    proptest::proptest! {
        /// Illegal transitions never panic and always return an error.
        #[test]
        fn step_never_panics(si in 0usize..5, ti in 0usize..7) {
            let _ = step(STATES[si], TRIGGERS[ti]); // must not panic
        }
    }
}

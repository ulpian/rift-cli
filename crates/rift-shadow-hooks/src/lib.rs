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
//! transitions are refused (logged, swallowed at the hook boundary), never panic.
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

pub mod install;
pub mod push;
pub mod session_cache;
pub mod strategy;
pub mod trailers;
pub mod wire;

pub use install::{install_hooks, shim_text, HookInstall, HOOK_NAMES};
pub use push::{screen_push, PushDecision, PushRef};
pub use session_cache::{cache_path, read_cache, write_cache, SessionCache};
pub use strategy::{decide, ArchiveDecision, ArchiveReason, GitEvent, StrategyKind, TriggerCtx};
pub use trailers::{AttributionSummary, Trailers};
pub use wire::{build_raw_hook, parse_bounded, schema_json, MAX_DEPTH, MAX_STDIN_BYTES};

// The canonical event schema is the stable agent↔ledger contract and lives in
// `rift-core` (P8 S2) so the plugin SDK and this ingest layer share one authority.
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
    /// Turn ordinal.
    pub turn: u32,
    /// Transaction-time (epoch ms).
    pub occurred_at_ms: i64,
    /// The agent-native payload (already redacted before durable write).
    pub payload: serde_json::Value,
}

// The plugin SDK trait and its companion types live in `rift-agent` (P8 S2).
// Re-exported so callers build the registry from one import surface; the SDK
// adapters have **no** redaction/chain authority — this ingest layer owns it.
/// Alias distinguishing the adapter-facing optional usage report from the
/// four-class `rift_shadow_cost::TokenUsage` (both are in scope in `rift-shadow`).
pub use rift_agent::TokenUsage as AdapterTokenUsage;
pub use rift_agent::{idempotency_key, AdapterError, AgentAdapter, CompatNotes, TokenUsage};

fn payload_hash(payload: &serde_json::Value) -> Sha256Id {
    sha256(&serde_json::to_vec(payload).unwrap_or_default())
}

/// A registry of agent adapters, keyed by the agent's string token so that
/// third-party adapters (open [`AgentId`]) can register alongside first-party ones.
#[derive(Default)]
pub struct AdapterRegistry {
    map: BTreeMap<String, Box<dyn AgentAdapter>>,
}

impl AdapterRegistry {
    /// A registry with the built-in first-party adapters (the eight P8 S3 agents).
    #[must_use]
    pub fn with_builtin() -> Self {
        let mut r = Self::default();
        r.register(Box::new(rift_agent_claude_code::ClaudeCodeAdapter));
        r.register(Box::new(rift_agent_codex::CodexAdapter));
        r.register(Box::new(rift_agent_gemini::GeminiAdapter));
        r.register(Box::new(rift_agent_cursor::CursorAdapter));
        r.register(Box::new(rift_agent_copilot::CopilotAdapter));
        r.register(Box::new(rift_agent_opencode::OpenCodeAdapter));
        r.register(Box::new(rift_agent_factory_droid::FactoryDroidAdapter));
        r.register(Box::new(rift_agent_ollama::OllamaAdapter));
        r
    }

    /// Register an adapter (keyed by its [`AgentId`] token).
    pub fn register(&mut self, adapter: Box<dyn AgentAdapter>) {
        self.map
            .insert(adapter.agent_id().as_str().to_owned(), adapter);
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

    /// The agents this registry can capture (sorted by token).
    #[must_use]
    pub fn agents(&self) -> Vec<AgentId> {
        self.map.keys().map(|k| AgentId::new(k.clone())).collect()
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
        ev.turn_ordinal = raw.turn;
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
    /// `session.start`.
    Start,
    /// `session.compaction`.
    Compaction,
    /// Compaction finished.
    CompactionDone,
    /// `session.end` / `Stop`.
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
/// # Errors
/// [`RiftError::Validity`] for an illegal transition (logged + swallowed at the
/// hook boundary; never panics, never aborts the agent).
pub fn step(state: SessionState, trigger: Trigger) -> Result<Transition> {
    use SessionState::{Active, Compacting, Ended, Ending, Idle};
    use Trigger::{Compaction, CompactionDone, CondenseDone, End, IdleTimeout, Start};
    let t = |next, effects: &[Effect]| Transition {
        next,
        effects: effects.to_vec(),
    };
    Ok(match (state, trigger) {
        (Idle, Start) => t(Active, &[Effect::OpenStateFile]),
        (Active, Compaction) => t(Compacting, &[Effect::MarkCompactionBoundary]),
        (Compacting, CompactionDone) => t(Active, &[]),
        (Active, End | IdleTimeout) => t(Ending, &[Effect::RequestCondense]),
        (Ending, CondenseDone) => t(Ended, &[Effect::CloseStateFile]),
        (s, _) => {
            return Err(RiftError::validity(format!(
                "illegal session transition from {s:?}"
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
            turn: 1,
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
            step(SessionState::Ended, Trigger::Start),
            Err(RiftError::Validity(_))
        ));
    }

    #[test]
    fn state_and_trigger_audit_tokens() {
        assert_eq!(SessionState::Active.as_str(), "active");
        assert_eq!(SessionState::Ended.as_str(), "ended");
        assert_eq!(Trigger::CondenseDone.as_str(), "condense-done");
        assert_eq!(Trigger::Start.as_str(), "start");
    }

    proptest::proptest! {
        /// Illegal transitions never panic and always return an error.
        #[test]
        fn step_never_panics(si in 0usize..5, ti in 0usize..6) {
            let states = [SessionState::Idle, SessionState::Active, SessionState::Compacting, SessionState::Ending, SessionState::Ended];
            let triggers = [Trigger::Start, Trigger::Compaction, Trigger::CompactionDone, Trigger::End, Trigger::IdleTimeout, Trigger::CondenseDone];
            let _ = step(states[si], triggers[ti]); // must not panic
        }
    }
}

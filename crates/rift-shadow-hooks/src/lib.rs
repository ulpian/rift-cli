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
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![allow(clippy::module_name_repetitions, clippy::doc_markdown)]

use std::collections::BTreeMap;

use rift_core::{Result, RiftError, Sha256Id};
use sha2::{Digest, Sha256};

/// The agent-independent canonical event vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A session began.
    SessionStart,
    /// A session ended.
    SessionEnd,
    /// A context compaction occurred.
    SessionCompaction,
    /// A turn began.
    TurnStart,
    /// A turn ended.
    TurnEnd,
    /// The user submitted a prompt.
    PromptSubmit,
    /// The model produced a response.
    PromptResponse,
    /// A tool was invoked.
    ToolCall,
    /// A tool returned a result.
    ToolResult,
    /// A file was edited.
    FileEdit,
    /// A subagent began.
    SubagentStart,
    /// A subagent ended.
    SubagentEnd,
}

/// Source-trust classification carried on every event payload (so untrusted
/// content is never later served as instruction-shaped text — `05` §5.4/§5.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    /// Structural code fact (file edit).
    CodeFact,
    /// Git fact (commit/branch).
    GitFact,
    /// Test runner output.
    TestOutput,
    /// Arbitrary tool output (untrusted).
    ToolOutputUntrusted,
    /// Transcript / prompt text (untrusted).
    TranscriptUntrusted,
}

/// A normalized, agent-independent capture event. Serialized canonically (fixed
/// field order) for hashing; content-addressed by `payload_sha256` for
/// idempotent re-ingest.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CanonicalEvent {
    /// Contract schema version.
    pub schema: u32,
    /// Session id (native id mapped to a Rift id upstream).
    pub session: String,
    /// Turn ordinal within the session.
    pub turn_ordinal: u32,
    /// Event ordinal within the turn.
    pub event_ordinal: u32,
    /// What happened.
    pub kind: EventKind,
    /// Source-trust class of the payload.
    pub trust: Trust,
    /// Transaction-time (epoch ms).
    pub occurred_at_ms: i64,
    /// Content address of the (redacted) payload.
    pub payload_sha256: Sha256Id,
}

impl CanonicalEvent {
    /// Deterministic canonical bytes (fixed field order) — the leaf preimage.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        // serde_json over a struct emits fields in declaration order → deterministic.
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// The SHA-256 Merkle leaf for this event (domain-separated, substrate-compatible).
    #[must_use]
    pub fn leaf(&self) -> Sha256Id {
        rift_hashchain::leaf_hash(&self.canonical_bytes())
    }
}

fn sha256(bytes: &[u8]) -> Sha256Id {
    let mut h = Sha256::new();
    h.update(bytes);
    let mut b = [0u8; 32];
    b.copy_from_slice(&h.finalize());
    Sha256Id::from_bytes(b)
}

/// A first-party agent identity (closed set; unknown agents are refused).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AgentId {
    /// Claude Code.
    ClaudeCode,
    /// OpenAI Codex.
    Codex,
    /// Cursor.
    Cursor,
    /// GitHub Copilot CLI.
    CopilotCli,
    /// Gemini.
    Gemini,
    /// OpenCode.
    OpenCode,
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

/// An adapter mapping one agent's native verbs onto canonical events.
pub trait AgentAdapter {
    /// The agent this adapter handles.
    fn agent(&self) -> AgentId;
    /// Map one raw hook to zero-or-more canonical events. An unknown verb yields
    /// an empty vec (skipped), never an error.
    ///
    /// # Errors
    /// [`RiftError`] only on a malformed payload that cannot be content-addressed.
    fn normalize(&self, raw: &RawHook) -> Result<Vec<CanonicalEvent>>;
}

fn payload_hash(payload: &serde_json::Value) -> Sha256Id {
    sha256(&serde_json::to_vec(payload).unwrap_or_default())
}

/// The Claude Code adapter.
pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    fn tool_name(raw: &RawHook) -> Option<&str> {
        raw.payload
            .get("tool_name")
            .and_then(serde_json::Value::as_str)
    }
}

impl AgentAdapter for ClaudeCodeAdapter {
    fn agent(&self) -> AgentId {
        AgentId::ClaudeCode
    }

    fn normalize(&self, raw: &RawHook) -> Result<Vec<CanonicalEvent>> {
        let hash = payload_hash(&raw.payload);
        let mut events = Vec::new();
        let push = |kind: EventKind, trust: Trust, events: &mut Vec<CanonicalEvent>| {
            let ordinal = u32::try_from(events.len()).unwrap_or(0);
            events.push(CanonicalEvent {
                schema: 1,
                session: raw.session.clone(),
                turn_ordinal: raw.turn,
                event_ordinal: ordinal,
                kind,
                trust,
                occurred_at_ms: raw.occurred_at_ms,
                payload_sha256: hash,
            });
        };
        match raw.verb.as_str() {
            "SessionStart" => push(EventKind::SessionStart, Trust::GitFact, &mut events),
            "Stop" => push(EventKind::SessionEnd, Trust::GitFact, &mut events),
            "UserPromptSubmit" => push(
                EventKind::PromptSubmit,
                Trust::TranscriptUntrusted,
                &mut events,
            ),
            "PreToolUse" => {
                push(EventKind::ToolCall, Trust::CodeFact, &mut events);
                if matches!(Self::tool_name(raw), Some("Edit" | "Write" | "MultiEdit")) {
                    push(EventKind::FileEdit, Trust::CodeFact, &mut events);
                }
            }
            "PostToolUse" => {
                push(
                    EventKind::ToolResult,
                    Trust::ToolOutputUntrusted,
                    &mut events,
                );
            }
            "SubagentStart" => push(EventKind::SubagentStart, Trust::GitFact, &mut events),
            "SubagentStop" => push(EventKind::SubagentEnd, Trust::GitFact, &mut events),
            _ => {} // unknown verb → skipped, no error
        }
        Ok(events)
    }
}

/// A closed registry of first-party adapters.
#[derive(Default)]
pub struct AdapterRegistry {
    map: BTreeMap<u8, Box<dyn AgentAdapter>>,
}

fn agent_key(a: AgentId) -> u8 {
    match a {
        AgentId::ClaudeCode => 0,
        AgentId::Codex => 1,
        AgentId::Cursor => 2,
        AgentId::CopilotCli => 3,
        AgentId::Gemini => 4,
        AgentId::OpenCode => 5,
    }
}

impl AdapterRegistry {
    /// A registry with the built-in first-party adapters.
    #[must_use]
    pub fn with_builtin() -> Self {
        let mut r = Self::default();
        r.register(Box::new(ClaudeCodeAdapter));
        r
    }

    /// Register an adapter.
    pub fn register(&mut self, adapter: Box<dyn AgentAdapter>) {
        self.map.insert(agent_key(adapter.agent()), adapter);
    }

    /// Look up an adapter by agent.
    #[must_use]
    pub fn get(&self, agent: AgentId) -> Option<&dyn AgentAdapter> {
        self.map.get(&agent_key(agent)).map(AsRef::as_ref)
    }
}

/// Dispatch a raw hook through the registry.
///
/// # Errors
/// [`RiftError::NotFound`] if the agent is unknown (closed registry).
pub fn dispatch(raw: &RawHook, reg: &AdapterRegistry) -> Result<Vec<CanonicalEvent>> {
    let adapter = reg
        .get(raw.agent)
        .ok_or_else(|| RiftError::NotFound("unknown agent".into()))?;
    adapter.normalize(raw)
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

// --- checkpoint strategies --------------------------------------------------

/// How checkpoints are archived.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CheckpointStrategy {
    /// Archive only at `git commit` (never pollutes the active branch). Default.
    #[default]
    ManualCommit,
    /// Archive on every turn boundary (heavier; opt-in).
    PerTurn,
}

impl CheckpointStrategy {
    /// Whether a `post-commit` git event should trigger archival under this
    /// strategy.
    #[must_use]
    pub fn archives_on_commit(self) -> bool {
        matches!(self, Self::ManualCommit)
    }

    /// Whether a turn-end should trigger archival under this strategy.
    #[must_use]
    pub fn archives_on_turn_end(self) -> bool {
        matches!(self, Self::PerTurn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(verb: &str, payload: serde_json::Value) -> RawHook {
        RawHook {
            agent: AgentId::ClaudeCode,
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
    fn session_start_stop_map() {
        let reg = AdapterRegistry::with_builtin();
        assert_eq!(
            dispatch(&raw("SessionStart", serde_json::json!({})), &reg).unwrap()[0].kind,
            EventKind::SessionStart
        );
        assert_eq!(
            dispatch(&raw("Stop", serde_json::json!({})), &reg).unwrap()[0].kind,
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
    fn manual_commit_strategy_archives_on_commit() {
        let s = CheckpointStrategy::default();
        assert_eq!(s, CheckpointStrategy::ManualCommit);
        assert!(s.archives_on_commit());
        assert!(!s.archives_on_turn_end());
        assert!(CheckpointStrategy::PerTurn.archives_on_turn_end());
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

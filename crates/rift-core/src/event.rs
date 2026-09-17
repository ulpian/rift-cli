//! The agent-independent canonical capture event — the stable contract between
//! agents and Rift's ledger.
//!
//! It lives in `rift-core` (not the capture stack) so the plugin SDK
//! (`rift-agent`) and the ingest layer (`rift-shadow-hooks`) share **one schema
//! authority** without either depending on the other or on the heavy capture
//! crates. The type carries a [`schema_version`](CanonicalEvent::schema_version)
//! so the contract can evolve additively.
//!
//! [`CanonicalEvent::canonical_bytes`] is the deterministic leaf preimage (pure
//! serde, fixed field order). The Merkle *leaf* itself is derived in
//! `rift-hashchain` (`EventLeaf::leaf`) so this crate stays free of the hash
//! layer — keeping the dependency graph acyclic (`rift-core` →
//! `rift-hashchain`, never the reverse).

use std::borrow::Cow;

use crate::Sha256Id;

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

impl EventKind {
    /// Every event kind, for `rift hooks --schema` introspection and tests.
    pub const ALL: &'static [EventKind] = &[
        Self::SessionStart,
        Self::SessionEnd,
        Self::SessionCompaction,
        Self::TurnStart,
        Self::TurnEnd,
        Self::PromptSubmit,
        Self::PromptResponse,
        Self::ToolCall,
        Self::ToolResult,
        Self::FileEdit,
        Self::SubagentStart,
        Self::SubagentEnd,
    ];

    /// The canonical snake_case token (matches the serde representation).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "session_start",
            Self::SessionEnd => "session_end",
            Self::SessionCompaction => "session_compaction",
            Self::TurnStart => "turn_start",
            Self::TurnEnd => "turn_end",
            Self::PromptSubmit => "prompt_submit",
            Self::PromptResponse => "prompt_response",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::FileEdit => "file_edit",
            Self::SubagentStart => "subagent_start",
            Self::SubagentEnd => "subagent_end",
        }
    }

    /// Parse the [`EventKind::as_str`] token back to the kind. Total round-trip
    /// with `as_str`; an unknown token is `None`, never a guess.
    ///
    /// **This is the STRICT parser and it stays strict.** `as_str` is the
    /// canonical stored spelling — it is what `captured_event.kind` carries and
    /// what a native `full.jsonl` line records — so a reader that wants to
    /// accept an archived dialect asks [`from_wire_token`](Self::from_wire_token)
    /// instead of loosening this one.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == token)
    }

    /// Parse an **archived** kind token, accepting the dot-separated dialect
    /// alongside the canonical snake_case one.
    ///
    /// The dot spelling is not hypothetical: the committed read-compat fixtures
    /// under `crates/rift-shadow/legacy-fixtures/` record
    /// `"kind":"session.start"`, and read-compat with that dialect is a
    /// standing invariant. [`from_token`](Self::from_token) answers `None` for
    /// it, and the artifact bridge turned that `None` into a dropped record — a
    /// whole dialect discarded into an anonymous counter.
    ///
    /// The rule is one substitution, `.` → `_`, applied only after the exact
    /// match fails, so:
    ///
    /// * a canonical token is never rewritten before it is tried, and
    /// * nothing else is guessed — an unrecognised token is still `None`, which
    ///   is what lets a caller report it by name instead of silently skipping.
    ///
    /// The mapping is injective over [`ALL`](Self::ALL) (no two canonical
    /// tokens differ only in `.` vs `_`), so this can never resolve one wire
    /// token to two kinds; that is asserted, not assumed.
    ///
    /// **Normalisation is inbound only.** Whatever spelling came in, what gets
    /// stored is `as_str` — the wire is a dialect, the store has one spelling.
    #[must_use]
    pub fn from_wire_token(token: &str) -> Option<Self> {
        Self::from_token(token).or_else(|| Self::from_token(&token.replace('.', "_")))
    }

    /// The trust class the first-party ingest assigns to this kind's payload —
    /// the single mapping both event sources share, so a bridge-derived row can
    /// never carry a *higher* trust than its hook-derived twin.
    #[must_use]
    pub fn default_trust(self) -> Trust {
        match self {
            Self::FileEdit => Trust::CodeFact,
            Self::ToolResult => Trust::ToolOutputUntrusted,
            _ => Trust::TranscriptUntrusted,
        }
    }
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

impl Trust {
    /// Every trust class, for `rift hooks --schema` introspection and tests.
    pub const ALL: &'static [Trust] = &[
        Self::CodeFact,
        Self::GitFact,
        Self::TestOutput,
        Self::ToolOutputUntrusted,
        Self::TranscriptUntrusted,
    ];

    /// The canonical snake_case token (matches the serde representation).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CodeFact => "code_fact",
            Self::GitFact => "git_fact",
            Self::TestOutput => "test_output",
            Self::ToolOutputUntrusted => "tool_output_untrusted",
            Self::TranscriptUntrusted => "transcript_untrusted",
        }
    }
}

/// An agent identity. An **open newtype** over a string token so third-party
/// adapters can register their own id, while first-party agents have consts.
///
/// The token is the CLI name used in `rift hooks <agent> <verb>` and
/// `rift enable --agent <agent>` (lowercase, hyphenated).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct AgentId(Cow<'static, str>);

impl AgentId {
    /// Claude Code.
    pub const CLAUDE_CODE: AgentId = AgentId(Cow::Borrowed("claude-code"));
    /// OpenAI Codex / OpenAI CLI.
    pub const CODEX: AgentId = AgentId(Cow::Borrowed("codex"));
    /// Cursor.
    pub const CURSOR: AgentId = AgentId(Cow::Borrowed("cursor"));
    /// GitHub Copilot CLI.
    pub const COPILOT_CLI: AgentId = AgentId(Cow::Borrowed("copilot-cli"));
    /// Gemini CLI.
    pub const GEMINI: AgentId = AgentId(Cow::Borrowed("gemini"));
    /// Google Antigravity IDE/CLI.
    pub const ANTIGRAVITY: AgentId = AgentId(Cow::Borrowed("antigravity"));
    /// OpenCode.
    pub const OPENCODE: AgentId = AgentId(Cow::Borrowed("opencode"));
    /// Factory Droid.
    pub const FACTORY_DROID: AgentId = AgentId(Cow::Borrowed("factory-droid"));
    /// Ollama / open-weights local models (best-effort capture).
    pub const OLLAMA: AgentId = AgentId(Cow::Borrowed("ollama"));

    /// The first-party agent ids (for `rift enable` discovery and help text).
    pub const FIRST_PARTY: &'static [AgentId] = &[
        Self::CLAUDE_CODE,
        Self::CODEX,
        Self::CURSOR,
        Self::COPILOT_CLI,
        Self::GEMINI,
        Self::ANTIGRAVITY,
        Self::OPENCODE,
        Self::FACTORY_DROID,
        Self::OLLAMA,
    ];

    /// Build an agent id from a runtime token (third-party agents).
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(Cow::Owned(token.into()))
    }

    /// The CLI token (`"claude-code"`, …).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether the token is a syntactically valid agent id: a non-empty run of
    /// ASCII lowercase letters, digits, and hyphens (no path/shell metacharacters).
    /// This is a shape check, not a registry check — an unknown-but-well-formed id
    /// is allowed (capture defers if no adapter is registered).
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        let s = self.as_str();
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    }
}

/// A normalized, agent-independent capture event. Serialized canonically (fixed
/// field order) for hashing; content-addressed by `payload_sha256` for
/// idempotent re-ingest.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CanonicalEvent {
    /// Contract schema version (bumped additively; old adapters keep working).
    pub schema_version: u16,
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
    ///
    /// `serde_json` over a struct emits fields in declaration order, so the bytes
    /// are deterministic. The Merkle leaf is derived from these bytes in
    /// `rift-hashchain` (`EventLeaf::leaf`).
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical stored spelling is frozen. This is the pin §1.5 asks for:
    /// every token is written out as a literal, so a "harmless" rename shows up
    /// as a diff on this list rather than as a silently re-spelled ledger.
    #[test]
    fn the_canonical_kind_tokens_are_frozen_snake_case() {
        let tokens: Vec<&str> = EventKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            tokens,
            vec![
                "session_start",
                "session_end",
                "session_compaction",
                "turn_start",
                "turn_end",
                "prompt_submit",
                "prompt_response",
                "tool_call",
                "tool_result",
                "file_edit",
                "subagent_start",
                "subagent_end",
            ]
        );
    }

    /// The wire parser accepts the archived dot dialect and normalises to the
    /// one stored spelling — and it is injective, so a wire token can never
    /// resolve to two kinds.
    #[test]
    fn the_wire_parser_accepts_the_dot_dialect_and_normalises() {
        for k in EventKind::ALL {
            let canonical = k.as_str();
            assert_eq!(EventKind::from_wire_token(canonical), Some(*k));
            let dotted = canonical.replace('_', ".");
            assert_eq!(
                EventKind::from_wire_token(&dotted),
                Some(*k),
                "the archived dialect must resolve: {dotted}"
            );
            // …and it is the STRICT parser that refuses it, which is exactly
            // why the bridge had to change rather than this one.
            assert_eq!(
                EventKind::from_token(&dotted),
                None,
                "from_token stays strict: {dotted}"
            );
        }
        // Injective: no two canonical tokens collapse onto one normal form.
        let mut normals: Vec<String> = EventKind::ALL
            .iter()
            .map(|k| k.as_str().replace('.', "_"))
            .collect();
        normals.sort_unstable();
        let before = normals.len();
        normals.dedup();
        assert_eq!(before, normals.len(), "the normalisation must be injective");
        // Nothing else is guessed.
        assert_eq!(EventKind::from_wire_token("banana"), None);
        assert_eq!(EventKind::from_wire_token("session start"), None);
        assert_eq!(EventKind::from_wire_token(""), None);
    }

    #[test]
    fn agent_id_token_round_trips() {
        assert_eq!(AgentId::CLAUDE_CODE.as_str(), "claude-code");
        assert_eq!(AgentId::GEMINI.as_str(), "gemini");
        assert_eq!(AgentId::new("acme-agent").as_str(), "acme-agent");
        assert_eq!(AgentId::FIRST_PARTY.len(), 9);
    }

    #[test]
    fn agent_id_well_formed_rejects_metacharacters() {
        assert!(AgentId::CLAUDE_CODE.is_well_formed());
        assert!(AgentId::new("acme-agent-2").is_well_formed());
        assert!(!AgentId::new("").is_well_formed());
        assert!(!AgentId::new("../evil").is_well_formed());
        assert!(!AgentId::new("a b").is_well_formed());
        assert!(!AgentId::new("Up").is_well_formed());
    }

    #[test]
    fn canonical_bytes_are_deterministic_and_use_schema_version() {
        let ev = CanonicalEvent {
            schema_version: 1,
            session: "s".into(),
            turn_ordinal: 0,
            event_ordinal: 0,
            kind: EventKind::SessionStart,
            trust: Trust::GitFact,
            occurred_at_ms: 1,
            payload_sha256: Sha256Id::from_bytes([0u8; 32]),
        };
        let bytes = ev.canonical_bytes();
        assert_eq!(bytes, ev.canonical_bytes());
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("\"schema_version\":1"));
        assert!(!s.contains("\"schema\":"));
    }
}

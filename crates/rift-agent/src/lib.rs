//! The Rift **plugin SDK** (P8 S2): the [`AgentAdapter`] trait every agent
//! integration implements, typed event [`builders`](event), an
//! [`idempotency_key`] helper, [`TokenUsage`] extraction types, and a
//! [`conformance`] kit so an adapter can prove its mapping is correct.
//!
//! An adapter is a **stateless, no-authority** mapping: it translates one
//! agent's native hook lifecycle (`verb` + the agent's own JSON payload) into
//! zero-or-more [`CanonicalEvent`]s. It has **no** redaction, hashing, or
//! chain-write authority — the ingest layer (`rift hooks`) owns those. An adapter
//! fills the *structural* fields of an event (`kind`, `trust`, and the schema
//! version); the ingest layer stamps identity/time/content-hash from the hook
//! envelope. This keeps the trust boundary at `rift hooks`, even for third-party
//! adapters that run in their own process.
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

use rift_core::{AgentId, CanonicalEvent, EventKind, Sha256Id, Trust};

pub mod conformance;

/// The canonical-event schema version this SDK builds (bumped additively, P8 S2).
pub const SCHEMA_VERSION: u16 = 1;

/// An error from mapping a native hook to canonical events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdapterError {
    /// A verb the adapter does not understand. Never silently dropped — the
    /// ingest layer records it as a skipped hook (P8 S3: "unknown verbs error").
    UnknownVerb(String),
    /// The native payload could not be interpreted (shape the adapter requires
    /// is absent/ill-typed). Carries a reason only — never payload bytes.
    MalformedPayload(String),
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownVerb(v) => write!(f, "unknown verb: {v}"),
            Self::MalformedPayload(r) => write!(f, "malformed payload: {r}"),
        }
    }
}

impl std::error::Error for AdapterError {}

/// Provider-reported token usage extracted from a native payload (feeds P6's
/// tier-1 verbatim counts). Absent fields stay `None` — adapters must **never**
/// fabricate a count (P6 S1's correctness-honesty rule).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// Input/prompt tokens.
    pub input: Option<u64>,
    /// Output/completion tokens.
    pub output: Option<u64>,
    /// Cache-read tokens (where the provider reports a cache split).
    pub cache_read: Option<u64>,
    /// Cache-write/creation tokens.
    pub cache_write: Option<u64>,
}

impl TokenUsage {
    /// Whether any field is present (i.e. the provider surfaced usage at all).
    #[must_use]
    pub fn is_present(&self) -> bool {
        self.input.is_some()
            || self.output.is_some()
            || self.cache_read.is_some()
            || self.cache_write.is_some()
    }
}

/// Per-agent compatibility notes: which lifecycle points the agent exposes,
/// whether it surfaces token usage, and whether capture is best-effort. Honestly
/// recording gaps here propagates to attribution confidence (P4 S3 lowers
/// confidence when hooks are sparse).
#[derive(Clone, Debug)]
pub struct CompatNotes {
    /// The agent these notes describe.
    pub agent: AgentId,
    /// Lifecycle points the agent does **not** expose (e.g. "no compaction signal").
    pub lifecycle_gaps: &'static [&'static str],
    /// Whether the agent surfaces provider-reported token usage.
    pub surfaces_token_usage: bool,
    /// Whether capture is best-effort (the agent has no stable hook API).
    pub best_effort: bool,
    /// A one-line human note.
    pub note: &'static str,
}

/// Build a *structural* canonical event: `kind` + `trust` + the SDK schema
/// version. Identity (`session`, ordinals), time, and content hash are stamped by
/// the ingest layer from the hook envelope — an adapter has no authority to set
/// them. The returned event's `session`/`payload_sha256` are placeholders.
#[must_use]
pub fn event(kind: EventKind, trust: Trust) -> CanonicalEvent {
    CanonicalEvent {
        schema_version: SCHEMA_VERSION,
        session: String::new(),
        turn_ordinal: 0,
        event_ordinal: 0,
        kind,
        trust,
        occurred_at_ms: 0,
        payload_sha256: Sha256Id::from_bytes([0u8; 32]),
    }
}

/// An adapter mapping one agent's native verbs onto canonical events.
///
/// Implementors are small and declarative: they know the agent's verb names and
/// payload shape and nothing else. They produce events only — redaction and
/// chaining are Rift's job.
pub trait AgentAdapter: Send + Sync {
    /// The agent this adapter handles.
    fn agent_id(&self) -> AgentId;

    /// Map a native hook (`verb` + the agent's own payload) to zero-or-more
    /// canonical events.
    ///
    /// # Errors
    /// [`AdapterError::UnknownVerb`] for a verb outside [`verbs`](Self::verbs)
    /// (never silently dropped); [`AdapterError::MalformedPayload`] if a required
    /// shape is absent.
    fn map(
        &self,
        verb: &str,
        native: &serde_json::Value,
    ) -> Result<Vec<CanonicalEvent>, AdapterError>;

    /// The native verbs this adapter understands (for `rift enable` validation).
    fn verbs(&self) -> &'static [&'static str];

    /// Per-agent compatibility notes.
    fn compat_notes(&self) -> CompatNotes;

    /// Extract provider-reported token usage from a native payload, if present.
    /// Default: none (the agent doesn't surface usage). Never fabricates.
    fn usage(&self, _verb: &str, _native: &serde_json::Value) -> Option<TokenUsage> {
        None
    }
}

/// The content/identity-derived idempotency key for a canonical event (P8 S2):
/// `sha256(agent ‖ session ‖ verb ‖ turn_ordinal ‖ event_ordinal ‖ payload_sha256)`.
///
/// Deterministic over identity-defining fields (not wall-clock, not receipt
/// order), so the *same* logical event always yields the *same* key regardless of
/// when/how often it's delivered — making re-ingest a true no-op (§4.7). The
/// `turn_ordinal` separates the same hook fired in different turns; the
/// `event_ordinal` separates the fan-out within one hook (e.g. `ToolCall` then
/// `FileEdit`); the `payload_sha256` separates distinct payloads in the same slot.
#[must_use]
pub fn idempotency_key(
    agent: &AgentId,
    session: &str,
    verb: &str,
    turn_ordinal: u32,
    event_ordinal: u32,
    payload_sha256: &Sha256Id,
) -> Sha256Id {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(agent.as_str().as_bytes());
    h.update([0]);
    h.update(session.as_bytes());
    h.update([0]);
    h.update(verb.as_bytes());
    h.update([0]);
    h.update(turn_ordinal.to_le_bytes());
    h.update([0]);
    h.update(event_ordinal.to_le_bytes());
    h.update([0]);
    h.update(payload_sha256.as_bytes());
    let mut b = [0u8; 32];
    b.copy_from_slice(&h.finalize());
    Sha256Id::from_bytes(b)
}

/// Read a `u64` token-count field from a native payload by JSON path of keys,
/// honestly returning `None` when absent (never fabricates).
#[must_use]
pub fn read_count(native: &serde_json::Value, path: &[&str]) -> Option<u64> {
    let mut cur = native;
    for key in path {
        cur = cur.get(key)?;
    }
    cur.as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_builder_sets_schema_and_leaves_envelope_unset() {
        let e = event(EventKind::ToolCall, Trust::CodeFact);
        assert_eq!(e.schema_version, SCHEMA_VERSION);
        assert!(e.session.is_empty());
        assert_eq!(e.kind, EventKind::ToolCall);
        assert_eq!(e.trust, Trust::CodeFact);
    }

    #[test]
    fn idempotency_key_is_stable_and_ordinal_sensitive() {
        let h = Sha256Id::from_bytes([7u8; 32]);
        let a = idempotency_key(&AgentId::CLAUDE_CODE, "s", "PreToolUse", 1, 0, &h);
        let b = idempotency_key(&AgentId::CLAUDE_CODE, "s", "PreToolUse", 1, 0, &h);
        let event_diff = idempotency_key(&AgentId::CLAUDE_CODE, "s", "PreToolUse", 1, 1, &h);
        let turn_diff = idempotency_key(&AgentId::CLAUDE_CODE, "s", "PreToolUse", 2, 0, &h);
        assert_eq!(a, b, "same logical event → same key");
        assert_ne!(a, event_diff, "a distinct event_ordinal → a distinct key");
        assert_ne!(a, turn_diff, "a distinct turn_ordinal → a distinct key");
    }

    #[test]
    fn read_count_is_honest_about_absence() {
        let v = serde_json::json!({"usage": {"input_tokens": 12}});
        assert_eq!(read_count(&v, &["usage", "input_tokens"]), Some(12));
        assert_eq!(read_count(&v, &["usage", "output_tokens"]), None);
        assert_eq!(read_count(&v, &["nope"]), None);
    }
}

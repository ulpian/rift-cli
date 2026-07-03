//! First-party Rift adapter **scaffold** for **OpenCode** (P8 S3).
//!
//! OpenCode is open-source and likely the most directly hookable; this scaffold
//! maps its session/message/tool points. Native names are `(verify)` and pinned
//! by the conformance fixtures.
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

use rift_agent::{event, AdapterError, AgentAdapter, CompatNotes};
use rift_core::{AgentId, CanonicalEvent, EventKind, Trust};

/// The OpenCode adapter (scaffold).
pub struct OpenCodeAdapter;

impl OpenCodeAdapter {
    /// The agent id this adapter serves.
    pub const AGENT_ID: AgentId = AgentId::OPENCODE;
}

impl AgentAdapter for OpenCodeAdapter {
    fn agent_id(&self) -> AgentId {
        Self::AGENT_ID
    }

    fn map(
        &self,
        verb: &str,
        _native: &serde_json::Value,
    ) -> Result<Vec<CanonicalEvent>, AdapterError> {
        use EventKind::{PromptSubmit, SessionEnd, SessionStart, ToolCall};
        use Trust::{CodeFact, GitFact, TranscriptUntrusted};
        Ok(match verb {
            "session.start" => vec![event(SessionStart, GitFact)],
            "message" => vec![event(PromptSubmit, TranscriptUntrusted)],
            "tool.execute" => vec![event(ToolCall, CodeFact)],
            "session.end" => vec![event(SessionEnd, GitFact)],
            other => return Err(AdapterError::UnknownVerb(other.to_owned())),
        })
    }

    fn verbs(&self) -> &'static [&'static str] {
        &["session.start", "message", "tool.execute", "session.end"]
    }

    fn compat_notes(&self) -> CompatNotes {
        CompatNotes {
            agent: Self::AGENT_ID,
            lifecycle_gaps: &["no explicit compaction signal"],
            surfaces_token_usage: false,
            best_effort: true,
            note: "Open-source; most directly hookable — usage to be wired once confirmed.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_agent::conformance::{check, Case};
    use rift_core::EventKind::{PromptSubmit, SessionEnd, SessionStart, ToolCall};

    #[test]
    fn conformance_kit_passes() {
        let cases = vec![
            Case::new("session.start", serde_json::json!({}), &[SessionStart]),
            Case::new("message", serde_json::json!({}), &[PromptSubmit]),
            Case::new("tool.execute", serde_json::json!({}), &[ToolCall]),
            Case::new("session.end", serde_json::json!({}), &[SessionEnd]),
        ];
        check(&OpenCodeAdapter, &cases).unwrap();
    }
}

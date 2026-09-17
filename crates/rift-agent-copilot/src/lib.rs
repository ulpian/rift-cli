//! First-party Rift adapter **scaffold** for **GitHub Copilot CLI**.
//!
//! The CLI agent lifecycle and its usage reporting are `(verify)`; this scaffold
//! maps the session/prompt/tool points and records gaps in
//! [`compat_notes`](AgentAdapter::compat_notes).
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

use rift_agent::{event, AdapterError, AgentAdapter, CompatNotes, TurnSignals};
use rift_core::{AgentId, CanonicalEvent, EventKind, Trust};

/// The Copilot CLI adapter (scaffold).
pub struct CopilotAdapter;

impl CopilotAdapter {
    /// The agent id this adapter serves.
    pub const AGENT_ID: AgentId = AgentId::COPILOT_CLI;
}

impl AgentAdapter for CopilotAdapter {
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
            "session_start" => vec![event(SessionStart, GitFact)],
            "prompt" => vec![event(PromptSubmit, TranscriptUntrusted)],
            "tool" => vec![event(ToolCall, CodeFact)],
            "session_end" => vec![event(SessionEnd, GitFact)],
            other => return Err(AdapterError::UnknownVerb(other.to_owned())),
        })
    }

    fn verbs(&self) -> &'static [&'static str] {
        &["session_start", "prompt", "tool", "session_end"]
    }

    fn turn_signals(&self) -> TurnSignals {
        TurnSignals {
            start: true,
            end: false,
        }
    }

    fn compat_notes(&self) -> CompatNotes {
        CompatNotes {
            agent: Self::AGENT_ID,
            lifecycle_gaps: &[
                "usage reporting availability (verify)",
                "no compaction signal",
            ],
            surfaces_token_usage: false,
            best_effort: true,
            note: "CLI agent lifecycle pinned per version; usage marked absent until verified.",
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
            Case::new("session_start", serde_json::json!({}), &[SessionStart]),
            Case::new("prompt", serde_json::json!({}), &[PromptSubmit]),
            Case::new("tool", serde_json::json!({}), &[ToolCall]),
            Case::new("session_end", serde_json::json!({}), &[SessionEnd]),
        ];
        check(&CopilotAdapter, &cases).unwrap();
    }
}

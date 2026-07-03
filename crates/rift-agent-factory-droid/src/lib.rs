//! First-party Rift adapter **scaffold** for **Factory Droid** (P8 S3).
//!
//! Droid runs agentic workflows; this scaffold maps session/task/tool boundaries
//! (a task ≈ a turn). Native names are `(verify)` and pinned by the conformance
//! fixtures.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

use rift_agent::{event, AdapterError, AgentAdapter, CompatNotes};
use rift_core::{AgentId, CanonicalEvent, EventKind, Trust};

/// The Factory Droid adapter (scaffold).
pub struct FactoryDroidAdapter;

impl FactoryDroidAdapter {
    /// The agent id this adapter serves.
    pub const AGENT_ID: AgentId = AgentId::FACTORY_DROID;
}

impl AgentAdapter for FactoryDroidAdapter {
    fn agent_id(&self) -> AgentId {
        Self::AGENT_ID
    }

    fn map(
        &self,
        verb: &str,
        _native: &serde_json::Value,
    ) -> Result<Vec<CanonicalEvent>, AdapterError> {
        use EventKind::{SessionEnd, SessionStart, ToolCall, TurnEnd, TurnStart};
        use Trust::{CodeFact, GitFact};
        Ok(match verb {
            "session_start" => vec![event(SessionStart, GitFact)],
            "task_start" => vec![event(TurnStart, GitFact)],
            "tool_use" => vec![event(ToolCall, CodeFact)],
            "task_end" => vec![event(TurnEnd, GitFact)],
            "session_end" => vec![event(SessionEnd, GitFact)],
            other => return Err(AdapterError::UnknownVerb(other.to_owned())),
        })
    }

    fn verbs(&self) -> &'static [&'static str] {
        &[
            "session_start",
            "task_start",
            "tool_use",
            "task_end",
            "session_end",
        ]
    }

    fn compat_notes(&self) -> CompatNotes {
        CompatNotes {
            agent: Self::AGENT_ID,
            lifecycle_gaps: &["no explicit compaction signal", "usage reporting (verify)"],
            surfaces_token_usage: false,
            best_effort: true,
            note: "Agentic workflows; tasks mapped to turn boundaries.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_agent::conformance::{check, Case};
    use rift_core::EventKind::{SessionEnd, SessionStart, ToolCall, TurnEnd, TurnStart};

    #[test]
    fn conformance_kit_passes() {
        let cases = vec![
            Case::new("session_start", serde_json::json!({}), &[SessionStart]),
            Case::new("task_start", serde_json::json!({}), &[TurnStart]),
            Case::new("tool_use", serde_json::json!({}), &[ToolCall]),
            Case::new("task_end", serde_json::json!({}), &[TurnEnd]),
            Case::new("session_end", serde_json::json!({}), &[SessionEnd]),
        ];
        check(&FactoryDroidAdapter, &cases).unwrap();
    }
}

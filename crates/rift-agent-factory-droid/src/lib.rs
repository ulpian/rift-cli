//! First-party Rift adapter **scaffold** for **Factory Droid**.
//!
//! Droid runs agentic workflows; this scaffold maps session/task/tool boundaries
//! (a task ≈ a turn). Native names are `(verify)` and pinned by the conformance
//! fixtures.
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

/// The Factory Droid adapter (scaffold).
pub struct FactoryDroidAdapter;

/// The tools Factory Droid creates or modifies files with.
///
/// Verified at the pinned tag: `upstream: factoryaidroid/types.go:98`
/// declares `FileModificationTools` as exactly Create, Write, Edit, MultiEdit
/// and NotebookEdit, and the hook payloads that carry a tool name spell it
/// `tool_name` (`taskHookInputRaw` at :63-65, `postToolHookInputRaw` at :72-75).
/// Both halves — the list and the field the adapter reads — are the tag's, not
/// a guess.
///
/// **Dormant until `factory-droid.verb_surface` closes.** Droid's real tool
/// hooks are `PreToolUse`/`PostToolUse` and this adapter declares `tool_use`, a
/// name Droid never sends, so nothing reaches the fan-out today. It lands now
/// anyway because it is the half that is verifiable and it survives the verb
/// rewrite unchanged — see the sweep row of the same name.
pub const FILE_MUTATING_TOOLS: &[&str] = &["Create", "Write", "Edit", "MultiEdit", "NotebookEdit"];

impl FactoryDroidAdapter {
    /// The agent id this adapter serves.
    pub const AGENT_ID: AgentId = AgentId::FACTORY_DROID;

    fn tool_name(native: &serde_json::Value) -> Option<&str> {
        native.get("tool_name").and_then(serde_json::Value::as_str)
    }
}

impl AgentAdapter for FactoryDroidAdapter {
    fn agent_id(&self) -> AgentId {
        Self::AGENT_ID
    }

    fn map(
        &self,
        verb: &str,
        native: &serde_json::Value,
    ) -> Result<Vec<CanonicalEvent>, AdapterError> {
        use EventKind::{FileEdit, SessionEnd, SessionStart, ToolCall, TurnEnd, TurnStart};
        use Trust::{CodeFact, GitFact};
        Ok(match verb {
            "session_start" => vec![event(SessionStart, GitFact)],
            "task_start" => vec![event(TurnStart, GitFact)],
            "tool_use" => {
                let mut events = vec![event(ToolCall, CodeFact)];
                if FILE_MUTATING_TOOLS.contains(&Self::tool_name(native).unwrap_or_default()) {
                    events.push(event(FileEdit, CodeFact));
                }
                events
            }
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
            Case::new(
                "tool_use",
                serde_json::json!({"tool_name": "MultiEdit"}),
                &[ToolCall, rift_core::EventKind::FileEdit],
            ),
            Case::new("task_end", serde_json::json!({}), &[TurnEnd]),
            Case::new("session_end", serde_json::json!({}), &[SessionEnd]),
        ];
        check(&FactoryDroidAdapter, &cases).unwrap();
    }

    /// Droid names five file-mutating tools and Rift recognized none of
    /// them, so a Droid file write carried no working-tree signal at all — the
    /// same hole that was open for Claude Code, Codex and Gemini until each
    /// was closed. The tool list and the `tool_name` field are both the pinned tag's
    /// (factoryaidroid/types.go:98 and :63-65).
    #[test]
    fn every_file_mutating_tool_fans_out_to_a_file_edit() {
        for tool in FILE_MUTATING_TOOLS {
            let kinds: Vec<_> = FactoryDroidAdapter
                .map("tool_use", &serde_json::json!({"tool_name": tool}))
                .unwrap()
                .iter()
                .map(|e| e.kind)
                .collect();
            assert_eq!(
                kinds,
                vec![ToolCall, rift_core::EventKind::FileEdit],
                "{tool} must imply an edit"
            );
        }
        // Droid's read-only and shell tools stay a bare tool call.
        for tool in ["Read", "Bash", "Grep", "Task"] {
            let kinds: Vec<_> = FactoryDroidAdapter
                .map("tool_use", &serde_json::json!({"tool_name": tool}))
                .unwrap()
                .iter()
                .map(|e| e.kind)
                .collect();
            assert_eq!(kinds, vec![ToolCall], "{tool} must not imply an edit");
        }
    }
}

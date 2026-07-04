//! First-party Rift adapter for **Claude Code** (P8 S3).
//!
//! Claude Code exposes a rich hook set including compaction. Note the lifecycle
//! distinction the canonical vocabulary preserves: `Stop` ends an assistant
//! **turn**, while `SessionEnd` terminates the **session** — they map to distinct
//! canonical kinds (`TurnEnd` vs `SessionEnd`).
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

use rift_agent::{event, AdapterError, AgentAdapter, CompatNotes, TokenUsage};
use rift_core::{AgentId, CanonicalEvent, EventKind, Trust};

pub mod transcript;

/// The Claude Code adapter.
pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    /// The agent id this adapter serves.
    pub const AGENT_ID: AgentId = AgentId::CLAUDE_CODE;

    fn tool_name(native: &serde_json::Value) -> Option<&str> {
        native.get("tool_name").and_then(serde_json::Value::as_str)
    }
}

impl AgentAdapter for ClaudeCodeAdapter {
    fn agent_id(&self) -> AgentId {
        Self::AGENT_ID
    }

    fn map(
        &self,
        verb: &str,
        native: &serde_json::Value,
    ) -> Result<Vec<CanonicalEvent>, AdapterError> {
        use EventKind::{
            FileEdit, PromptSubmit, SessionCompaction, SessionEnd, SessionStart, SubagentEnd,
            SubagentStart, ToolCall, ToolResult, TurnEnd,
        };
        use Trust::{CodeFact, GitFact, ToolOutputUntrusted, TranscriptUntrusted};
        Ok(match verb {
            "SessionStart" => vec![event(SessionStart, GitFact)],
            "UserPromptSubmit" => vec![event(PromptSubmit, TranscriptUntrusted)],
            "PreToolUse" => {
                let mut evs = vec![event(ToolCall, CodeFact)];
                if matches!(
                    Self::tool_name(native),
                    Some("Edit" | "Write" | "MultiEdit")
                ) {
                    evs.push(event(FileEdit, CodeFact));
                }
                evs
            }
            "PostToolUse" => vec![event(ToolResult, ToolOutputUntrusted)],
            "Stop" => vec![event(TurnEnd, GitFact)],
            "SubagentStart" => vec![event(SubagentStart, GitFact)],
            "SubagentStop" => vec![event(SubagentEnd, GitFact)],
            "PreCompact" => vec![event(SessionCompaction, GitFact)],
            "SessionEnd" => vec![event(SessionEnd, GitFact)],
            other => return Err(AdapterError::UnknownVerb(other.to_owned())),
        })
    }

    fn verbs(&self) -> &'static [&'static str] {
        &[
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "Stop",
            "SubagentStart",
            "SubagentStop",
            "PreCompact",
            "SessionEnd",
        ]
    }

    fn compat_notes(&self) -> CompatNotes {
        CompatNotes {
            agent: Self::AGENT_ID,
            lifecycle_gaps: &[],
            surfaces_token_usage: true,
            best_effort: false,
            note: "Rich hook set incl. compaction; usage when present in tool/turn payloads.",
        }
    }

    fn usage(&self, verb: &str, native: &serde_json::Value) -> Option<TokenUsage> {
        if !matches!(verb, "PostToolUse" | "Stop") {
            return None;
        }
        let u = TokenUsage {
            input: rift_agent::read_count(native, &["usage", "input_tokens"]),
            output: rift_agent::read_count(native, &["usage", "output_tokens"]),
            cache_read: rift_agent::read_count(native, &["usage", "cache_read_input_tokens"]),
            cache_write: rift_agent::read_count(native, &["usage", "cache_creation_input_tokens"]),
        };
        u.is_present().then_some(u)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_agent::conformance::{check, Case};
    use rift_core::EventKind::{
        FileEdit, PromptSubmit, SessionEnd, SessionStart, ToolCall, ToolResult, TurnEnd,
    };

    fn cases() -> Vec<Case> {
        vec![
            Case::new("SessionStart", serde_json::json!({}), &[SessionStart]),
            Case::new(
                "UserPromptSubmit",
                serde_json::json!({"text": "hi"}),
                &[PromptSubmit],
            ),
            Case::new(
                "PreToolUse",
                serde_json::json!({"tool_name": "Bash"}),
                &[ToolCall],
            ),
            Case::new(
                "PreToolUse",
                serde_json::json!({"tool_name": "Edit"}),
                &[ToolCall, FileEdit],
            ),
            Case::new(
                "PostToolUse",
                serde_json::json!({"tool_name": "Bash"}),
                &[ToolResult],
            ),
            Case::new("Stop", serde_json::json!({}), &[TurnEnd]),
            Case::new("SessionEnd", serde_json::json!({}), &[SessionEnd]),
        ]
    }

    #[test]
    fn conformance_kit_passes() {
        check(&ClaudeCodeAdapter, &cases()).unwrap();
    }

    #[test]
    fn unknown_verb_errors_not_dropped() {
        assert!(matches!(
            ClaudeCodeAdapter.map("MysteryVerb", &serde_json::Value::Null),
            Err(AdapterError::UnknownVerb(_))
        ));
    }

    #[test]
    fn token_usage_extracted_when_present_never_fabricated() {
        let with = serde_json::json!({"usage": {"input_tokens": 10, "output_tokens": 3}});
        let u = ClaudeCodeAdapter.usage("Stop", &with).unwrap();
        assert_eq!(u.input, Some(10));
        assert_eq!(u.output, Some(3));
        assert_eq!(u.cache_read, None);
        // absent usage → no fabricated counts
        assert!(ClaudeCodeAdapter
            .usage("Stop", &serde_json::json!({}))
            .is_none());
        assert!(ClaudeCodeAdapter.usage("SessionStart", &with).is_none());
    }
}

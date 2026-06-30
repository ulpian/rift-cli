//! First-party Rift adapter for **OpenAI Codex / OpenAI CLI** (P8 S3).
//!
//! Native verbs are pinned here with the adapter; the exact lifecycle names are
//! version-dependent (`(verify)` against the current Codex hook docs) and a drift
//! is caught by the conformance fixtures, not silent capture loss.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use rift_agent::{event, AdapterError, AgentAdapter, CompatNotes, TokenUsage};
use rift_core::{AgentId, CanonicalEvent, EventKind, Trust};

/// The Codex adapter.
pub struct CodexAdapter;

impl CodexAdapter {
    /// The agent id this adapter serves.
    pub const AGENT_ID: AgentId = AgentId::CODEX;
}

impl AgentAdapter for CodexAdapter {
    fn agent_id(&self) -> AgentId {
        Self::AGENT_ID
    }

    fn map(
        &self,
        verb: &str,
        _native: &serde_json::Value,
    ) -> Result<Vec<CanonicalEvent>, AdapterError> {
        use EventKind::{
            PromptSubmit, SessionCompaction, SessionEnd, SessionStart, ToolCall, ToolResult,
            TurnEnd,
        };
        use Trust::{CodeFact, GitFact, ToolOutputUntrusted, TranscriptUntrusted};
        Ok(match verb {
            "session_start" => vec![event(SessionStart, GitFact)],
            "user_prompt" => vec![event(PromptSubmit, TranscriptUntrusted)],
            "tool_begin" => vec![event(ToolCall, CodeFact)],
            "tool_end" => vec![event(ToolResult, ToolOutputUntrusted)],
            "turn_end" => vec![event(TurnEnd, GitFact)],
            "compact" => vec![event(SessionCompaction, GitFact)],
            "session_end" => vec![event(SessionEnd, GitFact)],
            other => return Err(AdapterError::UnknownVerb(other.to_owned())),
        })
    }

    fn verbs(&self) -> &'static [&'static str] {
        &[
            "session_start",
            "user_prompt",
            "tool_begin",
            "tool_end",
            "turn_end",
            "compact",
            "session_end",
        ]
    }

    fn compat_notes(&self) -> CompatNotes {
        CompatNotes {
            agent: Self::AGENT_ID,
            lifecycle_gaps: &["cache-token split availability varies by model (verify)"],
            surfaces_token_usage: true,
            best_effort: false,
            note: "Maps to the same canonical kinds; usage in tool_end/turn_end payloads.",
        }
    }

    fn usage(&self, verb: &str, native: &serde_json::Value) -> Option<TokenUsage> {
        if !matches!(verb, "tool_end" | "turn_end") {
            return None;
        }
        let u = TokenUsage {
            input: rift_agent::read_count(native, &["usage", "input_tokens"]),
            output: rift_agent::read_count(native, &["usage", "output_tokens"]),
            cache_read: rift_agent::read_count(native, &["usage", "cached_input_tokens"]),
            cache_write: None,
        };
        u.is_present().then_some(u)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_agent::conformance::{check, Case};
    use rift_core::EventKind::{
        PromptSubmit, SessionEnd, SessionStart, ToolCall, ToolResult, TurnEnd,
    };

    #[test]
    fn conformance_kit_passes() {
        let cases = vec![
            Case::new("session_start", serde_json::json!({}), &[SessionStart]),
            Case::new("user_prompt", serde_json::json!({}), &[PromptSubmit]),
            Case::new("tool_begin", serde_json::json!({}), &[ToolCall]),
            Case::new("tool_end", serde_json::json!({}), &[ToolResult]),
            Case::new("turn_end", serde_json::json!({}), &[TurnEnd]),
            Case::new("session_end", serde_json::json!({}), &[SessionEnd]),
        ];
        check(&CodexAdapter, &cases).unwrap();
    }

    #[test]
    fn usage_extracted_when_present() {
        let v = serde_json::json!({"usage": {"input_tokens": 5, "cached_input_tokens": 2}});
        let u = CodexAdapter.usage("turn_end", &v).unwrap();
        assert_eq!(u.input, Some(5));
        assert_eq!(u.cache_read, Some(2));
        assert!(CodexAdapter.usage("session_start", &v).is_none());
    }
}

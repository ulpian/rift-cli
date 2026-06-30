//! First-party Rift adapter for **Gemini CLI** (P8 S3).
//!
//! Provider-reported usage is read from `usageMetadata` where available (P6 S1
//! tier-1 counts); the native verb names are pinned here and `(verify)` against
//! the current Gemini CLI hook surface.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use rift_agent::{event, AdapterError, AgentAdapter, CompatNotes, TokenUsage};
use rift_core::{AgentId, CanonicalEvent, EventKind, Trust};

/// The Gemini CLI adapter.
pub struct GeminiAdapter;

impl GeminiAdapter {
    /// The agent id this adapter serves.
    pub const AGENT_ID: AgentId = AgentId::GEMINI;
}

impl AgentAdapter for GeminiAdapter {
    fn agent_id(&self) -> AgentId {
        Self::AGENT_ID
    }

    fn map(
        &self,
        verb: &str,
        _native: &serde_json::Value,
    ) -> Result<Vec<CanonicalEvent>, AdapterError> {
        use EventKind::{PromptSubmit, SessionEnd, SessionStart, ToolCall, ToolResult, TurnEnd};
        use Trust::{CodeFact, GitFact, ToolOutputUntrusted, TranscriptUntrusted};
        Ok(match verb {
            "session_start" => vec![event(SessionStart, GitFact)],
            "user_prompt" => vec![event(PromptSubmit, TranscriptUntrusted)],
            "tool_call" => vec![event(ToolCall, CodeFact)],
            "tool_result" => vec![event(ToolResult, ToolOutputUntrusted)],
            "turn_complete" => vec![event(TurnEnd, GitFact)],
            "session_end" => vec![event(SessionEnd, GitFact)],
            other => return Err(AdapterError::UnknownVerb(other.to_owned())),
        })
    }

    fn verbs(&self) -> &'static [&'static str] {
        &[
            "session_start",
            "user_prompt",
            "tool_call",
            "tool_result",
            "turn_complete",
            "session_end",
        ]
    }

    fn compat_notes(&self) -> CompatNotes {
        CompatNotes {
            agent: Self::AGENT_ID,
            lifecycle_gaps: &["no explicit compaction signal"],
            surfaces_token_usage: true,
            best_effort: false,
            note: "Provider-reported usage via usageMetadata where available (P6 tier-1).",
        }
    }

    fn usage(&self, verb: &str, native: &serde_json::Value) -> Option<TokenUsage> {
        if !matches!(verb, "tool_result" | "turn_complete") {
            return None;
        }
        let u = TokenUsage {
            input: rift_agent::read_count(native, &["usageMetadata", "promptTokenCount"]),
            output: rift_agent::read_count(native, &["usageMetadata", "candidatesTokenCount"]),
            cache_read: rift_agent::read_count(
                native,
                &["usageMetadata", "cachedContentTokenCount"],
            ),
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
            Case::new("tool_call", serde_json::json!({}), &[ToolCall]),
            Case::new("tool_result", serde_json::json!({}), &[ToolResult]),
            Case::new("turn_complete", serde_json::json!({}), &[TurnEnd]),
            Case::new("session_end", serde_json::json!({}), &[SessionEnd]),
        ];
        check(&GeminiAdapter, &cases).unwrap();
    }

    #[test]
    fn usage_from_usage_metadata() {
        let v = serde_json::json!({"usageMetadata": {"promptTokenCount": 8, "candidatesTokenCount": 4}});
        let u = GeminiAdapter.usage("turn_complete", &v).unwrap();
        assert_eq!(u.input, Some(8));
        assert_eq!(u.output, Some(4));
    }
}

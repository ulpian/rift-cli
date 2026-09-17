//! First-party Rift adapter for **Gemini CLI**.
//!
//! Provider-reported usage is read from `usageMetadata` where available — the
//! most trustworthy token counts there are; the native verb names are pinned here and `(verify)` against
//! the current Gemini CLI hook surface.
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

use rift_agent::{
    event, AdapterError, AgentAdapter, CompatNotes, HookFacts, ModelFact, ModelSource, TokenUsage,
};
use rift_core::{AgentId, CanonicalEvent, EventKind, Trust};

/// The Gemini CLI adapter.
pub struct GeminiAdapter;

/// The Gemini CLI tools whose use means a file on disk changed.
///
/// `edit_file` and `save_file` were missing. The trailing `edit` and `write`
/// are the display-name forms Gemini CLI uses in some surfaces; they are kept
/// (an unused name costs nothing) but they are not the internal names, which
/// is why they never fired on their own.
pub const FILE_MUTATING_TOOLS: &[&str] = &[
    "write_file",
    "edit_file",
    "save_file",
    "replace",
    "edit",
    "write",
];

impl GeminiAdapter {
    /// The agent id this adapter serves.
    pub const AGENT_ID: AgentId = AgentId::GEMINI;

    fn tool_name(native: &serde_json::Value) -> Option<&str> {
        native.get("tool_name").and_then(serde_json::Value::as_str)
    }
}

impl AgentAdapter for GeminiAdapter {
    fn agent_id(&self) -> AgentId {
        Self::AGENT_ID
    }

    fn map(
        &self,
        verb: &str,
        native: &serde_json::Value,
    ) -> Result<Vec<CanonicalEvent>, AdapterError> {
        use EventKind::{
            FileEdit, PromptResponse, PromptSubmit, SessionCompaction, SessionEnd, SessionStart,
            ToolCall, ToolResult, TurnEnd,
        };
        use Trust::{CodeFact, GitFact, ToolOutputUntrusted, TranscriptUntrusted};
        Ok(match verb {
            "SessionStart" => vec![event(SessionStart, GitFact)],
            "BeforeAgent" => vec![event(PromptSubmit, TranscriptUntrusted)],
            "BeforeModel" => Vec::new(),
            "AfterModel" => vec![event(PromptResponse, TranscriptUntrusted)],
            "BeforeTool" => {
                let mut events = vec![event(ToolCall, CodeFact)];
                if FILE_MUTATING_TOOLS.contains(&Self::tool_name(native).unwrap_or_default()) {
                    events.push(event(FileEdit, CodeFact));
                }
                events
            }
            "AfterTool" => vec![event(ToolResult, ToolOutputUntrusted)],
            "AfterAgent" => vec![event(TurnEnd, GitFact)],
            "PreCompress" => vec![event(SessionCompaction, GitFact)],
            "SessionEnd" => vec![event(SessionEnd, GitFact)],
            other => return Err(AdapterError::UnknownVerb(other.to_owned())),
        })
    }

    fn verbs(&self) -> &'static [&'static str] {
        &[
            "SessionStart",
            "BeforeAgent",
            "BeforeModel",
            "AfterModel",
            "BeforeTool",
            "AfterTool",
            "AfterAgent",
            "PreCompress",
            "SessionEnd",
        ]
    }

    fn compat_notes(&self) -> CompatNotes {
        CompatNotes {
            agent: Self::AGENT_ID,
            lifecycle_gaps: &[],
            surfaces_token_usage: true,
            best_effort: false,
            note: "Current Gemini CLI hooks; AfterModel carries nested model and usage metadata.",
        }
    }

    fn facts(&self, _verb: &str, native: &serde_json::Value) -> HookFacts {
        let model = native
            .get("llm_request")
            .and_then(|v| v.get("model"))
            .or_else(|| native.get("model"))
            .and_then(serde_json::Value::as_str)
            .filter(|m| !m.is_empty())
            .map(|model| ModelFact {
                model: model.to_owned(),
                source: ModelSource::RuntimeHook,
            });
        let tool_name = Self::tool_name(native).map(str::to_owned);
        let read_path = tool_name
            .as_deref()
            .filter(|name| matches!(*name, "read_file" | "read_many_files"))
            .and_then(|_| native.get("tool_input"))
            .and_then(|v| v.get("file_path").or_else(|| v.get("path")))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        HookFacts {
            model,
            tool_name,
            child_session: None,
            read_path,
            // The root-field convention: present when Gemini CLI's payload
            // carries a `cwd`, absent otherwise — never guessed.
            cwd: rift_agent::payload_cwd(native),
        }
    }

    fn usage(&self, verb: &str, native: &serde_json::Value) -> Option<TokenUsage> {
        if verb != "AfterModel" {
            return None;
        }
        let usage = native.get("llm_response").unwrap_or(native);
        let u = TokenUsage {
            input: rift_agent::read_count(usage, &["usageMetadata", "promptTokenCount"]),
            output: rift_agent::read_count(usage, &["usageMetadata", "candidatesTokenCount"]),
            cache_read: rift_agent::read_count(
                usage,
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
        PromptSubmit, SessionCompaction, SessionEnd, SessionStart, ToolCall, ToolResult, TurnEnd,
    };

    #[test]
    fn conformance_kit_passes() {
        let cases = vec![
            Case::new("SessionStart", serde_json::json!({}), &[SessionStart]),
            Case::new("BeforeAgent", serde_json::json!({}), &[PromptSubmit]),
            Case::new(
                "AfterModel",
                serde_json::json!({}),
                &[rift_core::EventKind::PromptResponse],
            ),
            Case::new("BeforeModel", serde_json::json!({}), &[]),
            Case::new("BeforeTool", serde_json::json!({}), &[ToolCall]),
            Case::new("AfterTool", serde_json::json!({}), &[ToolResult]),
            Case::new("AfterAgent", serde_json::json!({}), &[TurnEnd]),
            Case::new("PreCompress", serde_json::json!({}), &[SessionCompaction]),
            Case::new("SessionEnd", serde_json::json!({}), &[SessionEnd]),
        ];
        assert!(GeminiAdapter
            .verbs()
            .iter()
            .all(|verb| cases.iter().any(|case| case.verb == *verb)));
        check(&GeminiAdapter, &cases).unwrap();
    }

    /// `edit_file` and `save_file` are internal Gemini CLI tool names
    /// that were not in the matcher, so two of the four ways Gemini writes a
    /// file were captured as a bare `ToolCall`.
    #[test]
    fn every_file_mutating_tool_fans_out_to_a_file_edit() {
        for tool in FILE_MUTATING_TOOLS {
            let kinds: Vec<_> = GeminiAdapter
                .map("BeforeTool", &serde_json::json!({"tool_name": tool}))
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
        for tool in ["read_file", "read_many_files", "run_shell_command"] {
            let kinds: Vec<_> = GeminiAdapter
                .map("BeforeTool", &serde_json::json!({"tool_name": tool}))
                .unwrap()
                .iter()
                .map(|e| e.kind)
                .collect();
            assert_eq!(kinds, vec![ToolCall], "{tool} does not edit a file");
        }
    }

    #[test]
    fn usage_from_usage_metadata() {
        let v = serde_json::json!({"llm_response":{"usageMetadata": {"promptTokenCount": 8, "candidatesTokenCount": 4}}});
        let u = GeminiAdapter.usage("AfterModel", &v).unwrap();
        assert_eq!(u.input, Some(8));
        assert_eq!(u.output, Some(4));
    }

    #[test]
    fn nested_model_is_normalized() {
        let facts = GeminiAdapter.facts(
            "AfterModel",
            &serde_json::json!({"llm_request":{"model":"gemini-2.5-pro"}}),
        );
        assert_eq!(facts.model.unwrap().model, "gemini-2.5-pro");
    }
}

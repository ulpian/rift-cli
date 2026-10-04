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
            // No canonical event happens on these. `BeforeModel` and
            // `BeforeToolSelection` open one model call, which may be one of
            // several in a turn, so neither is a turn start; the call's model is
            // recorded on its `AfterModel` response, whose payload carries the
            // same `llm_request`. `Notification` is a tool-permission prompt,
            // and the tool call itself is `BeforeTool`'s.
            "BeforeModel" | "BeforeToolSelection" | "Notification" => Vec::new(),
            other => return Err(AdapterError::UnknownVerb(other.to_owned())),
        })
    }

    fn verbs(&self) -> &'static [&'static str] {
        &[
            "SessionStart",
            "BeforeAgent",
            "BeforeModel",
            "BeforeToolSelection",
            "AfterModel",
            "BeforeTool",
            "AfterTool",
            "AfterAgent",
            "PreCompress",
            "Notification",
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
            Case::new("BeforeToolSelection", serde_json::json!({}), &[]),
            Case::new("Notification", serde_json::json!({}), &[]),
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

    /// An undeclared event lands as a skipped-unknown-verb receipt, so all
    /// eleven Gemini publishes are declared — and these two invent no canonical
    /// event: a tool-selection pass is part of one model call, and a permission
    /// notification precedes a tool call that `BeforeTool` already records.
    #[test]
    fn every_published_event_maps_without_an_unknown_verb_error() {
        for verb in ["BeforeToolSelection", "Notification"] {
            let events = GeminiAdapter
                .map(verb, &serde_json::json!({"session_id": "s"}))
                .unwrap_or_else(|error| panic!("{verb} is refused: {error:?}"));
            assert!(events.is_empty(), "{verb} has no canonical event");
            assert!(GeminiAdapter.verbs().contains(&verb), "{verb} is declared");
        }
    }

    /// The model reaches the projection on the response: `AfterModel` carries
    /// the same `llm_request` `BeforeModel` does, and its one canonical event
    /// is what the projector records the payload's model against.
    #[test]
    fn a_model_call_records_its_model_on_the_response_event() {
        let payload = serde_json::json!({
            "session_id": "s",
            "llm_request": {"model": "gemini-2.5-pro"},
            "llm_response": {"usageMetadata": {"promptTokenCount": 8}}
        });
        let kinds: Vec<_> = GeminiAdapter
            .map("AfterModel", &payload)
            .unwrap()
            .iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, vec![rift_core::EventKind::PromptResponse]);
        let model = GeminiAdapter.facts("AfterModel", &payload).model.unwrap();
        assert_eq!(model.model, "gemini-2.5-pro");
        assert_eq!(model.source, ModelSource::RuntimeHook);
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

/// **Gemini CLI's own published hook surface, pinned into this tree.**
///
/// The artifact is `fixtures/gemini-cli-hook-surface.md`: the event enumeration
/// and the four event inputs from the published `@google/gemini-cli-core`
/// package. The sweep had graded this adapter against a Go wrapper that parsed
/// only `BeforeModel`'s payload, and concluded that Gemini names its model
/// nowhere else; the agent's own `AfterModelInput` says otherwise.
#[cfg(test)]
mod pinned_surface {
    use super::{AgentAdapter, GeminiAdapter};

    /// `include_str!` so a missing or renamed fixture is a compile error, not a
    /// skipped test.
    const FIXTURE: &str = include_str!("../fixtures/gemini-cli-hook-surface.md");

    /// A provenance field of the `nth` pinned artifact (`key: value`, indented).
    fn provenance(field: &str, nth: usize) -> &'static str {
        let needle = format!("{field}:");
        FIXTURE
            .lines()
            .filter_map(|line| line.trim().strip_prefix(&needle))
            .map(str::trim)
            .nth(nth)
            .unwrap_or_else(|| panic!("the pin states no {field} #{nth}"))
    }

    /// The members of the pinned `HookEventName` enumeration, read only between
    /// its opening line and its closing brace.
    fn published_events() -> Vec<&'static str> {
        FIXTURE
            .lines()
            .skip_while(|line| !line.starts_with("export declare enum HookEventName"))
            .skip(1)
            .take_while(|line| !line.starts_with('}'))
            .filter_map(|line| line.trim().split_once(" = "))
            .map(|(name, _)| name)
            .collect()
    }

    #[test]
    fn the_pin_states_a_provenance_a_reader_can_recheck() {
        let expected = [
            (
                "dist/src/hooks/types.d.ts",
                "07e68ea295b682a53300863fb06057061e10ea76",
            ),
            (
                "dist/src/hooks/hookEventHandler.js",
                "b118b97674dd67ad72b7668e0237a0a71cb514f6",
            ),
            (
                "dist/src/hooks/hookTranslator.js",
                "bae7987a02e6fde5f86c18cdc611ba0dee825adc",
            ),
        ];
        for (nth, (path, sha)) in expected.into_iter().enumerate() {
            assert_eq!(
                provenance("repository", nth),
                "https://github.com/google-gemini/gemini-cli"
            );
            assert_eq!(provenance("package", nth), "@google/gemini-cli-core");
            assert_eq!(provenance("tag", nth), "0.35.3");
            assert_eq!(provenance("path", nth), path);
            assert_eq!(provenance("blob-sha", nth), sha);
        }
    }

    /// The adapter declares exactly the events Gemini publishes — no fewer, so
    /// none lands as an unknown verb, and no more, so none is invented.
    #[test]
    fn the_adapter_declares_every_event_the_pin_publishes() {
        let mut published = published_events();
        let mut declared = GeminiAdapter.verbs().to_vec();
        assert_eq!(
            published.len(),
            11,
            "the pinned enumeration has eleven members"
        );
        published.sort_unstable();
        declared.sort_unstable();
        assert_eq!(declared, published);
    }

    /// The response event carries the request that names the model, so the
    /// model has a canonical event to be recorded against.
    #[test]
    fn after_model_carries_the_request_that_names_the_model() {
        assert!(FIXTURE.contains(
            "export interface AfterModelInput extends HookInput {\n    llm_request: LLMRequest;"
        ));
        assert!(
            FIXTURE.contains("llm_request: defaultHookTranslator.toHookLLMRequest(llmRequest),")
        );
        assert!(FIXTURE.contains("model: sdkRequest.model || DEFAULT_GEMINI_FLASH_MODEL,"));
    }
}

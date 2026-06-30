//! First-party Rift adapter **scaffold** for **Ollama / open-weights local
//! models** (P8 S3, user-requested).
//!
//! Honest gap: a local model *runner* (Ollama, llama.cpp, an OpenAI-compatible
//! local endpoint) emits **no agent-lifecycle hooks** — there is no agent harness
//! to fire `session_start`/`tool_*`/`compact`. Capture is therefore best-effort
//! via a thin OpenAI-compatible **proxy/wrapper** that forwards each request and
//! response as a `chat_request`/`chat_response` verb. We map only what exists
//! (prompt/response) and read token usage from the runtime's response metadata
//! where present (`prompt_eval_count`/`eval_count`); absent usage stays absent.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use rift_agent::{event, AdapterError, AgentAdapter, CompatNotes, TokenUsage};
use rift_core::{AgentId, CanonicalEvent, EventKind, Trust};

/// The Ollama / open-weights adapter (scaffold).
pub struct OllamaAdapter;

impl OllamaAdapter {
    /// The agent id this adapter serves.
    pub const AGENT_ID: AgentId = AgentId::OLLAMA;
}

impl AgentAdapter for OllamaAdapter {
    fn agent_id(&self) -> AgentId {
        Self::AGENT_ID
    }

    fn map(
        &self,
        verb: &str,
        _native: &serde_json::Value,
    ) -> Result<Vec<CanonicalEvent>, AdapterError> {
        use EventKind::{PromptResponse, PromptSubmit};
        use Trust::{ToolOutputUntrusted, TranscriptUntrusted};
        Ok(match verb {
            // A proxied request carries the user/system prompt (untrusted transcript).
            "chat_request" => vec![event(PromptSubmit, TranscriptUntrusted)],
            // A proxied response is model output (treated as untrusted content).
            "chat_response" => vec![event(PromptResponse, ToolOutputUntrusted)],
            other => return Err(AdapterError::UnknownVerb(other.to_owned())),
        })
    }

    fn verbs(&self) -> &'static [&'static str] {
        &["chat_request", "chat_response"]
    }

    fn compat_notes(&self) -> CompatNotes {
        CompatNotes {
            agent: Self::AGENT_ID,
            lifecycle_gaps: &[
                "no agent-lifecycle hooks (captured via an OpenAI-compatible proxy)",
                "no session/turn/tool/compaction boundaries",
            ],
            surfaces_token_usage: true,
            best_effort: true,
            note: "Open-weights local models; prompt/response only, usage from runtime metadata.",
        }
    }

    fn usage(&self, verb: &str, native: &serde_json::Value) -> Option<TokenUsage> {
        if verb != "chat_response" {
            return None;
        }
        let u = TokenUsage {
            input: rift_agent::read_count(native, &["prompt_eval_count"]),
            output: rift_agent::read_count(native, &["eval_count"]),
            cache_read: None,
            cache_write: None,
        };
        u.is_present().then_some(u)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_agent::conformance::{check, Case};
    use rift_core::EventKind::{PromptResponse, PromptSubmit};

    #[test]
    fn conformance_kit_passes() {
        let cases = vec![
            Case::new("chat_request", serde_json::json!({}), &[PromptSubmit]),
            Case::new("chat_response", serde_json::json!({}), &[PromptResponse]),
        ];
        check(&OllamaAdapter, &cases).unwrap();
    }

    #[test]
    fn usage_from_runtime_metadata_when_present() {
        let v = serde_json::json!({"prompt_eval_count": 20, "eval_count": 7});
        let u = OllamaAdapter.usage("chat_response", &v).unwrap();
        assert_eq!(u.input, Some(20));
        assert_eq!(u.output, Some(7));
        assert!(OllamaAdapter.usage("chat_request", &v).is_none());
    }
}

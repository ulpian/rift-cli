//! Turn-grain token usage from the Claude Code session transcript (M2-15).
//!
//! Claude Code's native hook stdin carries **no** `usage` fields; the
//! provider-reported numbers live in the session transcript (JSONL) the hook
//! names via `transcript_path`. [`turn_usage_from_tail`] is **pure over
//! bytes** — the caller does the bounded file read — and walks the tail
//! backward, summing `message.usage` across the assistant lines of the
//! just-ended turn: back to the last *real* user prompt (a `user` line that is
//! not a tool result, not meta, and not a sidechain). Sidechain (subagent)
//! assistant calls inside the window are part of the turn's spend and are
//! summed; the reported model is the last main-chain assistant's.
//!
//! Total over arbitrary bytes: an unparsable line is skipped; no usage found
//! is `None` — never a fabricated zero (the adapter honesty rule).

use rift_agent::TokenUsage;

/// The bounded tail window callers should read from the transcript file. A
/// turn's usage lines are small; 8 MiB of tail spans a heavy turn's tool
/// traffic. A turn longer than the window under-sums honestly (the missing
/// lines are simply outside the window).
pub const MAX_TAIL_BYTES: u64 = 8 * 1024 * 1024;

/// Provider-reported usage for one turn, plus the model that served it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnUsage {
    /// Summed usage across the turn's assistant messages (one per API call).
    pub usage: TokenUsage,
    /// The model of the turn's last main-chain assistant message.
    pub model: Option<String>,
    /// Freshest provider-reported usage from one non-sidechain assistant call.
    pub main_context: Option<MainContextUsage>,
}

/// One main-chain model context observed at a transcript boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainContextUsage {
    /// Usage from exactly one non-sidechain assistant message.
    pub usage: TokenUsage,
    /// Model named by that assistant message.
    pub model: Option<String>,
}

/// Sum the just-ended turn's provider-reported usage from a transcript tail.
/// Returns `None` when no assistant usage is visible in the window.
#[must_use]
pub fn turn_usage_from_tail(tail: &[u8]) -> Option<TurnUsage> {
    let mut usage = TokenUsage::default();
    let mut model: Option<String> = None;
    let mut main_context: Option<MainContextUsage> = None;
    for line in tail.split(|b| *b == b'\n').rev() {
        // A partial first line of a truncated tail fails to parse and is
        // skipped, like any other garbage line.
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        match v.get("type").and_then(serde_json::Value::as_str) {
            Some("assistant") => {
                if let Some(u) = v.pointer("/message/usage") {
                    add(&mut usage.input, u, "input_tokens");
                    add(&mut usage.output, u, "output_tokens");
                    add(&mut usage.cache_read, u, "cache_read_input_tokens");
                    add(&mut usage.cache_write, u, "cache_creation_input_tokens");
                }
                if !is_sidechain(&v) {
                    let line_model = v
                        .pointer("/message/model")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                    if model.is_none() {
                        model.clone_from(&line_model);
                    }
                    if main_context.is_none() {
                        let line_usage = usage_at(v.pointer("/message/usage"));
                        if line_usage.is_present() {
                            main_context = Some(MainContextUsage {
                                usage: line_usage,
                                model: line_model,
                            });
                        }
                    }
                }
            }
            Some("user") if is_real_prompt(&v) => break,
            _ => {}
        }
    }
    usage.is_present().then_some(TurnUsage {
        usage,
        model,
        main_context,
    })
}

fn usage_at(value: Option<&serde_json::Value>) -> TokenUsage {
    let Some(value) = value else {
        return TokenUsage::default();
    };
    TokenUsage {
        input: value
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64),
        output: value
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64),
        cache_read: value
            .get("cache_read_input_tokens")
            .and_then(serde_json::Value::as_u64),
        cache_write: value
            .get("cache_creation_input_tokens")
            .and_then(serde_json::Value::as_u64),
    }
}

/// Whether a `user` line is a real prompt (the turn boundary): tool-result
/// lines carry a `toolUseResult` key, meta lines set `isMeta`, and sidechain
/// prompts are agent-authored — none of those end the walk.
fn is_real_prompt(v: &serde_json::Value) -> bool {
    let has_tool_result = v
        .as_object()
        .is_some_and(|o| o.contains_key("toolUseResult"));
    let is_meta = v.get("isMeta").and_then(serde_json::Value::as_bool) == Some(true);
    !has_tool_result && !is_meta && !is_sidechain(v)
}

fn is_sidechain(v: &serde_json::Value) -> bool {
    v.get("isSidechain").and_then(serde_json::Value::as_bool) == Some(true)
}

/// Accumulate one per-line count into the running total. A field stays `None`
/// until the provider reports it at least once (never a fabricated zero).
fn add(acc: &mut Option<u64>, usage: &serde_json::Value, key: &str) {
    if let Some(n) = usage.get(key).and_then(serde_json::Value::as_u64) {
        *acc = Some(acc.unwrap_or(0).saturating_add(n));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(model: &str, input: u64, output: u64, sidechain: bool) -> String {
        serde_json::json!({
            "type": "assistant",
            "isSidechain": sidechain,
            "message": {
                "model": model,
                "usage": {
                    "input_tokens": input,
                    "output_tokens": output,
                    "cache_read_input_tokens": 100,
                    "cache_creation_input_tokens": 10,
                }
            }
        })
        .to_string()
    }

    fn prompt() -> String {
        serde_json::json!({"type": "user", "message": {"content": "do the thing"}}).to_string()
    }

    fn tool_result() -> String {
        serde_json::json!({
            "type": "user",
            "toolUseResult": {"stdout": "ok"},
            "message": {"content": [{"type": "tool_result"}]}
        })
        .to_string()
    }

    #[test]
    fn sums_the_turn_back_to_the_last_real_prompt() {
        // An older turn (must NOT be summed), a prompt, then the current turn:
        // two main-chain calls + one sidechain call + a tool result between.
        let lines = [
            assistant("claude-old", 999, 999, false),
            prompt(),
            assistant("claude-fable-5", 10, 1, false),
            tool_result(),
            assistant("claude-fable-5", 20, 2, true), // subagent spend counts
            assistant("claude-fable-5", 30, 3, false),
        ]
        .join("\n");
        let t = turn_usage_from_tail(lines.as_bytes()).unwrap();
        assert_eq!(t.usage.input, Some(60));
        assert_eq!(t.usage.output, Some(6));
        assert_eq!(t.usage.cache_read, Some(300));
        assert_eq!(t.usage.cache_write, Some(30));
        assert_eq!(t.model.as_deref(), Some("claude-fable-5"));
        let main = t.main_context.expect("freshest main-chain sample");
        assert_eq!(main.usage.input, Some(30));
        assert_eq!(main.usage.output, Some(3));
        assert_eq!(main.usage.cache_read, Some(100));
        assert_eq!(main.usage.cache_write, Some(10));
        assert_eq!(main.model.as_deref(), Some("claude-fable-5"));
    }

    #[test]
    fn no_usage_is_none_never_a_fabricated_zero() {
        assert!(turn_usage_from_tail(b"").is_none());
        assert!(turn_usage_from_tail(prompt().as_bytes()).is_none());
        // usage objects without any known count stay absent
        let bare = serde_json::json!({"type": "assistant", "message": {"usage": {}}}).to_string();
        assert!(turn_usage_from_tail(bare.as_bytes()).is_none());
    }

    #[test]
    fn garbage_and_partial_lines_are_skipped() {
        let lines = format!(
            "ens\": 5}}}}\n{}\nnot json at all\n{}",
            prompt(),
            assistant("m", 7, 7, false)
        );
        let t = turn_usage_from_tail(lines.as_bytes()).unwrap();
        assert_eq!(t.usage.input, Some(7));
    }

    #[test]
    fn meta_and_sidechain_user_lines_do_not_end_the_walk() {
        let meta = serde_json::json!({"type": "user", "isMeta": true, "message": {}}).to_string();
        let side_prompt =
            serde_json::json!({"type": "user", "isSidechain": true, "message": {}}).to_string();
        let lines = [
            assistant("m", 1, 1, false),
            meta,
            side_prompt,
            assistant("m", 2, 2, false),
        ]
        .join("\n");
        let t = turn_usage_from_tail(lines.as_bytes()).unwrap();
        assert_eq!(t.usage.input, Some(3), "walk crosses meta/sidechain lines");
    }

    #[test]
    fn saturates_rather_than_overflows() {
        let big = serde_json::json!({
            "type": "assistant",
            "message": {"usage": {"input_tokens": u64::MAX}}
        })
        .to_string();
        let lines = [big.clone(), big].join("\n");
        let t = turn_usage_from_tail(lines.as_bytes()).unwrap();
        assert_eq!(t.usage.input, Some(u64::MAX));
    }

    proptest::proptest! {
        /// Total over arbitrary bytes: never panics.
        #[test]
        fn turn_usage_is_total(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..4096)) {
            let _ = turn_usage_from_tail(&bytes);
        }
    }
}

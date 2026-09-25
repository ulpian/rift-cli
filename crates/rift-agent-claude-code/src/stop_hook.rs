//! The `Stop`-hook sentinel: the one sound recogniser of Claude Code's own
//! record that its `Stop` hooks have run.
//!
//! It is a module of its own because it answers the settle protocol, not the
//! hook-payload mapping: it reads transcript records, never a hook payload.

use rift_agent::settle::StopSentinel;

/// The invariant middle of the command line Rift installs for Claude Code's
/// `Stop` hook (`"<rift>" hooks claude-code Stop`). Every other verb —
/// `SubagentStop` included — differs in that tail, so the fragment names *this*
/// hook exactly. It is **not** a sentinel on its own: transcripts quote command
/// lines back constantly. See [`STOP_SENTINEL`].
pub const STOP_HOOK_COMMAND: &str = "hooks claude-code Stop";

/// Claude Code's own record of a finished `Stop` hook run: a `system` record
/// with `subtype = "stop_hook_summary"`, listing each hook it ran under
/// `hookInfos[*].command` together with that hook's `durationMs`.
///
/// **Unreachable during Rift's own Stop wait, and deliberately kept.** Claude
/// Code writes this summary only *after* the Stop hooks return — the record
/// reports how long each one took, so it cannot exist while one is still
/// running. The settle protocol therefore never takes the sentinel fast path
/// for this agent today; a Claude Code turn end settles on the quiet window
/// instead, and callers should budget for that (the original claim that this
/// marker is observable mid-wait was wrong).
///
/// It stays because it is the only *sound* recognizer of the marker: the
/// substring it replaced matched 40 records in this machine's real transcripts
/// that merely quote the command (tool results reading the settings file,
/// `hook_non_blocking_error` attachments, queued prompts), each of which ended
/// a wait early on a half-written transcript. If Claude Code ever publishes the
/// marker mid-turn, this predicate is already correct; until then it costs a
/// bounded tail read and can only answer "no".
pub const STOP_SENTINEL: StopSentinel =
    StopSentinel::new("claude-code:stop_hook_summary", is_stop_hook_summary);

/// Whether one parsed transcript record is a `stop_hook_summary` for *Rift's*
/// Stop hook. Structural: type, subtype, and the command under `hookInfos`.
fn is_stop_hook_summary(record: &serde_json::Value) -> bool {
    let field = |key: &str| record.get(key).and_then(serde_json::Value::as_str);
    field("type") == Some("system")
        && field("subtype") == Some("stop_hook_summary")
        && record
            .get("hookInfos")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|infos| {
                infos.iter().any(|info| {
                    info.get("command")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|command| command.contains(STOP_HOOK_COMMAND))
                })
            })
}

#[cfg(test)]
mod tests {
    use rift_agent::AgentAdapter;

    use crate::ClaudeCodeAdapter;

    use super::*;

    /// The genuine marker — the shape Claude Code writes once its Stop hooks
    /// have returned (note `durationMs`: the record can only exist afterwards,
    /// which is why the fast path is unreachable during Rift's own wait).
    #[test]
    fn the_sentinel_accepts_claude_codes_own_stop_hook_summary() {
        let record = serde_json::json!({
            "type": "system",
            "subtype": "stop_hook_summary",
            "hookCount": 1,
            "hookInfos": [{
                "command": "'/Users/dev/rift/target/debug/rift' hooks claude-code Stop",
                "durationMs": 332
            }],
            "hookErrors": [],
            "sessionId": "f5927747-7be6-4b5e-b379-579c7193520a"
        });
        assert!(STOP_SENTINEL.matches(&record));
        assert_eq!(STOP_SENTINEL.name(), "claude-code:stop_hook_summary");
        assert!(ClaudeCodeAdapter.stop_sentinel().is_some());
    }

    /// A transcript logs everything the session saw,
    /// including quotations of the hook command line: the settings file read
    /// back as a tool result, the agent's own `grep` for it, a
    /// `hook_non_blocking_error` attachment, a queued prompt. Real Claude Code
    /// transcripts on the development machine hold 40 such records against 115
    /// genuine summaries. Under the substring predicate this replaced, every
    /// one of them ended the settle wait early — on a still-being-written
    /// transcript, whose truncated turn was then condensed and chained.
    #[test]
    fn the_sentinel_rejects_records_that_merely_quote_the_hook_command() {
        let quoting = [
            (
                "user tool_result of the settings file",
                serde_json::json!({
                    "type": "user",
                    "message": {"role": "user", "content": [{
                        "type": "tool_result",
                        "content": "\"command\": \"'/bin/rift' hooks claude-code Stop\""
                    }]}
                }),
            ),
            (
                "assistant tool_use grepping for it",
                serde_json::json!({
                    "type": "assistant",
                    "message": {"role": "assistant", "content": [{
                        "type": "tool_use", "name": "Bash",
                        "input": {"command": "grep 'hooks claude-code Stop' settings.json"}
                    }]}
                }),
            ),
            (
                "hook_non_blocking_error attachment",
                serde_json::json!({
                    "type": "attachment",
                    "attachment": {
                        "type": "hook_non_blocking_error", "hookEvent": "Stop",
                        "command": "\"/bin/rift\" hooks claude-code Stop",
                        "exitCode": 127, "durationMs": 24
                    }
                }),
            ),
            (
                "queued prompt naming the command",
                serde_json::json!({
                    "type": "queue-operation", "operation": "enqueue",
                    "content": "re-run '/bin/rift' hooks claude-code Stop by hand"
                }),
            ),
            (
                "a summary for a different hook of this agent",
                serde_json::json!({
                    "type": "system", "subtype": "stop_hook_summary",
                    "hookInfos": [{"command": "'/bin/rift' hooks claude-code SubagentStop"}]
                }),
            ),
            (
                "a summary whose hookInfos is not an array of objects",
                serde_json::json!({
                    "type": "system", "subtype": "stop_hook_summary",
                    "hookInfos": "hooks claude-code Stop"
                }),
            ),
        ];
        for (shape, record) in quoting {
            assert!(
                !STOP_SENTINEL.matches(&record),
                "{shape}: quoting the command is not running it"
            );
        }
    }
}

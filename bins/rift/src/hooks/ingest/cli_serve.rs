//! The serve a `rift recall` command made inside an agent session, read back
//! off the command's own result.
//!
//! Separate because it is the one place a tool result's text is read for a
//! fact, and the fact is one Rift printed there itself: the two keys the skill
//! CLI booked its serve under. The CLI cannot know which session ran it, and
//! the hook that ingests its answer opens no recall store, so the hook reads
//! the keys and journals them and the worker writes the attribution.
//!
//! The command is recognised by a strict token check here rather than by the
//! shell grammar, because that grammar recognises reads and listings and
//! exposes no reading of an arbitrary program's argv.

use std::path::Path;

use crate::hooks::now_ms;

/// What begins the line an answer names its keys on.
const KEY_LINE_PREFIX: &str = "serve: ";

/// The two keys one `rift recall` answer printed, each in its exact shape.
#[derive(Debug, PartialEq, Eq)]
struct PrintedKeys {
    call_id: String,
    audit_id: String,
}

/// Journal the attribution of the serve this call's result names, and say
/// whether a job was journaled.
///
/// Nothing is journaled unless the command line is a `rift recall <verb>`
/// invocation and nothing else, and its result carries keys in their exact
/// shape: a malformed line, or a line some other program printed, queues
/// nothing. The window key is the window journal's, computed from the facts
/// the ingest already read off this payload.
pub(super) fn owe_cli_serve_attribution(
    git_dir: &Path,
    work_tree: &Path,
    raw: &rift_shadow::hooks::RawHook,
    facts: &rift_agent::HookFacts,
    command: &str,
) -> usize {
    if raw.session.is_empty() || !is_rift_recall(command) {
        return 0;
    }
    let Some(keys) = tool_result_text(&raw.payload).and_then(printed_keys) else {
        return 0;
    };
    let window = crate::window_journal::key(&raw.session, facts.child_session.as_deref());
    let job = crate::agent_queue::AgentJob {
        schema_version: 1,
        work_tree: work_tree.to_path_buf(),
        queued_at_ms: now_ms(),
        attempts: 0,
        kind: crate::agent_queue::JobKind::AttributeServe(crate::agent_queue::ServeClaim {
            session: raw.session.clone(),
            window: Some(window),
            call_id: keys.call_id,
            audit_id: keys.audit_id,
        }),
    };
    usize::from(crate::agent_queue::enqueue(git_dir, &job).is_ok())
}

/// Whether a command line is one `rift recall <verb>` invocation: the program
/// `rift` or a path ending `/rift`, then `recall`, then a verb.
///
/// A line with a shell operator in it is refused whole, because its output is
/// not the answer alone: a pipe can reshape it and a second command can print
/// a line of its own. What that refuses is an attribution, never an answer.
fn is_rift_recall(command: &str) -> bool {
    const OPERATORS: [char; 8] = ['|', ';', '&', '`', '$', '>', '<', '\n'];
    if command.contains(OPERATORS) {
        return false;
    }
    let mut words = command.split_ascii_whitespace();
    let is_rift = words
        .next()
        .is_some_and(|program| program == "rift" || program.ends_with("/rift"));
    is_rift
        && words.next() == Some("recall")
        && words.next().is_some_and(|verb| !verb.starts_with('-'))
}

/// The text a shell call's result handed the model: the result itself when
/// it is text, else its `stdout`.
fn tool_result_text(payload: &serde_json::Value) -> Option<&str> {
    let response = payload.get("tool_response")?;
    response
        .as_str()
        .or_else(|| response.get("stdout")?.as_str())
}

/// The keys an answer printed: its key line in the text form, or its `serve`
/// member in the `--json` form.
fn printed_keys(text: &str) -> Option<PrintedKeys> {
    text.lines().find_map(key_line).or_else(|| json_keys(text))
}

/// One `serve: <call_id> <audit_id>` line, exactly.
fn key_line(line: &str) -> Option<PrintedKeys> {
    let (call_id, audit_id) = line.strip_prefix(KEY_LINE_PREFIX)?.split_once(' ')?;
    exact_keys(call_id, audit_id)
}

/// The `serve` member of an answer printed as one JSON document.
fn json_keys(text: &str) -> Option<PrintedKeys> {
    let doc: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let serve = doc.get("serve")?;
    exact_keys(
        serve.get("call_id")?.as_str()?,
        serve.get("audit_id")?.as_str()?,
    )
}

/// The two keys, when each is lowercase hex of exactly its length: a call id
/// is a ULID's 32, an audit id a SHA-256's 64.
fn exact_keys(call_id: &str, audit_id: &str) -> Option<PrintedKeys> {
    let hex = |s: &str, len: usize| {
        s.len() == len
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    (hex(call_id, 32) && hex(audit_id, 64)).then(|| PrintedKeys {
        call_id: call_id.to_owned(),
        audit_id: audit_id.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const CALL: &str = "000000000000000000000000000000ab";

    fn audit() -> String {
        "ab".repeat(32)
    }

    fn answer() -> String {
        format!(
            "known-state: 12 tokens returned, ~340 avoided (cl100k_base)\nserve: {CALL} {}\n{{}}\n",
            audit()
        )
    }

    fn result_of(command: &str, stdout: &str) -> rift_shadow::hooks::RawHook {
        rift_shadow::hooks::RawHook {
            agent: rift_core::AgentId::CLAUDE_CODE,
            verb: "PostToolUse".to_owned(),
            session: "s-1".to_owned(),
            turn: Some(1),
            occurred_at_ms: 1_700_000_000_000,
            payload: json!({
                "tool_name": "Bash",
                "tool_input": { "command": command },
                "tool_response": { "stdout": stdout, "stderr": "", "interrupted": false },
            }),
        }
    }

    /// Journal what one result owes, and hand back the claims the queue holds.
    fn owed(
        raw: &rift_shadow::hooks::RawHook,
        child: Option<&str>,
    ) -> Vec<crate::agent_queue::ServeClaim> {
        let git_dir = tempfile::tempdir().unwrap();
        let facts = rift_agent::HookFacts {
            child_session: child.map(str::to_owned),
            ..rift_agent::HookFacts::default()
        };
        let command = raw.payload["tool_input"]["command"].as_str().unwrap();
        owe_cli_serve_attribution(git_dir.path(), Path::new("/w"), raw, &facts, command);
        crate::agent_queue::jobs(git_dir.path())
            .into_iter()
            .filter_map(|(_, job)| match job.kind {
                crate::agent_queue::JobKind::AttributeServe(claim) => Some(claim),
                _ => None,
            })
            .collect()
    }

    /// A `rift recall` answer's key line is one job naming the session, the
    /// window journal's key of the window that ran it, and both keys.
    #[test]
    fn a_rift_recall_result_with_its_key_line_owes_one_attribution() {
        let raw = result_of("rift recall known-state --task t --file a.rs", &answer());
        let claims = owed(&raw, Some("agent-7"));
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].session, "s-1");
        assert_eq!(claims[0].call_id, CALL);
        assert_eq!(claims[0].audit_id, audit());
        assert_eq!(
            claims[0].window,
            Some(crate::window_journal::key("s-1", Some("agent-7")))
        );
        let by_path = result_of("/usr/local/bin/rift recall changed-hunks", &answer());
        assert_eq!(owed(&by_path, None).len(), 1);
    }

    /// The `--json` answer names the same keys in its `serve` member.
    #[test]
    fn a_json_answer_names_its_keys_in_its_serve_member() {
        let doc =
            json!({ "verb": "known-state", "serve": { "call_id": CALL, "audit_id": audit() } });
        let raw = result_of(
            "rift recall known-state --json",
            &serde_json::to_string_pretty(&doc).unwrap(),
        );
        assert_eq!(owed(&raw, None)[0].audit_id, audit());
    }

    /// A key line out of shape queues nothing: wrong length, upper case, a
    /// third word, or a prefix that is not the answer's.
    #[test]
    fn a_malformed_key_line_queues_nothing() {
        for line in [
            format!("serve: {} {}", &CALL[..31], audit()),
            format!("serve: {} {}", CALL.to_uppercase(), audit()),
            format!("serve: {CALL} {} extra", audit()),
            format!("serve:  {CALL} {}", audit()),
            format!("served: {CALL} {}", audit()),
            format!("serve: {CALL} {}", &audit()[..63]),
        ] {
            let raw = result_of("rift recall known-state", &format!("head\n{line}\n"));
            assert!(owed(&raw, None).is_empty(), "{line}");
        }
    }

    /// Output that happens to carry the line queues nothing unless the
    /// command was a `rift recall` invocation and nothing else.
    #[test]
    fn a_command_that_is_not_rift_recall_queues_nothing() {
        for command in [
            format!("echo 'serve: {CALL} {}'", audit()),
            "rift status".to_owned(),
            "rift recall --help".to_owned(),
            "rift recall".to_owned(),
            "cargo run -- recall known-state".to_owned(),
            "rift recall known-state; cat saved.txt".to_owned(),
            "rift recall known-state | tee out.txt".to_owned(),
            "rift recall known-state && echo done".to_owned(),
            "myrift recall known-state".to_owned(),
        ] {
            let raw = result_of(&command, &answer());
            assert!(owed(&raw, None).is_empty(), "{command}");
        }
    }
}

//! The deferred settle, end to end: turns ingested the way the hooks deliver
//! them, a transcript written after the Stop returned, and the spend landing
//! at the Stop event's own ids.

use std::path::Path;

use rift_shadow_hooks::session_cache;
use rift_shadow_hooks::{AdapterRegistry, AgentId, RawHook};
use rift_store::{CanonicalStore, SqliteStore, Value};

use super::fixtures::store;
use super::{ingest_hook, settle_closed_turn_usage, SettleMode, Settled};

const SESSION: &str = "01JSETTLE";

async fn hook(store: &SqliteStore, git_dir: &Path, verb: &str, payload: serde_json::Value) {
    let raw = RawHook {
        agent: AgentId::CLAUDE_CODE,
        verb: verb.into(),
        session: SESSION.into(),
        turn: None,
        occurred_at_ms: 1,
        payload,
    };
    ingest_hook(
        &raw,
        &AdapterRegistry::with_builtin(),
        store,
        git_dir,
        "claude-code",
        "repo-1",
        10,
        None,
    )
    .await
    .unwrap();
}

fn line(kind: &str, input: u64) -> String {
    match kind {
        "prompt" => serde_json::json!({"type": "user", "message": {"content": "go"}}),
        _ => serde_json::json!({
            "type": "assistant",
            "isSidechain": kind == "side",
            "message": {"model": "claude-fable-5", "usage": {"input_tokens": input, "output_tokens": 1}}
        }),
    }
    .to_string()
}

fn write_lines(path: &Path, lines: &[String]) {
    let mut body = lines.join("\n");
    body.push('\n');
    std::fs::write(path, body).unwrap();
}

async fn usage_rows(store: &SqliteStore) -> Vec<(String, i64)> {
    store
        .query(
            "SELECT call_id, uncached_input FROM token_usage ORDER BY call_id",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get_text(0).unwrap(), r.get_i64(1).unwrap()))
        .collect()
}

async fn event_ordinal(store: &SqliteStore, turn: u32, kind: &str) -> u32 {
    let rows = store
        .query(
            "SELECT event_ordinal FROM captured_event WHERE session_id = ? AND turn_ordinal = ? \
             AND kind = ? ORDER BY event_ordinal DESC LIMIT 1",
            &[
                Value::text(SESSION.to_owned()),
                Value::Int(i64::from(turn)),
                Value::text(kind.to_owned()),
            ],
        )
        .await
        .unwrap();
    u32::try_from(rows[0].get_i64(0).unwrap()).unwrap()
}

async fn settle(store: &SqliteStore, git_dir: &Path, mode: SettleMode) -> Settled {
    settle_closed_turn_usage(store, git_dir, SESSION, "claude-code", "repo-1", mode, 99)
        .await
        .unwrap()
}

/// A whole turn, ingested the way the hooks deliver it: a session start
/// that seeds the sidecar, a prompt, a tool call, and the Stop. The
/// transcript is written AFTER the Stop returned — the deferred contract.
async fn one_turn(store: &SqliteStore, git_dir: &Path, transcript: &Path) {
    let path = transcript.display().to_string();
    hook(
        store,
        git_dir,
        "SessionStart",
        serde_json::json!({"transcript_path": path}),
    )
    .await;
    settle(
        store,
        git_dir,
        SettleMode::AfterHook {
            turn_closed_here: false,
        },
    )
    .await;
    hook(
        store,
        git_dir,
        "UserPromptSubmit",
        serde_json::json!({"prompt": "go"}),
    )
    .await;
    hook(
        store,
        git_dir,
        "PreToolUse",
        serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}),
    )
    .await;
    hook(
        store,
        git_dir,
        "Stop",
        serde_json::json!({"stop_hook_active": false}),
    )
    .await;
    write_lines(
        transcript,
        &[line("prompt", 0), line("side", 100), line("main", 20)],
    );
}

/// The load-bearing claim: the spend lands at the Stop event's OWN ids —
/// `model_call_id(session_row, turn, event_ordinal-of-turn_end)` — so
/// every reader of `turn → model_call → token_usage` sees what the
/// inline settle used to write, one hook later.
#[tokio::test]
async fn spend_lands_at_the_stop_events_own_ids() {
    let s = store();
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path().join("git");
    let transcript = tmp.path().join("t.jsonl");
    one_turn(&s, &git_dir, &transcript).await;
    assert!(
        usage_rows(&s).await.is_empty(),
        "the Stop itself records no spend"
    );

    // The next hook for the session settles turn 1.
    hook(
        &s,
        &git_dir,
        "UserPromptSubmit",
        serde_json::json!({"prompt": "again"}),
    )
    .await;
    let done = settle(
        &s,
        &git_dir,
        SettleMode::AfterHook {
            turn_closed_here: false,
        },
    )
    .await;
    let transcript_len = std::fs::metadata(&transcript).unwrap().len();
    assert_eq!(
        done,
        Settled::Recorded {
            turn: 1,
            usage_recorded: true,
            next_offset: transcript_len
        }
    );
    let stop = event_ordinal(&s, 1, "turn_end").await;
    let session_row = crate::ids::session_row_id("claude-code", SESSION);
    let call = crate::ids::model_call_id(&session_row, 1, stop);
    assert_eq!(usage_rows(&s).await, vec![(call.clone(), 120)]);
    let turns = s
        .query(
            "SELECT turn_id, model FROM model_call WHERE call_id = ?",
            &[Value::text(call)],
        )
        .await
        .unwrap();
    assert_eq!(
        turns[0].get_text(0).unwrap(),
        crate::ids::turn_row_id(&session_row, 1)
    );
    assert_eq!(turns[0].get_text(1).unwrap(), "claude-fable-5");
    let context = s
        .query("SELECT uncached_input FROM main_context_observation", &[])
        .await
        .unwrap();
    assert_eq!(context.len(), 1, "one context sample rides the same anchor");
    assert_eq!(context[0].get_i64(0).unwrap(), 20);
    let progress = session_cache::read_usage_progress(&git_dir, SESSION)
        .unwrap()
        .unwrap();
    assert_eq!(
        (progress.byte_offset, progress.settled_turn),
        (transcript_len, 1)
    );
}

/// The Stop is the one hook that must NOT settle the turn it closed: the
/// agent is still writing the transcript it fired from.
#[tokio::test]
async fn a_hook_that_closes_a_turn_never_settles_it() {
    let s = store();
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path().join("git");
    let transcript = tmp.path().join("t.jsonl");
    one_turn(&s, &git_dir, &transcript).await;
    let done = settle(
        &s,
        &git_dir,
        SettleMode::AfterHook {
            turn_closed_here: true,
        },
    )
    .await;
    assert_eq!(done, Settled::Skipped);
    assert!(usage_rows(&s).await.is_empty());
    let progress = session_cache::read_usage_progress(&git_dir, SESSION)
        .unwrap()
        .unwrap();
    assert_eq!(
        progress.settled_turn, 0,
        "nothing settled, offset untouched"
    );
    assert_eq!(progress.byte_offset, 0);
}

/// Redelivery, a retried hook, the sweep after a hook: the second settle
/// of the same turn is a no-op and the offset advanced exactly once.
#[tokio::test]
async fn a_second_settle_is_a_no_op_and_the_offset_advances_once() {
    let s = store();
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path().join("git");
    let transcript = tmp.path().join("t.jsonl");
    one_turn(&s, &git_dir, &transcript).await;
    let after = SettleMode::AfterHook {
        turn_closed_here: false,
    };
    let first = settle(&s, &git_dir, after).await;
    assert!(
        matches!(first, Settled::Recorded { turn: 1, .. }),
        "{first:?}"
    );
    let rows = usage_rows(&s).await;
    assert_eq!(rows.len(), 1);
    let offset = session_cache::read_usage_progress(&git_dir, SESSION)
        .unwrap()
        .unwrap()
        .byte_offset;

    assert_eq!(settle(&s, &git_dir, after).await, Settled::Skipped);
    assert_eq!(
        settle(&s, &git_dir, SettleMode::Final).await,
        Settled::Skipped
    );
    assert_eq!(usage_rows(&s).await, rows);
    let progress = session_cache::read_usage_progress(&git_dir, SESSION)
        .unwrap()
        .unwrap();
    assert_eq!(progress.byte_offset, offset);
    assert_eq!(progress.settled_turn, 1);
}

/// A crashed session: the turn never closed, so there is no `turn_end`
/// row. The sweep's Final settle anchors the spend on the turn's last
/// captured event instead of losing it.
#[tokio::test]
async fn final_anchors_an_open_turn_on_its_last_event() {
    let s = store();
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path().join("git");
    let transcript = tmp.path().join("t.jsonl");
    let path = transcript.display().to_string();
    hook(
        &s,
        &git_dir,
        "SessionStart",
        serde_json::json!({"transcript_path": path}),
    )
    .await;
    settle(
        &s,
        &git_dir,
        SettleMode::AfterHook {
            turn_closed_here: false,
        },
    )
    .await;
    hook(
        &s,
        &git_dir,
        "UserPromptSubmit",
        serde_json::json!({"prompt": "go"}),
    )
    .await;
    hook(
        &s,
        &git_dir,
        "PreToolUse",
        serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}),
    )
    .await;
    write_lines(&transcript, &[line("prompt", 0), line("main", 7)]);

    // An ordinary hook settles nothing: the turn is still open.
    assert_eq!(
        settle(
            &s,
            &git_dir,
            SettleMode::AfterHook {
                turn_closed_here: false
            }
        )
        .await,
        Settled::Skipped
    );
    let done = settle(&s, &git_dir, SettleMode::Final).await;
    assert!(
        matches!(
            done,
            Settled::Recorded {
                turn: 1,
                usage_recorded: true,
                ..
            }
        ),
        "{done:?}"
    );
    let last = event_ordinal(&s, 1, "tool_call").await;
    let session_row = crate::ids::session_row_id("claude-code", SESSION);
    assert_eq!(
        usage_rows(&s).await,
        vec![(crate::ids::model_call_id(&session_row, 1, last), 7)]
    );
}

/// Two ways an offset can be wrong, and neither re-counts anything: a
/// late start seeds at the transcript's current length (no back-fill),
/// and a transcript shorter than the recorded offset (relocated,
/// rewritten) resets to its length and counts nothing.
#[tokio::test]
async fn a_transcript_shorter_than_the_offset_counts_nothing() {
    let s = store();
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path().join("git");
    let transcript = tmp.path().join("t.jsonl");
    // Content that predates capture: never attributed.
    write_lines(&transcript, &[line("prompt", 0), line("main", 5000)]);
    let before = std::fs::metadata(&transcript).unwrap().len();
    let path = transcript.display().to_string();
    hook(
        &s,
        &git_dir,
        "SessionStart",
        serde_json::json!({"transcript_path": path}),
    )
    .await;
    assert_eq!(
        settle(
            &s,
            &git_dir,
            SettleMode::AfterHook {
                turn_closed_here: false
            }
        )
        .await,
        Settled::Skipped
    );
    let seeded = session_cache::read_usage_progress(&git_dir, SESSION)
        .unwrap()
        .unwrap();
    assert_eq!(seeded.byte_offset, before, "seeded at the current length");

    hook(
        &s,
        &git_dir,
        "UserPromptSubmit",
        serde_json::json!({"prompt": "go"}),
    )
    .await;
    hook(
        &s,
        &git_dir,
        "Stop",
        serde_json::json!({"stop_hook_active": false}),
    )
    .await;
    // The transcript was replaced by something shorter than the offset.
    write_lines(&transcript, &[line("main", 9)]);
    let shorter = std::fs::metadata(&transcript).unwrap().len();
    assert!(shorter < before);
    let done = settle(
        &s,
        &git_dir,
        SettleMode::AfterHook {
            turn_closed_here: false,
        },
    )
    .await;
    assert_eq!(
        done,
        Settled::Recorded {
            turn: 1,
            usage_recorded: false,
            next_offset: shorter
        }
    );
    assert!(usage_rows(&s).await.is_empty(), "nothing was re-counted");
}

// --- The transcript's address, whichever spelling the agent uses ---------
//
// Claude Code names it `transcript_path`; Antigravity names it
// `transcriptPath` on every hook. Every payload below copies a recorded field
// surface (`rift-agent-antigravity/fixtures/antigravity-tool-field-surface.md`,
// O1), never an invented one.

const CONVERSATION: &str = "f7c9fdf2-9408-405f-b657-c00c98593fa2";
const ANTIGRAVITY_TRANSCRIPT: &str = "/Users/ulpian/.gemini/antigravity-cli/brain/f7c9fdf2-9408-405f-b657-c00c98593fa2/.system_generated/logs/transcript_full.jsonl";

/// The O1 `view_file` payload, verbatim in shape, with `transcriptPath`
/// bound to `transcript` (or left out when `None`).
fn recorded_pre_tool_use(transcript: Option<&str>) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "artifactDirectoryPath": "/Users/ulpian/.gemini/antigravity-cli/brain/f7c9fdf2-9408-405f-b657-c00c98593fa2",
        "conversationId": CONVERSATION,
        "modelName": "gemini-3.6-flash-medium",
        "stepIdx": 5,
        "toolCall": {
            "args": {
                "AbsolutePath": "/Users/ulpian/rift-worktrees/imp-spikes/g/alpha.txt",
                "toolAction": "Viewing alpha.txt first time",
                "toolSummary": "View alpha.txt"
            },
            "name": "view_file"
        },
        "workspacePaths": ["/Users/ulpian/rift-worktrees/imp-spikes/g"]
    });
    if let (Some(path), Some(obj)) = (transcript, payload.as_object_mut()) {
        obj.insert("transcriptPath".to_owned(), serde_json::json!(path));
    }
    payload
}

async fn antigravity_hook(
    store: &SqliteStore,
    git_dir: &Path,
    verb: &str,
    payload: serde_json::Value,
    now_ms: i64,
) {
    let raw = RawHook {
        agent: AgentId::ANTIGRAVITY,
        verb: verb.into(),
        session: CONVERSATION.into(),
        turn: None,
        occurred_at_ms: now_ms,
        payload,
    };
    let reg = AdapterRegistry::with_builtin();
    ingest_hook(
        &raw,
        &reg,
        store,
        git_dir,
        "antigravity",
        "repo-1",
        now_ms,
        None,
    )
    .await
    .unwrap();
}

fn cached_address(git_dir: &Path) -> Option<String> {
    session_cache::read_cache(git_dir, CONVERSATION)
        .unwrap()
        .unwrap()
        .transcript_path
}

#[tokio::test]
async fn antigravitys_camel_case_transcript_path_is_copied_to_the_session_cache() {
    let s = store();
    let tmp = tempfile::tempdir().unwrap();
    let payload = recorded_pre_tool_use(Some(ANTIGRAVITY_TRANSCRIPT));

    antigravity_hook(&s, tmp.path(), "PreToolUse", payload, 10).await;

    assert_eq!(
        cached_address(tmp.path()).as_deref(),
        Some(ANTIGRAVITY_TRANSCRIPT)
    );
}

/// Only Antigravity sends the camelCase spelling, so a payload that somehow
/// carries both is read as Claude Code's: the snake_case value wins.
#[tokio::test]
async fn a_payload_carrying_both_spellings_keeps_the_snake_case_address() {
    let s = store();
    let tmp = tempfile::tempdir().unwrap();
    let mut payload = recorded_pre_tool_use(Some("/elsewhere/camel.jsonl"));
    payload.as_object_mut().unwrap().insert(
        "transcript_path".to_owned(),
        serde_json::json!("/elsewhere/snake.jsonl"),
    );

    antigravity_hook(&s, tmp.path(), "PreToolUse", payload, 10).await;

    assert_eq!(
        cached_address(tmp.path()).as_deref(),
        Some("/elsewhere/snake.jsonl")
    );
}

/// A later hook naming neither spelling keeps the remembered address rather
/// than forgetting it — the rule Claude Code already had, now covering both.
#[tokio::test]
async fn a_later_hook_without_either_spelling_keeps_the_previous_address() {
    let s = store();
    let tmp = tempfile::tempdir().unwrap();
    let named = recorded_pre_tool_use(Some(ANTIGRAVITY_TRANSCRIPT));
    let unnamed = recorded_pre_tool_use(None);

    antigravity_hook(&s, tmp.path(), "PreToolUse", named, 10).await;
    antigravity_hook(&s, tmp.path(), "PostToolUse", unnamed, 11).await;

    assert_eq!(
        cached_address(tmp.path()).as_deref(),
        Some(ANTIGRAVITY_TRANSCRIPT)
    );
}

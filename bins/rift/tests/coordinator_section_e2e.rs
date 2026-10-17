//! End to end, through the shipped binary: the section a subagent's window
//! opens with is composed by the hook process before it chooses who drains
//! capture, so the inline path (`RIFT_CAPTURE_SYNC` set) and the shipped
//! detached path hand the model the same bytes.
//!
//! Unix only: the detached arm is awaited through the process group each hook
//! leads, the way the hooks fixture awaits its workers, so no worker this
//! test started outlives it. Nothing asserted here reads the clock.
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::io::Write as _;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{agent_hook_out, enable_recall, enforce_for, json_str, seed_live_session_repo};

const SESSION: &str = "e2e-coord-1";
const CHILD: &str = "agent-7";

/// The hook `verb` at `turn` with `payload`, on whichever capture path `env`
/// selects, as its own process-group leader: `(pgid, stdout)`.
///
/// Its own spawn rather than the fixture's detached wrapper because the
/// fixture keeps stderr, and the text a boundary hands the model travels on
/// stdout. The group is what the detached arm is awaited by.
fn grouped_hook(
    repo: &Path,
    verb: &str,
    turn: u32,
    payload: &str,
    env: &[(&str, &str)],
) -> (u32, String) {
    let body = format!(r#"{{"schema":1,"session":"{SESSION}","turn":{turn},"payload":{payload}}}"#);
    let mut cmd = common::rift_cmd(repo);
    cmd.args(["hooks", "claude-code", verb])
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = cmd.spawn().unwrap();
    let pid = child.id();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "a hook never fails the agent: {verb}");
    (pid, String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Return once the process group `pgid` has no live member, asked of the
/// system `kill` with signal 0, which delivers nothing and fails once the
/// group is empty: every worker a hook of this group left behind has exited.
fn await_group(pgid: u32) {
    let has_member = || {
        Command::new("kill")
            .args(["-0", "--", &format!("-{pgid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    while has_member() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The main window's whole-file read of `file`.
fn read_payload(file: &Path) -> String {
    format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{}}}}}"#,
        json_str(file)
    )
}

/// The subagent's opening.
fn opening_payload() -> String {
    format!(
        r#"{{"hook_event_name":"SubagentStart","agent_id":"{CHILD}","agent_type":"general-purpose"}}"#
    )
}

/// A repository set up the same way on both paths: Recall on, declines on
/// for Claude Code so the main window's read is journaled, and one file for
/// it to read.
fn briefed_repo(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let repo = seed_live_session_repo(tmp);
    enable_recall(&repo);
    enforce_for(&repo, "claude-code");
    let file = repo.join("briefed.txt");
    let lines = "line\n".repeat(40);
    std::fs::write(&file, lines).unwrap();
    (repo, file)
}

/// The text the opening hook handed the model, out of the object the client
/// reads.
fn additional_context(out: &str) -> String {
    let response: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("the hook wrote an object the client can read: {e} in {out:?}"));
    response["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_else(|| panic!("the text travels in this family's field: {response}"))
        .to_owned()
}

/// The opening text on the inline path: the queue drained before each hook
/// returns.
fn opening_inline(tmp: &Path) -> String {
    let (repo, file) = briefed_repo(tmp);
    let cache = tmp.join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    agent_hook_out(
        &repo,
        "claude-code",
        "PreToolUse",
        SESSION,
        1,
        &read_payload(&file),
        &env,
    );
    let out = agent_hook_out(
        &repo,
        "claude-code",
        "SubagentStart",
        SESSION,
        2,
        &opening_payload(),
        &env,
    );
    additional_context(&out)
}

/// The opening text on the shipped detached path: each hook hands its queue
/// to a worker it does not wait for, and this test waits for every one of
/// them before it returns.
fn opening_detached(tmp: &Path) -> String {
    let (repo, file) = briefed_repo(tmp);
    let cache = tmp.join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
    ];
    let (read_group, _) = grouped_hook(&repo, "PreToolUse", 1, &read_payload(&file), &env);
    let (open_group, out) = grouped_hook(&repo, "SubagentStart", 2, &opening_payload(), &env);
    let text = additional_context(&out);
    await_group(read_group);
    await_group(open_group);
    text
}

/// The same opening, once with the capture queue drained inline and once on
/// the shipped detached path, hands the subagent byte-identical text: the
/// pointer and, after it, the section naming the revision its coordinator
/// read. The section rides in the hook process, before the branch that
/// chooses who drains, and the worker writes no line it is built from.
#[test]
fn the_opening_section_is_the_same_bytes_inline_and_detached() {
    let inline = tempfile::tempdir().unwrap();
    let detached = tempfile::tempdir().unwrap();
    let on_inline = opening_inline(inline.path());
    let on_detached = opening_detached(detached.path());
    assert!(
        on_inline.contains("Held by your coordinator") && on_inline.contains("briefed.txt@"),
        "the opening names what the coordinator read: {on_inline}"
    );
    assert_eq!(
        on_inline, on_detached,
        "one section, whichever path drains capture"
    );
}

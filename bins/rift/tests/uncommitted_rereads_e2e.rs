//! End to end, through the shipped binary: a re-read of a file no commit
//! holds is decided exactly as a re-read of a committed one.
//!
//! A revision, to Rift, is the SHA-256 of the bytes as a window read them;
//! whether git has committed those bytes is git's business. So a window that
//! reads a file and then asks for the same lines again, with the bytes
//! unchanged, is declined whether the file is tracked with uncommitted edits,
//! untracked, staged in a repository with no commit at all, or (once the
//! record is git-optional) in a directory git has never seen. The control is
//! the other half of the same promise: bytes that moved between the reads are
//! never declined as held, and the change is named against the snapshot the
//! worker kept of the revision the window read, because no commit holds it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    agent_hook, agent_hook_out, declined_with, enable_recall, enforce_for, git_in, journal_kinds,
    journal_kinds_of, journal_rows, json_str, seed_live_session_repo,
    seed_live_session_repo_unborn, sync_env, window_key, DetachedHooks,
};

const SESSION: &str = "e2e-live-1";

/// Forty numbered lines, with `extra` spliced in after line 7 when given.
fn numbered(extra: Option<&str>) -> String {
    use std::fmt::Write as _;
    let mut body = String::new();
    for n in 1..=40 {
        let _ = writeln!(body, "line {n}");
        if let (7, Some(extra)) = (n, extra) {
            let _ = writeln!(body, "{extra}");
        }
    }
    body
}

/// A whole-file `Read` of `file`, as Claude Code sends one.
fn read_of(file: &Path) -> String {
    format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":1,"limit":500}}}}"#,
        json_str(file)
    )
}

/// One read as the client makes it: the decision before, the result after.
/// Hands back what the decision wrote to stdout.
fn read_call(repo: &Path, turn: u32, file: &Path, env: &[(&str, &str)]) -> String {
    let payload = read_of(file);
    let out = agent_hook_out(
        repo,
        "claude-code",
        "PreToolUse",
        SESSION,
        turn,
        &payload,
        env,
    );
    if declined_with(&out).is_none() {
        agent_hook(repo, "PostToolUse", SESSION, turn, &payload, env);
    }
    out
}

/// The walk every case shares: a window reads the file, then asks for the
/// same lines again with the bytes untouched. Hands back the second answer.
fn reread_unchanged(repo: &Path, file: &Path, env: &[(&str, &str)]) -> String {
    let first = read_call(repo, 1, file, env);
    assert_eq!(declined_with(&first), None, "the first read is the saving");
    read_call(repo, 2, file, env)
}

/// The decline a held, unchanged re-read gets: it names the file and the
/// lines, and the record says the window was declined once.
fn assert_declined_as_held(repo: &Path, second: &str, name: &str) {
    let reason = declined_with(second).expect("an unchanged re-read is declined");
    assert!(reason.contains(name), "the reason names the file: {reason}");
    assert!(
        reason.contains("lines 1-40"),
        "and the lines it already holds: {reason}"
    );
    assert!(
        !reason.contains("line 4\n"),
        "never a byte of the file: {reason}"
    );
    assert_eq!(journal_kinds(repo), vec!["read", "deny"]);
}

/// (a) ★ A tracked file whose working bytes no commit holds is declined on
/// an unchanged re-read: the revision is the bytes, not `HEAD:<path>`.
#[test]
fn a_tracked_file_with_uncommitted_edits_is_declined_on_an_unchanged_reread() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    enforce_for(&repo, "claude-code");
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    let file = repo.join("tracked.txt");
    std::fs::write(&file, "committed\n").unwrap();
    git_in(&repo, &["add", "tracked.txt"]);
    git_in(&repo, &["commit", "-q", "-m", "tracked"]);
    std::fs::write(&file, numbered(None)).unwrap();

    let second = reread_unchanged(&repo, &file, &env);
    assert_declined_as_held(&repo, &second, "tracked.txt");
}

/// (b) ★ A file git has never been told about is declined on an unchanged
/// re-read.
#[test]
fn an_untracked_new_file_is_declined_on_an_unchanged_reread() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    enforce_for(&repo, "claude-code");
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    let file = repo.join("scratch.md");
    std::fs::write(&file, numbered(None)).unwrap();

    let second = reread_unchanged(&repo, &file, &env);
    assert_declined_as_held(&repo, &second, "scratch.md");
}

/// (c) ★ A repository with an unborn `HEAD` — no commit anywhere — still
/// declines an unchanged re-read of a staged file and of an untracked one.
#[test]
fn a_file_in_a_repository_with_an_unborn_head_is_declined_on_an_unchanged_reread() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo_unborn(tmp.path());
    enforce_for(&repo, "claude-code");
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    let file = repo.join("draft.txt");
    std::fs::write(&file, numbered(None)).unwrap();
    git_in(&repo, &["add", "draft.txt"]);
    assert!(
        std::process::Command::new("git")
            .args(["rev-parse", "--verify", "-q", "HEAD"])
            .current_dir(&repo)
            .output()
            .unwrap()
            .stdout
            .is_empty(),
        "the fixture has no commit"
    );

    let second = reread_unchanged(&repo, &file, &env);
    assert_declined_as_held(&repo, &second, "draft.txt");
}

/// A directory git has never seen.
fn seed_non_git_dir(tmp: &Path) -> PathBuf {
    let dir = tmp.join("plain");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// (d) A directory with no git at all declines an unchanged re-read once the
/// record is git-optional.
#[test]
#[ignore = "waits for a git-optional read record: a directory with no git has nowhere to keep the read fact yet"]
fn a_file_in_a_directory_git_has_never_seen_is_declined_on_an_unchanged_reread() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = seed_non_git_dir(tmp.path());
    enforce_for(&dir, "claude-code");
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    let file = dir.join("notes.txt");
    std::fs::write(&file, numbered(None)).unwrap();

    let second = reread_unchanged(&dir, &file, &env);
    assert_declined_as_held(&dir, &second, "notes.txt");
}

/// The control, by another hand: bytes of an untracked file that moved
/// between the reads are never declined as held, and the window, once
/// compacted, is told where the file changed — against the snapshot of the
/// revision it read, since no commit holds that revision.
#[test]
fn an_untracked_file_changed_between_reads_is_allowed_and_its_delta_named_from_the_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    enforce_for(&repo, "claude-code");
    enable_recall(&repo);
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    let file = repo.join("moving.txt");
    std::fs::write(&file, numbered(None)).unwrap();

    assert_eq!(declined_with(&read_call(&repo, 1, &file, &env)), None);
    std::fs::write(&file, numbered(Some("line 7b"))).unwrap();
    assert_eq!(
        declined_with(&read_call(&repo, 2, &file, &env)),
        None,
        "bytes the window never read are not held"
    );
    // Moved again, so the revision the window holds now is one no commit and
    // no working file has: only the worker's snapshot of it is left.
    std::fs::write(&file, numbered(Some("line 7c"))).unwrap();
    agent_hook(&repo, "PreCompact", SESSION, 3, "{}", &env);
    let start = agent_hook_out(
        &repo,
        "claude-code",
        "SessionStart",
        SESSION,
        4,
        r#"{"source":"compact"}"#,
        &env,
    );
    assert!(
        start.contains("moving.txt@"),
        "the map names the file: {start}"
    );
    assert!(
        start.contains("changed at lines 8-8"),
        "and where it moved since the revision the window held: {start}"
    );
    assert!(!start.contains("line 7c"), "never a byte of it: {start}");
}

/// The control, by the window's own hand: an uncommitted file the window
/// edited after reading it is declined with the delta, not as held.
#[test]
fn an_uncommitted_file_the_window_edited_is_declined_with_its_delta() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    enforce_for(&repo, "claude-code");
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    let file = repo.join("edited.txt");
    std::fs::write(&file, numbered(None)).unwrap();

    assert_eq!(declined_with(&read_call(&repo, 1, &file, &env)), None);
    std::fs::write(&file, numbered(Some("line 7b"))).unwrap();
    let edit = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":{},"old_string":"line 7\n","new_string":"line 7\nline 7b\n","replace_all":false}}}}"#,
        json_str(&file)
    );
    agent_hook(&repo, "PostToolUse", SESSION, 2, &edit, &env);

    let verify = read_call(&repo, 3, &file, &env);
    let delta = declined_with(&verify).expect("the verification read is declined");
    assert!(
        delta.contains("changed only at lines 7-8") && delta.contains("(+1 lines)"),
        "with the place and the shift: {delta}"
    );
    assert!(!delta.contains("line 7b"), "never a byte of it: {delta}");
}

/// The subagent the family walk spawns, under the coordinator's session id.
const CHILD: &str = "a0e79504";

/// A Rust file of `functions` functions, each a doc line, a signature, ten
/// statements, the value and a close: 14 lines a function.
fn functions_of(functions: u32) -> String {
    use std::fmt::Write as _;
    let mut body = String::new();
    for f in 1..=functions {
        let _ = writeln!(body, "/// Step {f} of the walk, spelled out line by line.");
        let _ = writeln!(body, "pub fn step_{f}(input: u64) -> u64 {{");
        for n in 1..=10 {
            let _ = writeln!(
                body,
                "    let input = input.wrapping_mul({n}).wrapping_add({f});"
            );
        }
        let _ = writeln!(body, "    input\n}}");
    }
    body
}

/// The repository asks for declines, the family-held one at enforce among
/// them, and has Recall on, so the worker binds the outline of what was read.
fn enforce_family_held(repo: &Path) {
    enforce_for(repo, "claude-code");
    let serve = repo.join(".rift").join("policies").join("serve.toml");
    let policy = std::fs::read_to_string(&serve).unwrap().replacen(
        "[interception]\n",
        "[interception]\ndecline_family_held = \"enforce\"\n",
        1,
    );
    std::fs::write(&serve, policy).unwrap();
    enable_recall(repo);
}

/// The hook envelope Claude Code's command line carries, around `payload`.
fn envelope(turn: u32, payload: &str) -> String {
    format!(r#"{{"schema":1,"session":"{SESSION}","turn":{turn},"payload":{payload}}}"#)
}

/// The coordinator's whole read of `file` on the detached path, as the
/// shipped hooks run it: the decision, the result, and the worker job behind
/// them waited out — the job that binds and caches the outline of the bytes
/// it read.
fn coordinator_reads_on_the_detached_path(repo: &Path, file: &Path, env: &[(&str, &str)]) {
    let detached: Vec<(&str, &str)> = env
        .iter()
        .copied()
        .filter(|(key, _)| *key != "RIFT_CAPTURE_SYNC")
        .collect();
    let hooks = DetachedHooks::default();
    let body = envelope(2, &read_of(file));
    for verb in ["PreToolUse", "PostToolUse"] {
        let (code, _) = hooks.run(repo, &["hooks", "claude-code", verb], &body, &detached);
        assert_eq!(code, 0, "an agent hook never fails");
    }
    hooks.await_workers(|| true);
}

/// ★ A live subagent's first read of an untracked file its coordinator read
/// whole, the bytes unchanged, is declined at enforce with the outline the
/// coordinator's read bound, and the subagent's journal records one acted
/// family decline on the outline route.
#[test]
fn a_live_subagents_first_read_of_an_untracked_file_its_coordinator_holds_is_declined_with_the_outline(
) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    enforce_family_held(&repo);
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    // 16 functions, 224 lines: past the outline frame's 95-line page floor,
    // under first contact's 400-line floor, so the family route is the one
    // that can answer.
    let file = repo.join("walk.rs");
    std::fs::write(&file, functions_of(16)).unwrap();

    agent_hook_out(
        &repo,
        "claude-code",
        "UserPromptSubmit",
        SESSION,
        1,
        "{}",
        &env,
    );
    coordinator_reads_on_the_detached_path(&repo, &file, &env);
    let coordinator = format!("{}.jsonl", window_key(SESSION, ""));
    assert_eq!(journal_kinds_of(&repo, &coordinator), vec!["read"]);

    let opened = format!(r#"{{"agent_id":"{CHILD}","agent_type":"general-purpose"}}"#);
    agent_hook_out(
        &repo,
        "claude-code",
        "SubagentStart",
        SESSION,
        3,
        &opened,
        &env,
    );
    let child_read = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":1,"limit":500}},"agent_id":"{CHILD}","agent_type":"general-purpose"}}"#,
        json_str(&file)
    );
    let out = agent_hook_out(
        &repo,
        "claude-code",
        "PreToolUse",
        SESSION,
        3,
        &child_read,
        &env,
    );

    let frame = declined_with(&out).expect("the subagent's first read is declined");
    assert!(
        frame.starts_with("[rift] Read outlined: walk.rs@"),
        "{frame}"
    );
    assert!(
        frame.contains("is held at this revision by another window of this repository"),
        "{frame}"
    );
    assert!(
        !frame.contains("input.wrapping_mul"),
        "never a byte: {frame}"
    );
    let child = format!("{}.jsonl", window_key(SESSION, CHILD));
    let family: Vec<serde_json::Value> = journal_rows(&repo, &child)
        .into_iter()
        .filter(|row| row["k"] == "family")
        .collect();
    assert_eq!(family.len(), 1, "one family decline: {family:?}");
    assert_eq!(family[0]["route"], "outline", "{family:?}");
    assert_eq!(family[0]["acted"], true, "{family:?}");
}

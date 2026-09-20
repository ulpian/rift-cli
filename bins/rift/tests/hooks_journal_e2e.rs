//! End-to-end: the window journal and the decline — a read and an edit leave
//! the window a ledger of what it holds, a verification read is declined once
//! with the ways out of it open, and a family whose edit echo nothing pins is
//! never declined on its own edit.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    agent_hook, agent_hook_out, declined_with, edit_old_to_new_payload, enforce_for, journal_kinds,
    journal_kinds_of, journal_rows, json_str, read_payload, seed_live_session_repo, sync_env,
    window_journal_files,
};

// --- the window journal ------------------------------------------------------

/// Through the real binary: the agent edits a file it read in full, and the
/// window's journal says where the file changed, by how much, and that the
/// window still holds every line of the revision it left behind.
///
/// The bytes the edit put there are bytes the agent authored, so the follow-up
/// read that verifies them is the largest thing this whole mechanism exists to
/// answer. Answering it needs three facts to be right at once, from the shipped
/// binary and not from a unit's idea of one: the place, in the numbering of the
/// new bytes; the shift; and the coverage, which is only whole because the
/// window had read the whole file first.
#[test]
fn an_edit_hook_leaves_the_window_holding_the_bytes_it_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let file = repo.join("b.txt");
    std::fs::write(&file, "a\nold\nc\n").unwrap();
    let read = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":1,"limit":500}}}}"#,
        json_str(&file)
    );
    agent_hook(&repo, "PreToolUse", "e2e-live-1", 1, &read, &env);

    // The edit the agent then makes, and the bytes it leaves behind.
    let edit = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":{},"old_string":"old\n","new_string":"new\nnewer\n","replace_all":false}}}}"#,
        json_str(&file)
    );
    std::fs::write(&file, "a\nnew\nnewer\nc\n").unwrap();
    agent_hook(&repo, "PostToolUse", "e2e-live-1", 2, &edit, &env);

    let names = window_journal_files(&repo);
    let name = names.first().expect("one context window, one journal");
    let path = repo.join(".rift").join("state").join("reads").join(name);
    let rows: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    let kinds: Vec<&str> = rows.iter().map(|r| r["k"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["read", "edit"], "{rows:?}");

    let edited = &rows[1];
    assert_eq!(edited["p"], "b.txt", "the checkout's own spelling");
    assert_eq!(
        edited["h_old"], rows[0]["h"],
        "the revision the window had read is the one the edit started from"
    );
    assert_eq!(
        edited["sites"],
        serde_json::json!([[2, 3]]),
        "lines 2-3 of the file as it is now: {edited}"
    );
    assert_eq!(edited["shift"], 1, "one line longer than it was");
    assert_eq!(
        edited["full"], true,
        "it had read every line, and it wrote the difference"
    );
    assert_eq!(
        edited["h_new"].as_str().map(str::len),
        Some(64),
        "a revision, in the only shape a reader accepts: {edited}"
    );
    assert!(
        !edited.as_object().unwrap().contains_key("content")
            && !serde_json::to_string(edited)
                .unwrap()
                .contains("new\\nnewer"),
        "identifiers and numbers, never a byte of the file: {edited}"
    );
}

/// Through the real binary: a read arrives, and the window's own journal says
/// which lines of which revision it was handed — one file under this
/// checkout's run state, named for the context window and nothing else.
///
/// The second read of the same lines is the whole point. Nothing is declined
/// while the lever is in observe, but the journal says out loud that a decline
/// is what those bytes were worth, which is the number the flip is decided on.
#[test]
fn a_read_hook_leaves_the_window_a_journal_of_the_lines_it_was_handed() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    let read = read_payload(&repo.join("a.txt"), 1, 1);

    agent_hook(&repo, "PreToolUse", "e2e-live-1", 1, &read, &env);

    let names = window_journal_files(&repo);
    assert_eq!(names.len(), 1, "one context window, one journal: {names:?}");
    let name = names.first().unwrap();
    let stem = name.strip_suffix(".jsonl").expect("a journal file: {name}");
    assert_eq!(
        stem.len(),
        64,
        "named for the window, not the session: {name}"
    );
    assert!(
        stem.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "the name is a hash and nothing else: {name}"
    );

    let first = journal_rows(&repo, name).remove(0);
    assert_eq!(first["k"], "read");
    assert_eq!(
        first["p"], "a.txt",
        "the checkout's own spelling of the path"
    );
    assert_eq!(first["lo"], 1);
    assert_eq!(first["hi"], 1);
    assert_eq!(
        first["n"], 1,
        "the file's own length, counted while hashing"
    );
    assert_eq!(
        first["h"].as_str().map(str::len),
        Some(64),
        "a revision, in the only shape a reader accepts: {first}"
    );

    // The same lines again, at the same revision.
    agent_hook(&repo, "PreToolUse", "e2e-live-1", 2, &read, &env);
    let rows = journal_rows(&repo, name);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(
        rows[1]["k"], "observe",
        "the repeat is what a decline would be for, and nothing was declined"
    );

    // And the compaction signal empties the window, so the next read of those
    // lines is a first read again rather than a repeat.
    agent_hook(&repo, "PreCompact", "e2e-live-1", 2, "{}", &env);
    agent_hook(&repo, "PreToolUse", "e2e-live-1", 3, &read, &env);
    let kinds = journal_kinds_of(&repo, name);
    assert_eq!(kinds, vec!["read", "observe", "evict", "read"], "{kinds:?}");
}

/// The whole walk, through the shipped binary and the repository's own policy
/// file: read a file in full, edit it, read it back to check the edit.
///
/// The verification read is the largest measured pool this mechanism exists
/// for, and every clause of the answer it gets is asserted here rather than in
/// a unit's idea of one: declined exactly once, with the places the file
/// changed in the numbering the reader is about to see, the shift, and a
/// window worth reading instead. Then the two escapes — the narrow read of the
/// changed lines, which is the behaviour the frame asks for, and the identical
/// repeat, which is what bounds a model that answers a decline by asking again
/// — both run untouched.
#[test]
fn a_verification_read_is_declined_once_and_the_ways_out_of_it_are_open() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    enforce_for(&repo, "claude-code");
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    let file = repo.join("b.txt");
    std::fs::write(&file, "a\nold\nc\n").unwrap();
    let whole = read_payload(&file, 1, 500);
    let read_at = |turn: u32, payload: &str| -> String {
        agent_hook_out(
            &repo,
            "claude-code",
            "PreToolUse",
            "e2e-gate",
            turn,
            payload,
            &env,
        )
    };

    let first = read_at(1, &whole);
    assert_eq!(declined_with(&first), None, "the first read is the saving");

    let edit = edit_old_to_new_payload(&file);
    std::fs::write(&file, "a\nnew\nnewer\nc\n").unwrap();
    agent_hook(&repo, "PostToolUse", "e2e-gate", 2, &edit, &env);

    let verify = read_at(3, &whole);
    let reason = declined_with(&verify).expect("the verification read is declined: {verify}");
    assert!(
        reason.contains("changed only at lines 2-3"),
        "the places, in the numbering of the bytes it is about to see: {reason}"
    );
    assert!(reason.contains("(+1 lines)"), "and the shift: {reason}");
    assert!(
        reason.contains("Read lines 1-4"),
        "and a window inside the file: {reason}"
    );
    assert!(
        !reason.contains("newer"),
        "identifiers and numbers, never a byte of the file: {reason}"
    );

    let inside = read_at(4, &read_payload(&file, 2, 2));
    assert_eq!(
        declined_with(&inside),
        None,
        "reading the changed lines is the behaviour the reason asks for"
    );

    let again = read_at(5, &whole);
    assert_eq!(
        declined_with(&again),
        None,
        "one decline per call, so the repeat is honoured"
    );

    assert_eq!(
        journal_kinds(&repo),
        vec!["read", "edit", "deny", "read", "read"],
        "declined once, then handed the bytes twice and recorded as holding them"
    );
}

/// The verification walk on Antigravity, through the shipped binary: read the
/// whole file, replace one line with two, read it back to check. Returns what
/// the hook answered that third call with.
fn antigravity_verification_walk(repo: &Path, file: &Path, env: &[(&str, &str)]) -> String {
    let view = format!(
        r#"{{"toolCall":{{"name":"view_file","args":{{"AbsolutePath":{},"StartLine":0,"EndLine":500}}}}}}"#,
        json_str(file)
    );
    let replace = format!(
        r#"{{"toolCall":{{"name":"replace_file_content","args":{{"TargetFile":{},"TargetContent":"old\n","ReplacementContent":"new\nnewer\n","AllowMultiple":false}}}}}}"#,
        json_str(file)
    );
    std::fs::write(file, "a\nold\nc\n").unwrap();
    assert_eq!(
        declined_with(&agent_hook_out(
            repo,
            "antigravity",
            "PreToolUse",
            "e2e-agy-echo",
            1,
            &view,
            env,
        )),
        None,
        "the first read is the saving"
    );
    std::fs::write(file, "a\nnew\nnewer\nc\n").unwrap();
    agent_hook_out(
        repo,
        "antigravity",
        "PostToolUse",
        "e2e-agy-echo",
        2,
        &replace,
        env,
    );
    agent_hook_out(
        repo,
        "antigravity",
        "PreToolUse",
        "e2e-agy-echo",
        3,
        &view,
        env,
    )
}

/// The same walk on Claude Code, whose edit result is pinned to tell the model
/// the file it just wrote is current in its context.
fn claude_code_verification_walk(repo: &Path, file: &Path, env: &[(&str, &str)]) -> String {
    let whole = read_payload(file, 1, 500);
    let edit = edit_old_to_new_payload(file);
    std::fs::write(file, "a\nold\nc\n").unwrap();
    agent_hook_out(
        repo,
        "claude-code",
        "PreToolUse",
        "e2e-cc-echo",
        1,
        &whole,
        env,
    );
    std::fs::write(file, "a\nnew\nnewer\nc\n").unwrap();
    agent_hook(repo, "PostToolUse", "e2e-cc-echo", 2, &edit, env);
    agent_hook_out(
        repo,
        "claude-code",
        "PreToolUse",
        "e2e-cc-echo",
        3,
        &whole,
        env,
    )
}

/// ★ One policy file, both families named `enforce`, one walk each: the family
/// whose edit result is pinned to say the model holds what it wrote is
/// declined once, and the family whose edit result nothing pins is not
/// declined at all.
///
/// This is the exact configuration an operator may legitimately write, so the
/// guard cannot live in a mode. Antigravity's own artifact says nothing about
/// what `replace_file_content` returns, which makes the verification read the
/// only sight of the edit that agent has; taking it away would buy a saving
/// with the one thing the read exists to provide. The edit is
/// still recorded — the coverage of the old bytes still has to stop being
/// coverage — so what is missing from that journal is the decision, not the
/// line.
#[test]
fn a_family_whose_edit_echo_is_not_pinned_is_never_declined_on_its_own_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let policies = repo.join(".rift").join("policies");
    std::fs::create_dir_all(&policies).unwrap();
    std::fs::write(
        policies.join("serve.toml"),
        "[interception]\nmode = \"enforce\"\nmin_slice_lines = 1\n\n\
         [interception.agents]\nantigravity = \"enforce\"\nclaude-code = \"enforce\"\n",
    )
    .unwrap();
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let file = repo.join("b.txt");

    let verify = antigravity_verification_walk(&repo, &file, &env);
    assert_eq!(
        declined_with(&verify),
        None,
        "a decline here would remove this agent's only sight of its own edit: {verify}"
    );
    let unpinned = window_journal_files(&repo);
    let [agy] = unpinned.as_slice() else {
        panic!("one window so far, got {unpinned:?}");
    };
    assert_eq!(
        journal_kinds_of(&repo, agy),
        vec!["read", "edit", "read"],
        "the edit is recorded and the read runs; only the decision is absent"
    );

    // The same policy file, the same shape of walk, on the family whose edit
    // result is pinned: still declined, exactly once. The guard is the
    // family's, not a stand-down for everyone.
    let reason = declined_with(&claude_code_verification_walk(&repo, &file, &env))
        .expect("the pinned family is still declined");
    assert!(
        reason.contains("changed only at lines 2-3"),
        "with the places, in the numbering it is about to see: {reason}"
    );
    let both = window_journal_files(&repo);
    let cc = both
        .iter()
        .find(|name| *name != agy)
        .expect("the second window has its own journal");
    assert_eq!(journal_kinds_of(&repo, cc), vec!["read", "edit", "deny"]);
}

/// Repository-wide enforcement reaches Antigravity without an agent-specific
/// override. Retry telemetry does not demote support or policy; the identical
/// third call is allowed as the universal one-deny safety escape.
#[test]
fn repository_wide_enforcement_reaches_antigravity_with_one_retry_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let policies = repo.join(".rift").join("policies");
    std::fs::create_dir_all(&policies).unwrap();
    std::fs::write(
        policies.join("serve.toml"),
        "[interception]\nmode = \"enforce\"\nmin_slice_lines = 1\n",
    )
    .unwrap();
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let file = repo.join("b.txt");
    std::fs::write(&file, "a\nb\nc\nd\n").unwrap();
    let view = format!(
        r#"{{"toolCall":{{"name":"view_file","args":{{"AbsolutePath":{},"StartLine":0,"EndLine":4}}}}}}"#,
        json_str(&file)
    );

    let first = agent_hook_out(
        &repo,
        "antigravity",
        "PreToolUse",
        "e2e-agy",
        1,
        &view,
        &env,
    );
    assert_eq!(declined_with(&first), None, "the first read runs");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(first.trim()).unwrap(),
        serde_json::json!({"decision": "allow"})
    );
    let second = agent_hook_out(
        &repo,
        "antigravity",
        "PreToolUse",
        "e2e-agy",
        2,
        &view,
        &env,
    );
    assert!(
        declined_with(&second).is_some(),
        "the unchanged repeat is declined"
    );
    let third = agent_hook_out(
        &repo,
        "antigravity",
        "PreToolUse",
        "e2e-agy",
        3,
        &view,
        &env,
    );
    assert_eq!(declined_with(&third), None, "the identical retry runs");

    assert_eq!(
        journal_kinds(&repo),
        vec!["read", "deny", "read"],
        "the journal records the active decision and bounded retry"
    );
}

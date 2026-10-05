//! Process-level proof that a session's window journal is frozen into its
//! checkpoint: a session reads a file, is declined on the re-read and is
//! emptied by a compaction through the real binary; the commit's shard
//! carries `window_journal.jsonl` with exactly those rows of that window and
//! none of another session's, the trailer `prepare-commit-msg` writes names
//! that very shard, `rift verify` reads the leaf back, and the session's end
//! with nothing new since condenses to the same checkpoint.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;

use hooks_fixture::{
    agent_hook, agent_hook_out, checkpoint_shards, declined_with, enforce_for, git_in,
    hook_pair_commit, read_payload, run_rift_in, run_rift_stdout, seed_live_session_repo,
    session_phase, shard_blob, sync_env, window_key, LIVE_SESSION,
};

/// The shard directory a checkpoint id names.
fn shard_dir(checkpoint: &str) -> String {
    format!("{}/{}", &checkpoint[..2], &checkpoint[2..])
}

/// Give another session a window in this checkout: a copy of `from`'s rows
/// under the other session's key, so the only thing telling the two apart
/// is whose window the file is.
fn another_sessions_window(repo: &Path, from: &str, other: &str) {
    let reads = repo.join(".rift").join("state").join("reads");
    let rows = std::fs::read_to_string(reads.join(format!("{from}.jsonl"))).unwrap();
    std::fs::write(reads.join(format!("{other}.jsonl")), rows).unwrap();
}

/// The `(window, kind)` of every frozen line in the shard at `checkpoint`.
fn frozen_rows(repo: &Path, checkpoint: &str) -> Vec<serde_json::Value> {
    let path = format!("{}/window_journal.jsonl", shard_dir(checkpoint));
    shard_blob(repo, &path)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// ★ A read, the decline of its repeat and the compaction that empties the
/// window are frozen into the session's checkpoint under the window's own
/// key, the trailer names that checkpoint, `rift verify` reads the leaf back,
/// and another session's window in the same checkout reaches no line. The
/// session's end then condenses again with nothing new since, which is the
/// same checkpoint rather than a second one.
#[test]
fn a_sessions_reads_declines_and_evictions_are_frozen_into_its_checkpoint_and_no_one_elses() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    enforce_for(&repo, "claude-code");
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    let file = repo.join("b.txt");
    std::fs::write(&file, "a\nb\nc\n").unwrap();
    let whole = read_payload(&file, 1, 500);

    agent_hook(&repo, "PreToolUse", LIVE_SESSION, 1, &whole, &env);
    let again = agent_hook_out(
        &repo,
        "claude-code",
        "PreToolUse",
        LIVE_SESSION,
        2,
        &whole,
        &env,
    );
    assert!(
        declined_with(&again).is_some(),
        "the repeat is declined: {again}"
    );
    agent_hook(&repo, "PreCompact", LIVE_SESSION, 2, "{}", &env);
    let own = window_key(LIVE_SESSION, "");
    let other = window_key("e2e-someone-else", "");
    another_sessions_window(&repo, &own, &other);

    git_in(&repo, &["add", "b.txt"]);
    let predicted = hook_pair_commit(&repo, tmp.path(), "frozen", &env);
    assert_eq!(checkpoint_shards(&repo), vec![shard_dir(&predicted)]);
    let rows = frozen_rows(&repo, &predicted);
    let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["read", "deny", "evict"], "{rows:?}");
    assert!(rows.iter().all(|r| r["window"] == own.as_str()), "{rows:?}");
    assert_eq!(rows[0]["path"], "b.txt", "the checkout's own spelling");
    assert_eq!(rows[2]["reason"], "pre_compact");
    let (code, out) = run_rift_stdout(&repo, &["verify"], &env);
    assert_eq!(code, 0, "verify reads the window-journal leaf back: {out}");

    let end = format!(
        r#"{{"schema":1,"session":"{LIVE_SESSION}","turn":3,"payload":{{"reason":"exit"}}}}"#
    );
    let code = run_rift_in(&repo, &["hooks", "claude-code", "SessionEnd"], &end, &env);
    assert_eq!(code, 0, "a capture hook never fails the agent");
    assert_eq!(
        session_phase(&repo, LIVE_SESSION),
        "ended",
        "the end condensed"
    );
    assert_eq!(
        checkpoint_shards(&repo),
        vec![shard_dir(&predicted)],
        "nothing new since, so the end's condense is the same checkpoint"
    );
}

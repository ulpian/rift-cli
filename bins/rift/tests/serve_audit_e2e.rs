//! Process-level serve-audit proof: the coverage the
//! retired stdio-transport audit test carried, replaced rather than merely
//! deleted.
//!
//! The old test spawned a real serving daemon, called a tool twice over its
//! stdio transport, and asserted the terminal audit rows
//! (`memd_call_event`/`mcp_connection_event`) landed with distinct call ids
//! and no leaked argument bytes. That transport and its connection-lease
//! lease scheme are gone; the surviving process-level serve path is
//! `rift recall <verb>` (the skill-CLI channel), and its audit
//! target is the successor pair, `serve_call_event` /
//! `serve_session_event` (schema 4), written by `record_serve`
//! (`bins/rift/src/recall/serve.rs`). This test drives the REAL `rift` binary twice
//! and reads the REAL `recall.db` projection back — same coverage class as
//! the test it replaces: real process, real audit write, a leak check over
//! every persisted column.
//!
//! The same serves are also frozen into the checkpoint shard, and the tests
//! at the foot of this file drive that through the shipped binary: a
//! decision attributed to its session in the recall projection and frozen by
//! the session's end from those rows, two sessions' interleaved serves each
//! frozen into their own session's shard only, a serve no session made left
//! an unattributed row and frozen nowhere, and the trailer's predicted
//! checkpoint id equal to the committed one with and without serves — with
//! `rift verify` reading the leaf back.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;
use std::process::{Command, Stdio};

mod common;
use common::rift_cmd;

#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
#[allow(dead_code)] // The read-gate suites share these builders; each file uses a subset.
#[path = "common/read_gate_fixture.rs"]
mod read_gate_fixture;

use hooks_fixture::{
    checkpoint_shards, git_in, hook_pair_commit, rift_json, seed_live_session_repo,
    shard_blob_bytes, sync_env, LIVE_SESSION,
};

use rift_store::{CanonicalStore, SqliteStore};

/// Run `rift <args>` from `cwd`; return the exit code and captured stdout.
/// Mirrors `common/hooks_fixture.rs`'s `run_rift_stdout` — each `tests/*.rs` file is its
/// own crate, so the helper is re-declared rather than shared (only
/// `common::rift_cmd`, the hermetic spawn chokepoint, is shared).
fn run_rift_stdout(cwd: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = rift_cmd(cwd);
    cmd.args(args).stdin(Stdio::null()).stderr(Stdio::null());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// A hermetic git repository with one commit — enough for `GixSubstrate::open`
/// and the recall ingest it feeds. Fixed content only; nothing here reads the
/// wall clock.
fn seed_repo(tmp: &Path) -> std::path::PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let st = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "e2e@example.com"]);
    git(&["config", "user.name", "e2e"]);
    std::fs::write(repo.join("a.rs"), "fn main() {}\n").unwrap();
    git(&["add", "a.rs"]);
    git(&["commit", "-q", "-m", "seed"]);
    repo
}

/// Recall declared on — it is on unless written off, but a fixture says what
/// it relies on — in the same TOML shape used throughout this crate's own
/// fixtures (`bins/rift/src/memory.rs`, `hooks.rs`). Enabled is half of what
/// `rift recall <verb>` asks before it answers; the repository must also be
/// set up on this machine, which `provision_here` below is.
fn enable_recall(repo: &Path) {
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift/config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
}

#[tokio::test]
async fn real_process_serve_calls_persist_serve_events_with_distinct_ids_and_no_leaked_arguments() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enable_recall(&repo);
    let cache = tmp.path().join("cache");
    common::provision_here(&repo, &cache);
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
    ];

    serve_twice_with_a_sentinel_argument(&repo, &env);

    // No detached audit subprocess here (unlike the retired daemon, whose
    // audit handoff was a sibling process race the old test polled for): the
    // CLI process awaits `record_serve` in the same `block_on` before it
    // prints and exits, so the projection is complete the moment the child
    // has returned — no poll loop needed.
    let repo_id = rift_store::recall_repo_id(&repo).unwrap();
    let db = rift_store::recall_db_under(&cache, &repo_id);
    assert!(db.is_file(), "the process must have created recall.db");
    let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();

    two_serve_calls_are_distinct_and_carry_no_argument(&store).await;
    one_session_row_per_channel(&store).await;
    the_plain_audit_twin_carries_no_argument(&store).await;
}

/// Two real process invocations of the surviving skill-CLI serve path, each
/// carrying a sentinel in exactly the argument shape the retired test planted
/// (a free-text task description and a file path) — the half of the surface a
/// hostile-argument leak would show up on.
fn serve_twice_with_a_sentinel_argument(repo: &Path, env: &[(&str, &str)]) {
    for _ in 0..2 {
        let (code, out) = run_rift_stdout(
            repo,
            &[
                "recall",
                "known-state",
                "--task",
                "PRIVATE_ARGUMENT_SENTINEL",
                "--file",
                "PRIVATE_ARGUMENT_SENTINEL.rs",
            ],
            env,
        );
        assert_eq!(
            code, 0,
            "known-state must succeed on an enabled repo: {out}"
        );
    }
}

/// Exactly two `serve_call_event` rows, with distinct call ids, the channel
/// and verb the CLI serves under, and the sentinel in no persisted column.
async fn two_serve_calls_are_distinct_and_carry_no_argument(store: &SqliteStore) {
    let rows = store
        .query(
            "SELECT call_id, channel, tool_name, terminal_verb, tokens_returned, tokens_avoided \
             FROM serve_call_event ORDER BY call_id",
            &[],
        )
        .await
        .unwrap();
    let [first, second] = rows.as_slice() else {
        panic!(
            "expected exactly two serve_call_event rows, got {}",
            rows.len()
        );
    };
    let first_id = first.get_text(0).unwrap();
    let second_id = second.get_text(0).unwrap();
    assert_ne!(
        first_id, second_id,
        "two real invocations remain distinct calls"
    );
    for row in &rows {
        assert_eq!(row.get_text(1).unwrap(), "skill_cli");
        assert_eq!(row.get_text(2).unwrap(), "known-state");
        assert!(row.get_text(3).unwrap().starts_with("memd."));
        assert!(row.get_i64(4).unwrap() >= 0);
        assert!(row.get_i64(5).unwrap() >= 0);
        for col in 0..4 {
            assert!(
                !row.get_text(col)
                    .unwrap()
                    .contains("PRIVATE_ARGUMENT_SENTINEL"),
                "a serve_call_event column carried the raw argument: column {col}"
            );
        }
    }
}

/// First-observed-per-channel: two real serves over the `skill_cli` channel
/// still leave exactly one `serve_session_event` row.
async fn one_session_row_per_channel(store: &SqliteStore) {
    let sessions = store
        .query("SELECT channel FROM serve_session_event", &[])
        .await
        .unwrap();
    let [session] = sessions.as_slice() else {
        panic!(
            "expected exactly one serve_session_event row, got {}",
            sessions.len()
        );
    };
    assert_eq!(session.get_text(0).unwrap(), "skill_cli");
}

/// The plain audit twin (schema 1, `memd_audit_event`) is the same real call
/// landing in the pre-existing audit relation `record_serve` also writes from
/// every serve, verb column checked for the same leak shape.
async fn the_plain_audit_twin_carries_no_argument(store: &SqliteStore) {
    let audit_rows = store
        .query("SELECT verb FROM memd_audit_event", &[])
        .await
        .unwrap();
    assert!(
        audit_rows.len() >= 2,
        "the plain audit relation must also see the real calls"
    );
    for row in &audit_rows {
        assert!(!row
            .get_text(0)
            .unwrap()
            .contains("PRIVATE_ARGUMENT_SENTINEL"));
    }
}

/// The family's recall projection under `cache`, open.
fn recall_store(repo: &Path, cache: &Path) -> SqliteStore {
    let repo_id = rift_store::recall_repo_id(repo).unwrap();
    let db = rift_store::recall_db_under(cache, &repo_id);
    rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap()
}

/// `(serve key, relation, window key)` of every serve the projection
/// attributes to `session`, sorted by key.
fn attributed(store: &SqliteStore, session: &str) -> Vec<(String, String, Option<String>)> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(store.query(
        "SELECT serve_key, relation, window_key FROM memd_audit_session \
         WHERE session_id = ? ORDER BY serve_key",
        &[rift_store::Value::text(session)],
    ))
    .unwrap()
    .iter()
    .map(|row| {
        (
            row.get_text(0).unwrap(),
            row.get_text(1).unwrap(),
            row.get_opt_text(2).unwrap(),
        )
    })
    .collect()
}

/// How many rows a relation of the projection holds.
fn count(store: &SqliteStore, relation: &str) -> i64 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(store.count(relation)).unwrap()
}

/// No side file holds a serve: nothing is written under the common dir's
/// `rift-serve-audit/`.
fn assert_no_serve_journal(repo: &Path) {
    assert!(
        !repo.join(".git").join("rift-serve-audit").exists(),
        "a serve is held by the projection and the shard, never by a journal"
    );
}

/// A line the shard holds is canonical: re-serialising it changes nothing,
/// so its keys are sorted and nothing but JSON is in it.
fn assert_canonical(line: &str) -> serde_json::Value {
    let value: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(serde_json::to_string(&value).unwrap(), line, "keys sorted");
    value
}

/// One read declined in `session`: the file `name` is read, then read again.
fn decline_a_repeat_read(repo: &Path, session: &str, name: &str, env: &[(&str, &str)]) {
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    let file = repo.join(name);
    std::fs::write(&file, read_gate_fixture::numbered(40, None)).unwrap();
    let read = read_gate_fixture::read_of(&file, 1, 40);
    let hook = |turn| {
        read_gate_fixture::hook(
            repo,
            "claude-code",
            "PreToolUse",
            session,
            turn,
            &read,
            &env,
        )
    };
    hook(1);
    assert!(
        read_gate_fixture::declined_with(&hook(2)).is_some(),
        "the repeat is declined"
    );
}

/// The bytes of `serve_audit.jsonl` in the shard `checkpoint` names, if any.
fn frozen_serves(repo: &Path, checkpoint: &str) -> Option<String> {
    let dir = format!("{}/{}", &checkpoint[..2], &checkpoint[2..]);
    checkpoint_shards(repo).contains(&dir).then(|| {
        String::from_utf8(shard_blob_bytes(repo, &format!("{dir}/serve_audit.jsonl"))).unwrap()
    })
}

/// The keys of a frozen artifact's lines, in file order.
fn keys_of(frozen: &str) -> Vec<String> {
    frozen
        .lines()
        .map(|line| {
            assert_canonical(line)["idempotency_key"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// ★ A decision's two rows are attributed to its session in the projection;
/// the session's end freezes exactly those rows into its checkpoint as
/// `serve_audit.jsonl`, each line naming the session and carrying the instant
/// its row was written at; and `rift verify` reads the leaf back.
#[test]
fn a_decline_is_frozen_into_the_session_end_checkpoint_from_the_rows_that_name_its_session() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    read_gate_fixture::enforce_for(&repo, &["claude-code"]);
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);

    decline_a_repeat_read(&repo, LIVE_SESSION, "booked.txt", &env);
    let rows = attributed(&recall_store(&repo, &cache), LIVE_SESSION);
    let relations: Vec<&str> = rows.iter().map(|(_, r, _)| r.as_str()).collect();
    assert_eq!(relations.len(), 2, "one decision, two halves: {rows:?}");
    assert!(relations.contains(&"memd_audit_event") && relations.contains(&"serve_call_event"));

    let stopped = rift_json(&repo, &["session", "stop", LIVE_SESSION, "--json"], &env);
    assert_eq!(stopped["finalized"][0], LIVE_SESSION, "{stopped}");
    let [checkpoint] = checkpoint_shards(&repo).try_into().unwrap();
    let frozen = frozen_serves(&repo, &checkpoint.replace('/', "")).unwrap();
    let keys: Vec<String> = rows.iter().map(|(key, _, _)| key.clone()).collect();
    assert_eq!(keys_of(&frozen), keys, "the rows, sorted by key");
    for line in frozen.lines() {
        let value = assert_canonical(line);
        assert_eq!(value["session_id"], LIVE_SESSION, "{line}");
        assert_eq!(
            value.get("window_key"),
            None,
            "the read gate names no window"
        );
        if value["schema"] == 1 {
            assert!(value["recorded_at"].is_i64(), "{line}");
        }
    }
    assert_no_serve_journal(&repo);
    let (code, out) = run_rift_stdout(&repo, &["verify"], &env);
    assert_eq!(code, 0, "verify reads the serve leaf back: {out}");
}

/// ★ Two sessions interleaving their serves: the checkpoint of one freezes
/// its own serves and none of the other's, whichever was made first.
#[test]
fn interleaved_sessions_freeze_only_their_own_serves() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    read_gate_fixture::enforce_for(&repo, &["claude-code"]);
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    let other = "e2e-other-session";

    decline_a_repeat_read(&repo, other, "theirs.txt", &env);
    decline_a_repeat_read(&repo, LIVE_SESSION, "mine.txt", &env);
    let store = recall_store(&repo, &cache);
    let keys = |session| -> Vec<String> {
        attributed(&store, session)
            .into_iter()
            .map(|(key, _, _)| key)
            .collect()
    };
    let (mine, theirs) = (keys(LIVE_SESSION), keys(other));
    assert_eq!((mine.len(), theirs.len()), (2, 2));

    rift_json(&repo, &["session", "stop", LIVE_SESSION, "--json"], &env);
    let [checkpoint] = checkpoint_shards(&repo).try_into().unwrap();
    let frozen = frozen_serves(&repo, &checkpoint.replace('/', "")).unwrap();
    assert_eq!(keys_of(&frozen), mine);
    assert!(theirs.iter().all(|key| !frozen.contains(key.as_str())));
    assert_no_serve_journal(&repo);
}

/// ★ A serve no session made — a person at a terminal — is an ordinary row
/// with no attribution, and no checkpoint freezes it.
#[test]
fn a_person_made_serve_stays_an_unattributed_row_and_is_frozen_nowhere() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    enable_recall(&repo);
    let cache = tmp.path().join("cache");
    common::provision_here(&repo, &cache);
    let env = sync_env(&cache);

    let (code, out) = run_rift_stdout(
        &repo,
        &["recall", "known-state", "--task", "t", "--file", "a.txt"],
        &env,
    );
    assert_eq!(code, 0, "{out}");
    let store = recall_store(&repo, &cache);
    assert_eq!(count(&store, "memd_audit_event"), 1);
    assert_eq!(count(&store, "serve_call_event"), 1);
    assert_eq!(count(&store, "memd_audit_session"), 0);

    rift_json(&repo, &["session", "stop", LIVE_SESSION, "--json"], &env);
    assert!(
        !checkpoint_shards(&repo).is_empty(),
        "the session condensed"
    );
    let names = hooks_fixture::shard_tree_names(&repo);
    assert!(!names.contains("serve_audit.jsonl"), "{names}");
    assert_no_serve_journal(&repo);
}

/// ★ **The riskiest line: one id, predicted and committed.** The trailer
/// `prepare-commit-msg` writes names the checkpoint the worker's condense of
/// that very commit archives — for a commit whose session made no serve, and
/// for one whose session made a decision, whose two rows the shard then
/// carries.
#[test]
fn the_trailer_names_the_checkpoint_the_commit_archives_with_and_without_serves() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    read_gate_fixture::enforce_for(&repo, &["claude-code"]);
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);

    std::fs::write(repo.join("w1.txt"), "one\n").unwrap();
    git_in(&repo, &["add", "w1.txt"]);
    let bare = hook_pair_commit(&repo, tmp.path(), "bare", &env);
    let names = hooks_fixture::shard_tree_names(&repo);
    assert!(
        names.contains(&format!("{}/{}/metadata.json", &bare[..2], &bare[2..])),
        "{names}"
    );
    assert!(
        !names.contains("serve_audit.jsonl"),
        "no serve, no artifact: {names}"
    );

    decline_a_repeat_read(&repo, LIVE_SESSION, "booked.txt", &env);
    assert_eq!(
        attributed(&recall_store(&repo, &cache), LIVE_SESSION).len(),
        2
    );
    std::fs::write(repo.join("w2.txt"), "two\n").unwrap();
    git_in(&repo, &["add", "w2.txt"]);
    let served = hook_pair_commit(&repo, tmp.path(), "served", &env);
    let frozen = frozen_serves(&repo, &served)
        .unwrap_or_else(|| panic!("no shard at the trailer's id {served}"));
    assert_eq!(frozen.lines().count(), 2, "{frozen}");
    assert_no_serve_journal(&repo);
}

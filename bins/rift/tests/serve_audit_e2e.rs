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
//! (`bins/rift/src/recall.rs`). This test drives the REAL `rift` binary twice
//! and reads the REAL `recall.db` projection back — same coverage class as
//! the test it replaces: real process, real audit write, a leak check over
//! every persisted column.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::{Command, Stdio};

mod common;
use common::rift_cmd;

use rift_store::CanonicalStore;

/// Run `rift <args>` from `cwd`; return the exit code and captured stdout.
/// Mirrors `hooks_e2e.rs`'s `run_rift_stdout` — each `tests/*.rs` file is its
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

    // Two real process invocations of the surviving skill-CLI serve path,
    // each carrying a sentinel in exactly the argument shape the retired
    // test planted (a free-text task description and a file path) — the
    // half of the surface a hostile-argument leak would show up on.
    for _ in 0..2 {
        let (code, out) = run_rift_stdout(
            &repo,
            &[
                "recall",
                "known-state",
                "--task",
                "PRIVATE_ARGUMENT_SENTINEL",
                "--file",
                "PRIVATE_ARGUMENT_SENTINEL.rs",
            ],
            &env,
        );
        assert_eq!(
            code, 0,
            "known-state must succeed on an enabled repo: {out}"
        );
    }

    // No detached audit subprocess here (unlike the retired daemon, whose
    // audit handoff was a sibling process race the old test polled for): the
    // CLI process awaits `record_serve` in the same `block_on` before it
    // prints and exits, so the projection is complete the moment the child
    // has returned — no poll loop needed.
    let repo_id = rift_store::recall_repo_id(&repo).unwrap();
    let db = rift_store::recall_db_under(&cache, &repo_id);
    assert!(db.is_file(), "the process must have created recall.db");
    let store = rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap();

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

    // First-observed-per-channel: two real serves over the `skill_cli`
    // channel still leave exactly one `serve_session_event` row.
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

    // The plain audit twin (schema 1, `memd_audit_event`) is the same real
    // call landing in the pre-existing audit relation `record_serve` also
    // writes from every serve, verb column checked for the same leak shape.
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

//! End to end, through the shipped binary: what an agent that asks for a list
//! of file names actually gets back.
//!
//! Nothing here is a stand-in. A throwaway repository with a live session is
//! driven the way a client drives a hook — one process, the payload on stdin,
//! the decision read off stdout — and the answer is checked against the call
//! the agent wrote. The walk exists because the unit tests can prove what the
//! gate decides and not that it is WIRED: that the answer leaves the binary in
//! the field the pinned channel names, that the repeat really does run the
//! search, and that the row an answer owes lands in the family's own
//! projection, priced by a worker this test did not stub.
// The answer this exercises is read from an index only the search feature
// builds, so the file has nothing to prove without it.
#![cfg(feature = "search")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod common;
use common::rift_cmd;

/// How many source files the fixture repository holds.
///
/// Enough that a listing of them is worth answering: the frame carries a
/// sentence of its own, so a handful of paths costs less said than framed and
/// the gate declines to answer one — which is a property this file also
/// proves, from the other side.
const FILES: usize = 60;

/// A hermetic repository with one commit and a live session, so the hooks
/// below land in a real family with a real journal.
fn seed_repo(tmp: &Path) -> PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    // Deliberately not shaped like an address: this file is part of the
    // corpus the redaction census screens.
    git(&["config", "user.email", "glob"]);
    git(&["config", "user.name", "glob"]);
    for n in 0..FILES {
        let dir = repo.join(format!("src/module{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("handler.rs"), format!("fn handler{n}() {{}}\n")).unwrap();
    }
    std::fs::write(repo.join("README.md"), "prose\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "seed"]);
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("glob-1.json"),
        r#"{"schema":1,"session_id":"glob-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,"updated_at_ms":1783000000000,"native_id_map":{}}"#,
    )
    .unwrap();
    repo
}

/// Drive one real agent hook the way the family sends it and keep its stdout.
fn hook(repo: &Path, verb: &str, payload: &str, cache: &Path) -> serde_json::Value {
    let mut child = rift_cmd(repo)
        .args(["hooks", "claude-code", verb])
        .env("RIFT_CACHE_DIR", cache)
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_CAPTURE_SYNC", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "a hook never fails the agent: {verb}");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        return serde_json::Value::Null;
    }
    serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("one object on stdout: {text} {e}"))
}

/// A session beginning, which is the moment the worker is asked for the walk
/// this member's index is built by — exactly as a client sends it.
fn session_start(repo: &Path, cache: &Path) {
    hook(
        repo,
        "SessionStart",
        &serde_json::json!({ "session_id": "glob-1", "source": "startup" }).to_string(),
        cache,
    );
}

/// One name search, spelled with the argument the family's own page names.
fn glob(pattern: &str) -> String {
    serde_json::json!({
        "session_id": "glob-1",
        "tool_name": "Glob",
        "tool_input": { "pattern": pattern },
    })
    .to_string()
}

/// The family's own recall projection, resolved the way the product resolves
/// it — through the family manifest, never through a path.
fn audit_rows(repo: &Path, cache: &Path) -> Vec<(String, i64, i64)> {
    use rift_store::CanonicalStore as _;
    let paths = rift_git::discover(repo).expect("the fixture is a repository");
    let cache_dir = cache.display().to_string();
    let lookup = |key: &str| (key == "RIFT_CACHE_DIR").then(|| cache_dir.clone());
    let rift_git::FamilyState::Minted(family) =
        rift_git::family::resolve_family_under(lookup, &paths, rift_git::MintPolicy::Never)
            .unwrap()
    else {
        panic!("the hooks above should have minted the family");
    };
    let store = rift_store::open_migrated(
        rift_store::recall_db_under(cache, &family.recall_id),
        &rift_store::recall_migrations(),
    )
    .unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        store
            .query(
                "SELECT verb, tokens_avoided, tokens_returned FROM memd_audit_event",
                &[],
            )
            .await
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row.get_text(0).unwrap_or_default(),
                    row.get_i64(1).unwrap_or_default(),
                    row.get_i64(2).unwrap_or_default(),
                )
            })
            .collect()
    })
}

/// **A name search is answered through the real binary, the repeat runs it,
/// and the row the answer owes is priced by the real worker.**
#[test]
fn a_glob_through_the_real_binary_is_answered_once_and_the_repeat_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    // The session's own first hook, which is where the worker is asked for
    // the walk this member's index is built by — and which creates the
    // run-state directories the index is keyed against, before it walks.
    session_start(&repo, &cache);
    assert!(
        repo.join(".rift/state/search/index.bin").is_file(),
        "the worker built the index this answer is read from"
    );

    the_first_search_is_answered_by_its_ranked_head(&repo, &cache);
    the_identical_repeat_runs_as_written(&repo, &cache);
    a_listing_the_answer_cannot_beat_is_not_answered(&repo, &cache);
    the_answer_owes_one_row_priced_by_the_worker(&repo, &cache);
}

/// The first search is denied with the ranked head of the listing as its
/// reason: names only, the total it is a head of, and the escape hatch.
fn the_first_search_is_answered_by_its_ranked_head(repo: &Path, cache: &Path) {
    let answered = hook(repo, "PreToolUse", &glob("**/*.rs"), cache);
    let reason = answered
        .pointer("/hookSpecificOutput/permissionDecisionReason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("the search is answered: {answered}"));
    assert_eq!(
        answered.pointer("/hookSpecificOutput/permissionDecision"),
        Some(&serde_json::Value::from("deny")),
        "an answered search does not also run: {answered}"
    );
    assert!(
        answered
            .pointer("/hookSpecificOutput/updatedInput")
            .is_none(),
        "a decision carries no amendment: {answered}"
    );
    assert!(
        reason.starts_with(&format!(
            "[rift] Search answered: 8 of {FILES} paths matching"
        )),
        "the ranked head, and the total it is a head of: {reason}"
    );
    assert_eq!(
        reason.lines().filter(|l| l.starts_with("- ")).count(),
        8,
        "one line per path: {reason}"
    );
    assert!(
        !reason.contains("fn handler"),
        "names, never contents: {reason}"
    );
    assert!(
        reason.contains("Repeat this exact call for the full list."),
        "the escape hatch the one-answer rule makes true: {reason}"
    );
}

/// The identical call, in the same window: it runs, exactly as written.
fn the_identical_repeat_runs_as_written(repo: &Path, cache: &Path) {
    let again = hook(repo, "PreToolUse", &glob("**/*.rs"), cache);
    assert!(
        again
            .pointer("/hookSpecificOutput/permissionDecision")
            .is_none(),
        "the repeat runs the search in full: {again}"
    );
}

/// A listing the answer could not beat is not answered at all.
fn a_listing_the_answer_cannot_beat_is_not_answered(repo: &Path, cache: &Path) {
    let small = hook(repo, "PreToolUse", &glob("*.md"), cache);
    assert!(
        small
            .pointer("/hookSpecificOutput/permissionDecision")
            .is_none(),
        "one path is cheaper listed than framed: {small}"
    );
}

/// The row, written by a worker this test did not stub: what the frame cost
/// and what the listing it stood in for would have.
fn the_answer_owes_one_row_priced_by_the_worker(repo: &Path, cache: &Path) {
    let rows = audit_rows(repo, cache);
    let answers: Vec<&(String, i64, i64)> = rows
        .iter()
        .filter(|(verb, _, _)| verb == "memd.search_answered")
        .collect();
    assert_eq!(answers.len(), 1, "one answer, one row: {rows:?}");
    let (_, avoided, returned) = answers[0];
    assert!(*returned > 0, "the frame was counted: {rows:?}");
    assert!(
        *avoided > *returned,
        "the listing it stood in for cost more than the answer: {rows:?}"
    );
    assert!(
        !repo
            .join(".rift/state/search/answers")
            .read_dir()
            .unwrap()
            .any(|e| e.is_ok()),
        "and the paths the worker priced are taken away with the pricing"
    );
}

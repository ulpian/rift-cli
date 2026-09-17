//! End to end, through the shipped binary: what an agent that searches file
//! contents without bounding the results actually gets back.
//!
//! Nothing here is a stand-in. A throwaway repository with a live session is
//! driven the way a client drives a hook — one process, the payload on stdin,
//! the decision read off stdout — and the answer is checked against the call
//! the agent wrote. The walk exists because the unit tests can prove what the
//! gate decides and not that it is WIRED: that the amendment leaves the binary
//! in the field the pinned channel allows, that the repeat really does run
//! unbounded, and that the row a verified bound owes lands in the family's own
//! projection, written by a worker this test did not stub.
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
    // corpus the redaction census screens, and one more address-shaped
    // literal in the tree is one more hit it has to classify by hand.
    git(&["config", "user.email", "search"]);
    git(&["config", "user.name", "search"]);
    std::fs::write(repo.join("a.rs"), "fn main() {}\n").unwrap();
    git(&["add", "a.rs"]);
    git(&["commit", "-q", "-m", "seed"]);
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("grep-1.json"),
        r#"{"schema":1,"session_id":"grep-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,"updated_at_ms":1783000000000,"native_id_map":{}}"#,
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

/// One content search, spelled with the argument names the family's own
/// published page names.
fn grep(input: serde_json::Value) -> String {
    serde_json::json!({
        "session_id": "grep-1",
        "tool_name": "Grep",
        "tool_input": input,
    })
    .to_string()
}

/// The family's own recall projection, resolved the way the product resolves
/// it — through the family manifest, never through a path.
fn audit_rows(repo: &Path, cache: &Path) -> Vec<(String, String)> {
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
            .query("SELECT verb, handle FROM memd_audit_event", &[])
            .await
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row.get_text(0).unwrap_or_default(),
                    row.get_text(1).unwrap_or_default(),
                )
            })
            .collect()
    })
}

/// **A content search with no result cap is given one, through the real
/// binary — and the repeat runs unbounded.**
#[test]
fn a_grep_without_a_result_cap_through_the_real_binary_is_given_one() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let input = serde_json::json!({
        "pattern": "fn main",
        "path": ".",
        "output_mode": "content",
    });
    let call = grep(input.clone());

    let answer = hook(&repo, "PreToolUse", &call, &cache);
    assert_eq!(
        answer.pointer("/hookSpecificOutput/permissionDecision"),
        Some(&serde_json::Value::from("allow")),
        "an amendment declines nothing: {answer}"
    );
    let amended = answer
        .pointer("/hookSpecificOutput/updatedInput")
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| panic!("the search is given a bound: {answer}"));
    assert_eq!(
        amended.get("head_limit"),
        Some(&serde_json::Value::from(40)),
        "the bound this repository ships: {answer}"
    );
    let mut expected = input.as_object().unwrap().clone();
    expected.insert("head_limit".to_owned(), serde_json::Value::from(40));
    assert_eq!(
        amended, &expected,
        "one field added and nothing else moved: {answer}"
    );

    // The escape hatch, through the same binary: the same search asked again
    // in this window runs with no bound at all.
    let again = hook(&repo, "PreToolUse", &call, &cache);
    assert!(
        again.pointer("/hookSpecificOutput/updatedInput").is_none(),
        "the repeat runs in full: {again}"
    );

    // A search that chose its own bound is never touched.
    let chosen = grep(serde_json::json!({ "pattern": "fn main", "head_limit": 0 }));
    let untouched = hook(&repo, "PreToolUse", &chosen, &cache);
    assert!(
        untouched
            .pointer("/hookSpecificOutput/updatedInput")
            .is_none(),
        "an explicit bound is the caller's own choice: {untouched}"
    );
}

/// **The row a verified bound owes lands in the family's projection.**
///
/// The executed call carries the bound Rift added, so the cap can be said to
/// have happened — and the row it books claims nothing: the search ran, and
/// Rift put nothing into its output and withheld nothing from it.
#[test]
fn a_carried_bound_books_one_row_through_the_real_worker() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let input = serde_json::json!({ "pattern": "fn main", "output_mode": "content" });
    let capped = hook(&repo, "PreToolUse", &grep(input), &cache);
    assert!(capped.pointer("/hookSpecificOutput/updatedInput").is_some());

    // The result the client returns carries the input the tool really ran
    // with — the bound included — and the counts the tool reports about what
    // it listed.
    let executed = serde_json::json!({
        "session_id": "grep-1",
        "tool_name": "Grep",
        "tool_input": {
            "pattern": "fn main",
            "output_mode": "content",
            "head_limit": 40,
        },
        "tool_response": { "mode": "content", "numFiles": 1, "numLines": 1, "appliedLimit": 40 },
    })
    .to_string();
    hook(&repo, "PostToolUse", &executed, &cache);

    let rows = audit_rows(&repo, &cache);
    let capped: Vec<&(String, String)> = rows
        .iter()
        .filter(|(verb, _)| verb == "memd.grep_capped")
        .collect();
    assert_eq!(rows.len(), 1, "one decision, one row: {rows:?}");
    assert_eq!(capped.len(), 1, "the cap is booked once: {rows:?}");
    assert!(
        capped[0].1.starts_with("grep:matches="),
        "the identity is Rift's own, never the agent's pattern: {rows:?}"
    );
    assert!(
        !rows.iter().any(|(_, handle)| handle.contains("fn main")),
        "no row carries the agent's own words: {rows:?}"
    );
}

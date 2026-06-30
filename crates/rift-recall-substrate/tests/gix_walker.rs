//! End-to-end tests for the gix-backed substrate reader against a real on-disk
//! repository built with the `git` CLI (dev-only fixtures).
//!
//! Covers: shard enumeration, artifact reads, the read-only invariant, the
//! traversal guard, and the headline metric (`tokens_avoided > 0`).
#![cfg(feature = "gix")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use rift_recall_ledger::RecallMetrics;
use rift_recall_substrate::{ingest, worktree, Dialect, GixSubstrate, ShardWalker};

/// Run `git` in `dir` with a fixed identity / no signing (deterministic).
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "rift-test")
        .env("GIT_AUTHOR_EMAIL", "test@rift.local")
        .env("GIT_COMMITTER_NAME", "rift-test")
        .env("GIT_COMMITTER_EMAIL", "test@rift.local")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .expect("spawn git");
    assert!(
        status.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&status.stderr)
    );
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

const A_RS: &str = "fn main() {}\n";

/// Build a repo with a real `main` branch carrying the worktree file `a.rs`, plus
/// an orphan `rift/checkpoints/v1` branch holding two sharded checkpoints. The
/// committed read facts use the *real* SHA-256 of `a.rs` (so that read is
/// "unchanged vs worktree") and a distinct hash for `b.rs`.
fn build_fixture(dir: &Path) {
    let hash_a = worktree::sha256_hex(A_RS.as_bytes());
    let hash_b = worktree::sha256_hex(b"other contents\n");

    // A real working branch with a.rs committed.
    git(dir, &["init", "-q", "-b", "main"]);
    write(&dir.join("a.rs"), A_RS);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "worktree"]);

    // The orphan checkpoint branch: clear the worktree, lay down sharded artifacts.
    git(dir, &["checkout", "-q", "--orphan", "rift/checkpoints/v1"]);
    git(dir, &["rm", "-rfq", "."]);

    // Checkpoint "ab12": a Read of a.rs@hash_a (matches worktree) and a repeated
    // Read of b.rs@hash_b. token_estimates supplies the input tokens.
    write(
        &dir.join("ab/12/tool_calls.jsonl"),
        &format!(
            concat!(
                r#"{{"id":"t1","name":"Read","file_path":"a.rs","sha256":"{a}"}}"#,
                "\n",
                r#"{{"id":"t2","name":"Read","file_path":"b.rs","sha256":"{b}"}}"#,
                "\n",
                r#"{{"id":"t3","name":"Read","file_path":"b.rs","sha256":"{b}"}}"#,
                "\n",
            ),
            a = hash_a,
            b = hash_b,
        ),
    );
    write(
        &dir.join("ab/12/token_estimates.json"),
        concat!(
            r#"{"id":"t1","input":100}"#,
            "\n",
            r#"{"id":"t2","input":100}"#,
            "\n",
            r#"{"id":"t3","input":100}"#,
            "\n",
        ),
    );
    write(
        &dir.join("ab/12/metadata.json"),
        r#"{"strategy":"manual-commit"}"#,
    );
    // Checkpoint "cd34": minimal, just to prove multi-shard enumeration.
    write(&dir.join("cd/34/tool_calls.jsonl"), "");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "checkpoints"]);

    // Back to the working branch for the metric comparison.
    git(dir, &["checkout", "-q", "main"]);
}

#[test]
fn walks_shards_and_reads_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path());

    let walker = ShardWalker::open(dir.path()).unwrap();
    assert_eq!(walker.present_dialects(), vec![Dialect::Rift]);

    let ids: Vec<String> = walker.shard_ids(Dialect::Rift).unwrap();
    assert_eq!(ids, vec!["ab12".to_string(), "cd34".to_string()]);

    // Artifact read by tree-path lookup inside the checkpoint subtree.
    let meta = walker
        .read_artifact_in(Dialect::Rift, "ab12", "metadata.json")
        .unwrap()
        .expect("metadata present");
    assert!(String::from_utf8_lossy(&meta).contains("manual-commit"));
    assert!(walker
        .read_artifact_in(Dialect::Rift, "ab12", "missing.json")
        .unwrap()
        .is_none());
}

#[test]
fn empty_repo_yields_no_shards() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    let walker = ShardWalker::open(dir.path()).unwrap();
    assert!(walker.shard_ids(Dialect::Rift).unwrap().is_empty());
    assert!(walker.present_dialects().is_empty());
}

#[test]
fn read_artifact_rejects_traversal() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path());
    let walker = ShardWalker::open(dir.path()).unwrap();
    // A traversal component just fails the tree lookup; no filesystem access.
    assert!(walker
        .read_artifact_in(Dialect::Rift, "ab12", "../../../etc/passwd")
        .unwrap()
        .is_none());
}

#[test]
fn walk_issues_no_writes() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path());

    let refs_before = list_refs(dir.path());
    let objects_before = count_objects(dir.path());

    let walker = ShardWalker::open(dir.path()).unwrap();
    let _ = walker.shards_all().unwrap();
    let _ = walker
        .read_artifact_in(Dialect::Rift, "ab12", "metadata.json")
        .unwrap();

    assert_eq!(
        refs_before,
        list_refs(dir.path()),
        "walk must not change refs"
    );
    assert_eq!(
        objects_before,
        count_objects(dir.path()),
        "walk must not write objects"
    );
}

#[test]
fn metrics_report_tokens_avoided_gt_zero() {
    let dir = tempfile::tempdir().unwrap();
    build_fixture(dir.path());

    let sub = GixSubstrate::open(dir.path()).unwrap();
    let (reads, issues) = ingest::read_facts_all(&sub).unwrap();
    assert!(
        issues.is_empty(),
        "fixture artifacts parse cleanly: {issues:?}"
    );
    // 3 reads: a.rs (once), b.rs (twice).
    assert_eq!(reads.len(), 3);

    let paths: BTreeSet<String> = ingest::read_paths(&reads);
    let wt = worktree::hash_paths(dir.path(), &paths);

    let m = RecallMetrics::compute(&reads, &wt);
    // a.rs (100, unchanged vs worktree) + b.rs second read (100, repeated) = 200.
    assert_eq!(m.total_input_tokens, 300);
    assert_eq!(m.tokens_avoided, 200, "a.rs unchanged + b.rs reread");
    assert!(
        m.tokens_avoided > 0,
        "definition-of-done: tokens_avoided > 0"
    );
}

fn list_refs(dir: &Path) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["show-ref"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn count_objects(dir: &Path) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["count-objects", "-v"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

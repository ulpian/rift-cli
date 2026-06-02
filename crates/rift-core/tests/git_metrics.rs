//! End-to-end test of the metrics reader against a real git repo and a real
//! `entire/checkpoints/v1` ref (exercises `ShellGitReader`, not a fake).
#![cfg(unix)]

use std::path::Path;
use std::process::Command;

use rift_core::git::ShellGitReader;
use rift_core::metrics::read_branch_metrics;
use rift_core::paths::CHECKPOINT_BRANCH;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

#[test]
fn reads_metrics_from_a_real_checkpoint_branch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@example.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["checkout", "-q", "-b", CHECKPOINT_BRANCH]);

    // A single checkpoint shard with one session.
    write(
        &root.join("a3/b2c4d5/metadata.json"),
        r#"{"id":"a3b2c4d5","created_at":"2026-01-01T00:00:00Z","files_touched":["src/a.rs","src/b.rs"]}"#,
    );
    write(
        &root.join("a3/b2c4d5/0/token_estimates.json"),
        r#"{"input_tokens":1200,"output_tokens":300,"cached_input_tokens":100}"#,
    );
    write(&root.join("a3/b2c4d5/0/tool_calls.jsonl"), "{}\n{}\n");

    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "checkpoint"]);

    let reader = ShellGitReader::new(root);
    let metrics = read_branch_metrics(&reader, CHECKPOINT_BRANCH).unwrap();

    assert_eq!(metrics.checkpoints.len(), 1);
    let c = &metrics.checkpoints[0];
    assert_eq!(c.id, "a3b2c4d5");
    assert_eq!(c.input_tokens, 1200);
    assert_eq!(c.output_tokens, 300);
    assert_eq!(c.cached_input_tokens, 100);
    assert_eq!(c.files_touched, 2);
    assert_eq!(c.tool_calls, 2);
    assert_eq!(metrics.total_input_tokens(), 1200);
}

#[test]
fn missing_branch_yields_empty_metrics() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);

    let reader = ShellGitReader::new(root);
    let metrics = read_branch_metrics(&reader, CHECKPOINT_BRANCH).unwrap();
    assert!(metrics.is_empty());
}

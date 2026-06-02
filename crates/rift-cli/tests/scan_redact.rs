//! Tests for the rift-native `scan` and `redact` commands against the
//! synthetic repo fixture.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::str::contains;

fn fixture_repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/repo")
}

#[test]
fn scan_detects_entire_setup() {
    Command::cargo_bin("rift")
        .unwrap()
        .arg("scan")
        .arg(fixture_repo())
        .assert()
        .success()
        .stdout(contains("ENABLED"))
        .stdout(contains("claude-code"));
}

#[test]
fn scan_json_reports_markers() {
    Command::cargo_bin("rift")
        .unwrap()
        .args(["scan", "--json"])
        .arg(fixture_repo())
        .assert()
        .success()
        .stdout(contains("\"settings_present\": true"));
}

#[test]
fn redact_dry_run_finds_secrets_without_writing() {
    let store = fixture_repo().join("checkpoints");
    Command::cargo_bin("rift")
        .unwrap()
        .args(["redact", "--dry-run", "--store"])
        .arg(&store)
        .assert()
        .success()
        .stdout(contains("finding(s)"));

    // The committed fixture must remain untouched by a dry run.
    let prompt = store.join("a3").join("b2c4d5").join("0").join("prompt.txt");
    let after = std::fs::read_to_string(&prompt).unwrap();
    assert!(after.contains("AKIAIOSFODNN7EXAMPLE"));
}

#[test]
fn redact_rewrites_a_working_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("checkpoints");
    copy_dir(&fixture_repo().join("checkpoints"), &store);

    Command::cargo_bin("rift")
        .unwrap()
        .args(["redact", "--store"])
        .arg(&store)
        .assert()
        .success();

    let prompt = store.join("a3").join("b2c4d5").join("0").join("prompt.txt");
    let after = std::fs::read_to_string(&prompt).unwrap();
    assert!(!after.contains("AKIAIOSFODNN7EXAMPLE"));
    assert!(after.contains("[REDACTED]"));

    // The JSONL transcript is still valid JSON per line, with the secret gone.
    let jsonl = store.join("a3").join("b2c4d5").join("0").join("full.jsonl");
    let contents = std::fs::read_to_string(&jsonl).unwrap();
    assert!(!contents.contains("AKIAIOSFODNN7EXAMPLE"));
    for line in contents.lines().filter(|l| !l.trim().is_empty()) {
        serde_json::from_str::<serde_json::Value>(line).expect("each line is valid JSON");
    }
}

/// Minimal recursive directory copy for the test.
fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

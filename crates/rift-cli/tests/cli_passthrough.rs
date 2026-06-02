//! Wrapper tests: drive `rift` against a fake `entire` binary.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

/// Copy the committed fake-entire script into a tempdir with the executable bit
/// set, so the test never depends on git preserving file modes.
fn fake_entire() -> (TempDir, PathBuf) {
    let src =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-entire/entire");
    let content = std::fs::read(&src).expect("read fake-entire fixture");

    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("entire");
    std::fs::write(&bin, content).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, bin)
}

#[test]
fn forwards_unknown_command_to_entire() {
    let (_dir, bin) = fake_entire();
    Command::cargo_bin("rift")
        .unwrap()
        .env("RIFT_ENTIRE_BIN", &bin)
        .args(["checkpoint", "list"])
        .assert()
        .success()
        .stdout(contains("fake-entire: checkpoint list"));
}

#[test]
fn propagates_nonzero_exit_code() {
    let (_dir, bin) = fake_entire();
    Command::cargo_bin("rift")
        .unwrap()
        .env("RIFT_ENTIRE_BIN", &bin)
        .arg("__fail")
        .assert()
        .code(3);
}

#[test]
fn status_is_augmented_with_scan_summary() {
    let (_dir, bin) = fake_entire();
    Command::cargo_bin("rift")
        .unwrap()
        .env("RIFT_ENTIRE_BIN", &bin)
        .arg("status")
        .assert()
        .success()
        // entire's own (faked) output is passed through ...
        .stdout(contains("fake"))
        // ... and rift appends its scan summary.
        .stdout(contains("rift scan summary"));
}

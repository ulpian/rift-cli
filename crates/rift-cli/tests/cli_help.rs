//! Smoke tests for the top-level CLI surface.

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn help_lists_rift_native_subcommands() {
    Command::cargo_bin("rift")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("scan"))
        .stdout(contains("redact"))
        .stdout(contains("tui"))
        .stdout(contains("status"));
}

#[test]
fn version_flag_succeeds() {
    Command::cargo_bin("rift")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains("rift"));
}

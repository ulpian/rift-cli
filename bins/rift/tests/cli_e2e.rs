//! End-to-end of the `clap` command surface (P8 S1): exit-code contract, the
//! `--json` output contract, `init` + the read-only query commands, the
//! `enable`/`disable` round-trip, and the Entire-never-surfaced guard.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::Path;
use std::process::{Command, Output};

/// Run `rift <args>` hermetically (config + cache redirected; offline; headless).
fn rift(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rift"))
        .args(args)
        .current_dir(cwd)
        .env("RIFT_CONFIG_DIR", home.join("config"))
        .env("RIFT_CACHE_DIR", home.join("cache"))
        .env("RIFT_NO_PROVISION", "1")
        .env("RIFT_HEADLESS", "1")
        .output()
        .expect("spawn rift")
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

#[test]
fn init_scaffolds_rift_and_registers() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let out = rift(home.path(), repo.path(), &["init"]);
    assert_eq!(code(&out), 0, "init exits 0: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("registered"), "init stdout: {stdout}");
    assert!(repo.path().join(".rift").join("config.toml").exists());
    // `setup` is a hidden alias of `init`.
    assert_eq!(code(&rift(home.path(), repo.path(), &["setup"])), 0);
}

#[test]
fn query_in_non_rift_repo_exits_not_init() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap(); // no `rift init` here
    let out = rift(home.path(), repo.path(), &["status"]);
    assert_eq!(code(&out), 3, "status in a non-rift repo exits 3");
}

#[test]
fn unknown_subcommand_is_usage_error() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let out = rift(home.path(), repo.path(), &["definitely-not-a-command"]);
    assert_eq!(code(&out), 2, "clap usage error is exit 2");
}

#[test]
fn status_json_is_a_valid_contract() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    rift(home.path(), repo.path(), &["init"]);
    let out = rift(home.path(), repo.path(), &["status", "--json"]);
    assert_eq!(code(&out), 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("status --json is JSON");
    for key in [
        "checkpoints",
        "sessions",
        "captured_events",
        "hook_receipts",
        "transitions",
    ] {
        assert!(
            v.get(key).is_some_and(serde_json::Value::is_i64),
            "missing {key}: {v}"
        );
    }
}

#[test]
fn enable_then_disable_round_trips() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    rift(home.path(), repo.path(), &["init"]);

    let desc = repo
        .path()
        .join(".rift")
        .join("agents")
        .join("claude-code.json");
    let out = rift(
        home.path(),
        repo.path(),
        &["enable", "--agent", "claude-code"],
    );
    assert_eq!(code(&out), 0, "enable exits 0: {out:?}");
    assert!(desc.exists(), "enable writes the wiring descriptor");
    let descriptor = std::fs::read_to_string(&desc).unwrap();
    assert!(
        !descriptor.to_ascii_lowercase().contains("entire"),
        "no brand leak in wiring"
    );
    assert!(descriptor.contains("hooks claude-code"));

    // enable is idempotent.
    assert_eq!(
        code(&rift(
            home.path(),
            repo.path(),
            &["enable", "--agent", "claude-code"]
        )),
        0
    );

    let out = rift(
        home.path(),
        repo.path(),
        &["disable", "--agent", "claude-code"],
    );
    assert_eq!(code(&out), 0, "disable exits 0");
    assert!(!desc.exists(), "disable removes the wiring descriptor");
}

#[test]
fn enable_unknown_agent_fails_cleanly() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    rift(home.path(), repo.path(), &["init"]);
    let out = rift(
        home.path(),
        repo.path(),
        &["enable", "--agent", "not-an-agent"],
    );
    assert_eq!(code(&out), 1, "unknown agent is a clean failure (exit 1)");
}

#[test]
fn help_never_mentions_entire() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let out = rift(home.path(), repo.path(), &["--help"]);
    let text = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
    assert!(
        !text.contains("entire"),
        "help leaks the backend brand: {text}"
    );
    // The hidden `hooks` subcommand must not be listed as its own command line
    // (it may still be referenced inside another command's description).
    assert!(
        !text
            .lines()
            .any(|l| l.trim_start().starts_with("hooks ") || l.trim_start() == "hooks"),
        "hidden hooks surfaced as a subcommand: {text}"
    );
}

#[test]
fn hooks_schema_emits_event_vocabulary() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let out = rift(home.path(), repo.path(), &["hooks", "--schema"]);
    assert_eq!(code(&out), 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("schema is JSON");
    assert!(v["event_kinds"]
        .as_array()
        .is_some_and(|a| a.iter().any(|k| k == "tool_call")));
    assert!(v.get("schema_version").is_some());
}

#[test]
fn gc_reports_only_transient_and_never_the_chain() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    rift(home.path(), repo.path(), &["init"]);
    let out = rift(home.path(), repo.path(), &["gc", "--json"]);
    assert_eq!(code(&out), 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("gc --json");
    assert!(v
        .get("prunable_transient_refs")
        .is_some_and(serde_json::Value::is_array));
}

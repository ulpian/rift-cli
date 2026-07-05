//! Headless end-to-end: `rift setup` registers the working directory into the
//! global registry, and a bare `rift` then lists that project instead of the
//! empty-registry wizard. Runs the real binary with `RIFT_CONFIG_DIR` pointed at
//! a tempdir (hermetic) and `RIFT_HEADLESS=1` (no interactive TUI).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::Path;
use std::process::{Command, Output};

fn rift(config_dir: &Path, cache_dir: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rift"))
        .args(args)
        .current_dir(cwd)
        .env("RIFT_CONFIG_DIR", config_dir)
        // The canopy UI-state store lives under the CACHE root — without this
        // the test reads (and pollutes!) the developer's real project list.
        .env("RIFT_CACHE_DIR", cache_dir)
        .env("RIFT_HEADLESS", "1")
        // Keep the headless frame fast: no live-bus drain needed here.
        .env("RIFT_HEADLESS_WAIT_MS", "0")
        // A fresh binary would trip the OS keychain ACL prompt.
        .env("RIFT_NO_KEYCHAIN", "1")
        // `rift setup` provisions the capture backend by default; keep the test
        // hermetic and offline.
        .env("RIFT_NO_PROVISION", "1")
        .output()
        .expect("spawn rift binary")
}

#[test]
fn setup_registers_cwd_and_dashboard_lists_it() {
    let cfg = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let repo_name = repo
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    // `rift setup` registers the repo directory.
    let out = rift(cfg.path(), cache.path(), repo.path(), &["setup"]);
    assert!(out.status.success(), "setup failed: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("registered"), "setup stdout: {stdout}");

    // The registry file was written and holds exactly one project entry.
    let registry_file = cfg.path().join("rift").join("projects.toml");
    assert!(registry_file.exists(), "registry file should be written");
    let text = std::fs::read_to_string(&registry_file).unwrap();
    assert_eq!(
        text.matches("[[projects]]").count(),
        1,
        "exactly one project: {text}"
    );
    assert!(text.contains(&repo_name), "registry names the repo: {text}");

    // Bare `rift` (headless) lists the project, not the first-run wizard.
    let dash = rift(cfg.path(), cache.path(), repo.path(), &[]);
    assert!(dash.status.success(), "dashboard run failed: {dash:?}");
    let frame = String::from_utf8_lossy(&dash.stdout);
    assert!(
        !frame.contains("No projects registered"),
        "should not show the wizard: {frame}"
    );
    assert!(
        frame.contains(&repo_name),
        "dashboard lists the repo: {frame}"
    );

    // Registering the same directory again is idempotent.
    let again = rift(cfg.path(), cache.path(), repo.path(), &["setup"]);
    assert!(again.status.success());
    let text2 = std::fs::read_to_string(&registry_file).unwrap();
    assert_eq!(
        text2.matches("[[projects]]").count(),
        1,
        "still one entry after a repeat setup: {text2}"
    );
}

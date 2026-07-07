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

/// M5-04: `rift projects list|prune` over a hermetic cache root. Victims are
/// exactly the path-missing rows; `--dry-run` names them and writes nothing;
/// a present-but-not-a-repo row is never pruned (append-only: remove events
/// supersede, history stays).
#[test]
fn projects_list_and_prune_sweep_only_missing_paths() {
    let cfg = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // Three registered rows: a live repo, a soon-dead tempdir, a plain dir.
    let live = tempfile::tempdir().unwrap();
    let dead = tempfile::tempdir().unwrap();
    let plain = tempfile::tempdir().unwrap(); // present, but never a git repo

    for dir in [live.path(), dead.path(), plain.path()] {
        let out = rift(cfg.path(), cache.path(), dir, &["setup"]);
        assert!(out.status.success(), "setup failed: {out:?}");
    }
    let dead_path = dead.path().to_path_buf();
    let dead_canon = dead_path.canonicalize().unwrap_or(dead_path.clone());
    drop(dead); // the registered path no longer exists as a directory

    let list = rift(cfg.path(), cache.path(), live.path(), &["projects", "list", "--json"]);
    assert!(list.status.success());
    let doc: serde_json::Value =
        serde_json::from_slice(&list.stdout).expect("projects list --json parses");
    let rows = doc["projects"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "all three rows listed: {doc}");
    let missing: Vec<&str> = rows
        .iter()
        .filter(|r| !r["present"].as_bool().unwrap())
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    assert_eq!(missing.len(), 1, "exactly the dead row is absent: {doc}");

    // Dry-run names exactly the dead row and writes nothing.
    let dry = rift(
        cfg.path(),
        cache.path(),
        live.path(),
        &["projects", "prune", "--dry-run", "--json"],
    );
    assert!(dry.status.success());
    let dry_doc: serde_json::Value = serde_json::from_slice(&dry.stdout).unwrap();
    let victims: Vec<&str> = dry_doc["victims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(victims.len(), 1, "one victim: {dry_doc}");
    assert!(
        victims[0] == dead_canon.to_string_lossy()
            || victims[0].ends_with(dead_canon.file_name().unwrap().to_str().unwrap()),
        "the victim is the dead row: {victims:?}"
    );
    let after_dry = rift(cfg.path(), cache.path(), live.path(), &["projects", "list", "--json"]);
    let after_dry_doc: serde_json::Value = serde_json::from_slice(&after_dry.stdout).unwrap();
    assert_eq!(
        after_dry_doc["projects"].as_array().unwrap().len(),
        3,
        "dry-run wrote nothing"
    );

    // The real prune removes exactly the victim; the plain dir stays.
    let prune = rift(cfg.path(), cache.path(), live.path(), &["projects", "prune"]);
    assert!(prune.status.success());
    let after = rift(cfg.path(), cache.path(), live.path(), &["projects", "list", "--json"]);
    let after_doc: serde_json::Value = serde_json::from_slice(&after.stdout).unwrap();
    let left = after_doc["projects"].as_array().unwrap();
    assert_eq!(left.len(), 2, "the dead row is gone: {after_doc}");
    assert!(
        left.iter().all(|r| r["present"].as_bool().unwrap()),
        "every remaining row is present: {after_doc}"
    );

    // Prune again: nothing left to sweep (idempotent surface).
    let again = rift(cfg.path(), cache.path(), live.path(), &["projects", "prune", "--json"]);
    let again_doc: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(again_doc["victims"].as_array().unwrap().len(), 0);
}

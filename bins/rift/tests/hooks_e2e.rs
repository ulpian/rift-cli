//! End-to-end (P3): drive the real `rift` binary through the git-hook surface —
//! install writes absolute-path shims, `pre-push` filters `rift/*` egress, the
//! commit-path hooks never block even on a panic, and a GUI-style sparse `PATH`
//! still runs the installed shim via its absolute path.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

/// The built binary under test (Cargo sets this for integration tests).
const RIFT: &str = env!("CARGO_BIN_EXE_rift");

fn git_dir(tmp: &Path) -> std::path::PathBuf {
    let gd = tmp.join(".git");
    std::fs::create_dir_all(gd.join("hooks")).unwrap();
    gd
}

/// Run `rift <args>` with optional stdin; return the exit code.
fn run_rift(args: &[&str], stdin: Option<&str>, extra_env: &[(&str, &str)]) -> i32 {
    let mut cmd = Command::new(RIFT);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    if let Some(s) = stdin {
        child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    }
    child.wait().unwrap().code().unwrap_or(-1)
}

#[test]
fn hook_install_writes_absolute_path_shims() {
    let tmp = tempfile::tempdir().unwrap();
    let gd = git_dir(tmp.path());
    let code = run_rift(
        &["hook", "install", "--git-dir", gd.to_str().unwrap()],
        None,
        &[],
    );
    assert_eq!(code, 0, "install exits 0");

    for name in [
        "post-commit",
        "pre-push",
        "prepare-commit-msg",
        "commit-msg",
    ] {
        let shim = gd.join("hooks").join(name);
        assert!(shim.exists(), "{name} shim written");
        let text = std::fs::read_to_string(&shim).unwrap();
        assert!(
            text.contains(RIFT),
            "{name} shim calls rift by absolute path"
        );
        assert!(text.contains(&format!("hooks git {name} ")));
        assert!(
            !text.to_ascii_lowercase().contains("entire"),
            "no brand leak"
        );
    }
}

#[test]
fn pre_push_rejects_rift_refs_only() {
    let normal = "refs/heads/main abc refs/heads/main def\n";
    assert_eq!(
        run_rift(&["hooks", "git", "pre-push"], Some(normal), &[]),
        0
    );

    let rift_ref = "refs/heads/rift/x abc refs/heads/rift/x def\n";
    assert_eq!(
        run_rift(&["hooks", "git", "pre-push"], Some(rift_ref), &[]),
        1,
        "a rift/* ref is rejected"
    );

    // a mixed push fails closed on the rift/* presence
    let mixed = format!("{normal}{rift_ref}");
    assert_eq!(
        run_rift(&["hooks", "git", "pre-push"], Some(&mixed), &[]),
        1
    );
}

#[test]
fn commit_path_hook_never_blocks_even_on_panic() {
    // The injected capture panic must be swallowed; git sees success.
    let code = run_rift(
        &["hooks", "git", "post-commit"],
        Some(""),
        &[("RIFT_HOOK_TEST_PANIC", "1")],
    );
    assert_eq!(code, 0, "post-commit exits 0 even when capture panics");
}

#[test]
fn agent_hook_never_fails_agent_on_bad_input() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    // Malformed stdin → swallowed, exit 0 (deferred). Cache redirected to a tempdir.
    let code = run_rift(
        &["hooks", "claude-code", "SessionStart"],
        Some("not json at all"),
        &[("RIFT_CACHE_DIR", cache.to_str().unwrap())],
    );
    assert_eq!(code, 0, "a bad payload never fails the agent");

    // A valid SessionStart is recorded (exit 0).
    let good = r#"{"schema":1,"session":"01JE2E","turn":0,"payload":{}}"#;
    let code2 = run_rift(
        &["hooks", "claude-code", "SessionStart"],
        Some(good),
        &[("RIFT_CACHE_DIR", cache.to_str().unwrap())],
    );
    assert_eq!(code2, 0);
}

#[cfg(unix)]
#[test]
fn gui_sparse_path_still_runs_installed_shim_via_absolute_path() {
    let tmp = tempfile::tempdir().unwrap();
    let gd = git_dir(tmp.path());
    assert_eq!(
        run_rift(
            &["hook", "install", "--git-dir", gd.to_str().unwrap()],
            None,
            &[]
        ),
        0
    );
    // Execute the installed shim directly with an EMPTY PATH (a GUI client's sparse
    // environment). The shim must still find `rift` via its absolute path.
    let shim = gd.join("hooks").join("post-commit");
    let status = Command::new(&shim)
        .env("PATH", "")
        .env("GIT_DIR", &gd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(
        status.code(),
        Some(0),
        "shim runs rift by absolute path under sparse PATH"
    );
}

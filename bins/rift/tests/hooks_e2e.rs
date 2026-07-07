//! End-to-end (P3): drive the real `rift` binary through the git-hook surface —
//! install writes absolute-path shims, `pre-push` filters `rift/*` egress, the
//! commit-path hooks never block even on a panic, and a GUI-style sparse `PATH`
//! still runs the installed shim via its absolute path.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

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
    run_rift_capture(args, stdin, extra_env).0
}

/// Run `rift <args>` with optional stdin; return the exit code and stderr text
/// (the hook surface's only user-visible channel — loudness assertions read it).
fn run_rift_capture(
    args: &[&str],
    stdin: Option<&str>,
    extra_env: &[(&str, &str)],
) -> (i32, String) {
    let mut cmd = Command::new(RIFT);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    if let Some(s) = stdin {
        child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Seed a hermetic git repo with one commit and a **live** native-hook session
/// cache file, so the post-commit leg sees an archivable session. Returns the
/// work-tree path. Fixed timestamps only — the test never reads the clock.
fn seed_live_session_repo(tmp: &Path) -> std::path::PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let st = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&["add", "a.txt"]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=T",
        "commit",
        "-q",
        "-m",
        "seed",
    ]);
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("e2e-live-1.json"),
        r#"{"schema":1,"session_id":"e2e-live-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,"updated_at_ms":1783000000000,"native_id_map":{}}"#,
    )
    .unwrap();
    repo
}

/// M5-05 condense regression: with the archive routed native
/// (`.rift/rewrite.toml`), a commit made while a native-hook session is live
/// archives a real checkpoint shard onto `refs/heads/rift/checkpoints/v1` —
/// no wrapper binary involved, hermetic cache root.
#[test]
fn native_route_post_commit_archives_a_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let rift_dir = repo.join(".rift");
    std::fs::create_dir_all(&rift_dir).unwrap();
    std::fs::write(
        rift_dir.join("rewrite.toml"),
        "[routes]\narchive = \"native\"\n",
    )
    .unwrap();
    let cache = tmp.path().join("cache");

    let (code, stderr) = run_rift_capture(
        &[
            "hooks",
            "git",
            "post-commit",
            "--git-dir",
            repo.join(".git").to_str().unwrap(),
        ],
        Some(""),
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
        ],
    );
    assert_eq!(code, 0, "hook never fails the git op; stderr: {stderr}");
    assert!(
        !stderr.contains("FAILED") && !stderr.contains("skipped"),
        "a successful archive is quiet: {stderr}"
    );

    let refs = Command::new("git")
        .args(["for-each-ref", "refs/heads/rift/checkpoints/v1"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&refs.stdout).into_owned();
    assert!(
        listing.contains("refs/heads/rift/checkpoints/v1"),
        "the checkpoint ref exists: {listing:?}"
    );
    // The shard carries the four structural artifacts of a native archive.
    let tree = Command::new("git")
        .args([
            "ls-tree",
            "-r",
            "--name-only",
            "refs/heads/rift/checkpoints/v1",
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    let names = String::from_utf8_lossy(&tree.stdout).into_owned();
    for artifact in [
        "content.redacted",
        "content_hash.txt",
        "metadata.json",
        "redaction_report.json",
    ] {
        assert!(names.contains(artifact), "{artifact} in shard: {names}");
    }
}

/// M5-05 loudness: the wrapper route with no provisioned backend names the
/// skip and the next command on stderr — a silently absent checkpoint is
/// structurally impossible.
#[test]
fn wrapper_route_unprovisioned_names_the_skip() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let empty_data_home = tmp.path().join("data-home"); // no substrate install under it

    let (code, stderr) = run_rift_capture(
        &[
            "hooks",
            "git",
            "post-commit",
            "--git-dir",
            repo.join(".git").to_str().unwrap(),
        ],
        Some(""),
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
            ("XDG_DATA_HOME", empty_data_home.to_str().unwrap()),
        ],
    );
    assert_eq!(code, 0, "the skip never fails the git op");
    assert!(
        stderr.contains("checkpoint skipped") && stderr.contains("rift setup"),
        "the skip is loud and names the next command: {stderr:?}"
    );
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

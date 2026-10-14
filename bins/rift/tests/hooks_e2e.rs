//! End-to-end: drive the real `rift` binary through the git-hook surface —
//! install writes absolute-path hook entries, `pre-push` filters `rift/*` egress, the
//! commit-path hooks never block even on a panic, and a GUI-style sparse `PATH`
//! still runs the installed entry via its absolute path.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;
use std::process::{Command, Stdio};

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    agent_hook, git_in, post_commit_args, run_rift, run_rift_in, seed_live_session_repo,
    DetachedHooks, RETIRED_BRAND,
};

fn git_dir(tmp: &Path) -> std::path::PathBuf {
    // A gitdir hardened discovery accepts (`HEAD` + `objects/`), not the bare
    // `<tmp>/.git` this helper used to leave behind: that shape is a phantom
    // and every store the product opens is now resolved through
    // the repository family a real gitdir names.
    let gd = rift_git::test_support::fake_repo(tmp).unwrap();
    std::fs::create_dir_all(gd.join("hooks")).unwrap();
    gd
}

/// The detach itself, on the DEFAULT path — no `RIFT_CAPTURE_SYNC`.
///
/// Every other end-to-end test here drains inline for determinism, which would
/// leave the shipped behaviour — post-commit queues, spawns a child and returns
/// before the archive exists — with no test at all. This one takes the real
/// path: the hook returns while the queue still holds its entry, and the shard
/// appears afterwards, written by a process this one only started.
///
/// It waits for the worker to exit rather than for a duration. How long the
/// child takes is a property of the machine; that it finishes, drains its
/// entry and leaves a shard is a property of the code.
#[test]
fn the_detached_worker_archives_after_the_hook_has_already_returned() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let queue = common::capture_queue_dir(&repo.join(".git"), &cache);

    let hooks = DetachedHooks::default();
    let (code, stderr) = hooks.run(
        &repo,
        &post_commit_args(&repo.join(".git")),
        "",
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
        ],
    );
    assert_eq!(code, 0, "the hook exits 0; stderr: {stderr}");
    assert!(queue.is_dir(), "the hook left a durable record of intent");

    let shard_exists = || {
        std::process::Command::new("git")
            .args(["rev-parse", "--verify", "refs/heads/rift/checkpoints/v1"])
            .current_dir(&repo)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    // Assert BOTH facts, not just the shard, and only once the worker is gone.
    //
    // The shard ref appearing does not mean the worker is finished: it still
    // records the checkpoint's chain position and runs the soak's verify before
    // it drains, so asserting the drain the instant the ref appears would be
    // asserting a duration after all — the one thing this test's own docstring
    // says it must not do.
    let entries_left = || {
        std::fs::read_dir(&queue)
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name())
                    .filter(|n| n.to_string_lossy().ends_with(".json"))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    hooks.await_workers(|| shard_exists() && entries_left().is_empty());
    assert!(
        shard_exists(),
        "the detached worker archived the checkpoint"
    );
    assert!(
        entries_left().is_empty(),
        "and drained its entry: {:?}",
        entries_left()
    );
}

/// The installed Unix entry is a direct symlink to Rift. Its detached child
/// must resolve that link before spawning `hooks worker`: otherwise argv0 is
/// still `post-commit`, the child re-enters the hook dispatcher, and the queue
/// can never drain.
#[cfg(unix)]
#[test]
fn a_direct_hook_link_spawns_the_worker_binary_not_another_hook() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let git_dir = repo.join(".git");
    let cache = tmp.path().join("cache");
    assert_eq!(
        run_rift(
            &repo,
            &["hook", "install", "--git-dir", git_dir.to_str().unwrap()],
            None,
            &[
                ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
                ("RIFT_NO_KEYCHAIN", "1"),
            ],
        ),
        0
    );
    let hook = git_dir.join("hooks/post-commit");
    assert!(std::fs::read_link(&hook).is_ok(), "the entry is a symlink");

    let mut cmd = hook_command(&hook);
    cmd.current_dir(&repo)
        .env("GIT_DIR", &git_dir)
        .env("RIFT_CACHE_DIR", &cache)
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_NO_PROVISION", "1")
        .env_remove("RIFT_CAPTURE_SYNC");
    let hooks = DetachedHooks::default();
    let (code, _) = hooks.output(cmd, "");
    assert_eq!(code, 0, "the direct hook never blocks git");

    let queue = common::capture_queue_dir(&git_dir, &cache);
    let entries_left = || {
        std::fs::read_dir(&queue)
            .map(|d| {
                d.flatten()
                    .filter(|entry| entry.path().extension().is_some_and(|x| x == "json"))
                    .count()
            })
            .unwrap_or_default()
    };
    let shard_exists = || {
        Command::new("git")
            .args(["rev-parse", "--verify", "refs/heads/rift/checkpoints/v1"])
            .current_dir(&repo)
            .output()
            .is_ok_and(|output| output.status.success())
    };
    hooks.await_workers(|| shard_exists() && entries_left() == 0);
    assert!(shard_exists(), "the direct-link worker archived the commit");
    assert_eq!(
        entries_left(),
        0,
        "the direct-link worker drained its queue"
    );
}

#[test]
fn hook_install_uses_absolute_binary_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let gd = git_dir(tmp.path());
    let cache = tmp.path().join("cache");
    let code = run_rift(
        tmp.path(),
        &["hook", "install", "--git-dir", gd.to_str().unwrap()],
        None,
        // Hermetic: install now warms the projection and mints the salt, so an
        // empty env would write into the developer's real cache root and their
        // real keychain. A test must not touch either.
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
        ],
    );
    assert_eq!(code, 0, "install exits 0");

    for name in [
        "post-commit",
        "pre-push",
        "prepare-commit-msg",
        "commit-msg",
    ] {
        let entry = gd.join("hooks").join(name);
        assert!(entry.exists(), "{name} hook entry installed");
        #[cfg(unix)]
        {
            assert_eq!(
                std::fs::read_link(&entry).unwrap(),
                Path::new(common::rift_bin()),
                "{name} links directly to the absolute Rift binary"
            );
            let marker = gd.join("hooks").join(format!("{name}.rift-managed-link"));
            let record = std::fs::read_to_string(marker).unwrap();
            assert!(record.contains("# Rift-managed hook"));
            assert!(!record.to_ascii_lowercase().contains(RETIRED_BRAND));
        }
        #[cfg(not(unix))]
        {
            let text = std::fs::read_to_string(&entry).unwrap();
            assert!(
                text.contains(common::rift_bin()),
                "{name} shim calls rift by absolute path"
            );
            assert!(text.contains(&format!("hooks git {name} ")));
            assert!(!text.to_ascii_lowercase().contains(RETIRED_BRAND));
        }
    }
}

#[test]
fn pre_push_rejects_rift_refs_only() {
    let tmp = tempfile::tempdir().unwrap();
    let normal = "refs/heads/main abc refs/heads/main def\n";
    assert_eq!(
        run_rift(tmp.path(), &["hooks", "git", "pre-push"], Some(normal), &[]),
        0
    );

    let rift_ref = "refs/heads/rift/x abc refs/heads/rift/x def\n";
    assert_eq!(
        run_rift(
            tmp.path(),
            &["hooks", "git", "pre-push"],
            Some(rift_ref),
            &[]
        ),
        1,
        "a rift/* ref is rejected"
    );

    // a mixed push fails closed on the rift/* presence
    let mixed = format!("{normal}{rift_ref}");
    assert_eq!(
        run_rift(tmp.path(), &["hooks", "git", "pre-push"], Some(&mixed), &[]),
        1
    );
}

#[test]
fn commit_path_hook_never_blocks_even_on_panic() {
    let tmp = tempfile::tempdir().unwrap();
    // The injected capture panic must be swallowed; git sees success.
    let code = run_rift(
        tmp.path(),
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
        tmp.path(),
        &["hooks", "claude-code", "SessionStart"],
        Some("not json at all"),
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
            ("RIFT_CAPTURE_SYNC", "1"),
        ],
    );
    assert_eq!(code, 0, "a bad payload never fails the agent");

    // A valid SessionStart is recorded (exit 0).
    let good = r#"{"schema":1,"session":"01JE2E","turn":0,"payload":{}}"#;
    let code2 = run_rift(
        tmp.path(),
        &["hooks", "claude-code", "SessionStart"],
        Some(good),
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
            ("RIFT_CAPTURE_SYNC", "1"),
        ],
    );
    assert_eq!(code2, 0);
}

/// **The journal follows the discovered repository's family.** An agent hook
/// resolves its repository from the process cwd, and the session journal
/// lives in that repository's run state under the cache root — so the cwd
/// still decides WHICH family records the session (the exact spawn shape that
/// once wrote fixture session `01JE2E` into the real repository's journal,
/// whence the real post-commit hook archived it as shard `51/4865…`), and
/// the cache redirect decides WHERE. Aimed here at a tempdir repo: the entry
/// lands in the family's run state under the redirected root, and nothing
/// session-named is left in the repository's git dir.
#[test]
fn the_session_journal_follows_the_discovered_repos_family_into_the_cache_redirect() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-q"]);
    let cache = tmp.path().join("cache");

    let body = r#"{"schema":1,"session":"01JRNLPIN","turn":0,"payload":{}}"#;
    let code = run_rift_in(
        &repo,
        &["hooks", "claude-code", "SessionStart"],
        body,
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
            ("RIFT_CAPTURE_SYNC", "1"),
        ],
    );
    assert_eq!(code, 0);

    assert!(
        common::sessions_dir(&repo.join(".git"), &cache)
            .join("01JRNLPIN.json")
            .exists(),
        "the journal entry follows the discovered repo's family"
    );
    fn names_under(dir: &Path, needle: &str, hits: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                names_under(&p, needle, hits);
            } else if p
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains(needle))
            {
                hits.push(p);
            }
        }
    }
    let mut git_dir_hits = Vec::new();
    names_under(&repo.join(".git"), "01JRNLPIN", &mut git_dir_hits);
    assert!(
        git_dir_hits.is_empty(),
        "the git dir keeps no run state of the session: {git_dir_hits:?}"
    );
}

#[cfg(unix)]
/// `hook install` creates what capture would otherwise create lazily on the
/// developer's first commit: the projection and the install salt.
///
/// The point is the *first commit*, not the install — so the test asserts the
/// artefacts exist under the isolated cache root after install and before any
/// hook has run, which is the only moment at which "warm" and "lazy" differ.
#[test]
fn install_warms_the_projection_and_the_salt_so_the_first_commit_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    let gd = git_dir(tmp.path());
    let cache = tmp.path().join("cache");
    let code = run_rift(
        tmp.path(),
        &["hook", "install", "--git-dir", gd.to_str().unwrap()],
        None,
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
        ],
    );
    assert_eq!(code, 0, "install exits 0");

    assert!(
        cache.join("install-salt").exists(),
        "the salt was minted at install, not at the first commit"
    );
    // The machine id's one derivation (an `ioreg` child on macOS) is paid
    // here too, so a session's first hook reads the salted digest instead.
    if cfg!(target_os = "macos") {
        assert!(
            cache.join("machine-id.json").exists(),
            "the machine id was derived and cached at install"
        );
    }
    // `<cache>/rift/<repo-id>/projection.db` — the repo id is a hash of the
    // canonical worktree path, so the test finds it by shape, not by spelling.
    let projections: Vec<_> = std::fs::read_dir(cache.join("rift"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().join("projection.db").exists())
        .collect();
    assert_eq!(
        projections.len(),
        1,
        "exactly one projection was created at install"
    );
}

#[test]
fn gui_sparse_path_still_runs_installed_hook_via_absolute_path() {
    let tmp = tempfile::tempdir().unwrap();
    let gd = git_dir(tmp.path());
    let cache = tmp.path().join("cache");
    assert_eq!(
        run_rift(
            tmp.path(),
            &["hook", "install", "--git-dir", gd.to_str().unwrap()],
            None,
            // Hermetic, for the same reason as above.
            &[
                ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
                ("RIFT_NO_KEYCHAIN", "1"),
            ]
        ),
        0
    );
    // Execute the installed entry directly with an EMPTY PATH (a GUI client's
    // sparse environment). It must still reach Rift via its absolute path.
    let hook = gd.join("hooks").join("post-commit");
    let status = hook_command(&hook)
        // `rift_cmd` cannot spawn a hook path — pin its cwd to the same hermetic
        // root by hand (the same leak class: an unpinned hook child discovers
        // whatever repository the runner sits in).
        .current_dir(tmp.path())
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
        "hook runs rift by absolute path under sparse PATH"
    );
}

/// The command that runs an installed hook shim the way git runs it.
///
/// On Unix the shim is executable and runs itself. On Windows there is no
/// execute bit and the installer writes the same `#!/bin/sh` shim (see
/// `rift-shadow-hooks::install`), which git runs through its bundled `sh`;
/// spawning the file directly is error 193. The shell is resolved here, in
/// the test process, before the child's `PATH` is scrubbed — so the property
/// under test (the shim reaches rift by absolute path with an empty `PATH`)
/// is the same on both platforms.
fn hook_command(hook: &Path) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new(bundled_sh());
        cmd.arg(hook.display().to_string().replace('\\', "/"));
        cmd
    }
    #[cfg(not(windows))]
    {
        Command::new(hook)
    }
}

/// Git for Windows' own `sh.exe`, found from `git --exec-path`
/// (`<root>/mingw64/libexec/git-core`): `<root>/usr/bin/sh.exe`, or the
/// `<root>/bin/sh.exe` launcher. The same lookup lives beside the document
/// helper's stand-in in `rift-recall-doc`; the two crates share no test edge.
#[cfg(windows)]
fn bundled_sh() -> std::path::PathBuf {
    let out = Command::new("git")
        .arg("--exec-path")
        .output()
        .expect("git is on PATH");
    let exec_path = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let root = exec_path
        .ancestors()
        .nth(3)
        .expect("exec-path is three levels under the install root")
        .to_path_buf();
    [root.join("usr").join("bin"), root.join("bin")]
        .into_iter()
        .map(|dir| dir.join("sh.exe"))
        .find(|sh| sh.is_file())
        .expect("git for Windows ships sh.exe")
}

/// The cached machine fingerprint, through the real binary with a counting shim
/// for the fingerprint read: the FIRST session's first hook spawns `ioreg`
/// once and caches the salted digest under the cache root; the next
/// session's first hook spawns nothing and binds to the same machine id.
/// macOS only — that is the platform whose fingerprint is a child process.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn the_machine_fingerprint_is_read_once_per_install_not_once_per_session() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let (shims, log) = install_counting_ioreg_shim(tmp.path());
    let path = format!(
        "{}:{}",
        shims.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
        ("PATH", path.as_str()),
        ("RIFT_E2E_SPAWN_LOG", log.to_str().unwrap()),
    ];
    let spawns = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .count()
    };

    session_start(&repo, "01JMACHINEIDCACHE0000000001", &env);
    assert_eq!(spawns(), 1, "the first session's first hook derived the id");
    let cache_file = cache.join("machine-id.json");
    assert!(cache_file.exists(), "and cached the salted digest");
    let body = std::fs::read_to_string(&cache_file).unwrap();
    assert!(
        !body.contains(&platform_uuid()),
        "the raw hardware id is never on disk"
    );

    session_start(&repo, "01JMACHINEIDCACHE0000000002", &env);
    assert_eq!(
        spawns(),
        1,
        "the second session's first hook spawned nothing"
    );

    let repo_id = rift_git::repo_id_for(&repo).unwrap();
    let store =
        rift_store::open_capture_projection(rift_store::projection_db_under(&cache, &repo_id))
            .unwrap();
    let rows = rift_store::CanonicalStore::query(
        &store,
        "SELECT machine_id FROM session_binding ORDER BY session_id",
        &[],
    )
    .await
    .unwrap();
    let ids: Vec<String> = rows.iter().map(|r| r.get_text(0).unwrap()).collect();
    assert_eq!(ids.len(), 2, "both sessions bound");
    assert_eq!(ids[0], ids[1], "to the same machine id, derived and cached");
}

/// An `ioreg` on the front of `PATH` that appends one line to the spawn log
/// and then runs the real one: `(the shim directory, the log)`.
#[cfg(target_os = "macos")]
fn install_counting_ioreg_shim(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    let shims = tmp.join("shims");
    std::fs::create_dir_all(&shims).unwrap();
    let log = tmp.join("spawns.log");
    let shim = shims.join("ioreg");
    std::fs::write(
        &shim,
        "#!/bin/sh\necho ioreg >> \"$RIFT_E2E_SPAWN_LOG\"\nexec /usr/sbin/ioreg \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    (shims, log)
}

/// This machine's raw platform uuid, read the way the binary reads it — the
/// one string that must never reach the cache file.
#[cfg(target_os = "macos")]
fn platform_uuid() -> String {
    let raw = Command::new("/usr/sbin/ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&raw.stdout)
        .lines()
        .find(|l| l.contains("IOPlatformUUID"))
        .and_then(|l| l.split('"').nth(3).map(str::to_owned))
        .expect("this machine has a platform uuid")
}

/// A session's first hook, in the raw shape Claude Code sends it.
#[cfg(target_os = "macos")]
fn session_start(repo: &Path, session: &str, env: &[(&str, &str)]) {
    let body = serde_json::json!({"session_id": session, "hook_event_name": "SessionStart", "source": "startup"});
    assert_eq!(
        run_rift_in(
            repo,
            &["hooks", "claude-code", "SessionStart"],
            &body.to_string(),
            env
        ),
        0
    );
}

/// Through the shipped binary: an agent hook fired with a cwd
/// **outside any repository** exits 0 and writes nothing at all.
///
/// The retired fallback answered `<cwd>/.git` when discovery found no
/// repository, and the capture path then created it — so a hook whose cwd
/// followed the agent's shell out of a checkout left a directory named `.git`
/// wherever it was standing, which the next discovery walking up through it
/// read as a repository of its own. Five of those existed in this tree at once.
/// Both shapes are driven here: a plain directory, and one already carrying the
/// empty `.git` an older build left behind.
#[test]
fn a_hook_outside_any_repository_writes_nothing() {
    /// Every file under `dir`, relative and sorted — the whole of what a run
    /// wrote, rather than a guess at which path to look for.
    fn files_under(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut queue = vec![dir.to_path_buf()];
        while let Some(d) = queue.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    queue.push(p);
                } else {
                    out.push(p.strip_prefix(dir).unwrap_or(&p).display().to_string());
                }
            }
        }
        out.sort();
        out
    }

    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("XDG_DATA_HOME", data.to_str().unwrap()),
        ("LOCALAPPDATA", data.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
    ];

    // A tempdir has no `.git` in any ancestor, which is precisely the
    // condition — a hook fired from a directory that belongs to no checkout.
    let plain = tmp.path().join("outside");
    std::fs::create_dir_all(&plain).unwrap();
    let haunted = tmp.path().join("haunted");
    rift_git::test_support::phantom_gitdir(&haunted).unwrap();

    for (n, dir) in [&plain, &haunted].into_iter().enumerate() {
        let session = format!("01JOUTSIDE00000000000000{n}");
        // SessionStart is what opens a session cache; PostToolUse is what
        // writes captured facts. Neither has anywhere to write.
        agent_hook(dir, "SessionStart", &session, 1, "{}", &env);
        agent_hook(
            dir,
            "PostToolUse",
            &session,
            1,
            r#"{"tool_name":"Read","tool_input":{"file_path":"/etc/hosts"}}"#,
            &env,
        );
    }

    assert!(
        !plain.join(".git").exists(),
        "the `<cwd>/.git` fallback is retired: a hook mints no repository"
    );
    assert_eq!(
        files_under(&plain),
        Vec::<String>::new(),
        "a hook outside a repository leaves the directory it ran in untouched"
    );
    assert_eq!(
        files_under(&haunted),
        Vec::<String>::new(),
        "and does not adopt a phantom `.git` someone else left behind"
    );
    assert_eq!(
        files_under(&cache),
        Vec::<String>::new(),
        "no projection is minted for a path that is not a repository"
    );
}

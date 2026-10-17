//! End-to-end: **the worktree-adoption hook.**
//!
//! `git worktree add` is the one moment git tells Rift a worktree was born, and
//! it tells it from *inside* the new tree: measured on git 2.50.1 (2026-08-30)
//! the hook runs with cwd = the new worktree and `GIT_DIR` **unset**, so the
//! direct link forwards a relative `--git-dir .git` — which in a linked member
//! names a `.git` *file*. Everything this file pins follows from that: the
//! handler discovers its member from the cwd, the note it may have to leave
//! goes in the discovered admin dir, and nothing it does can fail the checkout.
//!
//! **How the hook is driven here.** The enumerated tests invoke the shipped
//! binary through `rift hooks git post-checkout --git-dir … -- $1 $2 $3` — the
//! same argv `run_direct_git_hook` builds, and the same handler, without
//! depending on symlink dispatch. The one test that goes through the installed
//! link and a real `git worktree add` is Unix-only for that reason and is
//! deliberately not enumerated as load-bearing.
//!
//! The fixture creates its member **before** the family's hooks exist (the
//! hooks directory is moved aside for exactly one `git worktree add`), so each
//! test owns precisely one hook invocation and its arguments.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
use common::rift_cmd;

/// The all-zero oid git passes as `$1` for a checkout with no previous HEAD —
/// what `git worktree add` sends.
const NO_PREVIOUS_HEAD: &str = "0000000000000000000000000000000000000000";

/// The manifest whose presence is "this member is wired" on the fast path.
const PROVISIONED: &str = ".rift/state/native-integrations.json";
/// The member-local marker a deferred or refused adoption leaves behind.
const MARKER: &str = ".rift/state/adoption.json";

/// Hermetic `git` in `dir`: no system/global config, a fixed identity, every
/// re-targeting variable scrubbed, and `extra` layered on for the child
/// processes git spawns — which is how the hook gets its redirected roots.
fn git_in(dir: &Path, args: &[&str], extra: &[(&str, &str)]) -> String {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES");
    for (key, value) in extra {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repository family with one linked member, and the roots every child of
/// this fixture is redirected onto.
struct Family {
    _tmp: tempfile::TempDir,
    main: PathBuf,
    member: PathBuf,
    /// The member's administrative gitdir, `<common>/worktrees/member` — where
    /// `discover(cwd)` says the member's queue lives.
    admin: PathBuf,
    home: PathBuf,
}

impl Family {
    /// The hermetic environment every `rift` and every `git` child gets — the
    /// cache and config roots, no keychain, and a `HOME` this user's real
    /// `~/.codex` is never reached through.
    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            (
                "RIFT_CACHE_DIR",
                self.home.join("cache").display().to_string(),
            ),
            (
                "RIFT_CONFIG_DIR",
                self.home.join("config").display().to_string(),
            ),
            ("RIFT_HEADLESS", "1".to_owned()),
            ("RIFT_NO_KEYCHAIN", "1".to_owned()),
            ("HOME", self.home.display().to_string()),
            ("CODEX_HOME", self.home.join(".codex").display().to_string()),
        ]
    }

    /// This repository's HEAD, as git hands it to `$2`.
    fn head(&self) -> String {
        git_in(&self.main, &["rev-parse", "HEAD"], &[])
            .trim()
            .to_owned()
    }

    /// Run `rift <args>` from `cwd` with the fixture's roots; `(code, stdout)`.
    fn rift(&self, cwd: &Path, args: &[&str]) -> (i32, String) {
        let mut cmd = rift_cmd(cwd);
        cmd.args(args);
        for (key, value) in self.env() {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("rift runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    /// Drive `post-checkout` exactly as git drives the installed link:
    /// `$1` a previous HEAD, `$2` the new HEAD, `$3` the branch-checkout flag.
    /// Returns `(exit code, stderr)` — stderr is where the phase timer reports.
    fn post_checkout(
        &self,
        cwd: &Path,
        git_dir: &str,
        args: (&str, &str, &str),
        extra: &[(&str, &str)],
    ) -> (i32, String) {
        let mut cmd = rift_cmd(cwd);
        cmd.args([
            "hooks",
            "git",
            "post-checkout",
            "--git-dir",
            git_dir,
            "--",
            args.0,
            args.1,
            args.2,
        ]);
        for (key, value) in self.env() {
            cmd.env(key, value);
        }
        for (key, value) in extra {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("rift runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// The family's recorded binary of record, which every adoption must name.
    fn binary_of_record(&self) -> String {
        let bytes = std::fs::read(self.main.join(".git/rift-family/binary.json"))
            .expect("`rift init` records the family's binary");
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["rift_bin"]
            .as_str()
            .expect("a recorded path")
            .to_owned()
    }

    /// Every file under the member, with its bytes — the only honest way to say
    /// "the second run wrote nothing".
    fn member_snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![self.member.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    out.push((path, bytes));
                }
            }
        }
        out.sort();
        out
    }

    /// The notes the hook left in the member's own admin-dir capture queue.
    fn member_notes(&self) -> Vec<String> {
        notes_in(&self.admin, &self.home.join("cache"))
    }
}

/// Every `*.error` note in the capture queue of the member whose git dir is
/// `git_dir`.
fn notes_in(git_dir: &Path, cache: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(common::capture_queue_dir(git_dir, cache)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "error"))
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .collect();
    out.sort();
    out
}

/// A repository with one commit, and a linked member born **without** the hook.
///
/// `wired` decides whether Rift is set up at all: with it the family has a
/// projection, `claude-code` recorded as enabled and a recorded binary of
/// record; without it there is no `rift init` anywhere, which is how the
/// deferral arms get a projection that cannot be read.
///
/// The member is created while `<common>/hooks` is moved aside, so `git
/// worktree add` fires nothing and every test below owns exactly one hook
/// invocation with exactly the arguments it chose.
fn family(wired: bool) -> Family {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let main = root.join("main");
    let member = root.join("member");
    let home = root.join("home");
    std::fs::create_dir_all(&main).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let fixture = Family {
        admin: main.join(".git/worktrees/member"),
        _tmp: tmp,
        main,
        member,
        home,
    };

    git_in(&fixture.main, &["init", "-q", "-b", "main"], &[]);
    std::fs::write(fixture.main.join("a.txt"), "a\n").unwrap();
    git_in(&fixture.main, &["add", "a.txt"], &[]);
    git_in(&fixture.main, &["commit", "-q", "-m", "seed"], &[]);

    if wired {
        let main = fixture.main.clone();
        let (code, out) = fixture.rift(&main, &["init", "--no-recall"]);
        assert_eq!(code, 0, "rift init: {out}");
        let (code, out) = fixture.rift(&main, &["enable", "--agent", "claude-code"]);
        assert_eq!(code, 0, "rift enable: {out}");
    }

    // Born without the hook: the tests drive it themselves.
    let hooks = fixture.main.join(".git/hooks");
    let parked = fixture.main.join(".git/hooks-parked");
    let moved = std::fs::rename(&hooks, &parked).is_ok();
    git_in(
        &fixture.main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "member",
            fixture.member.to_str().unwrap(),
        ],
        &[],
    );
    if moved {
        std::fs::rename(&parked, &hooks).unwrap();
    }
    assert!(
        !fixture.member.join(PROVISIONED).exists(),
        "the fixture's member is born unwired"
    );
    fixture
}

/// ★ The hook `git worktree add` fires wires the member it was
/// fired inside, and firing it again writes nothing.
///
/// This is the whole point of `post-checkout`: a fresh `git worktree add`
/// leaves a checkout with no `.rift/agents/*.json` and no client fragment, so
/// an agent launched there runs with **zero** Rift hooks and nothing says so.
/// The wiring must name the family's **binary of record** — not the binary
/// running the hook — or a throwaway build would re-point every member it
/// touched.
///
/// Idempotence is not decoration here: git runs `post-checkout` on every branch
/// switch, so a hook that wrote on each one would rewrite the developer's
/// fragments under them forever.
#[test]
fn post_checkout_on_worktree_add_provisions_the_member_and_is_idempotent() {
    let f = family(true);
    let head = f.head();
    let of_record = f.binary_of_record();

    let (code, stderr) = f.post_checkout(&f.member, ".git", (NO_PREVIOUS_HEAD, &head, "1"), &[]);
    assert_eq!(code, 0, "a hook never blocks git: {stderr}");

    // Parsed, then compared as a path: JSON spells a Windows path with `\\`,
    // so a substring search over the raw text is platform-naive.
    let descriptor: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(f.member.join(".rift/agents/claude-code.json"))
            .expect("the enabled agent's descriptor is written into the member"),
    )
    .unwrap();
    assert_eq!(
        descriptor["rift_bin"].as_str().map(Path::new),
        Some(Path::new(&of_record)),
        "the member is wired with the family's binary of record ({of_record}), not the \
         binary that ran the hook: {descriptor}"
    );
    let fragment = std::fs::read_to_string(f.member.join(".claude/settings.local.json"))
        .expect("the client fragment is written into the member");
    assert!(fragment.contains("hooks claude-code"), "{fragment}");
    assert!(
        f.member.join(PROVISIONED).is_file(),
        "the ownership manifest is what the fast path reads next time"
    );
    assert!(
        !f.member.join(MARKER).exists(),
        "a clean adoption leaves no marker behind"
    );
    assert!(f.member_notes().is_empty(), "{:?}", f.member_notes());

    let after_first = f.member_snapshot();
    let (code, stderr) = f.post_checkout(&f.member, ".git", (&head, &head, "1"), &[]);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        after_first,
        f.member_snapshot(),
        "a second post-checkout in the same member wrote a byte"
    );
}

/// ★ What a branch switch in a wired member costs, asserted from
/// the shipped binary's own phase instrument rather than from a clock.
///
/// Three arms, and the first two exist so the third is not vacuous:
///
/// * `$3 = 0` (a file checkout) returns before anything touches the disk — not
///   even `discover` is recorded;
/// * the first branch checkout in an unwired member pays `intent` (the
///   projection read) and `adopt`, which is what the fast path must not;
/// * every branch checkout after it records `discover` and **neither** of them.
///
/// No duration is asserted here — a test may not wait on a clock: the cost
/// that remains (one rift process spawn plus discovery) is published as the
/// `member-switch` harness row under the 100 ms git-latency bar instead.
#[test]
fn git_switch_in_a_provisioned_member_takes_the_fast_path() {
    let f = family(true);
    let head = f.head();
    let timing = [("RIFT_HOOK_TIMING", "1")];

    // A file checkout: the exit that costs not even a `stat`.
    let (code, line) = f.post_checkout(&f.member, ".git", (&head, &head, "0"), &timing);
    assert_eq!(code, 0);
    assert!(
        line.contains("hook=post-checkout"),
        "the timer reports: {line}"
    );
    assert!(
        !line.contains(" discover_us="),
        "a file checkout created no worktree and must not even discover: {line}"
    );
    assert!(!f.member.join(PROVISIONED).exists(), "and wired nothing");

    // The first branch checkout in an unwired member: the full path.
    let (code, line) = f.post_checkout(&f.member, ".git", (NO_PREVIOUS_HEAD, &head, "1"), &timing);
    assert_eq!(code, 0);
    for phase in [" discover_us=", " intent_us=", " adopt_us="] {
        assert!(line.contains(phase), "the full path pays {phase}: {line}");
    }
    let wired = f.member_snapshot();

    // Every branch switch after it.
    let (code, line) = f.post_checkout(&f.member, ".git", (&head, &head, "1"), &timing);
    assert_eq!(code, 0);
    assert!(
        line.contains(" discover_us="),
        "the fast path discovers: {line}"
    );
    assert!(
        !line.contains(" intent_us="),
        "the fast path must not open the projection: {line}"
    );
    assert!(
        !line.contains(" adopt_us="),
        "the fast path must not adopt: {line}"
    );
    assert_eq!(wired, f.member_snapshot(), "and it wrote nothing");
}

/// ★ A member whose adoption failed does not pay for
/// the whole of it again on every branch switch.
///
/// Without the marker a member that cannot be adopted — no projection, an
/// unreadable one, a fragment Rift may not overwrite — would open the family's
/// store on every `git switch` forever to fail the same way. The marker is the
/// backoff, and it is a *fact on disk* rather than a process memory, so it
/// survives the hook exiting.
///
/// The last arm is what makes the claim falsifiable: clear the marker and the
/// hook pays `intent` again, so it was the marker that stopped it and not the
/// absence of anything to do.
#[test]
fn a_failed_adoption_is_not_retried_by_the_hook_until_the_marker_is_cleared() {
    let f = family(false);
    let head = f.head();
    let timing = [("RIFT_HOOK_TIMING", "1")];

    let (code, line) = f.post_checkout(&f.member, ".git", (NO_PREVIOUS_HEAD, &head, "1"), &timing);
    assert_eq!(code, 0, "a family with no projection still exits 0: {line}");
    assert!(
        line.contains(" intent_us="),
        "the first attempt reads: {line}"
    );

    let marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(f.member.join(MARKER)).unwrap()).unwrap();
    assert_eq!(marker["outcome"], "deferred", "{marker}");
    assert!(
        marker["reason"].as_str().unwrap().contains("projection"),
        "the marker says what stopped it: {marker}"
    );
    let notes = f.member_notes();
    assert!(
        notes
            .iter()
            .any(|note| note.contains("did not wire") && note.contains("rift worktree adopt")),
        "the failure is said out loud on the channel a hook has: {notes:?}"
    );

    let (code, line) = f.post_checkout(&f.member, ".git", (&head, &head, "1"), &timing);
    assert_eq!(code, 0);
    assert!(
        !line.contains(" intent_us="),
        "a member that already refused must not re-open the projection: {line}"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(f.member.join(MARKER)).unwrap())
            .unwrap(),
        marker,
        "and it does not even rewrite the marker"
    );

    // Falsification: `rift worktree adopt` and `rift doctor --fix` clear the
    // marker, and the hook must be reading it rather than giving up for some
    // other reason.
    std::fs::remove_file(f.member.join(MARKER)).unwrap();
    let (code, line) = f.post_checkout(&f.member, ".git", (&head, &head, "1"), &timing);
    assert_eq!(code, 0);
    assert!(
        line.contains(" intent_us="),
        "a cleared marker lets the hook try again: {line}"
    );
}

/// ★ A hook never blocks git, whatever adoption does.
///
/// Two arms, because "never" has two halves. A *refusal* — a member fragment
/// Rift does not own — is the ordinary failure: exit 0, the file byte-identical,
/// the reason recorded where the next reader finds it. A *panic* is the
/// extraordinary one, and `capture_guarded` swallows it into exit 0 as well;
/// git would abort the checkout on any other code.
#[test]
fn post_checkout_never_blocks_git_on_failure() {
    let f = family(true);
    let head = f.head();

    // Somebody else's Rift command in the member's Claude Code fragment: the
    // never-clobber rule refuses it, and the whole adoption reports rather than
    // forces.
    let fragment = f.member.join(".claude/settings.local.json");
    std::fs::create_dir_all(fragment.parent().unwrap()).unwrap();
    let theirs = br#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"\"/their/rift\" hooks claude-code SessionStart","timeout":90}]}]}}"#;
    std::fs::write(&fragment, theirs).unwrap();

    let (code, stderr) = f.post_checkout(&f.member, ".git", (NO_PREVIOUS_HEAD, &head, "1"), &[]);
    assert_eq!(code, 0, "a refused adoption still exits 0: {stderr}");
    assert_eq!(
        std::fs::read(&fragment).unwrap(),
        theirs,
        "a file Rift does not own comes back byte-identical"
    );
    let marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(f.member.join(MARKER)).unwrap()).unwrap();
    assert_eq!(marker["outcome"], "refused", "{marker}");
    assert!(
        f.member_notes()
            .iter()
            .any(|note| note.contains("claude-code")),
        "{:?}",
        f.member_notes()
    );

    // And the extraordinary half: a panic inside the capture body.
    let (code, _) = f.post_checkout(
        &f.member,
        ".git",
        (&head, &head, "1"),
        &[("RIFT_HOOK_TEST_PANIC", "1")],
    );
    assert_eq!(code, 0, "the never-block guard swallows a panic too");
}

/// ★ The forwarded `--git-dir` is not this hook's
/// answer, and the discovered one is.
///
/// On `git worktree add` git runs the hook with `GIT_DIR` unset, so
/// `run_direct_git_hook` forwards the relative default `.git` — which inside a
/// linked member is a `.git` **file**. A handler that trusted it would try to
/// create a queue directory *under a file*; a handler that trusted an absolute
/// `--git-dir` inherited from the parent checkout would adopt the wrong tree
/// and leave the note in the wrong family member.
///
/// Both spellings are driven here — the relative `.git` git really sends, and
/// the main worktree's own absolute gitdir, which is the worst realistic wrong
/// answer — and each must land in the member the cwd names.
#[test]
fn post_checkout_ignores_the_forwarded_git_dir_and_discovers_from_cwd() {
    let f = family(false);
    let head = f.head();
    let main_git_dir = f.main.join(".git");

    let (code, stderr) = f.post_checkout(
        &f.member,
        main_git_dir.to_str().unwrap(),
        (NO_PREVIOUS_HEAD, &head, "1"),
        &[],
    );
    assert_eq!(code, 0, "{stderr}");
    assert!(
        f.member.join(MARKER).is_file(),
        "the member the cwd names is the member that was adopted"
    );
    assert!(
        !f.main.join(MARKER).exists(),
        "the forwarded --git-dir named the MAIN work tree; it was not adopted"
    );
    assert!(
        !f.member_notes().is_empty(),
        "the note goes in the member's own admin dir: {}",
        f.admin.display()
    );
    assert!(
        notes_in(&main_git_dir, &f.home.join("cache")).is_empty(),
        "and never in the queue the forwarded --git-dir names"
    );

    // The spelling git really sends. `.git` in a member is a FILE, so a handler
    // that resolved the queue through it could not have created one at all.
    assert!(
        f.member.join(".git").is_file(),
        "a linked member's .git is a file, which is the whole hazard"
    );
    std::fs::remove_file(f.member.join(MARKER)).unwrap();
    let (code, stderr) = f.post_checkout(&f.member, ".git", (&head, &head, "1"), &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        f.member.join(MARKER).is_file(),
        "adopted from the cwd again"
    );
    assert!(
        f.member.join(".git").is_file(),
        "and the .git file was not turned into a directory"
    );
}

/// ★ The **third** fast-path arm: a checkout in the family's main
/// work tree is never adopted, whatever else is true of it.
///
/// The handler's exits are ordered `$3 != 1` → not a repository → **not a
/// linked member** → already provisioned → a marker from a previous failure.
/// The other four are pinned by the tests above; this one was not, and it is
/// the arm nearly every `post-checkout` in the world takes: `git switch` in an
/// ordinary clone, which has no linked worktree at all.
///
/// The fixture is deliberately the **unwired** family, so the two arms that
/// come *after* this one cannot be what stops the hook — the main tree has no
/// `native-integrations.json` and no marker, so with this arm deleted the hook
/// would read the projection and adopt the main work tree, which is `rift
/// init`'s job and no hook's. Both halves of that are asserted: the timer never
/// records `intent`, and no adoption marker is ever written into the main tree.
#[test]
fn post_checkout_in_the_main_work_tree_takes_the_fast_path() {
    let f = family(false);
    let head = f.head();
    let timing = [("RIFT_HOOK_TIMING", "1")];
    let main = f.main.clone();

    let (code, line) = f.post_checkout(&main, ".git", (NO_PREVIOUS_HEAD, &head, "1"), &timing);
    assert_eq!(code, 0, "{line}");
    assert!(
        line.contains(" discover_us="),
        "the arm is reached by discovering, so discovery is paid: {line}"
    );
    assert!(
        !line.contains(" intent_us="),
        "the main work tree must not open the family projection: {line}"
    );
    assert!(!line.contains(" adopt_us="), "and must not adopt: {line}");
    assert!(
        !main.join(MARKER).exists(),
        "adoption never runs here, so it never leaves its marker behind"
    );
    assert!(!main.join(PROVISIONED).exists(), "and nothing was wired");
    assert!(
        notes_in(&main.join(".git"), &f.home.join("cache")).is_empty(),
        "nor is the main tree's own queue given a failure to report"
    );
}

/// The installed link, routed by `argv[0]` and fired by git itself.
///
/// Unix-only and deliberately **not** enumerated as load-bearing: it depends on
/// the direct-symlink install (`#[cfg(unix)]` in `write_hooks`) and on git
/// executing that link, neither of which is the claim the enumerated tests
/// hold. What it proves is the one thing they cannot — that adding a name to
/// `HOOK_NAMES` is by itself enough for `git worktree add` to reach the
/// handler, with no other dispatch change.
#[cfg(unix)]
#[test]
fn git_worktree_add_runs_the_installed_adoption_link() {
    let f = family(true);
    let hook = f.main.join(".git/hooks/post-checkout");
    assert_eq!(
        std::fs::read_link(&hook).unwrap(),
        Path::new(common::rift_bin()),
        "this hook is a direct link to the binary, like every other one"
    );

    let second = f.main.parent().unwrap().join("second");
    let owned = f.env();
    let env: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
    git_in(
        &f.main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "second",
            second.to_str().unwrap(),
        ],
        &env,
    );

    assert!(
        second.join(PROVISIONED).is_file(),
        "git ran the adoption hook and it wired the member it was born in"
    );
    assert!(second.join(".rift/agents/claude-code.json").is_file());
    assert!(!second.join(MARKER).exists());
}

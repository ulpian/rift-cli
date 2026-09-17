//! End-to-end: **rift refs never reach a remote the
//! repository did not name for them.**
//!
//! This file is a containment guard, not a feature demo. The failure it exists
//! to make impossible was observed, not imagined: on the machine this was
//! written on, `origin` in the Rift repository resolves to an unrelated third
//! party's project, and rift history reached it three times by accident. A push
//! queue whose shipper defaulted to `origin`, or a `pre-push` drain that fired
//! for whichever remote the developer happened to push to, would turn that
//! accident into a designed behaviour — a capture ledger published into someone
//! else's repository as a side effect of an ordinary `git push`.
//!
//! So every test here builds **two** destinations and checks both: the one the
//! repository configured, and a bystander that must stay empty of `rift/*` refs
//! no matter what. Every remote is a filesystem path inside a temp dir; nothing
//! here contacts a network, and the binary is only ever spawned through
//! `common::rift_cmd`.
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

/// Hermetic `git`: no system/global config, fixed identity, every re-targeting
/// variable scrubbed.
fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
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
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `git push …` allowed to fail — the pushes under test must not be masked by
/// an assertion inside the helper.
fn git_try(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new("git")
        .args(args)
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
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .expect("git is on PATH");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Run `rift` from `cwd`; return `(code, stdout, stderr)`.
fn rift(cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let out = rift_cmd(cwd)
        .args(args)
        .env("RIFT_CACHE_DIR", cwd.join(".rift-cache"))
        .env("RIFT_CONFIG_DIR", cwd.join(".rift-conf"))
        .env("RIFT_HEADLESS", "1")
        .output()
        .expect("rift runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const CHAIN_REF: &str = "refs/rift/chain/v1";
const CHECKPOINTS_REF: &str = "refs/heads/rift/checkpoints/v1";

/// A repository with a commit, a valid policy file, both rift refs, and two
/// **path** remotes: `origin` (a bystander) and `rift-origin` (the intended
/// checkpoint destination). Nothing is configured as the checkpoint remote yet.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    git_dir: PathBuf,
    bystander: PathBuf,
    checkpoints: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    git_in(&repo, &["add", "a.txt"]);
    git_in(&repo, &["commit", "-q", "-m", "seed"]);

    // A real, loadable policy file — so a test that asserts policy survived an
    // edit has something to survive.
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift/config.toml"),
        "schema_version = 1\n\
         [project]\nname = \"fixture\"\n\
         [paths]\nsensitive_paths = [\"secrets/**\"]\n",
    )
    .unwrap();

    // The rift refs. Their contents do not matter to containment — that they
    // exist and are pushable does.
    let head = git_in(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    git_in(&repo, &["update-ref", CHAIN_REF, &head]);
    git_in(&repo, &["update-ref", CHECKPOINTS_REF, &head]);

    let bystander = root.join("bystander.git");
    let checkpoints = root.join("checkpoints.git");
    for bare in [&bystander, &checkpoints] {
        std::fs::create_dir_all(bare).unwrap();
        git_in(&root, &["init", "-q", "--bare", bare.to_str().unwrap()]);
    }
    git_in(
        &repo,
        &["remote", "add", "origin", bystander.to_str().unwrap()],
    );
    git_in(
        &repo,
        &[
            "remote",
            "add",
            "rift-origin",
            checkpoints.to_str().unwrap(),
        ],
    );

    let git_dir = repo.join(".git");
    Fixture {
        _tmp: tmp,
        repo,
        git_dir,
        bystander,
        checkpoints,
    }
}

/// Plant queue entries directly. The queue is a documented local artifact and
/// the checkpoint pipeline that fills it is exercised elsewhere; what this file
/// tests is where a *full* queue is allowed to go.
fn plant_queue(git_dir: &Path) {
    let dir = git_dir.join("rift-push-queue");
    std::fs::create_dir_all(&dir).unwrap();
    let lines = format!(
        "{{\"schema_version\":1,\"ref_name\":\"{CHAIN_REF}\",\"chain_seq\":1,\
          \"enqueued_at_ms\":1700000000000}}\n\
         {{\"schema_version\":1,\"ref_name\":\"{CHECKPOINTS_REF}\",\"chain_seq\":1,\
          \"enqueued_at_ms\":1700000000000}}\n"
    );
    std::fs::write(dir.join("queue.jsonl"), lines).unwrap();
}

/// Every ref a bare repository holds.
fn refs_of(bare: &Path) -> Vec<String> {
    git_in(bare, &["for-each-ref", "--format=%(refname)"])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The assertion this whole file exists for.
fn assert_no_rift_refs(bare: &Path, label: &str) {
    let refs = refs_of(bare);
    let leaked: Vec<&String> = refs
        .iter()
        .filter(|r| r.contains("rift/chain") || r.contains("rift/checkpoints"))
        .collect();
    assert!(
        leaked.is_empty(),
        "{label} holds rift refs it was never configured to receive: {leaked:?} (all: {refs:?})"
    );
}

fn configure_checkpoint_remote(f: &Fixture, name: &str) {
    let (code, out, err) = rift(&f.repo, &["configure", "--checkpoint-remote", name]);
    assert_eq!(code, 0, "configure failed: {out}{err}");
}

/// (a) **No implicit remote.** With nothing configured, `rift sync flush`
/// refuses with an actionable, typed error — and both candidate destinations
/// stay empty. The bystander is the one that matters: it is what `origin`
/// resolves to, and a fallback would have filled it.
#[test]
fn flush_without_a_configured_remote_refuses_and_ships_nothing() {
    let f = fixture();
    plant_queue(&f.git_dir);

    let (code, out, err) = rift(&f.repo, &["--json", "sync", "flush"]);
    assert_ne!(
        code, 0,
        "a flush with nowhere to ship must not report success"
    );
    let doc: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("flush must emit a --json document ({e}): {out}{err}"));
    assert_eq!(doc["error"], "no_checkpoint_remote");
    assert_eq!(
        doc["configure"], "rift configure --checkpoint-remote <name-or-url>",
        "the refusal must name what to configure"
    );
    assert_eq!(doc["shipped"].as_array().unwrap().len(), 0);

    assert!(
        refs_of(&f.bystander).is_empty(),
        "`origin` must have received nothing at all"
    );
    assert!(refs_of(&f.checkpoints).is_empty());

    // And the work is still there — refusing is not discarding.
    let (code, out, _) = rift(&f.repo, &["--json", "sync", "status"]);
    assert_eq!(code, 0);
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["configured"], false);
    assert_eq!(doc["queued_entries"], 2);
}

/// (b) **The drain fires only for the configured remote.** A push to `origin`
/// — the bystander — drains nothing, says nothing on stdout, and leaves the
/// queue exactly as it was.
#[test]
fn pushing_to_another_remote_drains_nothing_and_says_nothing() {
    let f = fixture();
    configure_checkpoint_remote(&f, "rift-origin");
    plant_queue(&f.git_dir);
    let (code, _, err) = rift(
        &f.repo,
        &["hook", "install", "--git-dir", f.git_dir.to_str().unwrap()],
    );
    assert_eq!(code, 0, "hook install failed: {err}");

    let (ok, stdout, stderr) = git_try(&f.repo, &["push", "-q", "origin", "main"]);
    assert!(ok, "the developer's own push must still succeed: {stderr}");
    assert!(
        !stdout.contains("rift:"),
        "a push to another remote must say nothing on stdout: {stdout}"
    );
    assert!(
        !stderr.contains("shipped"),
        "a push to another remote must not report shipping: {stderr}"
    );

    assert_no_rift_refs(&f.bystander, "the bystander remote (`origin`)");
    assert!(
        refs_of(&f.bystander).iter().any(|r| r == "refs/heads/main"),
        "the developer's branch did reach their remote"
    );
    assert_no_rift_refs(&f.checkpoints, "the checkpoint remote (not pushed to)");

    let (_, out, _) = rift(&f.repo, &["--json", "sync", "status"]);
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        doc["queued_entries"], 2,
        "a push elsewhere must not consume the queue"
    );
}

/// (b), the other direction: pushing **to** the configured checkpoint remote
/// drains the queue there, and only there.
#[test]
fn pushing_to_the_configured_remote_drains_only_there() {
    let f = fixture();
    configure_checkpoint_remote(&f, "rift-origin");
    plant_queue(&f.git_dir);
    let (code, _, err) = rift(
        &f.repo,
        &["hook", "install", "--git-dir", f.git_dir.to_str().unwrap()],
    );
    assert_eq!(code, 0, "hook install failed: {err}");

    let (ok, _, stderr) = git_try(&f.repo, &["push", "-q", "rift-origin", "main"]);
    assert!(ok, "the push must succeed: {stderr}");

    let landed = refs_of(&f.checkpoints);
    assert!(
        landed.iter().any(|r| r == CHAIN_REF),
        "the chain ref must reach the configured remote: {landed:?}"
    );
    assert!(
        landed.iter().any(|r| r == CHECKPOINTS_REF),
        "the checkpoints ref must reach the configured remote: {landed:?}"
    );
    assert_no_rift_refs(&f.bystander, "the bystander remote (`origin`)");

    let (_, out, _) = rift(&f.repo, &["--json", "sync", "status"]);
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["queued_entries"], 0, "the drain consumed the queue");
}

/// The on-demand path, and the idempotency the crash story rests on: flushing
/// twice ships the same refs a second time without error and without moving
/// anything, because a git push of an unchanged ref is a no-op.
#[test]
fn flush_ships_to_the_configured_remote_and_re_flush_converges() {
    let f = fixture();
    configure_checkpoint_remote(&f, "rift-origin");
    plant_queue(&f.git_dir);

    let (code, out, err) = rift(&f.repo, &["--json", "sync", "flush"]);
    assert_eq!(code, 0, "flush failed: {out}{err}");
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["checkpoint_remote"], "rift-origin");
    assert_eq!(doc["shipped"].as_array().unwrap().len(), 2);
    let after_first = refs_of(&f.checkpoints);
    assert!(after_first.iter().any(|r| r == CHAIN_REF));
    assert_no_rift_refs(&f.bystander, "the bystander remote (`origin`)");

    // Re-flush with a re-planted queue: same refs, same result, nothing moved.
    plant_queue(&f.git_dir);
    let (code, out, err) = rift(&f.repo, &["--json", "sync", "flush"]);
    assert_eq!(code, 0, "re-flush failed: {out}{err}");
    assert_eq!(
        refs_of(&f.checkpoints),
        after_first,
        "re-shipping an unchanged ref must converge, not diverge"
    );
}

/// `rift configure --checkpoint-remote` used to replace `.rift/config.toml`
/// with a two-line document, discarding the project's declared policy and
/// leaving a file the strict loader then refused. This pins the repair from the
/// outside: the declared policy is still there afterwards, and the file still
/// loads.
#[test]
fn configuring_the_remote_preserves_the_declared_policy() {
    let f = fixture();
    configure_checkpoint_remote(&f, "rift-origin");
    let text = std::fs::read_to_string(f.repo.join(".rift/config.toml")).unwrap();
    assert!(
        text.contains("secrets/**"),
        "the project's declared sensitive paths must survive: {text}"
    );
    assert!(
        text.contains("rift-origin"),
        "the remote was recorded: {text}"
    );
    // And the whole document still loads as policy (an unknown-key document
    // would make every policy read fall back to the built-in floor).
    let (code, out, _) = rift(&f.repo, &["--json", "sync", "status"]);
    assert_eq!(code, 0);
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["checkpoint_remote"], "rift-origin");
}

/// The `pre-push` egress screen still rejects a **developer's** push carrying
/// `rift/*` refs, to any remote. The shipper's own push bypasses it explicitly
/// (`--no-verify`); this is the arm that proves the bypass did not become
/// general.
#[test]
fn a_developer_push_of_rift_refs_is_still_refused() {
    let f = fixture();
    configure_checkpoint_remote(&f, "rift-origin");
    let (code, _, err) = rift(
        &f.repo,
        &["hook", "install", "--git-dir", f.git_dir.to_str().unwrap()],
    );
    assert_eq!(code, 0, "hook install failed: {err}");

    let (ok, _, stderr) = git_try(&f.repo, &["push", "-q", "rift-origin", CHAIN_REF]);
    assert!(
        !ok,
        "a hand-written push of a rift ref must be refused by the egress screen"
    );
    assert!(
        stderr.contains("refusing to push internal rift/* refs"),
        "the refusal must say why: {stderr}"
    );
    assert_no_rift_refs(&f.checkpoints, "the checkpoint remote (push refused)");
}

/// **The queue is the family's.** A `pre-push` fired from a
/// *linked worktree* ships the refs the main checkout enqueued.
///
/// This is the measured defect, end to end and through the shipped binary: the
/// queue used to live under `<git_dir>`, which a linked worktree does not
/// share, so a push from a member read its own empty admin dir and shipped
/// nothing while the main checkout's queue accrued unreachably (62 entries on
/// the machine this was written on). The member's admin dir is asserted to hold
/// no queue at all — because the fix is not "look in both places", it is "there
/// is one place, and it is the one the refs already live in".
#[test]
fn a_prepush_from_a_member_ships_refs_enqueued_by_another_member() {
    let f = fixture();
    configure_checkpoint_remote(&f, "rift-origin");
    // Enqueued by the MAIN work tree, whose gitdir is the common dir.
    plant_queue(&f.git_dir);
    let (code, _, err) = rift(
        &f.repo,
        &["hook", "install", "--git-dir", f.git_dir.to_str().unwrap()],
    );
    assert_eq!(code, 0, "hook install failed: {err}");

    // A second member of the same family. Its hooks are the family's (git
    // resolves `hooks/` through the common dir), so the shim fires there too.
    let member = f.repo.parent().unwrap().join("member");
    git_in(
        &f.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "member",
            member.to_str().unwrap(),
        ],
    );
    // The repository's policy file reaches the member the way it reaches every
    // checkout — as a file in the tree. (The fixture leaves it uncommitted, so
    // a fresh worktree would not have it; a real family's is tracked.)
    std::fs::create_dir_all(member.join(".rift")).unwrap();
    std::fs::copy(
        f.repo.join(".rift/config.toml"),
        member.join(".rift/config.toml"),
    )
    .unwrap();

    let admin = f.git_dir.join("worktrees").join("member");
    assert!(admin.is_dir(), "the member's admin dir exists");
    assert!(
        !admin.join("rift-push-queue").exists(),
        "a member holds no push queue of its own — the split this ends"
    );

    // The push that ships: from the member, to the configured remote.
    let (ok, _, stderr) = git_try(&member, &["push", "-q", "rift-origin", "main"]);
    assert!(ok, "the member's push must succeed: {stderr}");

    let landed = refs_of(&f.checkpoints);
    assert!(
        landed.iter().any(|r| r == CHAIN_REF),
        "a member's pre-push ships what another member enqueued: {landed:?}"
    );
    assert!(
        landed.iter().any(|r| r == CHECKPOINTS_REF),
        "both queued refs reached the configured remote: {landed:?}"
    );
    assert_no_rift_refs(&f.bystander, "the bystander remote (`origin`)");

    // And the queue is consumed for the whole family, read from either member.
    for (label, cwd) in [("main", &f.repo), ("the member", &member)] {
        let (_, out, _) = rift(cwd, &["--json", "sync", "status"]);
        let doc: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            doc["queued_entries"], 0,
            "the drain consumed the family's queue, seen from {label}: {out}"
        );
    }
}

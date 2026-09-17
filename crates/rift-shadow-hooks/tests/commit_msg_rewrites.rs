//! Which git rewrites actually reach `commit-msg`, measured against real git
//! rather than reasoned about.
//!
//! An earlier review recorded one hazard it could verify and one it could
//! not:
//!
//! * **Verified there, pinned here:** a message carrying two or more column-0
//!   `Rift-Checkpoint` trailers used to lose all but the last whenever it
//!   passed through `commit-msg`, because the normalizer was single-valued.
//! * **Not verified there:** whether git runs `commit-msg` during `git rebase
//!   -i` squash/fixup — if it did, every interactive squash would be a loss
//!   path. It does **not**, and that is now a test rather than an open
//!   question.
//!
//! These fixtures drive real `git`, hermetically: user/system config off, every
//! inherited `GIT_*` re-targeting variable scrubbed, fixed author/committer
//! identity and dates so nothing depends on the wall clock.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
// Every test here drives a shell-shaped `commit-msg` hook through a real git,
// which is a Unix shape; off Unix the file has no tests and its helpers would
// only be dead code, so the whole crate is gated rather than each helper.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use rift_shadow_hooks::trailers;

/// A hermetic `git` invocation inside `dir`.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_AUTHOR_DATE", "2025-01-01T00:00:00+00:00")
        .env("GIT_COMMITTER_DATE", "2025-01-01T00:00:00+00:00")
        // Non-interactive rewrites: `--autosquash` rewrites the todo list
        // *before* the sequence editor runs, so a no-op editor performs a real
        // squash without any platform-specific scripting.
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_EDITOR", "true")
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

/// A repository whose `commit-msg` hook appends each message's subject to
/// `<repo>/../commit-msg.log`, so "did the hook run?" is an observation.
fn repo_recording_commit_msg(tmp: &Path) -> (PathBuf, PathBuf) {
    let repo = tmp.join("repo");
    let log = tmp.join("commit-msg.log");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "."]);
    let hooks = repo.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("commit-msg");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\nhead -1 \"$1\" >> \"{}\"\n",
            log.to_str().unwrap()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (repo, log)
}

fn log_lines(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The question the earlier review could not settle: `git rebase -i`
/// squash/fixup does
/// **not** run `commit-msg`, so an interactive squash never passes a fused
/// message through the normalizer at all. (Ordinary commits do run it — the
/// same fixture proves the hook is wired, so the empty rebase log is evidence
/// and not a silently broken hook.)
#[cfg(unix)]
#[test]
fn interactive_rebase_squash_does_not_run_commit_msg() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, log) = repo_recording_commit_msg(tmp.path());

    std::fs::write(repo.join("f"), "a\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    std::fs::write(repo.join("f"), "a\nb\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "target"]);
    let target = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    std::fs::write(repo.join("f"), "a\nb\nc\n").unwrap();
    git(&repo, &["add", "f"]);
    git(
        &repo,
        &["commit", "-q", &format!("--squash={target}"), "-m", ""],
    );

    // The hook is wired: three ordinary commits, three invocations.
    assert_eq!(log_lines(&log).len(), 3, "commit-msg runs for a commit");

    std::fs::write(&log, "").unwrap();
    git(&repo, &["rebase", "-i", "--autosquash", &base]);
    assert_eq!(
        git(&repo, &["rev-list", "--count", "HEAD"]).trim(),
        "2",
        "the squash really happened"
    );
    assert!(
        log_lines(&log).is_empty(),
        "git rebase -i does not run commit-msg: {:?}",
        log_lines(&log)
    );
}

/// The loss path that *is* real, end to end: a forge-shaped squash message
/// carries several column-0 claims, and `git commit --amend` runs `commit-msg`
/// over it. Normalizing that message must return every claim.
#[cfg(unix)]
#[test]
fn amending_a_multi_claim_message_keeps_every_claim() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, log) = repo_recording_commit_msg(tmp.path());
    std::fs::write(repo.join("f"), "a\n").unwrap();
    git(&repo, &["add", "f"]);
    let forge_message = "Squash and merge (#12)\n\n\
                         * first\n* second\n\n\
                         Rift-Checkpoint: aaa1\n\
                         Rift-Checkpoint: bbb2\n";
    git(&repo, &["commit", "-q", "-m", forge_message]);
    std::fs::write(&log, "").unwrap();
    git(&repo, &["commit", "-q", "--amend", "--no-edit"]);
    assert_eq!(
        log_lines(&log).len(),
        1,
        "commit-msg runs on --amend — this is the live loss path"
    );

    // What the hook hands the normalizer is the stored message; both claims
    // must come back out.
    let stored = git(&repo, &["log", "-1", "--format=%B"]);
    let normalized = trailers::normalize(&stored);
    assert!(normalized.contains("Rift-Checkpoint: aaa1"), "{normalized}");
    assert!(normalized.contains("Rift-Checkpoint: bbb2"), "{normalized}");
    assert_eq!(
        trailers::normalize(&normalized),
        normalized,
        "idempotent over a real git message"
    );
}

/// A locally squashed commit (`git merge --squash` + `git commit`) fuses
/// `SQUASH_MSG`, whose constituent trailers are **indented** — quoted history,
/// not claims by the fused commit. The anchored match must leave them exactly
/// where they are, and add nothing.
#[cfg(unix)]
#[test]
fn a_local_squash_leaves_its_indented_history_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, _log) = repo_recording_commit_msg(tmp.path());
    std::fs::write(repo.join("f"), "a\n").unwrap();
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(repo.join("g"), "g\n").unwrap();
    git(&repo, &["add", "g"]);
    git(
        &repo,
        &[
            "commit",
            "-q",
            "-m",
            "feature work\n\nRift-Checkpoint: ccc3\n",
        ],
    );
    git(&repo, &["checkout", "-q", "-"]);
    git(&repo, &["merge", "--squash", "feature"]);

    let squash_msg = std::fs::read_to_string(repo.join(".git").join("SQUASH_MSG")).unwrap();
    assert!(
        squash_msg.contains("    Rift-Checkpoint: ccc3"),
        "git indents SQUASH_MSG: {squash_msg:?}"
    );
    let normalized = trailers::normalize(&squash_msg);
    assert!(
        normalized.contains("    Rift-Checkpoint: ccc3"),
        "quoted history survives verbatim: {normalized}"
    );
    assert!(
        !normalized.contains("\nRift-Checkpoint:"),
        "and is never promoted into this commit's trailer block: {normalized}"
    );
}

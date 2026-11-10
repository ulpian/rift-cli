//! The argv-safe shell-outs: validated oids and refspecs, `--`-terminated argv,
//! and one spawn site with an explicit `GIT_DIR`/`GIT_WORK_TREE` and no shell.

use std::process::Command;

use rift_core::{Result, RiftError};

use crate::{child_process_path, RepoPaths};

/// Rift's own state directory, relative to a work tree's root.
const RIFT_STATE_DIR: &str = ".rift/state";

/// Validate a git object id: non-empty lowercase hex of a sha1/sha256 length.
///
/// # Errors
/// [`RiftError::Validity`] if not a hex oid.
pub fn validate_oid(oid: &str) -> Result<()> {
    let ok = matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit());
    if ok {
        Ok(())
    } else {
        Err(RiftError::validity("invalid git oid"))
    }
}

/// Validate a refspec: no leading `-` (flag injection), no shell metacharacters,
/// no whitespace/NUL. Argument injection cannot enter here.
///
/// # Errors
/// [`RiftError::Validity`] if the refspec is unsafe.
pub fn validate_refspec(refspec: &str) -> Result<()> {
    let unsafe_char = |b: u8| {
        b == 0
            || b.is_ascii_whitespace()
            || matches!(
                b,
                b';' | b'|' | b'&' | b'$' | b'`' | b'(' | b')' | b'<' | b'>' | b'\\' | b'\'' | b'"'
            )
    };
    if refspec.is_empty() || refspec.starts_with('-') || refspec.bytes().any(unsafe_char) {
        return Err(RiftError::validity("unsafe refspec"));
    }
    Ok(())
}

/// The argv for `reset --hard <oid>`. The oid goes before the `--`: git reads
/// what follows the terminator as paths and refuses a hard reset with paths.
/// [`validate_oid`] (hex only) is what keeps the oid from reading as a flag.
#[must_use]
pub fn reset_hard_argv(oid: &str) -> Vec<String> {
    vec!["reset".into(), "--hard".into(), oid.into(), "--".into()]
}

/// The argv for `checkout <refspec>`. The refspec goes before the `--`, so git
/// takes it as a revision (a branch stays checked out as a branch) and never
/// as a path to restore; [`validate_refspec`] refuses a leading `-`, so it
/// cannot read as a flag either.
#[must_use]
pub fn checkout_argv(refspec: &str) -> Vec<String> {
    vec!["checkout".into(), refspec.into(), "--".into()]
}

/// `git reset --hard <oid>` against `repo`'s exact worktree (validated, no shell).
///
/// # Errors
/// [`RiftError::Validity`] for a bad oid; [`RiftError::Storage`] on a non-zero exit.
pub fn git_reset_hard(repo: &RepoPaths, oid: &str) -> Result<()> {
    validate_oid(oid)?;
    run_git(repo, &reset_hard_argv(oid))
}

/// `git checkout <refspec>` against `repo`'s exact worktree (validated, no shell).
///
/// # Errors
/// [`RiftError::Validity`] for a bad refspec; [`RiftError::Storage`] on a non-zero exit.
pub fn git_checkout(repo: &RepoPaths, refspec: &str) -> Result<()> {
    validate_refspec(refspec)?;
    run_git(repo, &checkout_argv(refspec))
}

/// Invoke the user's `git` with an **argv array (no shell)** and explicit
/// `GIT_DIR`/`GIT_WORK_TREE` (never inferred). Maps a non-zero exit to a
/// [`RiftError`] and **never** lets a git failure abort the developer's own op.
///
/// # Errors
/// [`RiftError::Storage`] if `git` cannot be spawned or exits non-zero.
pub fn run_git(repo: &RepoPaths, args: &[String]) -> Result<()> {
    run_git_capture(repo, args, &[]).map(|_| ())
}

/// [`run_git`], returning captured stdout bytes and feeding `stdin` to the
/// child. The argv-array, explicit-env discipline is identical; stdout is the
/// **only** channel read back (stderr is dropped, never surfaced).
///
/// # Errors
/// [`RiftError::Storage`] if `git` cannot be spawned or exits non-zero.
pub fn run_git_capture(repo: &RepoPaths, args: &[String], stdin: &[u8]) -> Result<Vec<u8>> {
    run_git_capture_env(repo, args, stdin, &[])
}

/// [`run_git_capture`] with additional explicit environment variables (e.g.
/// `GIT_INDEX_FILE`, `GIT_AUTHOR_DATE`) layered on top of the always-explicit
/// `GIT_DIR`/`GIT_WORK_TREE`. Still an argv array, no shell.
///
/// # Errors
/// [`RiftError::Storage`] if `git` cannot be spawned or exits non-zero.
pub fn run_git_capture_env(
    repo: &RepoPaths,
    args: &[String],
    stdin: &[u8],
    env: &[(&str, &str)],
) -> Result<Vec<u8>> {
    match run_git_status(repo, args, stdin, env)? {
        (true, out) => Ok(out),
        (false, _) => Err(RiftError::storage("git exited non-zero")),
    }
}

/// [`run_git_capture_env`], but a non-zero exit is **not** an error: returns
/// `(success, stdout)`. For probes like `rev-parse --verify` / `cat-file -e`
/// where "not found" is an ordinary answer, not a failure. A spawn failure is
/// still an [`Err`].
///
/// # Errors
/// [`RiftError::Storage`] if `git` cannot be spawned.
pub fn run_git_status(
    repo: &RepoPaths,
    args: &[String],
    stdin: &[u8],
    env: &[(&str, &str)],
) -> Result<(bool, Vec<u8>)> {
    use std::io::Write as _;
    use std::process::Stdio;

    let mut cmd = Command::new("git");
    cmd.args(args)
        .env("GIT_DIR", child_process_path(&repo.git_dir))
        .env("GIT_WORK_TREE", child_process_path(&repo.work_tree));
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(if stdin.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| RiftError::storage(format!("git invocation failed: {e}")))?;
    // stdin is fed from its own thread while this one drains stdout, as std's
    // `Command::output` does: a command that answers while it still reads
    // (`hash-object --stdin-paths`) fills the stdout pipe, and writing all of
    // stdin first would leave both processes waiting on each other.
    let pipe = child.stdin.take();
    std::thread::scope(|scope| {
        let feeder = scope.spawn(move || match pipe {
            Some(mut pipe) => pipe.write_all(stdin),
            None => Ok(()),
        });
        let output = child
            .wait_with_output()
            .map_err(|e| RiftError::storage(format!("git invocation failed: {e}")))?;
        let fed = feeder
            .join()
            .map_err(|_| RiftError::storage("git stdin writer stopped"))?;
        fed.map_err(|e| RiftError::storage(format!("git stdin failed: {e}")))?;
        Ok((output.status.success(), output.stdout))
    })
}

/// The worktree paths that differ from HEAD (modified/added/deleted, staged or
/// not, plus untracked files) via `git status --porcelain -z`. Paths are
/// worktree-relative; rename records yield the **new** path.
///
/// # Errors
/// [`RiftError::Storage`] if `git` cannot be spawned or exits non-zero.
pub fn changed_worktree_paths(repo: &RepoPaths) -> Result<Vec<String>> {
    let out = run_git_capture(repo, &status_argv(&[]), &[])?;
    Ok(porcelain_z_paths(&out))
}

/// [`changed_worktree_paths`] without anything under Rift's own state
/// directory (`.rift/state/`).
///
/// A turn boundary observes what a person or an agent changed; Rift's search
/// index and serve records are neither, and nothing makes a user's repository
/// ignore them, so the exclusion is a pathspec on the status call itself
/// rather than an ignore rule written into the user's repository. `top` roots
/// the pathspec at the work tree whatever directory the hook runs from, and
/// `literal` keeps the name from being read as a glob. `GIT_LITERAL_PATHSPECS`
/// is pinned off because, set in the environment, it turns the magic into a
/// literal name and the status would answer with nothing.
///
/// # Errors
/// [`RiftError::Storage`] if `git` cannot be spawned or exits non-zero.
pub fn changed_worktree_paths_outside_rift_state(repo: &RepoPaths) -> Result<Vec<String>> {
    let exclude = format!(":(top,exclude,literal){RIFT_STATE_DIR}");
    let out = run_git_capture_env(
        repo,
        &status_argv(&[exclude]),
        &[],
        &[("GIT_LITERAL_PATHSPECS", "0")],
    )?;
    Ok(porcelain_z_paths(&out))
}

/// `git status --porcelain=v1 -z --untracked-files=all`, then `pathspecs`
/// after a `--` terminator. Porcelain v1 names paths from the work tree's
/// root whatever the process's directory, and `-z` never quotes them.
fn status_argv(pathspecs: &[String]) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        "status".into(),
        "--porcelain=v1".into(),
        "-z".into(),
        "--untracked-files=all".into(),
    ];
    if !pathspecs.is_empty() {
        argv.push("--".into());
        argv.extend_from_slice(pathspecs);
    }
    argv
}

/// The paths of a `status --porcelain=v1 -z` answer, sorted and deduplicated.
fn porcelain_z_paths(out: &[u8]) -> Vec<String> {
    let mut paths = Vec::new();
    let mut fields = out.split(|b| *b == 0);
    while let Some(entry) = fields.next() {
        if entry.len() < 4 {
            continue; // status entries are `XY <path>`; anything shorter is noise
        }
        let status = entry.get(..2).unwrap_or_default();
        let path = String::from_utf8_lossy(entry.get(3..).unwrap_or_default()).into_owned();
        // A rename record (`R`/`C` in either column) is followed by the ORIGIN
        // path as its own NUL field — consume it; the new path is what changed.
        if status.contains(&b'R') || status.contains(&b'C') {
            let _ = fields.next();
        }
        if !path.is_empty() {
            paths.push(path);
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refspec_and_oid_injection_rejected() {
        assert!(
            validate_refspec("-X").is_err(),
            "leading dash is flag injection"
        );
        assert!(validate_refspec("a; rm -rf /").is_err(), "metacharacters");
        assert!(validate_refspec("a b").is_err(), "whitespace");
        assert!(validate_refspec("rift/abc-123").is_ok());
        assert!(validate_oid(&"a".repeat(40)).is_ok());
        assert!(validate_oid(&"a".repeat(64)).is_ok());
        assert!(validate_oid("-deadbeef").is_err());
        assert!(validate_oid("xyz").is_err());
    }

    /// The revision sits before the `--`: git reads everything after the
    /// terminator as a pathspec, so `checkout -- <branch>` restores a file
    /// named like the branch and `reset --hard -- <oid>` is refused outright.
    /// The terminator still closes the argv, so no path can follow unseen.
    #[test]
    fn destructive_argv_names_the_revision_before_the_terminator() {
        let oid = "a".repeat(40);
        assert_eq!(
            reset_hard_argv(&oid),
            vec!["reset", "--hard", oid.as_str(), "--"]
        );
        assert_eq!(checkout_argv("rift/x"), vec!["checkout", "rift/x", "--"]);
    }

    /// A real repository with one commit and a linked worktree beside it.
    #[cfg(feature = "gix")]
    fn repo_with_a_linked_worktree(root: &std::path::Path) -> std::path::PathBuf {
        let git = |dir: &std::path::Path, args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        let main = root.join("main");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["config", "user.email", "t@t"]);
        git(&main, &["config", "user.name", "T"]);
        git(&main, &["config", "commit.gpgsign", "false"]);
        git(&main, &["config", "core.quotePath", "true"]);
        std::fs::write(main.join("a.txt"), b"one\n").unwrap();
        git(&main, &["add", "a.txt"]);
        git(&main, &["commit", "-q", "-m", "seed"]);
        let linked = root.join("linked");
        git(&main, &["worktree", "add", "-q", linked.to_str().unwrap()]);
        linked
    }

    /// `git symbolic-ref HEAD` in `dir`: the branch HEAD is on, empty when detached.
    #[cfg(feature = "gix")]
    fn symbolic_head(dir: &std::path::Path) -> String {
        let out = Command::new("git")
            .args(["symbolic-ref", "-q", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    #[cfg(feature = "gix")]
    #[test]
    fn checking_out_a_branch_leaves_head_on_the_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let linked = repo_with_a_linked_worktree(tmp.path());
        let repo = crate::discover(&linked).unwrap();
        run_git(&repo, &["branch".into(), "side".into()]).unwrap();

        git_checkout(&repo, "side").unwrap();

        assert_eq!(symbolic_head(&linked), "refs/heads/side");
    }

    #[cfg(feature = "gix")]
    #[test]
    fn a_hard_reset_to_an_oid_puts_the_work_tree_back_at_that_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let linked = repo_with_a_linked_worktree(tmp.path());
        let repo = crate::discover(&linked).unwrap();
        let head = run_git_capture(&repo, &["rev-parse".into(), "HEAD".into()], &[]).unwrap();
        let head = String::from_utf8(head).unwrap().trim().to_owned();
        std::fs::write(linked.join("a.txt"), b"edited\n").unwrap();

        git_reset_hard(&repo, &head).unwrap();

        assert_eq!(std::fs::read(linked.join("a.txt")).unwrap(), b"one\n");
    }

    #[cfg(feature = "gix")]
    #[test]
    fn a_refspec_that_reads_as_a_flag_is_refused_before_git_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let linked = repo_with_a_linked_worktree(tmp.path());
        let repo = crate::discover(&linked).unwrap();
        let before = symbolic_head(&linked);

        let res = git_checkout(&repo, "--detach");

        assert!(matches!(res, Err(RiftError::Validity(_))), "{res:?}");
        assert_eq!(symbolic_head(&linked), before, "HEAD did not move");
    }

    #[cfg(feature = "gix")]
    #[test]
    fn the_boundary_status_leaves_rift_state_out_of_a_linked_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let linked = repo_with_a_linked_worktree(tmp.path());
        let state = linked.join(".rift/state/na\u{ef}ve dir");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("x\u{e9}.json"), b"{}\n").unwrap();
        std::fs::write(linked.join(".rift/config.toml"), b"[x]\n").unwrap();
        std::fs::write(linked.join("na\u{ef}ve.txt"), b"user\n").unwrap();
        std::fs::write(linked.join("a.txt"), b"one\ntwo\n").unwrap();
        let repo = crate::discover(&linked).unwrap();

        assert_eq!(
            changed_worktree_paths_outside_rift_state(&repo).unwrap(),
            vec![".rift/config.toml", "a.txt", "na\u{ef}ve.txt"],
            "Rift's state is left out; its committed configuration is not"
        );
        assert_eq!(
            changed_worktree_paths(&repo).unwrap().len(),
            4,
            "the plain status still names every untracked file"
        );
    }
}

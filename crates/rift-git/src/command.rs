//! The argv-safe shell-outs: validated oids and refspecs, `--`-terminated argv,
//! and one spawn site with an explicit `GIT_DIR`/`GIT_WORK_TREE` and no shell.

use std::process::Command;

use rift_core::{Result, RiftError};

use crate::{child_process_path, RepoPaths};

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

/// The argv for `reset --hard <oid>` — note the `--` terminator so the oid can
/// never be parsed as a flag.
#[must_use]
pub fn reset_hard_argv(oid: &str) -> Vec<String> {
    vec!["reset".into(), "--hard".into(), "--".into(), oid.into()]
}

/// The argv for `checkout <refspec>` — `--`-terminated.
#[must_use]
pub fn checkout_argv(refspec: &str) -> Vec<String> {
    vec!["checkout".into(), "--".into(), refspec.into()]
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
    if !stdin.is_empty() {
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(stdin)
                .map_err(|e| RiftError::storage(format!("git stdin failed: {e}")))?;
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| RiftError::storage(format!("git invocation failed: {e}")))?;
    Ok((output.status.success(), output.stdout))
}

/// The worktree paths that differ from HEAD (modified/added/deleted, staged or
/// not, plus untracked files) via `git status --porcelain -z`. Paths are
/// worktree-relative; rename records yield the **new** path.
///
/// # Errors
/// [`RiftError::Storage`] if `git` cannot be spawned or exits non-zero.
pub fn changed_worktree_paths(repo: &RepoPaths) -> Result<Vec<String>> {
    let out = run_git_capture(
        repo,
        &[
            "status".into(),
            "--porcelain=v1".into(),
            "-z".into(),
            "--untracked-files=all".into(),
        ],
        &[],
    )?;
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
    Ok(paths)
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

    #[test]
    fn destructive_argv_is_double_dash_terminated() {
        let oid = "a".repeat(40);
        let argv = reset_hard_argv(&oid);
        assert_eq!(argv, vec!["reset", "--hard", "--", oid.as_str()]);
        assert_eq!(checkout_argv("rift/x"), vec!["checkout", "--", "rift/x"]);
        // the `--` precedes the untrusted value → it can't parse as a flag.
        assert!(argv.iter().position(|a| a == "--").unwrap() < argv.len() - 1);
    }
}

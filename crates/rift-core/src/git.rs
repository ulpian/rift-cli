//! A minimal git read abstraction.
//!
//! Rift only ever *reads* from the `entire/checkpoints/v1` orphan branch, so we
//! shell out to the `git` binary rather than linking a native git library. The
//! [`GitReader`] trait keeps callers decoupled from that choice; swapping in
//! `gix` later is a one-file change.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Errors surfaced while reading from git.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("failed to spawn `git`: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("`git {args}` exited with status {code}: {stderr}")]
    NonZeroExit {
        args: String,
        code: i32,
        stderr: String,
    },
    #[error("output was not valid UTF-8")]
    NotUtf8,
}

/// Read-only access to a git repository.
pub trait GitReader {
    /// Whether a ref (branch, tag, …) exists in the repo.
    fn ref_exists(&self, refname: &str) -> Result<bool, GitError>;
    /// Read a file's bytes from a tree-ish, e.g. `git show <treeish>:<path>`.
    fn show_file(&self, treeish: &str, path: &str) -> Result<Vec<u8>, GitError>;
    /// List entries under a path in a tree-ish (one relative path per line).
    fn list_tree(&self, treeish: &str, path: &str) -> Result<Vec<String>, GitError>;
}

/// [`GitReader`] backed by the system `git` binary.
pub struct ShellGitReader {
    repo: PathBuf,
}

impl ShellGitReader {
    #[must_use]
    pub fn new(repo: impl AsRef<Path>) -> Self {
        Self {
            repo: repo.as_ref().to_path_buf(),
        }
    }

    fn run(&self, args: &[&str]) -> Result<std::process::Output, GitError> {
        Command::new("git")
            .args(args)
            .current_dir(&self.repo)
            .output()
            .map_err(GitError::Spawn)
    }
}

impl GitReader for ShellGitReader {
    fn ref_exists(&self, refname: &str) -> Result<bool, GitError> {
        // `rev-parse --verify` returns non-zero (without spawning failure) when absent.
        let out = self.run(&["rev-parse", "--verify", "--quiet", refname])?;
        Ok(out.status.success())
    }

    fn show_file(&self, treeish: &str, path: &str) -> Result<Vec<u8>, GitError> {
        let spec = format!("{treeish}:{path}");
        let out = self.run(&["show", &spec])?;
        if !out.status.success() {
            return Err(GitError::NonZeroExit {
                args: format!("show {spec}"),
                code: out.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        Ok(out.stdout)
    }

    fn list_tree(&self, treeish: &str, path: &str) -> Result<Vec<String>, GitError> {
        // An empty path lists the whole tree; otherwise scope to that pathspec.
        let mut args = vec!["ls-tree", "--name-only", "-r", treeish];
        if !path.is_empty() {
            args.push(path);
        }
        let out = self.run(&args)?;
        if !out.status.success() {
            return Err(GitError::NonZeroExit {
                args: format!("ls-tree {treeish} {path}"),
                code: out.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        let text = String::from_utf8(out.stdout).map_err(|_| GitError::NotUtf8)?;
        Ok(text.lines().map(str::to_owned).collect())
    }
}

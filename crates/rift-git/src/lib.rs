//! Native git plumbing for the Rust rewrite path (P7 S3).
//!
//! Three pieces the `NativeBackend` (P7 S2) composes:
//!
//! * [`RepoPaths`] + [`discover`] — explicit **worktree / common-dir** handling so
//!   multi-worktree repos capture correctly (objects in the common dir, per-worktree
//!   shadow refs, destructive ops isolated to the intended worktree).
//! * [`NativeGitWrite`] — additive object/ref writes where the **durable** writer
//!   accepts only [`RedactedBytes`] (the P5 S2 type wall survives into plumbing; a
//!   durable write of raw bytes does not compile) and the local-only writer takes
//!   [`SecretBytes`].
//! * [`git_reset_hard`] / [`git_checkout`] — the two **destructive** working-tree
//!   ops shell out to the user's `git` (mirroring Entire's documented go-git
//!   workaround), via [`run_git`]: an **argv array, no shell**, explicit
//!   `GIT_DIR`/`GIT_WORK_TREE`, with refspecs/oids validated so no argument is
//!   interpretable as a flag — defeating argument injection.
//!
//! This crate is the single home for the LAYOUT contract (ref naming + shard
//! paths). The recall-side substrate reader converges onto it as a follow-up; for
//! now the read side stays in `rift-recall-substrate` (gix) and this crate owns the
//! write side + worktree discovery + the destructive-op shell-out.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![allow(clippy::module_name_repetitions)]

use std::path::{Path, PathBuf};
use std::process::Command;

use rift_core::{RedactedBytes, Result, RiftError, SecretBytes};
use sha2::{Digest, Sha256};

/// Lowercase hex of a byte slice.
fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The resolved git paths for one worktree.
///
/// Objects live in the **common dir** (shared, written once); shadow refs are
/// per-worktree (named by [`worktree_hash`](RepoPaths::worktree_hash)); destructive
/// ops act on exactly this worktree via explicit `GIT_DIR`/`GIT_WORK_TREE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoPaths {
    /// This worktree's gitdir (`.git`, or `.git/worktrees/<name>` for a linked worktree).
    pub git_dir: PathBuf,
    /// The shared object/ref store (the main `.git`).
    pub common_dir: PathBuf,
    /// This worktree's working directory.
    pub work_tree: PathBuf,
    /// Identifies the worktree in the shadow-branch name `rift/<c7>-<wt6>` (LAYOUT).
    pub worktree_hash: [u8; 6],
}

impl RepoPaths {
    /// The worktree hash as 12 lowercase hex chars (the `wt6` token).
    #[must_use]
    pub fn worktree_token(&self) -> String {
        to_hex(&self.worktree_hash)
    }
}

/// Discover the repo paths from `start`, handling linked worktrees.
///
/// Walks up from `start` to a `.git` entry. A `.git` **directory** is the main
/// worktree (git_dir = common_dir = `.git`). A `.git` **file** (`gitdir: <path>`)
/// is a linked worktree; the common dir is read from `<git_dir>/commondir`.
///
/// # Errors
/// [`RiftError::NotFound`] if no `.git` is found walking up from `start`.
pub fn discover(start: &Path) -> Result<RepoPaths> {
    let mut dir = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent().unwrap_or(start).to_path_buf()
    };
    loop {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return Ok(build_paths(dot_git.clone(), dot_git, dir));
        }
        if dot_git.is_file() {
            let git_dir = parse_gitdir_file(&dot_git, &dir)?;
            let common_dir = read_common_dir(&git_dir);
            return Ok(build_paths(git_dir, common_dir, dir));
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => return Err(RiftError::NotFound("no .git found".into())),
        }
    }
}

fn build_paths(git_dir: PathBuf, common_dir: PathBuf, work_tree: PathBuf) -> RepoPaths {
    let mut h = Sha256::new();
    h.update(work_tree.to_string_lossy().as_bytes());
    let digest = h.finalize();
    let mut worktree_hash = [0u8; 6];
    worktree_hash.copy_from_slice(&digest[..6]);
    RepoPaths {
        git_dir,
        common_dir,
        work_tree,
        worktree_hash,
    }
}

/// Parse a `.git` **file** (`gitdir: <path>`) into the linked worktree's gitdir.
fn parse_gitdir_file(dot_git: &Path, base: &Path) -> Result<PathBuf> {
    let text = std::fs::read_to_string(dot_git)?;
    let rel = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .ok_or_else(|| RiftError::validity("malformed .git file (no gitdir:)"))?;
    let p = PathBuf::from(rel);
    Ok(if p.is_absolute() { p } else { base.join(p) })
}

/// Read `<git_dir>/commondir` (a path, relative to `git_dir`) → the common dir.
/// Absent → the git_dir is its own common dir (a non-worktree gitdir).
fn read_common_dir(git_dir: &Path) -> PathBuf {
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(rel) => {
            let rel = rel.trim();
            let p = PathBuf::from(rel);
            if p.is_absolute() {
                p
            } else {
                git_dir.join(p)
            }
        }
        Err(_) => git_dir.to_path_buf(),
    }
}

// --- additive object/ref writes (the type wall) -----------------------------

/// Additive git object/ref writes. The **durable** writer accepts only
/// [`RedactedBytes`]; the **local-only** writer accepts [`SecretBytes`] and may
/// only target a local shadow ref — the plumbing cannot durably persist raw bytes
/// (it does not compile). This is the P5 S2 gate surviving into the write layer.
pub trait NativeGitWrite {
    /// Write a durable blob from already-redacted bytes; returns its object id (hex).
    ///
    /// # Errors
    /// [`RiftError`] on a write failure.
    fn write_durable_blob(&self, body: &RedactedBytes) -> Result<String>;

    /// Write raw bytes to the **local-only** shadow ref (never a durable branch).
    ///
    /// # Errors
    /// [`RiftError`] on a write failure.
    fn write_shadow_raw(&self, body: &SecretBytes) -> Result<()>;

    /// Point a ref at an object id.
    ///
    /// # Errors
    /// [`RiftError`] on a write failure.
    fn update_ref(&self, name: &str, oid: &str) -> Result<()>;
}

/// An in-memory [`NativeGitWrite`] for tests: records durable blobs and refs, and
/// keeps local-only raw writes strictly separate from durable storage.
#[derive(Default)]
pub struct MemoryGitWrite {
    durable: std::cell::RefCell<Vec<(String, Vec<u8>)>>,
    refs: std::cell::RefCell<Vec<(String, String)>>,
    local_raw: std::cell::RefCell<usize>,
}

impl MemoryGitWrite {
    /// Number of durable blobs written.
    #[must_use]
    pub fn durable_count(&self) -> usize {
        self.durable.borrow().len()
    }

    /// Number of local-only raw writes (never durable).
    #[must_use]
    pub fn local_raw_count(&self) -> usize {
        *self.local_raw.borrow()
    }
}

impl NativeGitWrite for MemoryGitWrite {
    fn write_durable_blob(&self, body: &RedactedBytes) -> Result<String> {
        let bytes = body.as_slice().to_vec();
        let mut h = Sha256::new();
        h.update(&bytes);
        let oid = to_hex(&h.finalize());
        self.durable.borrow_mut().push((oid.clone(), bytes));
        Ok(oid)
    }

    fn write_shadow_raw(&self, body: &SecretBytes) -> Result<()> {
        let _ = body.as_slice().len(); // local-only; never enters durable storage
        *self.local_raw.borrow_mut() += 1;
        Ok(())
    }

    fn update_ref(&self, name: &str, oid: &str) -> Result<()> {
        self.refs
            .borrow_mut()
            .push((name.to_owned(), oid.to_owned()));
        Ok(())
    }
}

// --- destructive working-tree ops (shell out to `git`) ----------------------

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
    let output = Command::new("git")
        .args(args)
        .env("GIT_DIR", &repo.git_dir)
        .env("GIT_WORK_TREE", &repo.work_tree)
        .output()
        .map_err(|e| RiftError::storage(format!("git invocation failed: {e}")))?;
    if output.status.success() {
        Ok(())
    } else {
        // Carry the exit code only — never stdout/stderr bytes (no transcript leak).
        Err(RiftError::storage(format!(
            "git exited {}",
            output.status.code().unwrap_or(-1)
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_main_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        let p = discover(tmp.path()).unwrap();
        assert_eq!(
            p.git_dir, p.common_dir,
            "main worktree: git_dir == common_dir"
        );
        assert_eq!(p.work_tree, tmp.path());
        assert_eq!(p.worktree_token().len(), 12);
    }

    #[test]
    fn discover_linked_worktree_resolves_common_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // main repo .git with a worktrees/<name> gitdir + commondir pointer.
        let main_git = tmp.path().join("main").join(".git");
        let wt_git = main_git.join("worktrees").join("feature");
        std::fs::create_dir_all(&wt_git).unwrap();
        std::fs::write(wt_git.join("commondir"), "../..\n").unwrap();
        // the linked worktree dir with a `.git` file pointing at wt_git.
        let wt = tmp.path().join("feature");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", wt_git.display())).unwrap();

        let p = discover(&wt).unwrap();
        assert_eq!(p.git_dir, wt_git);
        assert_eq!(p.work_tree, wt);
        // commondir "../.." from worktrees/feature → main/.git
        assert_eq!(p.common_dir, wt_git.join("../.."));
        assert_ne!(p.git_dir, p.common_dir, "linked worktree splits the dirs");
    }

    #[test]
    fn distinct_worktrees_get_distinct_hashes() {
        let tmp = tempfile::tempdir().unwrap();
        let a = build_paths(
            tmp.path().join("a/.git"),
            tmp.path().join("a/.git"),
            tmp.path().join("a"),
        );
        let b = build_paths(
            tmp.path().join("b/.git"),
            tmp.path().join("b/.git"),
            tmp.path().join("b"),
        );
        assert_ne!(a.worktree_hash, b.worktree_hash);
    }

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

    #[test]
    fn durable_writer_separates_from_local_raw() {
        let w = MemoryGitWrite::default();
        // A RedactedBytes only via the sanctioned redaction gate (its sole constructor).
        let redacted = rift_shadow_redact::redact(&SecretBytes::new(b"durable bytes".to_vec()))
            .unwrap()
            .redacted;
        let oid = w.write_durable_blob(&redacted).unwrap();
        assert_eq!(oid.len(), 64);
        assert_eq!(w.durable_count(), 1);
        w.write_shadow_raw(&SecretBytes::new(b"raw local".to_vec()))
            .unwrap();
        assert_eq!(w.local_raw_count(), 1);
        // local-only writes never become durable blobs.
        assert_eq!(w.durable_count(), 1);
    }
}

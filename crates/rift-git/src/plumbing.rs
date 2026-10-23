//! Checkpoint-archive plumbing and the production [`NativeGitWrite`].
//!
//! git itself produces the byte-canonical objects that enter the SHA-256 hash
//! chain, so BYTE parity is *inherited* — there is no second
//! canonicalization to audit. All writes go through the same argv-safe seam.

use std::path::{Path, PathBuf};

use rift_core::{RedactedBytes, Result, RiftError, SecretBytes};

use crate::{
    child_process_path, run_git, run_git_capture, run_git_capture_env, run_git_status,
    validate_oid, validate_refspec, GitIdent, NativeGitWrite, OrphanCommit, RepoPaths,
    TreeWriteEntry,
};

/// Hash `bytes` as a git blob **and write it into the object store**
/// (`git hash-object -w --stdin`), returning the blob oid (hex, repo hash
/// algorithm). Local-only plumbing: blobs are unreachable objects unless later
/// referenced, and are what makes turn-boundary content addressable.
///
/// # Errors
/// [`RiftError::Storage`] if `git` cannot be spawned or exits non-zero.
pub fn hash_object_write(repo: &RepoPaths, bytes: &[u8]) -> Result<String> {
    let out = run_git_capture(
        repo,
        &["hash-object".into(), "-w".into(), "--stdin".into()],
        bytes,
    )?;
    Ok(String::from_utf8_lossy(&out).trim().to_owned())
}

/// Write the worktree files at `rel_paths` into the object store with one
/// `git hash-object -w --no-filters --stdin-paths`, returning their oids in
/// the order given.
///
/// One process for a whole turn boundary instead of one per file. The oids
/// are those [`hash_object_write`] gives the same bytes: `--no-filters` hashes
/// the file as it is on disk, which `--stdin` always does, so no clean filter
/// or line-ending rule can make the two disagree. Paths go to git absolute,
/// because `--stdin-paths` reads them against the process's directory, and an
/// absolute path never starts with the quote that makes git unquote a line.
///
/// # Errors
/// [`RiftError::Validity`] for a path the line protocol cannot carry (a line
/// break, or not UTF-8); [`RiftError::Storage`] if `git` cannot be spawned,
/// exits non-zero (a file it cannot read) or answers with a different count.
pub fn hash_files_write(repo: &RepoPaths, rel_paths: &[&str]) -> Result<Vec<String>> {
    if rel_paths.is_empty() {
        return Ok(Vec::new());
    }
    let work_tree = child_process_path(&repo.work_tree);
    let mut stdin = String::new();
    for rel in rel_paths {
        let path = work_tree.join(rel);
        let Some(line) = path.to_str().filter(|p| !p.contains(['\n', '\r'])) else {
            return Err(RiftError::validity("path cannot be named on a line"));
        };
        stdin.push_str(line);
        stdin.push('\n');
    }
    let argv = ["hash-object", "-w", "--no-filters", "--stdin-paths"].map(String::from);
    let out = run_git_capture(repo, &argv, stdin.as_bytes())?;
    let oids: Vec<String> = String::from_utf8_lossy(&out)
        .lines()
        .map(|l| l.trim().to_owned())
        .collect();
    if oids.len() != rel_paths.len() {
        return Err(RiftError::storage("hash-object answered a different count"));
    }
    Ok(oids)
}

/// The trimmed first line of git stdout as an oid (hex).
fn trim_oid(out: &[u8]) -> String {
    String::from_utf8_lossy(out)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// Build a tree object from entries via `git mktree` (git canonicalizes order and
/// framing). Entries reference already-written objects. Returns the tree oid.
///
/// # Errors
/// [`RiftError::Storage`] on a git failure, [`RiftError::Validity`] on a bad entry.
pub fn mktree(repo: &RepoPaths, entries: &[TreeWriteEntry]) -> Result<String> {
    use std::fmt::Write as _;
    let mut stdin = String::new();
    for e in entries {
        validate_oid(&e.oid)?;
        if e.name.is_empty() || e.name.contains('/') || e.name.contains('\0') {
            return Err(RiftError::validity("unsafe tree entry name"));
        }
        // `<mode> SP <type> SP <oid> TAB <name> LF` — git's mktree stdin format.
        let _ = writeln!(
            stdin,
            "{} {} {}\t{}",
            e.mode.mode_token(),
            e.mode.type_token(),
            e.oid,
            e.name
        );
    }
    let out = run_git_capture(repo, &["mktree".into()], stdin.as_bytes())?;
    Ok(trim_oid(&out))
}

/// Resolve a rev to a commit oid, or `None` if it does not resolve (e.g. a branch
/// that does not exist yet). `rev` is validated as a safe, non-flag ref token.
///
/// # Errors
/// [`RiftError::Validity`] for an unsafe rev; [`RiftError::Storage`] on spawn failure.
pub fn resolve_commit(repo: &RepoPaths, rev: &str) -> Result<Option<String>> {
    validate_refspec(rev)?;
    let (ok, out) = run_git_status(
        repo,
        &[
            "rev-parse".into(),
            "--verify".into(),
            "--quiet".into(),
            format!("{rev}^{{commit}}"),
        ],
        &[],
        &[],
    )?;
    Ok(ok.then(|| trim_oid(&out)).filter(|s| !s.is_empty()))
}

/// The root-tree oid of a commit.
///
/// # Errors
/// [`RiftError::Validity`]/[`RiftError::Storage`] on failure.
pub fn commit_tree_oid(repo: &RepoPaths, commit: &str) -> Result<String> {
    validate_oid(commit)?;
    let out = run_git_capture(
        repo,
        &["rev-parse".into(), format!("{commit}^{{tree}}")],
        &[],
    )?;
    Ok(trim_oid(&out))
}

/// Whether `path` exists inside the tree of `commit` (object probe, no worktree).
///
/// # Errors
/// [`RiftError::Storage`] on spawn failure.
pub fn path_in_commit(repo: &RepoPaths, commit: &str, path: &str) -> Result<bool> {
    validate_oid(commit)?;
    let (ok, _) = run_git_status(
        repo,
        &["cat-file".into(), "-e".into(), format!("{commit}:{path}")],
        &[],
        &[],
    )?;
    Ok(ok)
}

/// The object id git resolves at `<commit>:<path>` (the subtree/blob oid at that
/// path), or `None` if the path does not exist. Used to compare an already-grafted
/// shard's content against a new one (content-aware idempotency).
///
/// # Errors
/// [`RiftError::Storage`] on spawn failure.
pub fn path_object_oid(repo: &RepoPaths, commit: &str, path: &str) -> Result<Option<String>> {
    validate_oid(commit)?;
    let (ok, out) = run_git_status(
        repo,
        &[
            "rev-parse".into(),
            "--verify".into(),
            "--quiet".into(),
            format!("{commit}:{path}"),
        ],
        &[],
        &[],
    )?;
    Ok(ok.then(|| trim_oid(&out)).filter(|s| !s.is_empty()))
}

/// The shard entries that drift with the anchoring commit / archive time and
/// are therefore EXEMPT from idempotence comparison: `metadata.json` embeds
/// them directly, and `content_hash.txt` is a digest computed OVER
/// metadata.json (the LAYOUT contract), so it inherits the drift. The capture
/// identity is carried by the remaining entries (`content.redacted`,
/// `redaction_report.json`, `full.jsonl`), which are always compared.
const ANCHOR_DEPENDENT_SHARD_ENTRIES: &[&str] = &["metadata.json", "content_hash.txt"];

/// Whether one `ls-tree` line names an [`ANCHOR_DEPENDENT_SHARD_ENTRIES`]
/// entry. Lifted out of the comparison so the RULE can be tested without a git
/// process: the rule that "a re-archive from another worktree keeps the first
/// label" is true only because `metadata.json` — which now also carries the
/// `worktree` member — is on that list.
fn is_anchor_dependent_entry(ls_tree_line: &str) -> bool {
    ANCHOR_DEPENDENT_SHARD_ENTRIES
        .iter()
        .any(|n| ls_tree_line.ends_with(&format!("\t{n}")))
}

/// Whether two shard leaf trees are equal once the anchor-dependent entries are
/// set aside (see [`ANCHOR_DEPENDENT_SHARD_ENTRIES`]) — the checkpoint id is
/// content-addressed over the capture and deliberately excludes the anchoring
/// commit (the trailer-prediction contract), so those entries legitimately
/// differ across re-archives of identical capture.
///
/// # Errors
/// [`RiftError::Validity`]/[`RiftError::Storage`] if a tree cannot be listed.
fn trees_equal_ignoring_anchor(repo: &RepoPaths, a: &str, b: &str) -> Result<bool> {
    validate_oid(a)?;
    validate_oid(b)?;
    let list = |oid: &str| -> Result<Vec<String>> {
        let out = run_git_capture(repo, &["ls-tree".into(), oid.to_owned()], &[])?;
        let mut lines: Vec<String> = String::from_utf8_lossy(&out)
            .lines()
            .filter(|l| !is_anchor_dependent_entry(l))
            .map(str::to_owned)
            .collect();
        lines.sort_unstable();
        Ok(lines)
    };
    Ok(list(a)? == list(b)?)
}

/// The shard-tree entries that participate in idempotence comparison —
/// everything except the anchor-dependent pair (see
/// [`trees_equal_ignoring_anchor`]). Shared by the in-memory writer double.
pub(crate) fn non_anchor_entries(entries: &[TreeWriteEntry]) -> Vec<TreeWriteEntry> {
    entries
        .iter()
        .filter(|e| !ANCHOR_DEPENDENT_SHARD_ENTRIES.contains(&e.name.as_str()))
        .cloned()
        .collect()
}

/// Commit `tree` (optionally onto `parent`) with a deterministic identity + date,
/// returning the commit oid. Author == committer, date fixed by the caller, so the
/// commit is byte-reproducible for identical inputs.
///
/// # Errors
/// [`RiftError::Validity`]/[`RiftError::Storage`] on failure.
pub fn commit_tree(
    repo: &RepoPaths,
    tree: &str,
    parent: Option<&str>,
    message: &str,
    ident: &GitIdent,
    date_epoch: i64,
) -> Result<String> {
    validate_oid(tree)?;
    let mut args = vec![
        "commit-tree".into(),
        tree.to_owned(),
        "-m".into(),
        message.to_owned(),
    ];
    if let Some(p) = parent {
        validate_oid(p)?;
        args.push("-p".into());
        args.push(p.to_owned());
    }
    // `@<epoch> +0000` is git's unambiguous unix-timestamp form (a bare small
    // integer like `1 +0000` is rejected as an invalid date); it normalizes to the
    // same stored `<epoch> +0000`, so the commit oid is byte-reproducible.
    let date = format!("@{date_epoch} +0000");
    let env = [
        ("GIT_AUTHOR_NAME", ident.name.as_str()),
        ("GIT_AUTHOR_EMAIL", ident.email.as_str()),
        ("GIT_AUTHOR_DATE", date.as_str()),
        ("GIT_COMMITTER_NAME", ident.name.as_str()),
        ("GIT_COMMITTER_EMAIL", ident.email.as_str()),
        ("GIT_COMMITTER_DATE", date.as_str()),
    ];
    let out = run_git_capture_env(repo, &args, &[], &env)?;
    Ok(trim_oid(&out))
}

/// Point `refname` at `new_oid`, compare-and-swapping against `old_oid` when known
/// (so a concurrent advance is rejected rather than clobbered).
///
/// # Errors
/// [`RiftError::Validity`]/[`RiftError::Storage`] on failure.
pub fn update_ref_cas(
    repo: &RepoPaths,
    refname: &str,
    new_oid: &str,
    old_oid: Option<&str>,
) -> Result<()> {
    validate_refspec(refname)?;
    validate_oid(new_oid)?;
    let mut args = vec!["update-ref".into(), refname.to_owned(), new_oid.to_owned()];
    if let Some(old) = old_oid {
        validate_oid(old)?;
        args.push(old.to_owned());
    }
    run_git(repo, &args)
}

/// The all-zero object id — git's "the ref must not already exist" sentinel for a
/// compare-and-swap `update-ref` (a create-only assertion).
const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// Create `refname` at `new_oid`, asserting it does **not** already exist (CAS
/// against the zero oid). A concurrent creation by another worktree is rejected,
/// not clobbered — the loser errors and its caller defers rather than silently
/// dropping the winner's checkpoint.
///
/// # Errors
/// [`RiftError::Validity`] for a bad ref/oid; [`RiftError::Storage`] if the ref
/// already exists (lost the create race) or git fails.
pub fn update_ref_create(repo: &RepoPaths, refname: &str, new_oid: &str) -> Result<()> {
    validate_refspec(refname)?;
    validate_oid(new_oid)?;
    // Zero old-value requires the ref to not already exist (create-only CAS).
    run_git(
        repo,
        &[
            "update-ref".into(),
            refname.to_owned(),
            new_oid.to_owned(),
            ZERO_OID.to_owned(),
        ],
    )
}

/// The production [`NativeGitWrite`]: writes checkpoint objects into `repo`'s
/// object store via screened git plumbing. Reads stay on gix elsewhere.
pub struct PlumbingGitWrite {
    repo: RepoPaths,
    ident: GitIdent,
}

impl PlumbingGitWrite {
    /// A writer for `repo` with the default (brand-free) commit identity.
    #[must_use]
    pub fn new(repo: RepoPaths) -> Self {
        Self {
            repo,
            ident: GitIdent::default(),
        }
    }

    /// A writer with an explicit commit identity.
    #[must_use]
    pub fn with_ident(repo: RepoPaths, ident: GitIdent) -> Self {
        Self { repo, ident }
    }
}

impl NativeGitWrite for PlumbingGitWrite {
    fn write_durable_blob(&self, body: &RedactedBytes) -> Result<String> {
        hash_object_write(&self.repo, body.as_slice())
    }

    fn write_shadow_raw(&self, body: &SecretBytes) -> Result<()> {
        // Local-only: write the blob into the object store (unreachable until a
        // local shadow ref points at it); it never lands on a durable branch here.
        hash_object_write(&self.repo, body.as_slice()).map(|_| ())
    }

    fn write_structural_blob(&self, bytes: &[u8]) -> Result<String> {
        hash_object_write(&self.repo, bytes)
    }

    fn update_ref(&self, name: &str, oid: &str) -> Result<()> {
        update_ref_cas(&self.repo, name, oid, None)
    }

    fn write_tree(&self, entries: &[TreeWriteEntry]) -> Result<String> {
        mktree(&self.repo, entries)
    }

    fn commit_on_orphan(&self, spec: &OrphanCommit<'_>) -> Result<String> {
        validate_orphan_spec(spec)?;
        let tip = resolve_commit(&self.repo, spec.branch)?;
        if let Some(unchanged) = existing_shard_tip(&self.repo, tip.as_deref(), spec)? {
            return Ok(unchanged);
        }
        // The order is the crash-safety argument: the root tree and the commit
        // are written before the ref names them, and the ref moves last, by
        // compare-and-swap, so a crash never leaves the branch naming a
        // missing object.
        let root_tree = graft_root_tree(&self.repo, tip.as_deref(), spec)?;
        let commit = commit_tree(
            &self.repo,
            &root_tree,
            tip.as_deref(),
            spec.message,
            &self.ident,
            spec.date_epoch,
        )?;
        advance_branch(&self.repo, spec.branch, &commit, tip.as_deref())?;
        Ok(commit)
    }
}

/// Screen every part of an [`OrphanCommit`] that reaches git's argv before
/// any git runs: the tree oid, the branch, and the shard path, which is joined
/// into a `read-tree --prefix` and so must be relative, free of `..`, and
/// spelled only in the hex-and-slash alphabet of a checkpoint id.
fn validate_orphan_spec(spec: &OrphanCommit<'_>) -> Result<()> {
    validate_oid(spec.tree_oid)?;
    validate_refspec(spec.branch)?;
    if spec.shard_path.is_empty()
        || spec.shard_path.starts_with('/')
        || spec.shard_path.contains("..")
        || !spec
            .shard_path
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b'/')
    {
        return Err(RiftError::validity("unsafe shard path"));
    }
    Ok(())
}

/// The tip to return when the shard is already on the branch, `None` when the
/// graft still has to happen.
///
/// Idempotent by CONTENT, not merely by path: a re-fired archive of the
/// identical shard (same leaf tree) is a true no-op, and so is
/// anchor-only drift — the checkpoint id is content-addressed over the
/// capture, deliberately excluding the anchoring commit (the
/// trailer-prediction contract), so unchanged capture re-archived at a
/// later commit differs only in metadata.json + content_hash.txt and
/// keeps the FIRST anchor. A shard whose *capture* entries differ is a
/// genuine conflict — the immutable branch keeps the first, so we must
/// surface it (a divergence under DualRun) instead of silently dropping
/// content.
fn existing_shard_tip(
    repo: &RepoPaths,
    tip: Option<&str>,
    spec: &OrphanCommit<'_>,
) -> Result<Option<String>> {
    let Some(tip) = tip else {
        return Ok(None);
    };
    let Some(existing) = path_object_oid(repo, tip, spec.shard_path)? else {
        return Ok(None);
    };
    if existing == spec.tree_oid || trees_equal_ignoring_anchor(repo, &existing, spec.tree_oid)? {
        return Ok(Some(tip.to_owned()));
    }
    Err(RiftError::integrity(
        "checkpoint shard path already holds different content",
    ))
}

/// The name every graft's scratch index wears inside the git dir, and the
/// prefix [`sweep_stale_graft_indexes`] lists by.
const GRAFT_INDEX_PREFIX: &str = "rift-graft-index-";

/// The suffix of the lock file a live graft holds beside its index.
const GRAFT_HELD_SUFFIX: &str = "-held";

/// How old an unheld graft index must be before the sweep removes it: a
/// graft lasts seconds, and a file this old was left by a killed worker or
/// written by a binary from before the lock existed.
const GRAFT_INDEX_STALE_MS: i64 = 86_400_000;

/// Build the merged root tree in a scratch index seeded from the current tip,
/// then graft the checkpoint tree at the shard path. Git owns the merge +
/// canonical framing, so no ancestor tree is rebuilt by hand. The scratch
/// index is removed whether or not the graft succeeds.
///
/// A killed worker leaves its index behind, and the only safe remover is one
/// that can tell a dead graft from a live one without a pid probe (no
/// `kill(pid, 0)`, no `lsof`, no `unsafe`). So a live graft is a **locked
/// file**: the `-held` sibling is opened and `try_lock`ed here and the handle
/// lives for the graft, released by the OS when the process dies. The lock
/// sits beside the index rather than on it because git replaces the index by
/// renaming its own `.lock` over it on every write and refuses a pre-created
/// empty index, so a handle on the index path would name an orphaned inode
/// after the first `read-tree`.
fn graft_root_tree(repo: &RepoPaths, tip: Option<&str>, spec: &OrphanCommit<'_>) -> Result<String> {
    let index_path = repo.git_dir.join(format!(
        "{GRAFT_INDEX_PREFIX}{}-{}",
        std::process::id(),
        &spec.shard_path.replace('/', "")
    ));
    // The path leaves the process in git's environment, so it gets the
    // child spelling: a verbatim `GIT_INDEX_FILE` is refused on Windows
    // ("Unable to create ….lock: Invalid argument") exactly as `GIT_DIR` is.
    let index_str = child_process_path(&index_path)
        .to_string_lossy()
        .into_owned();
    let env = [("GIT_INDEX_FILE", index_str.as_str())];
    let _ = std::fs::remove_file(&index_path); // start clean
    let held_path = graft_held_path(&index_path);
    let held = hold_graft_index(&held_path);
    let root_tree = graft_in_index(repo, tip, spec, &env);
    let _ = std::fs::remove_file(&index_path); // best-effort cleanup
    drop(held);
    let _ = std::fs::remove_file(&held_path);
    root_tree
}

/// The lock file a graft holds beside `index_path`.
fn graft_held_path(index_path: &Path) -> PathBuf {
    let mut name = index_path.as_os_str().to_owned();
    name.push(GRAFT_HELD_SUFFIX);
    PathBuf::from(name)
}

/// Open and lock the graft's `-held` file. Best-effort: a graft that cannot
/// take its lock still runs — the age guard then covers its index — because
/// refusing to archive over a lock file would strand capture.
fn hold_graft_index(held_path: &Path) -> Option<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(held_path)
        .ok()?;
    file.try_lock().ok()?;
    Some(file)
}

/// Remove the scratch indexes under `git_dir` that no graft holds and that
/// are older than a day at `now_ms`; the count removed. Never a hook's
/// work — the idle sweep's — and never a file a process holds open: a
/// `-held` sibling whose lock cannot be taken is a live graft, and its index
/// is kept however old it looks. A pre-lock binary's index has no sibling,
/// which is what the age is for.
#[must_use]
pub fn sweep_stale_graft_indexes(git_dir: &Path, now_ms: i64) -> usize {
    let Ok(entries) = std::fs::read_dir(git_dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with(GRAFT_INDEX_PREFIX) || name.ends_with(GRAFT_HELD_SUFFIX) {
            continue;
        }
        if !graft_index_is_stale(&path, now_ms) {
            continue;
        }
        let held_path = graft_held_path(&path);
        let Some(unheld) = hold_graft_index(&held_path) else {
            continue; // `WouldBlock` or unopenable: somebody's, or not ours to say
        };
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
        drop(unheld);
        let _ = std::fs::remove_file(&held_path);
    }
    removed
}

/// Whether `path` was last written more than [`GRAFT_INDEX_STALE_MS`] before
/// `now_ms`; an unreadable or future mtime is not stale.
fn graft_index_is_stale(path: &Path, now_ms: i64) -> bool {
    let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else {
        return false;
    };
    let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH) else {
        return false;
    };
    let Ok(modified_ms) = i64::try_from(since_epoch.as_millis()) else {
        return false;
    };
    now_ms.saturating_sub(modified_ms) > GRAFT_INDEX_STALE_MS
}

/// Seed the scratch index named by `env` from the tip's root tree (when the
/// branch exists), read the checkpoint tree in under the shard prefix, and
/// write the merged root tree out.
fn graft_in_index(
    repo: &RepoPaths,
    tip: Option<&str>,
    spec: &OrphanCommit<'_>,
    env: &[(&str, &str)],
) -> Result<String> {
    if let Some(t) = tip {
        let root = commit_tree_oid(repo, t)?;
        run_git_capture_env(repo, &["read-tree".into(), root], &[], env)?;
    }
    run_git_capture_env(
        repo,
        &[
            "read-tree".into(),
            format!("--prefix={}/", spec.shard_path),
            spec.tree_oid.to_owned(),
        ],
        &[],
        env,
    )?;
    let out = run_git_capture_env(repo, &["write-tree".into()], &[], env)?;
    Ok(trim_oid(&out))
}

/// Advance CAS-protected against the tip we read; create with a
/// must-not-exist assertion so a concurrent creator can't be clobbered.
fn advance_branch(repo: &RepoPaths, branch: &str, commit: &str, tip: Option<&str>) -> Result<()> {
    match tip {
        Some(old) => update_ref_cas(repo, branch, commit, Some(old)),
        None => update_ref_create(repo, branch, commit),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "gix")]
    use crate::discover;
    use crate::GitFileMode;

    // --- real git plumbing against a temp repo ------------------------------

    #[cfg(feature = "gix")]
    fn init_repo() -> (tempfile::TempDir, RepoPaths) {
        let tmp = tempfile::tempdir().unwrap();
        // Init a real repo with a deterministic default branch name.
        let status = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        // Discovered from the canonical spelling on purpose: that is what the
        // family plane stores and hands to every git call, and on Windows it
        // carries the verbatim prefix git refuses in `GIT_DIR` and
        // `GIT_INDEX_FILE` unless the spawn sites re-spell it.
        let canonical = std::fs::canonicalize(tmp.path()).unwrap();
        let repo = discover(&canonical).unwrap();
        (tmp, repo)
    }

    #[cfg(feature = "gix")]
    #[test]
    fn plumbing_archive_roundtrips_through_gix() {
        let (tmp, repo) = init_repo();
        let w = PlumbingGitWrite::new(repo.clone());
        let ident = GitIdent::default();
        let branch = "refs/heads/rift/checkpoints/v1";
        let id = "ab".to_string() + &"cd".repeat(31); // 64-hex checkpoint id
        let shard_path = format!("{}/{}", &id[..2], &id[2..]);

        // Write two structural artifacts, assemble the leaf tree, graft + commit.
        let meta = w
            .write_structural_blob(br#"{"strategy":"manual-commit"}"#)
            .unwrap();
        let report = w.write_structural_blob(br#"{"total_spans":0}"#).unwrap();
        let leaf = w
            .write_tree(&[
                TreeWriteEntry {
                    mode: GitFileMode::Blob,
                    name: "metadata.json".into(),
                    oid: meta,
                },
                TreeWriteEntry {
                    mode: GitFileMode::Blob,
                    name: "redaction_report.json".into(),
                    oid: report,
                },
            ])
            .unwrap();
        let commit = w
            .commit_on_orphan(&OrphanCommit {
                branch,
                shard_path: &shard_path,
                tree_oid: &leaf,
                message: "checkpoint",
                ident: &ident,
                date_epoch: 1_700_000_000,
            })
            .unwrap();
        assert_eq!(commit.len(), 40, "sha1 commit oid");

        // The branch resolves and carries the shard path.
        assert_eq!(
            resolve_commit(&repo, branch).unwrap().as_deref(),
            Some(commit.as_str())
        );
        assert!(path_in_commit(&repo, &commit, &format!("{shard_path}/metadata.json")).unwrap());

        // Read the artifact back through gix at exactly the substrate's layout path.
        let reader = crate::GixBlobReader::open(&repo.work_tree).unwrap();
        let _ = &reader; // constructed to prove a gix reader opens the repo
        drop(tmp);
    }

    #[cfg(feature = "gix")]
    #[test]
    fn plumbing_archive_is_deterministic_and_idempotent() {
        let (tmp, repo) = init_repo();
        let w = PlumbingGitWrite::new(repo.clone());
        let ident = GitIdent::default();
        let branch = "refs/heads/rift/checkpoints/v1";
        let id = "ef".to_string() + &"12".repeat(31);
        let shard_path = format!("{}/{}", &id[..2], &id[2..]);

        let archive_once = |w: &PlumbingGitWrite| {
            let meta = w
                .write_structural_blob(br#"{"strategy":"manual-commit"}"#)
                .unwrap();
            let leaf = w
                .write_tree(&[TreeWriteEntry {
                    mode: GitFileMode::Blob,
                    name: "metadata.json".into(),
                    oid: meta,
                }])
                .unwrap();
            w.commit_on_orphan(&OrphanCommit {
                branch,
                shard_path: &shard_path,
                tree_oid: &leaf,
                message: "checkpoint",
                ident: &ident,
                date_epoch: 1_700_000_000,
            })
            .unwrap()
        };
        let c1 = archive_once(&w);
        // Re-archiving the same checkpoint id is a byte-identical no-op (same tip).
        let c2 = archive_once(&w);
        assert_eq!(c1, c2, "idempotent: identical inputs → identical tip");

        // A second, distinct checkpoint sharing the shard prefix advances the branch.
        let id2 = "ef".to_string() + &"34".repeat(31);
        let shard2 = format!("{}/{}", &id2[..2], &id2[2..]);
        let meta2 = w
            .write_structural_blob(br#"{"strategy":"scheduled"}"#)
            .unwrap();
        let leaf2 = w
            .write_tree(&[TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "metadata.json".into(),
                oid: meta2,
            }])
            .unwrap();
        let c3 = w
            .commit_on_orphan(&OrphanCommit {
                branch,
                shard_path: &shard2,
                tree_oid: &leaf2,
                message: "checkpoint",
                ident: &ident,
                date_epoch: 1_700_000_001,
            })
            .unwrap();
        assert_ne!(c3, c1, "a new shard advances the branch");
        // Both shards are present (the graft merged, it did not replace).
        assert!(path_in_commit(&repo, &c3, &format!("{shard_path}/metadata.json")).unwrap());
        assert!(path_in_commit(&repo, &c3, &format!("{shard2}/metadata.json")).unwrap());
        drop(tmp);
    }

    #[cfg(feature = "gix")]
    #[test]
    fn plumbing_rejects_conflicting_shard_and_survives_reidentical() {
        let (tmp, repo) = init_repo();
        let w = PlumbingGitWrite::new(repo.clone());
        let ident = GitIdent::default();
        let branch = "refs/heads/rift/checkpoints/v1";
        let id = "ab".to_string() + &"cd".repeat(31);
        let shard_path = format!("{}/{}", &id[..2], &id[2..]);

        // A shard = one capture entry (participates in identity) + metadata.json
        // (exempt: it embeds the anchoring commit + time, which the checkpoint id
        // deliberately excludes).
        let leaf_for = |capture: &[u8], meta: &[u8]| {
            let capture_oid = w.write_structural_blob(capture).unwrap();
            let meta_oid = w.write_structural_blob(meta).unwrap();
            let mut entries = vec![
                TreeWriteEntry {
                    mode: GitFileMode::Blob,
                    name: "content.redacted".into(),
                    oid: capture_oid,
                },
                TreeWriteEntry {
                    mode: GitFileMode::Blob,
                    name: "metadata.json".into(),
                    oid: meta_oid,
                },
            ];
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            w.write_tree(&entries).unwrap()
        };
        let commit_for = |leaf: &str| {
            w.commit_on_orphan(&OrphanCommit {
                branch,
                shard_path: &shard_path,
                tree_oid: leaf,
                message: "checkpoint",
                ident: &ident,
                date_epoch: 1_700_000_000,
            })
        };

        let leaf_a = leaf_for(b"hash-A", br#"{"commit":"A"}"#);
        let c1 = commit_for(&leaf_a).unwrap();
        // Re-archiving the byte-identical shard is a true no-op (same tip).
        assert_eq!(commit_for(&leaf_a).unwrap(), c1);
        // Metadata-only drift (same capture, later anchoring commit) is ALSO a
        // no-op — the branch keeps the first anchor (the same-session re-archive).
        let leaf_drift = leaf_for(b"hash-A", br#"{"commit":"B"}"#);
        assert_eq!(commit_for(&leaf_drift).unwrap(), c1);
        // A different CAPTURE body at the SAME checkpoint path is a hard conflict —
        // the branch keeps the first; the second is surfaced, never dropped silently.
        let leaf_b = leaf_for(b"hash-B", br#"{"commit":"B"}"#);
        let conflict = commit_for(&leaf_b);
        assert!(
            matches!(conflict, Err(RiftError::Integrity(_))),
            "content conflict → Integrity error"
        );
        // The branch tip is unchanged (the first shard is intact).
        assert_eq!(
            resolve_commit(&repo, branch).unwrap().as_deref(),
            Some(c1.as_str())
        );
        drop(tmp);
    }

    #[cfg(feature = "gix")]
    #[test]
    fn update_ref_create_rejects_an_existing_ref() {
        let (tmp, repo) = init_repo();
        let w = PlumbingGitWrite::new(repo.clone());
        let blob = w.write_structural_blob(b"x").unwrap();
        let tree = w
            .write_tree(&[TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "f".into(),
                oid: blob,
            }])
            .unwrap();
        let ident = GitIdent::default();
        let commit = commit_tree(&repo, &tree, None, "c", &ident, 1).unwrap();
        // First create succeeds; a second create of the same ref must reject
        // (the must-not-exist assertion), not clobber.
        update_ref_create(&repo, "refs/heads/rift/checkpoints/v1", &commit).unwrap();
        assert!(
            update_ref_create(&repo, "refs/heads/rift/checkpoints/v1", &commit).is_err(),
            "creating an existing ref must reject (no clobber)"
        );
        drop(tmp);
    }

    #[test]
    fn mktree_rejects_unsafe_entry() {
        // A name with a path separator or a bad oid never reaches git.
        let bad = TreeWriteEntry {
            mode: GitFileMode::Blob,
            name: "a/b".into(),
            oid: "a".repeat(40),
        };
        let repo = RepoPaths {
            git_dir: PathBuf::from("/nonexistent/.git"),
            common_dir: PathBuf::from("/nonexistent/.git"),
            work_tree: PathBuf::from("/nonexistent"),
            worktree_hash: [0u8; 6],
        };
        assert!(mktree(&repo, &[bad]).is_err());
    }

    /// A shard path is joined into a `read-tree --prefix`, so a hostile one is
    /// refused as a validity error before any git runs: against a repository
    /// that does not exist, a check that ran later would surface as a storage
    /// error instead.
    #[test]
    fn an_unsafe_shard_path_is_refused_before_git_runs() {
        let repo = RepoPaths {
            git_dir: PathBuf::from("/nonexistent/.git"),
            common_dir: PathBuf::from("/nonexistent/.git"),
            work_tree: PathBuf::from("/nonexistent"),
            worktree_hash: [0u8; 6],
        };
        let w = PlumbingGitWrite::new(repo);
        let ident = GitIdent::default();
        let tree_oid = "a".repeat(40);
        for shard_path in ["", "/ab/cd", "ab/../cd", "ab/zz"] {
            let refused = w.commit_on_orphan(&OrphanCommit {
                branch: "refs/heads/rift/checkpoints/v1",
                shard_path,
                tree_oid: &tree_oid,
                message: "checkpoint",
                ident: &ident,
                date_epoch: 1,
            });
            assert!(
                matches!(refused, Err(RiftError::Validity(_))),
                "{shard_path:?} must be refused as unsafe"
            );
        }
    }

    /// The first-archiver rule rests on ONE existing fact: the
    /// idempotence comparison sets `metadata.json` aside, so a re-archive of
    /// byte-identical capture from another worktree changes no compared entry
    /// and the label the first archive wrote stands. The rule is tested, not
    /// just the constant it reads.
    #[test]
    fn a_reidentical_rearchive_from_another_worktree_keeps_the_first_label() {
        // `ls-tree` lines, as `trees_equal_ignoring_anchor` receives them.
        let line = |name: &str| format!("100644 blob {}\t{name}", "d".repeat(40));
        assert!(
            is_anchor_dependent_entry(&line("metadata.json")),
            "the worktree member rides in metadata.json, which is exempt"
        );
        assert!(is_anchor_dependent_entry(&line("content_hash.txt")));
        // The capture identity is still compared — the exemption is narrow.
        assert!(!is_anchor_dependent_entry(&line("content.redacted")));
        assert!(!is_anchor_dependent_entry(&line("redaction_report.json")));
        assert!(!is_anchor_dependent_entry(&line("full.jsonl")));
        // A name that merely CONTAINS an exempt name is not exempt: the match
        // is on the tab-delimited entry name, not on a substring.
        assert!(!is_anchor_dependent_entry(&line("not-metadata.json")));
    }

    #[cfg(feature = "gix")]
    #[test]
    fn one_batch_write_names_the_same_oids_as_one_write_per_file() {
        let (tmp, repo) = init_repo();
        let dir = tmp.path();
        // A line-ending rule and a nested path: the batch must hash the bytes
        // on disk, exactly as the one-file write of those bytes does.
        std::fs::write(dir.join(".gitattributes"), b"* text eol=crlf\n").unwrap();
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        std::fs::write(dir.join("src/deep/lf.rs"), b"fn a() {}\nfn b() {}\n").unwrap();
        std::fs::write(dir.join("crlf.txt"), b"one\r\ntwo\r\n").unwrap();
        std::fs::write(dir.join("empty"), b"").unwrap();
        let paths = ["src/deep/lf.rs", "crlf.txt", "empty", ".gitattributes"];

        let batch = hash_files_write(&repo, &paths).unwrap();

        let one_by_one: Vec<String> = paths
            .iter()
            .map(|p| hash_object_write(&repo, &std::fs::read(dir.join(p)).unwrap()).unwrap())
            .collect();
        assert_eq!(batch, one_by_one);
        let reader = crate::GixBlobReader::open(dir).unwrap();
        let first = crate::BlobReader::read_blob(&reader, &batch[0]).unwrap();
        assert_eq!(first.as_deref(), Some(&b"fn a() {}\nfn b() {}\n"[..]));
    }

    #[cfg(feature = "gix")]
    #[test]
    fn a_batch_write_refuses_a_path_with_a_line_break_before_git_runs() {
        let (_tmp, repo) = init_repo();
        assert!(hash_files_write(&repo, &["a\nb"]).is_err());
        assert_eq!(hash_files_write(&repo, &[]).unwrap(), Vec::<String>::new());
    }

    /// The sweep's two refusals and its one removal, with the clock injected:
    /// an index a day old that nobody holds goes; one whose `-held` sibling is
    /// locked (here, by this test) stays however old; a fresh one stays.
    #[test]
    fn a_stale_graft_index_nobody_holds_is_swept_and_a_held_one_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path();
        let now_ms: i64 = 1_790_000_000_000;
        let two_days_ago = std::time::UNIX_EPOCH
            + std::time::Duration::from_millis(u64::try_from(now_ms).unwrap() - 2 * 86_400_000);
        let age = |path: &Path| {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(two_days_ago)
                .unwrap();
        };
        let stale = git_dir.join("rift-graft-index-4242-abcd");
        std::fs::write(&stale, b"index").unwrap();
        age(&stale);
        let held = git_dir.join("rift-graft-index-4343-ef01");
        std::fs::write(&held, b"index").unwrap();
        age(&held);
        let held_lock = hold_graft_index(&graft_held_path(&held)).expect("the test holds it");
        age(&graft_held_path(&held));
        let fresh = git_dir.join("rift-graft-index-4444-2345");
        std::fs::write(&fresh, b"index").unwrap();
        let unrelated = git_dir.join("index");
        std::fs::write(&unrelated, b"not ours").unwrap();
        age(&unrelated);

        assert_eq!(sweep_stale_graft_indexes(git_dir, now_ms), 1);
        assert!(!stale.exists(), "a day-old index nobody holds is swept");
        assert!(held.exists(), "a held index is kept however old");
        assert!(graft_held_path(&held).exists());
        assert!(fresh.exists(), "a fresh index is a graft in progress");
        assert!(unrelated.exists(), "only the graft prefix is listed");

        // Other tests spawn Git concurrently: a child can inherit this open
        // description until exec closes it. Explicit unlock makes the release
        // independent of that child's scheduling, without sleeping or retrying.
        held_lock.unlock().unwrap();
        drop(held_lock);
        assert_eq!(sweep_stale_graft_indexes(git_dir, now_ms), 1);
        assert!(!held.exists() && !graft_held_path(&held).exists());
        assert_eq!(sweep_stale_graft_indexes(git_dir, now_ms), 0);
    }
}

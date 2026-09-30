//! `refs/rift/chain/v1` — the checkpoint chain's **order**, recorded in git.
//!
//! A chain ref was decided long before it was built. Everything downstream
//! behaved in the meantime as though an order existed: `to_shards` linked checkpoints by
//! **sorted content hash**, which has no relation to write time, so a newly
//! arrived checkpoint sorted into the middle about half the time and duplicated
//! an existing predecessor. Append-only forbids correcting the older row, so the
//! projection forked and `verify` — correctly — called it tamper. This module is
//! the missing order.
//!
//! # Shape
//!
//! One commit per checkpoint. Each entry's **parent is the previous entry**, so
//! the ref's own history *is* the order: monotonic by construction, never
//! rewritten, and containing no timestamp that anything is allowed to sort by.
//!
//! The checkpoint id rides a **commit trailer**, not a tree. That is the
//! treeless-metadata constraint from sync-format's remote-read section: a
//! consumer must recover the id without fetching trees or blobs, and a trailer
//! is part of the commit object itself. Every entry's tree is the empty tree, so
//! the ref costs one small commit per checkpoint and nothing else.
//!
//! ```text
//! rift-chain
//!
//! Rift-Chain-Checkpoint: <64-hex checkpoint id>
//! ```
//!
//! # What this module refuses to do
//!
//! It never rewrites a parent, never deletes an entry, and
//! [`init_from_branch`] refuses outright if the ref already exists —
//! append-only applies to the ref exactly as it applies to a canonical table.

use std::path::Path;
use std::process::Command;

use rift_core::{Result, RiftError};

/// The chain ref. A serialized artifact: its name is a fact other tools will
/// read, so it is versioned in the ref name and frozen once published.
pub const CHAIN_REF: &str = "refs/rift/chain/v1";

/// The trailer carrying an entry's checkpoint id.
pub const CHECKPOINT_TRAILER: &str = "Rift-Chain-Checkpoint";

/// The subject line of every entry. Brand-free and constant, so an entry is
/// recognizable without parsing.
const ENTRY_SUBJECT: &str = "rift-chain";

/// Git's empty tree — every entry's tree, because the payload is the trailer.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// A checkpoint id is a 64-char lowercase hex SHA-256. Validated before it ever
/// reaches an argv (the crate's argv-safety discipline).
fn is_checkpoint_id(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Ensure git's empty tree object exists in this repository.
fn write_empty_tree(git_dir: &Path) -> Result<()> {
    use std::process::Stdio;
    let child = Command::new("git")
        .arg("--git-dir")
        .arg(crate::child_process_path(git_dir))
        .arg("mktree")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| RiftError::substrate(format!("git spawn: {e}")))?;
    if child.success() {
        Ok(())
    } else {
        Err(RiftError::substrate("could not write the empty tree"))
    }
}

fn git(git_dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(crate::child_process_path(git_dir))
        .args(args)
        .output()
        .map_err(|e| RiftError::substrate(format!("git spawn: {e}")))?;
    if !out.status.success() {
        return Err(RiftError::substrate(format!(
            "git {} failed",
            args.first().copied().unwrap_or("?")
        )));
    }
    String::from_utf8(out.stdout).map_err(|_| RiftError::substrate("git output not utf-8"))
}

/// Whether the chain ref exists.
///
/// # Errors
/// Never — an unreadable ref is simply absent.
#[must_use]
pub fn exists(git_dir: &Path) -> bool {
    git(git_dir, &["rev-parse", "--verify", "--quiet", CHAIN_REF]).is_ok()
}

/// The chain ref's tip oid, or `None` when the ref does not exist yet.
///
/// # Errors
/// [`RiftError::Substrate`] on a git failure other than "no such ref".
pub fn tip(git_dir: &Path) -> Result<Option<String>> {
    match git(git_dir, &["rev-parse", "--verify", "--quiet", CHAIN_REF]) {
        Ok(s) => Ok(Some(s.trim().to_owned())),
        Err(_) => Ok(None),
    }
}

/// The checkpoint ids in chain order, **oldest first**.
///
/// This is *the* order. Nothing else derives one: not a timestamp, not a sort of
/// content hashes, not the arrival order of a rebuild.
///
/// # Errors
/// [`RiftError::Substrate`] on a read failure.
pub fn read_order(git_dir: &Path) -> Result<Vec<String>> {
    if !exists(git_dir) {
        return Ok(Vec::new());
    }
    // `%x01` delimits records; the body is read from the commit object only —
    // no tree, no blob (the treeless-metadata constraint).
    let raw = git(
        git_dir,
        &[
            "log",
            "--first-parent",
            "--reverse",
            "--format=%B%x01",
            CHAIN_REF,
        ],
    )?;
    let mut ids = Vec::new();
    for record in raw.split('\u{1}') {
        if record.trim().is_empty() {
            continue;
        }
        if let Some(id) = trailer_of(record) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Extract the checkpoint-id trailer from an entry's message body.
fn trailer_of(body: &str) -> Option<String> {
    let prefix = format!("{CHECKPOINT_TRAILER}:");
    for line in body.lines() {
        if let Some(rest) = line.trim().strip_prefix(&prefix) {
            let id = rest.trim();
            if is_checkpoint_id(id) {
                return Some(id.to_owned());
            }
        }
    }
    None
}

/// Append one checkpoint to the chain, returning the new entry's oid.
///
/// Compare-and-set on the ref: the update is rejected if another writer moved
/// the tip in between, so two concurrent workers cannot produce a fork. A
/// rejected append is an error the caller reports — never a silent skip, which
/// is how a missing entry would become permanent.
///
/// # Errors
/// [`RiftError::Validity`] if `checkpoint_id` is not a checkpoint id;
/// [`RiftError::Substrate`] on a git failure or a lost compare-and-set.
pub fn append(git_dir: &Path, checkpoint_id: &str, date_epoch: i64) -> Result<String> {
    let parent = tip(git_dir)?;
    append_onto(git_dir, parent.as_deref(), checkpoint_id, date_epoch)
}

/// Append one checkpoint on top of `expected_tip` — the tip the caller read
/// when it decided this entry was owed — returning the new entry's oid.
///
/// [`append`] guards only the instant between its own tip read and its ref
/// update. A caller that reads the order, decides what is missing, and only
/// then appends has a much wider window, and two such callers in two processes
/// would each append the same checkpoint: a fork is impossible, a duplicate
/// position is not. Pinning the parent to the tip the decision was made
/// against closes that window — a chain that moved since is refused, never
/// appended to.
///
/// # Errors
/// [`RiftError::Validity`] if `checkpoint_id` is not a checkpoint id or
/// `expected_tip` is not an object id; [`RiftError::Substrate`] on a git
/// failure or when the tip is no longer `expected_tip`.
pub fn append_onto(
    git_dir: &Path,
    expected_tip: Option<&str>,
    checkpoint_id: &str,
    date_epoch: i64,
) -> Result<String> {
    if !is_checkpoint_id(checkpoint_id) {
        return Err(RiftError::validity("not a checkpoint id"));
    }
    if let Some(oid) = expected_tip {
        crate::command::validate_oid(oid)?;
    }
    // `commit-tree` needs the empty tree to EXIST as an object. A repository
    // that has never written an empty tree does not have it, and the failure is
    // an opaque "commit failed" — found by the bottle, on the first append into
    // a fresh repo. `mktree` with empty input writes it and is idempotent.
    write_empty_tree(git_dir)?;
    let parent = expected_tip.map(str::to_owned);
    let message = format!("{ENTRY_SUBJECT}\n\n{CHECKPOINT_TRAILER}: {checkpoint_id}\n");

    // Fixed identity + caller-supplied date: entries are byte-reproducible, so a
    // backfill run twice over the same branch yields the same oids.
    // `@<epoch> +0000` is git's UNAMBIGUOUS raw-date form. A bare small integer
    // goes through git's natural-language date parser and can be rejected — the
    // bottle found it: a backfill dated 1_700_000_000 worked and one dated 1 did
    // not, with the same opaque "commit failed".
    let date = format!("@{date_epoch} +0000");
    let mut args: Vec<String> = vec!["commit-tree".into(), EMPTY_TREE.into()];
    if let Some(p) = &parent {
        args.push("-p".into());
        args.push(p.clone());
    }
    args.push("-m".into());
    args.push(message);
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();

    let out = Command::new("git")
        .arg("--git-dir")
        .arg(crate::child_process_path(git_dir))
        .args(&borrowed)
        .env("GIT_AUTHOR_NAME", "rift")
        .env("GIT_AUTHOR_EMAIL", "rift@local")
        .env("GIT_COMMITTER_NAME", "rift")
        .env("GIT_COMMITTER_EMAIL", "rift@local")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .map_err(|e| RiftError::substrate(format!("git spawn: {e}")))?;
    if !out.status.success() {
        return Err(RiftError::substrate("chain entry commit failed"));
    }
    let entry = String::from_utf8(out.stdout)
        .map_err(|_| RiftError::substrate("git output not utf-8"))?
        .trim()
        .to_owned();

    // Compare-and-set. `update-ref <ref> <new> <old>` fails if the tip moved;
    // the empty old-value form creates only when absent.
    let old = parent.unwrap_or_default();
    git(git_dir, &["update-ref", CHAIN_REF, &entry, &old])?;
    Ok(entry)
}

/// Checkpoint ids in the **checkpoint branch's own commit order** (first-parent,
/// oldest first) — the backfill witness.
///
/// # Errors
/// [`RiftError::Substrate`] on a read failure; [`RiftError::Integrity`] if the
/// branch graph is not linear, which must be reported and never invented around.
pub fn branch_order(git_dir: &Path, branch: &str) -> Result<Vec<String>> {
    if git(git_dir, &["rev-parse", "--verify", "--quiet", branch]).is_err() {
        return Ok(Vec::new());
    }
    if !git(git_dir, &["rev-list", "--merges", branch])?
        .trim()
        .is_empty()
    {
        return Err(RiftError::integrity(
            "checkpoint branch graph is not linear; refusing to infer an order",
        ));
    }
    // ONE process for the whole branch. This used to be a `diff-tree` per
    // commit, which is O(checkpoints) subprocesses: measured on this
    // repository's real 102-commit branch, the loop takes 2.50 s and this form
    // 0.12 s for a byte-identical 102-id sequence. That mattered the moment the
    // worker started consulting the witness on EVERY archive rather than only
    // at init.
    //
    // `%x01` delimits records, so each record is `<commit hex>` then the paths
    // that commit touched.
    let raw = git(
        git_dir,
        &[
            "log",
            "--first-parent",
            "--reverse",
            // `--root` was LOAD-BEARING in the `diff-tree` form this replaced:
            // measured on this repository's root checkpoint commit, `diff-tree`
            // reports 4 paths with it and 0 without, so the first checkpoint
            // ever written vanished silently at position zero. In the `log`
            // form it is INERT — 102 metadata lines with it and 102 without, on
            // git 2.50.1 — and removing it reddens no test. Kept as insurance
            // against a git that draws the line where `diff-tree` does; an
            // ARGUMENT, not a claim. Root inclusion itself IS pinned, by
            // `a_checkpoint_whose_hash_sorts_between_two_others_chains…`, whose
            // first checkpoint is the root commit.
            "--root",
            // Rename detection is ON by default for `git log` and OFF for the
            // `diff-tree` plumbing this replaced. Nothing on a checkpoint
            // branch is ever deleted, so no rename can be detected and this
            // changes no output today — it is here so a future shape that does
            // delete cannot quietly re-classify an added shard. Also an
            // argument, not a claim: no test reddens if it is removed.
            "--no-renames",
            "--name-only",
            "--format=%x01%H",
            branch,
        ],
    )?;
    let mut ids = Vec::new();
    for record in raw.split('\u{1}') {
        // Line 0 is the commit hex; the rest are its paths.
        for path in record.lines().skip(1) {
            let Some(dir) = path.strip_suffix("/metadata.json") else {
                continue;
            };
            let id: String = dir.split('/').collect();
            if is_checkpoint_id(&id) && !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    Ok(ids)
}

/// Build the chain ref once, from the checkpoint branch's own commit order.
///
/// **Why branch order is the honest witness:** each checkpoint commit was
/// parented on the branch tip as it stood when that checkpoint was written, so
/// the branch's first-parent walk *is* the write order — git recorded it all
/// along, and nothing else in the repository did. Content-hash sort is not a
/// witness to anything: it is an artifact of the hash function.
///
/// Refuses if the ref already exists. Append-only applies to the ref, and a
/// second "init" would be a rewrite wearing an initializer's name.
///
/// # Errors
/// [`RiftError::Immutability`] if the chain ref already exists;
/// [`RiftError::Integrity`] if the branch graph is non-linear;
/// [`RiftError::Substrate`] on a git failure.
pub fn init_from_branch(git_dir: &Path, branch: &str, date_epoch: i64) -> Result<usize> {
    if exists(git_dir) {
        return Err(RiftError::immutability(
            "chain ref already exists; refusing to re-initialize it",
        ));
    }
    let ids = branch_order(git_dir, branch)?;
    for id in &ids {
        append(git_dir, id, date_epoch)?;
    }
    Ok(ids.len())
}

/// Checkpoints present in `have` but absent from the chain, in `have` order —
/// the heal set for a worker whose checkpoint landed and whose chain entry did
/// not.
///
/// # Errors
/// [`RiftError::Substrate`] on a read failure.
pub fn missing_entries(git_dir: &Path, have: &[String]) -> Result<Vec<String>> {
    let order = read_order(git_dir)?;
    Ok(have
        .iter()
        .filter(|id| !order.contains(id))
        .cloned()
        .collect())
}

/// Chain entries naming a checkpoint that is **not** in `have`, in chain order
/// — the orphan direction, and the one `missing_entries` cannot see.
///
/// The two are not symmetric in what they mean. A checkpoint with no entry is
/// an order that has not caught up, and the next append heals it. An entry with
/// no checkpoint is an order referring to something the repository does not
/// have: the branch was rewritten, or this chain belongs to another repository.
/// Nothing heals that, so nothing here tries — it is named and reported.
///
/// # Errors
/// [`RiftError::Substrate`] on a read failure.
pub fn orphan_entries(git_dir: &Path, have: &[String]) -> Result<Vec<String>> {
    Ok(read_order(git_dir)?
        .into_iter()
        .filter(|id| !have.contains(id))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checkpoint_id_is_exactly_lowercase_64_hex() {
        assert!(is_checkpoint_id(&"a".repeat(64)));
        assert!(!is_checkpoint_id(&"A".repeat(64)));
        assert!(!is_checkpoint_id(&"a".repeat(63)));
        assert!(!is_checkpoint_id(&"a".repeat(65)));
        assert!(!is_checkpoint_id("not hex"));
        assert!(!is_checkpoint_id(""));
    }

    #[test]
    fn the_trailer_is_read_out_of_a_message_body_and_validated() {
        let id = "b".repeat(64);
        let body = format!("{ENTRY_SUBJECT}\n\n{CHECKPOINT_TRAILER}: {id}\n");
        assert_eq!(trailer_of(&body).as_deref(), Some(id.as_str()));
        // A body with no trailer, and one whose trailer is not an id.
        assert_eq!(trailer_of("rift-chain\n\nnothing here\n"), None);
        assert_eq!(
            trailer_of(&format!("x\n\n{CHECKPOINT_TRAILER}: not-an-id\n")),
            None
        );
    }

    /// The window `append_onto` exists to close: a writer that decided what
    /// was owed at one tip must not land on a chain another writer has moved.
    #[test]
    fn an_append_onto_a_tip_that_moved_is_refused_and_leaves_the_chain_alone() {
        let d = tempfile::tempdir().unwrap();
        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(d.path())
            .status()
            .unwrap();
        assert!(init.success());
        let git_dir = d.path().join(".git");
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let first = append(&git_dir, &a, 1_700_000_000).unwrap();

        let stale = append_onto(&git_dir, None, &b, 1_700_000_000);

        assert!(stale.is_err(), "a genesis over an existing chain landed");
        assert_eq!(tip(&git_dir).unwrap().as_deref(), Some(first.as_str()));
        assert_eq!(read_order(&git_dir).unwrap(), vec![a.clone()]);
        append_onto(&git_dir, Some(&first), &b, 1_700_000_000).unwrap();
        assert_eq!(read_order(&git_dir).unwrap(), vec![a, b]);
    }

    #[test]
    fn an_expected_tip_that_is_not_an_object_id_never_reaches_git() {
        let d = tempfile::tempdir().unwrap();
        let refused = append_onto(d.path(), Some("--upload-pack=x"), &"a".repeat(64), 1);
        assert!(
            matches!(refused, Err(RiftError::Validity(_))),
            "{refused:?}"
        );
    }

    /// The published names are a serialized artifact. Freezing them here means a
    /// rename has to be a decision, not a refactor.
    #[test]
    fn the_published_names_are_frozen() {
        assert_eq!(CHAIN_REF, "refs/rift/chain/v1");
        assert_eq!(CHECKPOINT_TRAILER, "Rift-Chain-Checkpoint");
        assert_eq!(EMPTY_TREE, "4b825dc642cb6eb9a060e54bf8d69288fbee4904");
    }
}

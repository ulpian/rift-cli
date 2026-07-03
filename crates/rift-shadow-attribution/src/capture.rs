//! Live diff-capture at the commit boundary (P4 B3).
//!
//! The pure engine in [`crate::diff`] takes byte pairs; this is the producer that
//! reads the **real** git objects at a commit boundary through the [`BlobReader`]
//! seam (no checkout, no working tree) and turns them into the durable
//! diff-capture facts: per changed file a content-addressed [`FileRevisionId`], the
//! committed-diff hunks (`C` = `diff(parent → commit)`), and a per-file
//! [`LineAttribution`].
//!
//! At the commit boundary alone there is no per-turn writer history, so every
//! changed line is `unknown` (honest — it lowers `capture_completeness`, so the
//! confidence is low until turn-boundary `H_i`/`A_i` snapshots enrich it). The
//! durable artifacts (`file_revision` + `changed_hunk`) are exact regardless, and
//! the persistence layer (in `rift-shadow`) maps this structure to append-only
//! rows. `gix` types never reach here — the reader yields hex ids + `Vec<u8>`.
//!
//! Turn-level agent/human splitting (`H_i`/`A_i` writer assignment) is the P4 S2
//! follow-up: it needs working-tree snapshots at turn boundaries plus a per-turn
//! actor relation, and is intentionally out of this commit-boundary producer.

use std::collections::BTreeMap;

use rift_core::{FileRevisionId, Result, Sha256Id};
use rift_git::BlobReader;
use sha2::{Digest, Sha256};

use crate::diff::{changed_hunk_id, diff_attributable, file_revision_id, Hunk};
use crate::{attribute, confidence, factors_from_signals, Actor, LineAttribution};

/// The attribution method version stamped on rows produced here (provenance).
pub const METHOD_VERSION: &str = "commit-diff@1";

/// One changed file in a commit's `C` diff: its head revision, the committed
/// hunks, and the (commit-boundary) attribution.
#[derive(Clone, Debug)]
pub struct FileChange {
    /// File path (from the tree root).
    pub path: String,
    /// The head blob oid (hex) this revision points at.
    pub head_blob_oid: String,
    /// Content-addressed `= sha256(path ‖ 0x00 ‖ head_blob_oid)`.
    pub head_revision: FileRevisionId,
    /// SHA-256 over the head blob bytes (the byte-compatible `content_sha256`).
    pub head_content_sha256: Sha256Id,
    /// The committed-diff hunks (`diff(parent → commit)` for this file).
    pub hunks: Vec<Hunk>,
    /// Per-file authorship counts (all `unknown` at the commit boundary).
    pub attribution: LineAttribution,
    /// Confidence in `[0,1]` (low at the commit boundary — capture is incomplete).
    pub confidence: f64,
    /// Whether the file was line-attributable (text + within the size cap).
    pub attributable: bool,
}

impl FileChange {
    /// The content-addressed `changed_hunk` id for `hunks[i]` under this revision.
    #[must_use]
    pub fn hunk_id(&self, commit_id: &str, hunk: &Hunk) -> Sha256Id {
        changed_hunk_id(
            commit_id,
            &self.head_revision,
            hunk.old_start,
            hunk.new_start,
        )
    }
}

/// The full `C`-diff capture for one commit: every file whose content changed
/// (or was added) relative to the first parent, in deterministic path order.
#[derive(Clone, Debug)]
pub struct CommitCapture {
    /// The commit oid (hex) this capture is for.
    pub commit_id: String,
    /// Changed/added files (path order).
    pub files: Vec<FileChange>,
}

fn sha256_of(bytes: &[u8]) -> Sha256Id {
    let mut h = Sha256::new();
    h.update(bytes);
    let mut b = [0u8; 32];
    b.copy_from_slice(&h.finalize());
    Sha256Id::from_bytes(b)
}

fn unknown_ratio(a: &LineAttribution) -> f64 {
    let total = a.total_committed();
    if total == 0 {
        1.0
    } else {
        f64::from(a.unknown_lines) / f64::from(total)
    }
}

/// Read a commit's tree as a `path → blob_oid` map.
fn tree_map(reader: &impl BlobReader, commit_hex: &str) -> Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for e in reader.tree_entries_at(commit_hex)? {
        map.insert(e.path, e.blob_oid);
    }
    Ok(map)
}

/// Capture the committed change `C = diff(parent → commit)` for `commit_hex`.
///
/// Reads the commit's first-parent tree and the commit tree through the
/// [`BlobReader`], diffs each file whose blob oid changed (or was newly added),
/// and returns the per-file revisions + hunks + a commit-boundary attribution.
/// A root commit (no parent) diffs every file against empty. Deterministic.
///
/// # Errors
/// [`rift_core::RiftError`] if a tree/blob read through the reader fails.
pub fn capture_commit(reader: &impl BlobReader, commit_hex: &str) -> Result<CommitCapture> {
    let head = tree_map(reader, commit_hex)?;
    let base = match reader.parent_of(commit_hex)? {
        Some(parent) => tree_map(reader, &parent)?,
        None => BTreeMap::new(),
    };

    let mut files = Vec::new();
    for (path, head_oid) in &head {
        // Unchanged in this commit (same blob oid) → not part of `C`.
        if base.get(path) == Some(head_oid) {
            continue;
        }
        let new_bytes = reader.read_blob(head_oid)?.unwrap_or_default();
        let old_bytes = match base.get(path) {
            Some(old_oid) => reader.read_blob(old_oid)?.unwrap_or_default(),
            None => Vec::new(), // added file: diff against empty
        };

        // Binary/oversize on either side → non-attributable (no bogus diff).
        let (hunks, attributable) = match diff_attributable(&old_bytes, &new_bytes) {
            Some(h) => (h, true),
            None => (Vec::new(), false),
        };

        // At the commit boundary there is no writer history: each new line is
        // `unknown` (one empty writer list per added/replaced line).
        let changed_new_lines: u32 = hunks.iter().map(|h| h.new_len).sum();
        let writers: Vec<Vec<Actor>> = vec![Vec::new(); changed_new_lines as usize];
        let attribution = attribute(&writers, 0);
        let factors = factors_from_signals(unknown_ratio(&attribution), false, false, false, true);

        files.push(FileChange {
            path: path.clone(),
            head_blob_oid: head_oid.clone(),
            head_revision: file_revision_id(path, head_oid.as_bytes()),
            head_content_sha256: sha256_of(&new_bytes),
            hunks,
            attribution,
            confidence: confidence(&factors),
            attributable,
        });
    }
    Ok(CommitCapture {
        commit_id: commit_hex.to_owned(),
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_git::{MemoryBlobReader, TreeEntry};

    fn entry(path: &str, oid: &str) -> TreeEntry {
        TreeEntry {
            path: path.into(),
            blob_oid: oid.into(),
        }
    }

    /// parent c0: a.txt@A0="one\ntwo\n", keep.txt@K="stable\n"
    /// head   c1: a.txt@A1="one\nTWO\n" (changed), new.txt@N="brand\n" (added),
    ///            keep.txt@K (unchanged), bin@B (binary, added)
    fn reader() -> MemoryBlobReader {
        MemoryBlobReader::new()
            .with_blob("A0", b"one\ntwo\n")
            .with_blob("A1", b"one\nTWO\n")
            .with_blob("N", b"brand\n")
            .with_blob("K", b"stable\n")
            .with_blob("B", b"bin\x00ary")
            .with_tree("c0", vec![entry("a.txt", "A0"), entry("keep.txt", "K")])
            .with_tree(
                "c1",
                vec![
                    entry("a.txt", "A1"),
                    entry("new.txt", "N"),
                    entry("keep.txt", "K"),
                    entry("bin", "B"),
                ],
            )
            .with_parent("c1", "c0")
    }

    #[test]
    fn captures_changed_and_added_skips_unchanged() {
        let cap = capture_commit(&reader(), "c1").unwrap();
        assert_eq!(cap.commit_id, "c1");
        let paths: Vec<&str> = cap.files.iter().map(|f| f.path.as_str()).collect();
        // keep.txt is unchanged (same oid) → excluded; bin/new.txt/a.txt included.
        assert_eq!(paths, vec!["a.txt", "bin", "new.txt"]);
    }

    #[test]
    fn changed_file_has_one_hunk_and_unknown_attribution() {
        let cap = capture_commit(&reader(), "c1").unwrap();
        let a = cap.files.iter().find(|f| f.path == "a.txt").unwrap();
        assert_eq!(a.hunks.len(), 1, "single line changed");
        assert!(a.attributable);
        // commit-boundary: the changed line is unknown (no turn writer history).
        assert_eq!(a.attribution.unknown_lines, 1);
        assert_eq!(a.attribution.agent_lines, 0);
        assert_eq!(a.attribution.human_lines, 0);
        // low confidence: nothing is explained by captured agent/human edits.
        assert!(a.confidence.abs() < 1e-9);
        // content-addressed revision + content hash are exact.
        assert_eq!(a.head_revision, file_revision_id("a.txt", b"A1"));
        assert_eq!(a.head_content_sha256, sha256_of(b"one\nTWO\n"));
    }

    #[test]
    fn added_file_diffs_against_empty() {
        let cap = capture_commit(&reader(), "c1").unwrap();
        let n = cap.files.iter().find(|f| f.path == "new.txt").unwrap();
        assert_eq!(n.hunks.len(), 1);
        assert_eq!(n.hunks[0].new_len, 1, "one inserted line");
        assert_eq!(n.hunks[0].old_len, 0, "added: nothing on the old side");
    }

    #[test]
    fn binary_file_is_non_attributable_no_hunks() {
        let cap = capture_commit(&reader(), "c1").unwrap();
        let b = cap.files.iter().find(|f| f.path == "bin").unwrap();
        assert!(!b.attributable);
        assert!(b.hunks.is_empty(), "binary is never diffed");
    }

    #[test]
    fn root_commit_diffs_everything_against_empty() {
        // c0 has no parent → every file is an addition.
        let cap = capture_commit(&reader(), "c0").unwrap();
        let paths: Vec<&str> = cap.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "keep.txt"]);
        for f in &cap.files {
            assert!(f.hunks.iter().all(|h| h.old_len == 0), "all additions");
        }
    }

    #[test]
    fn re_capture_is_deterministic() {
        let r = reader();
        let a = capture_commit(&r, "c1").unwrap();
        let b = capture_commit(&r, "c1").unwrap();
        let key = |c: &CommitCapture| {
            c.files
                .iter()
                .map(|f| (f.path.clone(), f.head_revision, f.hunks.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(key(&a), key(&b));
    }

    #[test]
    fn hunk_id_is_stable() {
        let cap = capture_commit(&reader(), "c1").unwrap();
        let a = cap.files.iter().find(|f| f.path == "a.txt").unwrap();
        let h = &a.hunks[0];
        assert_eq!(a.hunk_id("c1", h), a.hunk_id("c1", h));
        assert_eq!(
            a.hunk_id("c1", h),
            changed_hunk_id("c1", &a.head_revision, h.old_start, h.new_start)
        );
    }
}

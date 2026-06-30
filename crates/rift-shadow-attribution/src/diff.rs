//! Diff capture points (P4 S1) — the clean inputs line attribution rests on.
//!
//! Attribution is computed over three families of diffs between content-addressed
//! [`rift_core::FileRevisionId`] snapshots taken at session boundaries:
//!
//! * **`H_i`** — human edits *between* agent turns: `diff(post_t{i-1} → pre_t{i})`.
//! * **`A_i`** — agent edits *during* turn `i`:        `diff(pre_t{i} → post_t{i})`.
//! * **`C`**   — the net committed change:             `diff(commit_base → commit)`.
//!
//! This module is the **pure diff engine** + content-addressed keys: line hunks via
//! `imara-diff` over two byte snapshots (no working-tree checkout — the bytes are
//! read elsewhere through the `gix` seam), plus the binary/oversize guard so a
//! pathological file degrades to "non-attributable" rather than panicking or being
//! mis-credited. Which snapshots map to `H_i`/`A_i`/`C` is the capture lifecycle's
//! job (turn boundaries / the commit hook); this engine just diffs a pair.

use std::ops::Range;

use imara_diff::intern::InternedInput;
use imara_diff::sources::byte_lines;
use imara_diff::{diff, Algorithm, Sink};
use rift_core::{FileRevisionId, Sha256Id};
use sha2::{Digest, Sha256};

/// Files larger than this are treated as non-attributable (skipped, never diffed).
pub const MAX_DIFF_BYTES: usize = 5 * 1024 * 1024;

/// One changed line-region: half-open line ranges in the old and new revisions
/// (0-based line indices, `*_len == 0` denotes a pure insertion/deletion point).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
pub struct Hunk {
    /// First changed line in the old revision.
    pub old_start: u32,
    /// Number of old lines replaced/removed.
    pub old_len: u32,
    /// First changed line in the new revision.
    pub new_start: u32,
    /// Number of new lines added/replacing.
    pub new_len: u32,
}

struct HunkSink {
    hunks: Vec<Hunk>,
}

impl Sink for HunkSink {
    type Out = Vec<Hunk>;
    fn process_change(&mut self, before: Range<u32>, after: Range<u32>) {
        self.hunks.push(Hunk {
            old_start: before.start,
            old_len: before.end - before.start,
            new_start: after.start,
            new_len: after.end - after.start,
        });
    }
    fn finish(self) -> Self::Out {
        self.hunks
    }
}

/// Whether `content` can be line-attributed: text (no NUL byte) and within the
/// size cap. Binary or oversize content is non-attributable.
#[must_use]
pub fn is_attributable(content: &[u8]) -> bool {
    content.len() <= MAX_DIFF_BYTES && !content.contains(&0)
}

/// Line hunks between two byte snapshots (histogram diff over line tokens).
/// Identical content yields no hunks. Pure and deterministic.
#[must_use]
pub fn diff_lines(old: &[u8], new: &[u8]) -> Vec<Hunk> {
    let input = InternedInput::new(byte_lines(old), byte_lines(new));
    diff(Algorithm::Histogram, &input, HunkSink { hunks: Vec::new() })
}

/// Diff only when **both** sides are attributable (text + within cap); a binary or
/// oversize side returns `None` so the caller records the file as non-attributable
/// (lowering confidence, P4 S3) rather than crediting a bogus diff.
#[must_use]
pub fn diff_attributable(old: &[u8], new: &[u8]) -> Option<Vec<Hunk>> {
    if is_attributable(old) && is_attributable(new) {
        Some(diff_lines(old, new))
    } else {
        None
    }
}

/// The content-addressed `FileRevision` id: `sha256(path ‖ 0x00 ‖ blob_oid)`
/// (db-design §4.4). Deterministic, so re-capturing identical state is idempotent.
#[must_use]
pub fn file_revision_id(path: &str, blob_oid: &[u8]) -> FileRevisionId {
    let mut h = Sha256::new();
    h.update(path.as_bytes());
    h.update([0u8]);
    h.update(blob_oid);
    let out = h.finalize();
    let mut b = [0u8; 32];
    b.copy_from_slice(&out);
    FileRevisionId::new(Sha256Id::from_bytes(b))
}

/// The content-addressed `changed_hunk` id (P4 S1 schema):
/// `sha256(commit_id ‖ file_revision ‖ old_start ‖ new_start)` — re-diffing the
/// same commit/file is idempotent (a no-op insert).
#[must_use]
pub fn changed_hunk_id(
    commit_id: &str,
    file_revision: &FileRevisionId,
    old_start: u32,
    new_start: u32,
) -> Sha256Id {
    let mut h = Sha256::new();
    h.update(commit_id.as_bytes());
    h.update([0u8]);
    h.update(file_revision.sha256().as_bytes());
    h.update(old_start.to_le_bytes());
    h.update(new_start.to_le_bytes());
    let out = h.finalize();
    let mut b = [0u8; 32];
    b.copy_from_slice(&out);
    Sha256Id::from_bytes(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_content_has_no_hunks() {
        assert!(diff_lines(b"a\nb\nc\n", b"a\nb\nc\n").is_empty());
    }

    #[test]
    fn single_line_change_is_one_hunk() {
        let hunks = diff_lines(b"a\nb\nc\n", b"a\nX\nc\n");
        assert_eq!(hunks.len(), 1);
        let h = hunks[0];
        // line index 1 (the "b"→"X" line) changed on both sides
        assert_eq!((h.old_start, h.old_len), (1, 1));
        assert_eq!((h.new_start, h.new_len), (1, 1));
    }

    #[test]
    fn insertion_and_deletion_detected() {
        // pure insertion
        let ins = diff_lines(b"a\nc\n", b"a\nb\nc\n");
        assert_eq!(ins.len(), 1);
        assert_eq!(ins[0].new_len, 1);
        // pure deletion
        let del = diff_lines(b"a\nb\nc\n", b"a\nc\n");
        assert_eq!(del.len(), 1);
        assert_eq!(del[0].old_len, 1);
    }

    #[test]
    fn binary_and_oversize_are_non_attributable() {
        assert!(!is_attributable(b"text\x00with nul"));
        assert!(is_attributable(b"plain text"));
        assert!(diff_attributable(b"a\x00b", b"c").is_none());
        assert!(diff_attributable(b"a\n", b"b\n").is_some());
        let big = vec![b'a'; MAX_DIFF_BYTES + 1];
        assert!(!is_attributable(&big));
    }

    #[test]
    fn file_revision_id_is_deterministic_and_content_addressed() {
        let a = file_revision_id("src/lib.rs", b"oid1");
        let b = file_revision_id("src/lib.rs", b"oid1");
        let c = file_revision_id("src/lib.rs", b"oid2");
        let d = file_revision_id("src/main.rs", b"oid1");
        assert_eq!(a, b);
        assert_ne!(a, c, "different oid → different id");
        assert_ne!(a, d, "different path → different id");
    }

    #[test]
    fn changed_hunk_id_is_deterministic() {
        let rev = file_revision_id("f", b"oid");
        let x = changed_hunk_id("commitA", &rev, 1, 1);
        let y = changed_hunk_id("commitA", &rev, 1, 1);
        let z = changed_hunk_id("commitA", &rev, 2, 1);
        assert_eq!(x, y);
        assert_ne!(x, z);
    }

    proptest::proptest! {
        /// Never panics on arbitrary byte pairs (untrusted file content).
        #[test]
        fn diff_never_panics(
            a in proptest::collection::vec(proptest::num::u8::ANY, 0..512),
            b in proptest::collection::vec(proptest::num::u8::ANY, 0..512),
        ) {
            let _ = diff_attributable(&a, &b);
        }
    }
}

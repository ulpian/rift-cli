//! Resolving a handle to the exact bytes it names, never the current file at
//! its path: a content-addressed blob seam and the self-validating resolver.

use rift_recall_types::{Handle, LineRange, OmitReason};

use crate::summary::{sha256_hex, ValidSummary, Validity};

/// A seam onto immutable, content-addressed bytes — Git blobs by content hash
/// and captured tool results by id. Kept minimal (no `gix`) so validity stays
/// dependency-light; `rift-recall-ledger` implements it over the substrate.
pub trait BlobResolver {
    /// Candidate bytes for the file revision named by `(path, content_sha256)`.
    /// Looked up **by content**, never by reading the current path off disk;
    /// [`resolve_handle`] still self-validates the returned bytes.
    fn file_blob(&self, path: &str, content_sha256: &str) -> Option<Vec<u8>>;
    /// The captured tool-result bytes for a `tool_use_id`.
    fn tool_result(&self, tool_use_id: &str) -> Option<Vec<u8>>;
}

/// What resolving a handle yields. `Unavailable` carries the reason; it is never
/// "the current file at that path".
pub enum Resolved {
    /// Exact bytes (a Git blob or a captured tool result), range-sliced.
    Bytes(Vec<u8>),
    /// A validity-checked summary (from a `summary:` handle).
    Summary(ValidSummary),
    /// The named object is gone / stale / a hash mismatch — never wrong bytes.
    Unavailable(OmitReason),
}

/// Slice `bytes` to a 1-based inclusive line range (lines split on `\n`, keeping
/// terminators so concatenation round-trips).
fn slice_lines(bytes: &[u8], range: LineRange) -> Vec<u8> {
    let mut lines: Vec<&[u8]> = Vec::new();
    let mut start = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' {
            if let Some(line) = bytes.get(start..=i) {
                lines.push(line);
            }
            start = i + 1;
        }
    }
    if let Some(tail) = bytes.get(start..) {
        if !tail.is_empty() {
            lines.push(tail);
        }
    }
    let lo = (range.lo.max(1) as usize) - 1;
    let hi = range.hi as usize; // inclusive → exclusive upper after clamp
    let mut out = Vec::new();
    for l in lines.get(lo..hi.min(lines.len())).unwrap_or(&[]) {
        out.extend_from_slice(l);
    }
    out
}

/// Resolve a [`Handle`] to its **exact named bytes** (or `Unavailable`) — never
/// the current file at the path. `file:` resolves by content hash and
/// **self-validates** (resolved bytes must hash to the handle's `sha256`, else
/// `Unavailable` — a hostile/path-traversal handle can never read arbitrary
/// content); `log:` by captured-result id; `summary:` through `resolve_summary`
/// (a stale summary handle yields `Unavailable`, never bytes).
pub fn resolve_handle(
    h: &Handle,
    blobs: &dyn BlobResolver,
    resolve_summary: impl FnOnce(&str) -> Validity,
) -> Resolved {
    match h {
        Handle::File {
            path,
            sha256,
            range,
        } => {
            let Some(bytes) = blobs.file_blob(path, sha256) else {
                return Resolved::Unavailable(OmitReason::SourceDeletedOrRedacted);
            };
            if &sha256_hex(&bytes) != sha256 {
                // self-validation: never serve bytes that don't match the handle
                return Resolved::Unavailable(OmitReason::SourceDeletedOrRedacted);
            }
            match range {
                Some(r) => Resolved::Bytes(slice_lines(&bytes, *r)),
                None => Resolved::Bytes(bytes),
            }
        }
        Handle::Log { tool_use_id } => match blobs.tool_result(tool_use_id) {
            Some(bytes) => Resolved::Bytes(bytes),
            None => Resolved::Unavailable(OmitReason::SourceDeletedOrRedacted),
        },
        Handle::Summary { key } => match resolve_summary(key) {
            Validity::Valid(s) => Resolved::Summary(s),
            Validity::Invalid { .. } => {
                Resolved::Unavailable(OmitReason::SummaryInvalidSourceChanged)
            }
            Validity::Degraded { .. } => Resolved::Unavailable(OmitReason::SourceDeletedOrRedacted),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::summary::tests::versions;
    use crate::summary::{resolve, MemoryDigestStore, SummaryStore, WorktreeSnapshot};

    /// A test resolver keyed by content hash (never by disk path).
    struct MapResolver {
        files: BTreeMap<(String, String), Vec<u8>>,
        logs: BTreeMap<String, Vec<u8>>,
    }
    impl BlobResolver for MapResolver {
        fn file_blob(&self, path: &str, content_sha256: &str) -> Option<Vec<u8>> {
            self.files
                .get(&(path.to_string(), content_sha256.to_string()))
                .cloned()
        }
        fn tool_result(&self, tool_use_id: &str) -> Option<Vec<u8>> {
            self.logs.get(tool_use_id).cloned()
        }
    }

    #[test]
    fn file_handle_resolves_by_hash_not_path() {
        let bytes = b"line1\nline2\n".to_vec();
        let sha = sha256_hex(&bytes);
        let mut files = BTreeMap::new();
        files.insert(("a.rs".to_string(), sha.clone()), bytes.clone());
        let r = MapResolver {
            files,
            logs: BTreeMap::new(),
        };
        // resolving the OLD hash yields the OLD bytes regardless of disk state
        let h = Handle::File {
            path: "a.rs".into(),
            sha256: sha,
            range: None,
        };
        match resolve_handle(&h, &r, |_| Validity::Degraded { key: String::new() }) {
            Resolved::Bytes(b) => assert_eq!(b, bytes),
            _ => panic!("expected the named bytes"),
        }
        // a different (new) hash the resolver doesn't hold → Unavailable, never disk
        let h2 = Handle::File {
            path: "a.rs".into(),
            sha256: "newhash".into(),
            range: None,
        };
        assert!(matches!(
            resolve_handle(&h2, &r, |_| Validity::Degraded { key: String::new() }),
            Resolved::Unavailable(_)
        ));
    }

    #[test]
    fn hash_mismatch_yields_unavailable() {
        // resolver returns bytes that do NOT hash to the handle's sha → Unavailable
        let mut files = BTreeMap::new();
        files.insert(
            ("a.rs".to_string(), "claimed".to_string()),
            b"actually different".to_vec(),
        );
        let r = MapResolver {
            files,
            logs: BTreeMap::new(),
        };
        let h = Handle::File {
            path: "a.rs".into(),
            sha256: "claimed".into(),
            range: None,
        };
        assert!(matches!(
            resolve_handle(&h, &r, |_| Validity::Degraded { key: String::new() }),
            Resolved::Unavailable(_)
        ));
    }

    #[test]
    fn resolves_summary_handle_through_validity() {
        let store = SummaryStore::new();
        let dg = MemoryDigestStore::default();
        let source = b"fn x() {}\n";
        let key = store.put_summary(&dg, "x.rs", source, versions(), b"digest");
        let mut snap = WorktreeSnapshot::new();
        snap.insert("x.rs".to_string(), sha256_hex(source));
        let r = MapResolver {
            files: BTreeMap::new(),
            logs: BTreeMap::new(),
        };
        // current source → Valid summary returned
        let h = Handle::Summary { key: key.clone() };
        match resolve_handle(&h, &r, |k| resolve(&store, &dg, &snap, k)) {
            Resolved::Summary(s) => assert_eq!(s.digest(), b"digest"),
            _ => panic!("expected a valid summary"),
        }
        // changed source → Unavailable (never bytes)
        let mut changed = WorktreeSnapshot::new();
        changed.insert("x.rs".to_string(), "different".to_string());
        assert!(matches!(
            resolve_handle(&h, &r, |k| resolve(&store, &dg, &changed, k)),
            Resolved::Unavailable(_)
        ));
    }

    // --- line slicing: the one place validity handles multi-line bytes -------

    #[test]
    fn slicing_a_range_of_lines_keeps_terminators_so_slices_concatenate() {
        // The docstring's promise is a PARTITION promise, and the only way it
        // can be wrong is by dropping or duplicating a `\n`. A file whose last
        // line has no terminator is the case that catches an off-by-one.
        let bytes = b"one\ntwo\nthree".to_vec();
        let whole = slice_lines(&bytes, LineRange { lo: 1, hi: 3 });
        assert_eq!(whole, bytes, "the full range is the input, byte for byte");
        for cut in 0..=3_u32 {
            let mut joined = slice_lines(&bytes, LineRange { lo: 1, hi: cut });
            joined.extend(slice_lines(&bytes, LineRange { lo: cut + 1, hi: 3 }));
            assert_eq!(
                joined, bytes,
                "split after line {cut} lost or duplicated bytes"
            );
        }
    }

    #[test]
    fn a_range_outside_the_file_clamps_rather_than_panicking_or_inventing() {
        let bytes = b"one\ntwo\n".to_vec();
        // Past the end: clamp to what exists.
        assert_eq!(slice_lines(&bytes, LineRange { lo: 1, hi: 99 }), bytes);
        // Fully past the end: empty, never a wrap-around slice.
        assert_eq!(slice_lines(&bytes, LineRange { lo: 50, hi: 99 }), b"");
        // Inverted range: empty, not a panic.
        assert_eq!(slice_lines(&bytes, LineRange { lo: 2, hi: 1 }), b"");
        // 0 is not a line number; it must not underflow into the last line.
        assert_eq!(slice_lines(&bytes, LineRange { lo: 0, hi: 1 }), b"one\n");
        assert_eq!(slice_lines(b"", LineRange { lo: 1, hi: 1 }), b"");
        // A file that is one bare newline is one line, and it survives.
        assert_eq!(slice_lines(b"\n", LineRange { lo: 1, hi: 1 }), b"\n");
    }
}

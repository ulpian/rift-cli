//! The recall-side half of a delta's provenance: which checkpoints bracket a
//! change, and which commit anchored one.
//!
//! Everything here is a read over the recall projection's own graph — the
//! `n_checkpoint_commit` mirror and the `n_changed_hunk` payload — and nothing
//! here joins a capture table. The capture-and-git half arrives through
//! [`ProvenanceOracle`](rift_recall_types::ProvenanceOracle); the two are kept
//! apart because the two projections rebuild independently.
//!
//! The checkpoint a revision belongs to is **not** re-queried here. The
//! candidate loader already picks it, in scope, to decide recency, and it now
//! carries it ([`LoadedCandidate::checkpoint`](crate::candidates::LoadedCandidate)):
//! one row, one answer, no chance of two queries disagreeing about which
//! checkpoint recorded a revision.

use std::collections::BTreeMap;

use rift_core::Result;
use rift_store::{CanonicalStore, Value};

use crate::graph::file_revision_id;

/// The anchor commits of a set of checkpoints, keyed by checkpoint id.
///
/// One query for the whole set rather than one per checkpoint: a manifest
/// names up to 64 files and the relation is one row per checkpoint, so a
/// per-entry round trip would pay 64 times for a table that fits in a page.
/// Checkpoints with no mirrored commit simply have no entry — absence reads as
/// *not committed here*, never as a guess.
///
/// # Errors
/// Propagates store read errors.
pub async fn commit_oids(
    store: &impl CanonicalStore,
    ids: &[String],
) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let wanted: std::collections::BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    for r in store
        .query("SELECT node_id, commit_oid FROM n_checkpoint_commit", &[])
        .await?
    {
        let (node, oid) = (r.get_text(0)?, r.get_text(1)?);
        if wanted.contains(node.as_str()) && !oid.is_empty() {
            out.insert(node, oid);
        }
    }
    Ok(out)
}

/// One persisted changed hunk, in the numbering of the revision it lands in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HunkOrigin {
    /// First changed line in the new revision, **1-based** (the graph stores
    /// the 0-based index the differ emits; it is raised here so every line
    /// number that leaves this crate is in the numbering a reader sees).
    pub new_lo: u32,
    /// Last changed line in the new revision, inclusive and 1-based. Equal to
    /// `new_lo - 1` would be an empty range, so a zero-length hunk is dropped
    /// by the reader rather than rendered as a place to look.
    pub new_hi: u32,
}

/// The checkpoint each persisted hunk of `path@content_sha256` was recorded
/// in, in ascending line order.
///
/// The revision is addressed the way the graph keys it — `sha256(path ‖ hash)`
/// — so a hunk can only ever be attributed to the file it was computed for.
/// Binary hunks are skipped: a whole-file marker is not a place to look.
///
/// # Errors
/// Propagates store read errors.
pub async fn hunk_origins(
    store: &impl CanonicalStore,
    path: &str,
    content_sha256: &str,
) -> Result<Vec<(HunkOrigin, String)>> {
    let revision = file_revision_id(path, content_sha256);
    let mut out = Vec::new();
    for r in store
        .query(
            "SELECT commit_id, new_start, new_len, is_binary FROM n_changed_hunk \
             WHERE file_revision = ?",
            &[Value::text(revision)],
        )
        .await?
    {
        if r.get_i64(3)? != 0 {
            continue;
        }
        let (Ok(start), Ok(len)) = (r.get_i64(1), r.get_i64(2)) else {
            continue;
        };
        let (Ok(lo), Ok(len)) = (u32::try_from(start), u32::try_from(len)) else {
            continue;
        };
        if len == 0 {
            continue;
        }
        let lo = lo.saturating_add(1);
        out.push((
            HunkOrigin {
                new_lo: lo,
                new_hi: lo.saturating_add(len).saturating_sub(1),
            },
            r.get_text(0)?,
        ));
    }
    out.sort_by_key(|(h, _)| (h.new_lo, h.new_hi));
    Ok(out)
}

/// The checkpoint that recorded the one persisted hunk covering `lo..=hi`, or
/// `None`.
///
/// **Exactly one, or nothing.** A window that overlaps two recorded hunks was
/// not established by either of them alone, and naming one of the two would
/// attribute a line to a change that did not make it. That is the same
/// conservative direction the delta frame takes when no site survives its
/// clamp: an attribution Rift cannot prove is absent, not approximate.
#[must_use]
pub fn origin_of(origins: &[(HunkOrigin, String)], lo: u32, hi: u32) -> Option<&str> {
    let mut hit = None;
    for (h, ck) in origins {
        if h.new_lo <= hi && lo <= h.new_hi {
            if hit.is_some() {
                return None;
            }
            hit = Some(ck.as_str());
        }
    }
    hit
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_store::{recall_migrations, SqliteStore};

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&s, &recall_migrations()).unwrap();
        s
    }

    fn put_hunk(s: &SqliteStore, id: &str, ck: &str, rev: &str, start: i64, len: i64, bin: i64) {
        block(s.insert_or_ignore(
            "n_changed_hunk",
            &[
                "node_id",
                "commit_id",
                "file_revision",
                "old_start",
                "old_len",
                "new_start",
                "new_len",
                "is_binary",
            ],
            &[
                Value::text(id),
                Value::text(ck),
                Value::text(rev),
                Value::Int(0),
                Value::Int(0),
                Value::Int(start),
                Value::Int(len),
                Value::Int(bin),
            ],
        ))
        .unwrap();
    }

    /// The graph's 0-based start becomes the 1-based range a reader sees, and
    /// a binary marker is not a place to look.
    #[test]
    fn hunks_come_back_in_reader_numbering_and_binaries_do_not() {
        let s = store();
        let rev = file_revision_id("src/a.rs", &"a".repeat(64));
        put_hunk(&s, "h1", "ck-1", &rev, 11, 7, 0);
        put_hunk(&s, "h2", "ck-2", &rev, 0, 3, 1);
        put_hunk(&s, "h3", "ck-3", &rev, 40, 0, 0);
        let got = block(hunk_origins(&s, "src/a.rs", &"a".repeat(64))).unwrap();
        assert_eq!(
            got,
            vec![(
                HunkOrigin {
                    new_lo: 12,
                    new_hi: 18
                },
                "ck-1".to_owned()
            )]
        );
    }

    /// A revision is addressed by path AND hash, so another file's hunks are
    /// not reachable by naming this one.
    #[test]
    fn a_revision_of_another_path_is_not_this_ones_origin() {
        let s = store();
        let other = file_revision_id("src/b.rs", &"a".repeat(64));
        put_hunk(&s, "h1", "ck-1", &other, 1, 2, 0);
        assert!(block(hunk_origins(&s, "src/a.rs", &"a".repeat(64)))
            .unwrap()
            .is_empty());
    }

    /// One overlapping hunk attributes; two attribute nothing.
    #[test]
    fn an_ambiguous_window_names_no_checkpoint() {
        let origins = vec![
            (
                HunkOrigin {
                    new_lo: 10,
                    new_hi: 20,
                },
                "ck-a".to_owned(),
            ),
            (
                HunkOrigin {
                    new_lo: 30,
                    new_hi: 40,
                },
                "ck-b".to_owned(),
            ),
        ];
        assert_eq!(origin_of(&origins, 12, 15), Some("ck-a"));
        assert_eq!(origin_of(&origins, 35, 50), Some("ck-b"));
        assert_eq!(origin_of(&origins, 15, 35), None);
        assert_eq!(origin_of(&origins, 21, 29), None);
        assert_eq!(origin_of(&[], 1, 2), None);
    }

    /// The commit mirror answers for the ids asked about and no others.
    #[test]
    fn only_the_asked_checkpoints_commits_come_back() {
        let s = store();
        for (node, oid) in [("ck-1", "abc123"), ("ck-2", "def456"), ("ck-3", "")] {
            block(s.insert_or_ignore(
                "n_checkpoint_commit",
                &["node_id", "commit_oid", "schema_version"],
                &[Value::text(node), Value::text(oid), Value::Int(1)],
            ))
            .unwrap();
        }
        let got = block(commit_oids(
            &s,
            &["ck-1".to_owned(), "ck-3".to_owned(), "ck-9".to_owned()],
        ))
        .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got.get("ck-1").map(String::as_str), Some("abc123"));
        assert!(block(commit_oids(&s, &[])).unwrap().is_empty());
    }
}

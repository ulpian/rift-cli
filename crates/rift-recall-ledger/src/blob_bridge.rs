//! Bridging a recorded revision to the git object that already holds its bytes.
//!
//! A read is recorded as a path and a SHA-256 of the bytes the agent was given.
//! A basis is a blob in git's object store. Nothing joins the two until
//! something proves that some blob's bytes hash to the recorded address — and
//! that proof is the whole of this module.
//!
//! The route is deliberately one-directional. We never write the bytes we were
//! handed at read time anywhere: the redaction gate is the only path to durable
//! storage, so a basis exists because *git already has it*, not because Rift
//! kept a copy. What is persisted is three strings — the content address, the
//! object id, the path — into the append-only `n_blob_oid_map` the exact
//! resolvers already read.
//!
//! What that leaves unbridgeable is the honest half of the answer: bytes that
//! were read and then never committed (an uncommitted edit, a scratch file) are
//! in no object store, and the revision stays unavailable. That count is
//! returned rather than hidden, because a basis-availability rate is the thing
//! the pull verb's usefulness is actually made of.
//!
//! **Opens a store and walks history**, so it belongs to the ingest pass the
//! worker runs and to nothing on a tool-call gate path.

use std::collections::{BTreeMap, BTreeSet};

use rift_core::Result;
use rift_recall_substrate::worktree::{sha256_hex, WorktreeBlob};
use rift_recall_substrate::SubstrateReader;
use rift_store::CanonicalStore;

/// How far back the candidate walk goes: the last sixty-four commits of the
/// checked-out line, first-parent. Deep enough to cover the sessions a
/// projection is still serving deltas for, shallow enough that a first ingest
/// on a long history stays a bounded cost. A revision older than this is not
/// found, which costs a saving and never names wrong bytes.
pub const MAX_HISTORY_COMMITS: usize = 64;

/// How many distinct paths one pass will walk history for. The unmapped set
/// shrinks monotonically (the map is append-only), so a repository with more
/// than this many unbridged paths finishes over consecutive passes rather than
/// paying for all of them at once.
pub const MAX_BRIDGED_PATHS: usize = 64;

/// The largest blob this will hash. A basis past it could not be diffed
/// anyway — the diff refuses at the same ceiling — so reading it would be work
/// spent to learn nothing.
pub const MAX_BASIS_BYTES: usize = 1024 * 1024;

/// What one bridging pass found. `wanted + 0 = mapped + unavailable` by
/// construction: every recorded revision the pass looked at is in exactly one
/// of the two.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BridgeReport {
    /// Recorded revisions that had no blob row when the pass started.
    pub wanted: usize,
    /// Of those, the ones git turned out to hold, now mapped.
    pub mapped: usize,
    /// Of those, the ones no candidate blob matched — uncommitted bytes.
    pub basis_unavailable: usize,
}

/// Every `(path, content_sha256)` a read recorded that the blob map does not
/// yet carry, oldest-sorted for determinism.
async fn unmapped_read_revisions(
    store: &impl CanonicalStore,
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let rows = store
        .query(
            "SELECT DISTINCT r.path, r.content_sha256 \
             FROM n_file_revision r \
             JOIN n_file_read k ON k.file_revision = r.node_id \
             LEFT JOIN n_blob_oid_map m ON m.content_sha256 = r.content_sha256 \
             WHERE m.content_sha256 IS NULL \
             ORDER BY r.path, r.content_sha256",
            &[],
        )
        .await?;
    let mut wanted: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for r in &rows {
        let (Ok(path), Ok(hash)) = (r.get_text(0), r.get_text(1)) else {
            continue;
        };
        if path.is_empty() || hash.len() != 64 {
            continue;
        }
        if wanted.len() >= MAX_BRIDGED_PATHS && !wanted.contains_key(&path) {
            continue;
        }
        wanted.entry(path).or_default().insert(hash);
    }
    Ok(wanted)
}

/// Map every recorded read revision whose bytes git already holds to the blob
/// that holds them.
///
/// Idempotent: the map's primary key is the content address and the write is
/// insert-or-ignore, so a second pass re-proves nothing and writes nothing. A
/// reader that cannot walk history (the fail-safe default) bridges nothing and
/// reports every wanted revision unavailable, which is what it truly knows.
///
/// # Errors
/// Propagates store read/write errors. A history-walk failure is not an
/// error — it yields fewer candidates.
pub async fn bridge_read_revisions(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    now: i64,
) -> Result<BridgeReport> {
    let mut wanted = unmapped_read_revisions(store).await?;
    let mut report = BridgeReport {
        wanted: wanted.values().map(BTreeSet::len).sum(),
        ..BridgeReport::default()
    };
    if wanted.is_empty() {
        return Ok(report);
    }
    let paths: BTreeSet<String> = wanted.keys().cloned().collect();
    let candidates = reader
        .path_blob_oids(&paths, MAX_HISTORY_COMMITS)
        .unwrap_or_default();
    for (path, oids) in &candidates {
        let Some(missing) = wanted.get_mut(path) else {
            continue;
        };
        for oid in oids {
            if missing.is_empty() {
                break;
            }
            let Ok(Some(bytes)) = reader.blob(oid) else {
                continue;
            };
            if bytes.len() > MAX_BASIS_BYTES {
                continue;
            }
            let hash = sha256_hex(&bytes);
            if !missing.remove(&hash) {
                continue;
            }
            // Three strings, and none of them is a byte of what was read: the
            // address that was recorded, the object git already has, and the
            // file it was recorded as.
            crate::blob_map::persist_blob_oid_map(
                store,
                &[WorktreeBlob {
                    path: path.clone(),
                    content_sha256: hash,
                    blob_oid: oid.clone(),
                }],
                now,
            )
            .await?;
            report.mapped += 1;
        }
    }
    report.basis_unavailable = report.wanted.saturating_sub(report.mapped);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use rift_recall_substrate::worktree::git_blob_oid;
    use rift_recall_substrate::MemorySubstrate;
    use rift_store::{apply_migrations, recall_migrations, SqliteStore, Value};

    use super::*;

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime")
            .block_on(f)
    }

    fn recall_store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        apply_migrations(&s, &recall_migrations()).unwrap();
        s
    }

    /// Record that a session read `path` and was handed `bytes` — a revision
    /// node and the read that refers to it, which is what an ingest writes.
    fn record_read(store: &SqliteStore, node: &str, path: &str, bytes: &[u8]) -> String {
        let hash = sha256_hex(bytes);
        block(store.insert_or_ignore(
            "n_file_revision",
            &["node_id", "path", "blob_oid", "content_sha256", "size"],
            &[
                Value::text(node),
                Value::text(path),
                Value::text(&hash),
                Value::text(&hash),
                Value::Int(i64::try_from(bytes.len()).unwrap()),
            ],
        ))
        .unwrap();
        block(store.insert_or_ignore(
            "n_file_read",
            &[
                "node_id",
                "checkpoint_id",
                "tool_call_id",
                "file_revision",
                "input_tokens",
                "read_ordinal",
                "byte_range_lo",
                "byte_range_hi",
            ],
            &[
                Value::text(format!("read_{node}")),
                Value::text("c1"),
                Value::text(format!("t_{node}")),
                Value::text(node),
                Value::Int(10),
                Value::Int(1),
                Value::Null,
                Value::Null,
            ],
        ))
        .unwrap();
        hash
    }

    const COMMITTED: &[u8] = b"alpha\nbravo\ncharlie\n";
    const UNCOMMITTED: &[u8] = b"alpha\nBRAVO\ncharlie\n";

    /// ★ The bridge's whole claim. A session read bytes that git already has
    /// under some commit; nothing had joined the address it recorded to the
    /// object that holds those bytes, and after the ingest pass the join
    /// exists — which is the difference between the pull verb answering with
    /// the changed lines and answering that it cannot reach the basis.
    #[test]
    fn a_held_revision_that_exists_as_a_blob_is_bridged_at_ingest() {
        let store = recall_store();
        let hash = record_read(&store, "rev1", "src/a.rs", COMMITTED);
        let oid = git_blob_oid(COMMITTED);
        let sub = MemorySubstrate::local()
            .with_blob(&oid, COMMITTED)
            .with_path_blob("src/a.rs", &oid);

        // The premise: before the pass, the address resolves to no object.
        assert_eq!(
            block(crate::blob_map::blob_oid_for(&store, &hash)).unwrap(),
            None
        );

        let report = block(bridge_read_revisions(&store, &sub, 7)).unwrap();
        assert_eq!(
            report,
            BridgeReport {
                wanted: 1,
                mapped: 1,
                basis_unavailable: 0
            }
        );
        assert_eq!(
            block(crate::blob_map::blob_oid_for(&store, &hash)).unwrap(),
            Some(oid),
            "the recorded address now names the object whose bytes hash to it"
        );

        // Proved once. A second pass has nothing left to want and writes
        // nothing, so re-ingest is not re-work.
        let again = block(bridge_read_revisions(&store, &sub, 8)).unwrap();
        assert_eq!(again, BridgeReport::default());
        assert_eq!(block(store.count("n_blob_oid_map")).unwrap(), 1);
    }

    /// ★ The other half, and the one that must never be papered over. Bytes
    /// that were read and never committed are in no object store; the pass
    /// says so, counts it, and — the part that matters — does not make the
    /// basis available by keeping the bytes it was handed. Nothing durable
    /// holds them, because the only way in is the redaction gate.
    #[test]
    fn an_uncommitted_revision_stays_basis_unavailable_and_persists_no_bytes() {
        let store = recall_store();
        let committed = record_read(&store, "rev1", "src/a.rs", COMMITTED);
        let uncommitted = record_read(&store, "rev2", "src/a.rs", UNCOMMITTED);
        let oid = git_blob_oid(COMMITTED);
        let sub = MemorySubstrate::local()
            .with_blob(&oid, COMMITTED)
            .with_path_blob("src/a.rs", &oid);

        let report = block(bridge_read_revisions(&store, &sub, 0)).unwrap();
        assert_eq!(
            report,
            BridgeReport {
                wanted: 2,
                mapped: 1,
                basis_unavailable: 1
            },
            "the uncommitted revision is named as unreachable, not as unchanged"
        );
        assert_eq!(
            block(crate::blob_map::blob_oid_for(&store, &uncommitted)).unwrap(),
            None
        );
        assert!(block(crate::blob_map::blob_oid_for(&store, &committed))
            .unwrap()
            .is_some());

        // The bytes themselves are nowhere: the one row written carries three
        // strings, and none of them is a line of the file.
        let rows = block(store.query(
            "SELECT content_sha256, blob_oid, path FROM n_blob_oid_map",
            &[],
        ))
        .unwrap();
        assert_eq!(rows.len(), 1);
        for i in 0..3 {
            let cell = rows[0].get_text(i).unwrap();
            assert!(
                !cell.contains("alpha") && !cell.contains("BRAVO"),
                "a durable cell carries content: {cell:?}"
            );
        }
    }

    /// A reader that cannot walk history bridges nothing and reports every
    /// wanted revision unavailable — absence of evidence stays absence, and
    /// the default is the conservative one.
    #[test]
    fn a_reader_with_no_history_bridges_nothing_and_says_so() {
        let store = recall_store();
        record_read(&store, "rev1", "src/a.rs", COMMITTED);
        let report = block(bridge_read_revisions(&store, &MemorySubstrate::local(), 0)).unwrap();
        assert_eq!(
            report,
            BridgeReport {
                wanted: 1,
                mapped: 0,
                basis_unavailable: 1
            }
        );
        assert_eq!(block(store.count("n_blob_oid_map")).unwrap(), 0);
    }

    /// A revision recorded for one file is not bridged from another file's
    /// history, even when the bytes are identical: the map's row keeps the
    /// path it was proved at, and the path-bound resolvers read it.
    #[test]
    fn the_bridged_row_carries_the_path_it_was_proved_at() {
        let store = recall_store();
        let hash = record_read(&store, "rev1", "src/a.rs", COMMITTED);
        let oid = git_blob_oid(COMMITTED);
        let sub = MemorySubstrate::local()
            .with_blob(&oid, COMMITTED)
            .with_path_blob("src/a.rs", &oid)
            .with_path_blob("src/copy.rs", &oid);
        block(bridge_read_revisions(&store, &sub, 0)).unwrap();
        let rows = block(store.query(
            "SELECT path FROM n_blob_oid_map WHERE content_sha256 = ?",
            &[Value::text(&hash)],
        ))
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_text(0).unwrap(), "src/a.rs");
    }
}

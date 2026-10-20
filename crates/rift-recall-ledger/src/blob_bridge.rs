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
//! The walk is the expensive half, and its answer for one revision is a
//! function of two things only: the path it was read at and the line of
//! history `HEAD` names. So a walk that finished records a proof for each
//! revision it left unavailable — one `ingest_watermark` row whose id digests
//! `HEAD`, the path and the content address — and the incremental pass skips a
//! walk when every revision it wants has its proof. A new read or a moved
//! `HEAD` has no proof and walks again; the full pass always walks.
//!
//! The same proofs keep the cap on paths per pass from starving the rest. A
//! pass takes the paths with a revision no walk from this `HEAD` has tried
//! before the ones already shown unavailable, so bytes git will never hold
//! cannot occupy the cap forever, and every wanted path is walked within as
//! many passes as it takes the cap to cover them.
//!
//! **Opens a store and walks history**, so it belongs to the ingest pass the
//! worker runs and to nothing on a tool-call gate path.

use std::collections::{BTreeMap, BTreeSet};

use rift_core::Result;
use rift_recall_substrate::worktree::{sha256_hex, WorktreeBlob};
use rift_recall_substrate::SubstrateReader;
use rift_store::{CanonicalStore, Value};

use crate::graph::sha256_id;

/// How far back the candidate walk goes: the last sixty-four commits of the
/// checked-out line, first-parent. Deep enough to cover the sessions a
/// projection is still serving deltas for, shallow enough that a first ingest
/// on a long history stays a bounded cost. A revision older than this is not
/// found, which costs a saving and never names wrong bytes.
pub const MAX_HISTORY_COMMITS: usize = 64;

/// How many distinct paths one pass will walk history for. Paths not yet tried
/// from the current `HEAD` are taken first, so a repository with more than
/// this many unbridged paths reaches every one of them over consecutive passes
/// rather than paying for all of them at once.
pub const MAX_BRIDGED_PATHS: usize = 64;

/// The largest blob this will hash. A basis past it could not be diffed
/// anyway — the diff refuses at the same ceiling — so reading it would be work
/// spent to learn nothing.
pub const MAX_BASIS_BYTES: usize = 1024 * 1024;

/// The `ingest_watermark` branch a finished walk's proofs are recorded under. No
/// checkpoint dialect is spelled like a bare name, so a dialect's resume never
/// reads one of these rows as its tip.
pub const PROOF_BRANCH: &str = "blob-bridge";

/// The revisions a pass wants, grouped by the path they were recorded at.
type Wanted = BTreeMap<String, BTreeSet<String>>;

/// The revisions one pass takes, and whether a walk from the current `HEAD`
/// has already shown every one of them unavailable.
struct Selection {
    wanted: Wanted,
    proven: bool,
}

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
    /// Whether this pass walked history, rather than finding a proof that a
    /// walk from the same `HEAD` had already left these revisions unavailable.
    pub history_walked: bool,
}

/// Map every recorded read revision whose bytes git already holds to the blob
/// that holds them — the full pass, which walks whenever anything is wanted.
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
    let head = head_of(reader);
    let selection = select_wanted(store, head.as_deref()).await?;
    walk_and_prove(store, reader, selection.wanted, now).await
}

/// [`bridge_read_revisions`] for the incremental pass: a wanted set a walk from
/// this same `HEAD` already left unavailable is reported as that walk found it,
/// without walking again.
///
/// Equal to the full pass by construction: both take the same selection, the
/// walk reads nothing for a revision but its path's first-parent line from
/// `HEAD` and the immutable objects on it, and each proof's id digests the
/// revision, its path and `HEAD`. A reader that cannot name `HEAD` has no proof
/// to find and always walks.
///
/// # Errors
/// Propagates store read/write errors.
pub async fn bridge_unproven_revisions(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    now: i64,
) -> Result<BridgeReport> {
    let head = head_of(reader);
    let selection = select_wanted(store, head.as_deref()).await?;
    if !selection.proven {
        return walk_and_prove(store, reader, selection.wanted, now).await;
    }
    let n_wanted = selection.wanted.values().map(BTreeSet::len).sum();
    Ok(BridgeReport {
        wanted: n_wanted,
        mapped: 0,
        basis_unavailable: n_wanted,
        history_walked: false,
    })
}

/// The paths one pass walks for: those with a revision no walk from `head` has
/// tried come first, then those whose every revision a walk from `head` left
/// unavailable, each group in path order, up to [`MAX_BRIDGED_PATHS`].
///
/// Taking the untried first is what lets every path be reached: a pass that
/// leaves a revision unavailable records its proof, which moves its path behind
/// every untried one on the next pass. Filling the rest of the cap with proven
/// paths keeps their revisions in the count, so a pass with nothing new to try
/// reports what the last walk found rather than nothing at all.
/// Without a `HEAD` nothing is proven, and the order is path order alone.
async fn select_wanted(store: &impl CanonicalStore, head: Option<&str>) -> Result<Selection> {
    let unmapped = unmapped_read_revisions(store).await?;
    let (proven, untried): (Wanted, Wanted) = match head {
        Some(head) => {
            let proofs = recorded_proofs(store, head).await?;
            unmapped.into_iter().partition(|(path, hashes)| {
                hashes
                    .iter()
                    .all(|hash| proofs.contains(&proof_id(head, path, hash)))
            })
        }
        None => (Wanted::new(), unmapped),
    };
    let all_proven = untried.is_empty() && !proven.is_empty();
    let wanted = untried
        .into_iter()
        .chain(proven)
        .take(MAX_BRIDGED_PATHS)
        .collect();
    Ok(Selection {
        wanted,
        proven: all_proven,
    })
}

/// Every `(path, content_sha256)` a read recorded that the blob map does not
/// yet carry, in path order for determinism.
async fn unmapped_read_revisions(store: &impl CanonicalStore) -> Result<Wanted> {
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
    let mut wanted = Wanted::new();
    for r in &rows {
        let (Ok(path), Ok(hash)) = (r.get_text(0), r.get_text(1)) else {
            continue;
        };
        if path.is_empty() || hash.len() != 64 {
            continue;
        }
        wanted.entry(path).or_default().insert(hash);
    }
    Ok(wanted)
}

/// Walk history for `wanted`, map what it finds, and record the proof of what
/// it left unavailable when the walk was whole.
///
/// The proofs name the revisions left over, not the ones asked for: those are
/// what the next pass still wants, and it finds them proven without walking.
async fn walk_and_prove(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    mut wanted: Wanted,
    now: i64,
) -> Result<BridgeReport> {
    let n_wanted = wanted.values().map(BTreeSet::len).sum();
    if n_wanted == 0 {
        return Ok(BridgeReport::default());
    }
    let head = head_of(reader);
    let walk = walk_history(store, reader, &mut wanted, now).await?;
    if let Some(head) = head {
        record_proof(store, reader, &head, &walk, &wanted).await?;
    }
    Ok(BridgeReport {
        wanted: n_wanted,
        mapped: walk.mapped,
        basis_unavailable: n_wanted.saturating_sub(walk.mapped),
        history_walked: true,
    })
}

/// What one walk did: how many revisions it mapped, and whether it read every
/// candidate it was offered.
struct Walk {
    mapped: usize,
    whole: bool,
}

/// Hash every candidate blob of the wanted paths and map each one whose bytes
/// a read recorded, removing it from `wanted`.
async fn walk_history(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    wanted: &mut Wanted,
    now: i64,
) -> Result<Walk> {
    let paths: BTreeSet<String> = wanted.keys().cloned().collect();
    let Ok(candidates) = reader.path_blob_oids(&paths, MAX_HISTORY_COMMITS) else {
        return Ok(Walk {
            mapped: 0,
            whole: false,
        });
    };
    let mut walk = Walk {
        mapped: 0,
        whole: true,
    };
    for (path, oids) in &candidates {
        let Some(missing) = wanted.get_mut(path) else {
            continue;
        };
        for oid in oids {
            if missing.is_empty() {
                break;
            }
            let Ok(Some(bytes)) = reader.blob(oid) else {
                // A candidate the object store cannot produce leaves the walk
                // partial, and a partial walk proves nothing.
                walk.whole = false;
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
            walk.mapped += 1;
        }
    }
    Ok(walk)
}

/// Record that a walk from `head` left each revision of `left_over`
/// unavailable — unless the walk missed a candidate or raced a moving `HEAD`,
/// when it proves nothing about the line it was meant to read.
async fn record_proof(
    store: &impl CanonicalStore,
    reader: &impl SubstrateReader,
    head: &str,
    walk: &Walk,
    left_over: &Wanted,
) -> Result<()> {
    let head_held = head_of(reader).as_deref() == Some(head);
    if !walk.whole || !head_held {
        return Ok(());
    }
    for (path, hashes) in left_over {
        for hash in hashes {
            let id = proof_id(head, path, hash);
            // A proof row records no walk position, so readiness never reads
            // its time; the fixed value keeps the bridge's rows stable.
            crate::ingest::record_watermark(
                store,
                &id,
                PROOF_BRANCH,
                head,
                crate::graph::INGEST_TS,
            )
            .await?;
        }
    }
    Ok(())
}

/// The commit `HEAD` names, when the reader can say.
fn head_of(reader: &impl SubstrateReader) -> Option<String> {
    reader.resolve_commitish("HEAD").ok().flatten()
}

/// The id of the proof that a walk from `head` left the revision `hash` read
/// at `path` unavailable. The domain carries the walk's bounds, so changing
/// them retires every proof recorded under the old ones; the cap on paths per
/// pass is not among them, because it decides which revisions a walk tries and
/// never what the walk finds for one.
fn proof_id(head: &str, path: &str, hash: &str) -> String {
    let domain = format!("rift.bridge.v2/{MAX_HISTORY_COMMITS}/{MAX_BASIS_BYTES}");
    sha256_id(
        &domain,
        &[head.as_bytes(), path.as_bytes(), hash.as_bytes()],
    )
}

/// The ids of every proof recorded for a walk from `head`.
async fn recorded_proofs(store: &impl CanonicalStore, head: &str) -> Result<BTreeSet<String>> {
    let rows = store
        .query(
            "SELECT watermark_id FROM ingest_watermark WHERE branch = ?1 AND branch_tip = ?2",
            &[Value::text(PROOF_BRANCH), Value::text(head)],
        )
        .await?;
    Ok(rows.iter().filter_map(|r| r.get_text(0).ok()).collect())
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
                basis_unavailable: 0,
                history_walked: true,
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
                basis_unavailable: 1,
                history_walked: true,
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
                basis_unavailable: 1,
                history_walked: true,
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

    /// A walk from a `HEAD` the reader names is recorded as a proof, and the
    /// incremental bridge answers the same wanted set from it without walking.
    #[test]
    fn a_recorded_proof_answers_the_same_wanted_set_without_a_walk() {
        let store = recall_store();
        record_read(&store, "rev1", "src/a.rs", UNCOMMITTED);
        let sub = MemorySubstrate::local().with_revision("HEAD", "c0ffee01");
        let walked = block(bridge_unproven_revisions(&store, &sub, 0)).unwrap();
        assert!(walked.history_walked);
        let proved = block(bridge_unproven_revisions(&store, &sub, 0)).unwrap();
        assert_eq!(
            proved,
            BridgeReport {
                history_walked: false,
                ..walked
            }
        );
    }

    /// A reader that cannot name `HEAD` has no line to prove anything about,
    /// so the incremental bridge walks on every pass and records nothing.
    #[test]
    fn a_reader_with_no_head_walks_every_time_and_records_no_proof() {
        let store = recall_store();
        record_read(&store, "rev1", "src/a.rs", UNCOMMITTED);
        let sub = MemorySubstrate::local();
        for _ in 0..2 {
            let report = block(bridge_unproven_revisions(&store, &sub, 0)).unwrap();
            assert!(report.history_walked);
        }
        assert_eq!(block(store.count("ingest_watermark")).unwrap(), 0);
    }

    /// More unbridgeable paths than one pass takes must not starve the rest:
    /// once a walk from this `HEAD` has shown the first paths unavailable, the
    /// next pass reaches the path after them, whose bytes git does hold.
    #[test]
    fn paths_past_the_cap_are_walked_once_the_first_ones_are_proven_unavailable() {
        let store = recall_store();
        for i in 0..MAX_BRIDGED_PATHS {
            let bytes = format!("never committed {i}\n");
            record_read(
                &store,
                &format!("rev{i}"),
                &format!("src/a{i:03}.rs"),
                bytes.as_bytes(),
            );
        }
        let hash = record_read(&store, "last", "src/z.rs", COMMITTED);
        let oid = git_blob_oid(COMMITTED);
        let sub = MemorySubstrate::local()
            .with_revision("HEAD", "c0ffee01")
            .with_blob(&oid, COMMITTED)
            .with_path_blob("src/z.rs", &oid);

        let first = block(bridge_unproven_revisions(&store, &sub, 0)).unwrap();
        assert_eq!(first.mapped, 0, "the first pass takes the first paths");
        let second = block(bridge_unproven_revisions(&store, &sub, 0)).unwrap();
        assert_eq!(second.mapped, 1, "{second:?}");
        assert_eq!(
            block(crate::blob_map::blob_oid_for(&store, &hash)).unwrap(),
            Some(oid)
        );
    }

    /// A candidate the object store cannot produce makes the walk partial, and
    /// a partial walk proves nothing: the next pass walks again.
    #[test]
    fn a_walk_that_missed_a_candidate_records_no_proof() {
        let store = recall_store();
        record_read(&store, "rev1", "src/a.rs", UNCOMMITTED);
        let sub = MemorySubstrate::local()
            .with_revision("HEAD", "c0ffee01")
            .with_path_blob("src/a.rs", "an-object-the-store-lost");
        block(bridge_unproven_revisions(&store, &sub, 0)).unwrap();
        let again = block(bridge_unproven_revisions(&store, &sub, 0)).unwrap();
        assert!(again.history_walked);
    }
}

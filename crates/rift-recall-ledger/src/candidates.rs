//! Scope-contained candidate loading for ranking.
//!
//! Ranking-sensitive recall tools must **order within** the current scope and
//! never widen it. This module is the bridge: it loads the file-revision
//! candidates the agent might want, routing **every** row through the single
//! [`in_scope_nodes`] chokepoint, then
//! attaches each candidate's provenance outcome labels. Ranking
//! ([`rift_recall_rank::rank`]) reorders this set; it can never introduce a row
//! outside the scope, because no out-of-scope row is ever loaded.
//!
//! Recency note: the v1 graph records a deterministic checkpoint **ordinal**, not
//! a wall-clock read time. We map the most-recent in-scope checkpoint to `now`
//! and step each older one back by [`RECENCY_STEP_MS`], so the recency signal
//! decays monotonically with checkpoint age until per-read capture times land.

use std::collections::{BTreeMap, BTreeSet};

use rift_core::{Result, Timestamp};
use rift_recall_rank::Candidate;
use rift_recall_types::{AncestryOracle, LabelSubject, Scope};
use rift_store::{CanonicalStore, Value};

use crate::outcome::effective_labels;
use crate::scope_query::{in_scope_nodes, ScopeSet};

/// One "day" of recency decay per checkpoint-ordinal step (the ordinal proxy for
/// per-read capture time; see the module note).
pub const RECENCY_STEP_MS: i64 = 24 * 60 * 60 * 1000;

/// How many content addresses one [`recorded_reads`] statement binds — well
/// under the engine's variable limit, and far above what a caller states.
pub const RECORDED_READS_CHUNK: usize = 500;

/// A ranking candidate plus the serving metadata the serve handlers need (the
/// content hash, for handles + known-hash comparison).
#[derive(Clone, Debug)]
pub struct LoadedCandidate {
    /// The ranking candidate (path, recency, outcomes, est-tokens).
    pub candidate: Candidate,
    /// The file revision's content SHA-256 (hex) — the validity comparator.
    pub content_sha256: String,
    /// The most-recent **in-scope** checkpoint that recorded this revision.
    ///
    /// Carried rather than re-queried: the loader already picks it to decide
    /// recency, and a second query for the same row is a second chance for the
    /// two to disagree about which checkpoint a revision belongs to. It is what
    /// a delta's provenance is anchored on — the story of a change is the story
    /// of the checkpoint that recorded it.
    pub checkpoint: String,
}

/// Load the scope-contained ranking candidates: one per in-scope file revision,
/// carrying its path + content hash, an est-tokens estimate, a recency proxy, and
/// its provenance outcome labels (effective as of `now`).
///
/// **Containment:** a candidate is emitted only if its source checkpoint is
/// in [`in_scope_nodes`], so the returned set never escapes `scope` — including
/// the branch axis, which `ancestry` decides.
///
/// # Errors
/// Propagates store read errors.
pub async fn load_candidates(
    store: &impl CanonicalStore,
    scope: &Scope,
    now: Timestamp,
    ancestry: &dyn AncestryOracle,
) -> Result<Vec<LoadedCandidate>> {
    Ok(load_candidates_with_scope(store, scope, now, ancestry)
        .await?
        .0)
}

/// [`load_candidates`], keeping the resolved [`ScopeSet`] beside the rows.
///
/// The set is what a response needs to report its omissions honestly
/// (the omission line grows an `out_of_branch` cause): the
/// candidates say what a caller may see, and the set says how much it did not
/// and why. Handed back rather than recomputed, so the two can never disagree
/// and the ancestry is still one question per serve.
///
/// # Errors
/// Propagates store read errors.
pub async fn load_candidates_with_scope(
    store: &impl CanonicalStore,
    scope: &Scope,
    now: Timestamp,
    ancestry: &dyn AncestryOracle,
) -> Result<(Vec<LoadedCandidate>, ScopeSet)> {
    let in_scope = in_scope_nodes(store, scope, ancestry).await?;
    if in_scope.is_empty() {
        return Ok((Vec::new(), in_scope));
    }

    let rows = store
        .query(
            "SELECT fr.node_id, fr.path, fr.content_sha256, fread.checkpoint_id, ck.ordinal, fread.input_tokens \
             FROM n_file_read fread \
             JOIN n_file_revision fr ON fr.node_id = fread.file_revision \
             JOIN n_checkpoint ck ON ck.node_id = fread.checkpoint_id",
            &[],
        )
        .await?;

    // Per file revision, keep its most-recent in-scope appearance.
    struct Acc {
        path: String,
        content_sha256: String,
        checkpoint: String,
        ordinal: i64,
        est_tokens: u32,
    }
    let mut best: BTreeMap<String, Acc> = BTreeMap::new();
    for r in &rows {
        let checkpoint = r.get_text(3)?;
        if !in_scope.contains(&checkpoint) {
            continue; // the containment chokepoint: out-of-scope rows are never loaded
        }
        let rev = r.get_text(0)?;
        let ordinal = r.get_i64(4)?;
        let est_tokens = u32::try_from(r.get_i64(5)?.max(0)).unwrap_or(u32::MAX);
        best.entry(rev)
            .and_modify(|a| {
                if ordinal > a.ordinal {
                    a.ordinal = ordinal;
                    a.checkpoint.clone_from(&checkpoint);
                    a.est_tokens = est_tokens;
                }
            })
            .or_insert_with(|| Acc {
                path: r.get_text(1).unwrap_or_default(),
                content_sha256: r.get_text(2).unwrap_or_default(),
                checkpoint: checkpoint.clone(),
                ordinal,
                est_tokens,
            });
    }

    let max_ord = best.values().map(|a| a.ordinal).max().unwrap_or(0);
    let mut out = Vec::with_capacity(best.len());
    for (rev, a) in best {
        let outcomes =
            effective_labels(store, &LabelSubject::Checkpoint(a.checkpoint.clone()), now).await?;
        let age_steps = (max_ord - a.ordinal).max(0);
        let last_touched =
            Timestamp::from_millis(now.as_millis() - age_steps.saturating_mul(RECENCY_STEP_MS));
        out.push(LoadedCandidate {
            candidate: Candidate {
                id: rev,
                path: a.path,
                error_signatures: BTreeSet::new(), // TODO: link ErrorSignature nodes
                last_touched,
                outcomes,
                est_tokens: a.est_tokens,
            },
            content_sha256: a.content_sha256,
            checkpoint: a.checkpoint,
        });
    }
    Ok((out, in_scope))
}

/// The subset of `hashes` this family is recorded as having read: a content
/// address some read in the projection landed on.
///
/// This is the evidence a pull's avoided figure rests on. A caller's word that
/// it holds some bytes is text an agent wrote; a read the capture recorded is a
/// fact Rift saw, so a spared re-read is credited only where the two agree.
/// Asked by content address, not by path, because what was handed to a window
/// is the bytes, and bytes read under another name were still read.
///
/// One statement per [`RECORDED_READS_CHUNK`] hashes, so a caller that states
/// thousands cannot push the statement past the engine's variable limit; a real
/// call states a handful and costs one query.
///
/// # Errors
/// Propagates store read errors.
pub async fn recorded_reads(
    store: &impl CanonicalStore,
    hashes: &BTreeSet<&str>,
) -> Result<BTreeSet<String>> {
    let wanted: Vec<&str> = hashes.iter().copied().collect();
    let mut out = BTreeSet::new();
    for chunk in wanted.chunks(RECORDED_READS_CHUNK) {
        let marks = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "SELECT DISTINCT r.content_sha256 FROM n_file_revision r \
             JOIN n_file_read k ON k.file_revision = r.node_id \
             WHERE r.content_sha256 IN ({marks})"
        );
        let params: Vec<Value> = chunk.iter().map(|h| Value::text(*h)).collect();
        for row in store.query(&sql, &params).await? {
            out.insert(row.get_text(0)?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_types::NoAncestry;
    use rift_store::{recall_migrations, SqliteStore, Value};

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    fn recall_store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&s, &recall_migrations()).unwrap();
        s
    }

    fn put_checkpoint(store: &SqliteStore, id: &str, ordinal: i64, developer: Option<&str>) {
        block(store.insert_or_ignore(
            "n_checkpoint",
            &[
                "node_id",
                "dialect",
                "ordinal",
                "prev_checkpoint",
                "strategy",
                "merkle_root",
                "developer",
            ],
            &[
                Value::text(id),
                Value::text("rift"),
                Value::Int(ordinal),
                Value::Null,
                Value::Null,
                Value::Null,
                developer.map_or(Value::Null, Value::text),
            ],
        ))
        .unwrap();
    }

    fn put_revision(store: &SqliteStore, node_id: &str, path: &str) {
        block(store.insert_or_ignore(
            "n_file_revision",
            &["node_id", "path", "blob_oid", "content_sha256", "size"],
            &[
                Value::text(node_id),
                Value::text(path),
                Value::text("a".repeat(64)),
                Value::text("a".repeat(64)),
                Value::Int(10),
            ],
        ))
        .unwrap();
    }

    fn put_read(
        store: &SqliteStore,
        id: &str,
        checkpoint: &str,
        revision: &str,
        ord: i64,
        tokens: i64,
    ) {
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
                Value::text(id),
                Value::text(checkpoint),
                Value::text(format!("tc_{id}")),
                Value::text(revision),
                Value::Int(tokens),
                Value::Int(ord),
                Value::Null,
                Value::Null,
            ],
        ))
        .unwrap();
    }

    #[test]
    fn loads_in_scope_candidates_with_recency_proxy() {
        let store = recall_store();
        put_checkpoint(&store, "c1", 1, None);
        put_checkpoint(&store, "c2", 2, None);
        put_revision(&store, "rev_a", "src/a.rs");
        put_revision(&store, "rev_b", "src/b.rs");
        put_read(&store, "r1", "c1", "rev_a", 1, 100);
        put_read(&store, "r2", "c2", "rev_b", 2, 50);
        let now = Timestamp::from_millis(1_000 * RECENCY_STEP_MS);
        let cands = block(load_candidates(
            &store,
            &Scope::unbounded("repo"),
            now,
            &NoAncestry,
        ))
        .unwrap();
        let by_path: BTreeMap<_, _> = cands
            .iter()
            .map(|c| (c.candidate.path.clone(), c))
            .collect();
        assert_eq!(by_path.len(), 2);
        // c2 (ordinal 2) is the most recent → last_touched == now (age 0).
        assert_eq!(by_path["src/b.rs"].candidate.last_touched, now);
        // c1 (ordinal 1) is one step older.
        assert_eq!(
            by_path["src/a.rs"].candidate.last_touched,
            Timestamp::from_millis(now.as_millis() - RECENCY_STEP_MS)
        );
        assert_eq!(by_path["src/a.rs"].candidate.est_tokens, 100);
        assert_eq!(by_path["src/a.rs"].content_sha256.len(), 64);
    }

    /// A read is what makes a hash recorded: a revision the projection holds
    /// but nobody read is not evidence, and a hash nothing holds is not either.
    #[test]
    fn only_a_hash_some_read_landed_on_is_recorded() {
        let store = recall_store();
        put_checkpoint(&store, "c1", 1, None);
        put_revision(&store, "rev_a", "src/a.rs");
        put_read(&store, "r1", "c1", "rev_a", 1, 100);
        block(store.insert_or_ignore(
            "n_file_revision",
            &["node_id", "path", "blob_oid", "content_sha256", "size"],
            &[
                Value::text("rev_b"),
                Value::text("src/b.rs"),
                Value::text("b".repeat(64)),
                Value::text("b".repeat(64)),
                Value::Int(10),
            ],
        ))
        .unwrap();
        let (a, b, c) = ("a".repeat(64), "b".repeat(64), "c".repeat(64));
        let asked: BTreeSet<&str> = [a.as_str(), b.as_str(), c.as_str()].into();
        let recorded = block(recorded_reads(&store, &asked)).unwrap();
        assert_eq!(recorded, BTreeSet::from([a.clone()]));
        assert!(block(recorded_reads(&store, &BTreeSet::new()))
            .unwrap()
            .is_empty());
    }

    /// A stated set larger than one statement binds is answered whole.
    #[test]
    fn a_stated_set_past_one_statement_is_answered_whole() {
        let store = recall_store();
        put_checkpoint(&store, "c1", 1, None);
        put_revision(&store, "rev_a", "src/a.rs");
        put_read(&store, "r1", "c1", "rev_a", 1, 100);
        let noise: Vec<String> = (0..RECORDED_READS_CHUNK * 2)
            .map(|i| format!("{i:064x}"))
            .collect();
        let a = "a".repeat(64);
        let mut asked: BTreeSet<&str> = noise.iter().map(String::as_str).collect();
        asked.insert(&a);
        let recorded = block(recorded_reads(&store, &asked)).unwrap();
        assert_eq!(recorded, BTreeSet::from([a.clone()]));
    }

    proptest::proptest! {
        /// The containment keystone for ranking: candidate loading is bounded to the
        /// in-scope checkpoints, so a default (current-developer) scope never
        /// surfaces a path that exists ONLY in another developer's checkpoint —
        /// ranking can only reorder what scope already admitted.
        #[test]
        fn scope_containment_via_ranking(
            devs in proptest::collection::vec(proptest::option::of("dev[0-9]"), 1..10)
        ) {
            let store = recall_store();
            for (i, d) in devs.iter().enumerate() {
                let c = format!("c{i}");
                let rev = format!("rev{i}");
                put_checkpoint(&store, &c, i as i64 + 1, d.as_deref());
                put_revision(&store, &rev, &format!("p{i}.rs"));
                put_read(&store, &format!("r{i}"), &c, &rev, i as i64 + 1, 100);
            }
            let scope = Scope::unbounded("repo");
            let now = Timestamp::from_millis(1_000_000);
            let cands = block(load_candidates(&store, &scope, now, &NoAncestry)).unwrap();
            let paths: BTreeSet<String> =
                cands.iter().map(|c| c.candidate.path.clone()).collect();
            for (i, d) in devs.iter().enumerate() {
                let present = paths.contains(&format!("p{i}.rs"));
                proptest::prop_assert_eq!(present, d.is_none(), "out-of-scope path leaked into ranking");
            }
        }
    }
}

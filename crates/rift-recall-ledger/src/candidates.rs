//! Scope-contained candidate loading for ranking (P4, W14).
//!
//! Ranking-sensitive recall tools must **order within** the current scope and
//! never widen it. This module is the bridge: it loads the file-revision
//! candidates the agent might want, routing **every** row through the single
//! [`in_scope_nodes`](crate::scope_query::in_scope_nodes) chokepoint, then
//! attaches each candidate's provenance outcome labels (P4 S2). Ranking
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
use rift_recall_types::{LabelSubject, Scope};
use rift_store::CanonicalStore;

use crate::outcome::effective_labels;
use crate::scope_query::in_scope_nodes;

/// One "day" of recency decay per checkpoint-ordinal step (the ordinal proxy for
/// per-read capture time; see the module note).
pub const RECENCY_STEP_MS: i64 = 24 * 60 * 60 * 1000;

/// A ranking candidate plus the serving metadata the MCP handlers need (the
/// content hash, for handles + known-hash comparison).
#[derive(Clone, Debug)]
pub struct LoadedCandidate {
    /// The ranking candidate (path, recency, outcomes, est-tokens).
    pub candidate: Candidate,
    /// The file revision's content SHA-256 (hex) — the validity comparator.
    pub content_sha256: String,
}

/// Load the scope-contained ranking candidates: one per in-scope file revision,
/// carrying its path + content hash, an est-tokens estimate, a recency proxy, and
/// its provenance outcome labels (effective as of `now`).
///
/// **Containment (W14):** a candidate is emitted only if its source checkpoint is
/// in [`in_scope_nodes`], so the returned set never escapes `scope`.
///
/// # Errors
/// Propagates store read errors.
pub async fn load_candidates(
    store: &impl CanonicalStore,
    scope: &Scope,
    now: Timestamp,
) -> Result<Vec<LoadedCandidate>> {
    let in_scope = in_scope_nodes(store, scope).await?;
    if in_scope.is_empty() {
        return Ok(Vec::new());
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
            continue; // the W14 chokepoint: out-of-scope rows are never loaded
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
                error_signatures: BTreeSet::new(), // TODO(P4): link ErrorSignature nodes
                last_touched,
                outcomes,
                est_tokens: a.est_tokens,
            },
            content_sha256: a.content_sha256,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
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
            &Scope::default_for("repo", u32::MAX),
            now,
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

    proptest::proptest! {
        /// W14 keystone for ranking: candidate loading is bounded to the
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
            let scope = Scope::default_for("repo", u32::MAX);
            let now = Timestamp::from_millis(1_000_000);
            let cands = block(load_candidates(&store, &scope, now)).unwrap();
            let paths: BTreeSet<String> =
                cands.iter().map(|c| c.candidate.path.clone()).collect();
            for (i, d) in devs.iter().enumerate() {
                let present = paths.contains(&format!("p{i}.rs"));
                proptest::prop_assert_eq!(present, d.is_none(), "out-of-scope path leaked into ranking");
            }
        }
    }
}

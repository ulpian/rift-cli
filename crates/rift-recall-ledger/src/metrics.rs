//! Graph-backed, scope-bounded repeated/avoidable-context metrics (Stage 3 Phase
//! 2 Step 5 — *measure before optimize*).
//!
//! These are the same two numbers the in-memory [`crate::RecallMetrics`] reports,
//! but computed over the durable recall graph and routed through the scope
//! chokepoint ([`crate::scope_query::in_scope_nodes`]) so they honor W14
//! containment. A `repeated` read is the 2nd..Nth read of an already-seen
//! `path@hash` (a `FileRevision`); `avoidable` additionally counts reads of
//! content identical to the current worktree (servable as a delta/handle).

use std::collections::{BTreeMap, BTreeSet};

use rift_core::Result;
use rift_recall_types::Scope;
use rift_store::{CanonicalStore, Value};
use sha2::{Digest, Sha256};

use crate::graph::sha256_id;
use crate::scope_query::in_scope_nodes;

/// The scoped delta-ledger metrics (a superset of [`crate::RecallMetrics`] with
/// the scope it was computed over).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ContextMetrics {
    /// A stable hash of the scope these metrics were computed over.
    pub scope_hash: String,
    /// Σ input tokens over in-scope reads.
    pub total_input_tokens: u64,
    /// Input tokens spent re-reading an already-seen `path@hash`.
    pub repeated_read_tokens: u64,
    /// Repeated + unchanged-vs-worktree (servable as a delta).
    pub avoidable_tokens: u64,
    /// `repeated_read_tokens / total_input_tokens`.
    pub repeated_context_rate: f64,
    /// `avoidable_tokens / total_input_tokens`.
    pub avoidable_context_rate: f64,
}

/// A stable hash of a scope (for the `metric_snapshot.scope_hash` column).
#[must_use]
pub fn scope_hash(scope: &Scope) -> String {
    let canon = format!(
        "repo={};branch={:?};dev={:?};win={:?};cross={:?}",
        scope.repo_id, scope.branch, scope.developer, scope.checkpoints, scope.cross
    );
    let mut h = Sha256::new();
    h.update(canon.as_bytes());
    let out = h.finalize();
    let mut s = String::with_capacity(64);
    use core::fmt::Write as _;
    for b in out {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn ratio(n: u64, d: u64) -> f64 {
    if d == 0 {
        0.0
    } else {
        n as f64 / d as f64
    }
}

/// Compute the scoped metrics over the recall graph. `worktree` maps repo-relative
/// path → current SHA-256 (from the worktree hasher); a read whose content equals
/// the current worktree content is `avoidable` (servable as `known_unchanged`).
///
/// # Errors
/// Propagates store read errors.
pub async fn compute_metrics(
    store: &impl CanonicalStore,
    scope: &Scope,
    worktree: &BTreeMap<String, String>,
) -> Result<ContextMetrics> {
    let scope_set = in_scope_nodes(store, scope).await?;
    let rows = store
        .query(
            "SELECT r.checkpoint_id, r.file_revision, fr.path, fr.content_sha256, \
                    r.input_tokens, r.read_ordinal \
             FROM n_file_read r \
             JOIN n_file_revision fr ON fr.node_id = r.file_revision \
             ORDER BY r.read_ordinal",
            &[],
        )
        .await?;

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let (mut total, mut repeated, mut avoidable) = (0u64, 0u64, 0u64);
    for row in &rows {
        let checkpoint_id = row.get_text(0)?;
        if !scope_set.contains(&checkpoint_id) {
            continue; // scope containment — out-of-scope reads never contribute
        }
        let file_rev = row.get_text(1)?;
        let path = row.get_text(2)?;
        let sha = row.get_text(3)?;
        let tokens = u64::try_from(row.get_i64(4)?).unwrap_or(0);

        total += tokens;
        let is_repeat = !seen.insert(file_rev);
        let unchanged = worktree.get(&path).is_some_and(|h| h == &sha);
        if is_repeat {
            repeated += tokens;
        }
        if is_repeat || unchanged {
            avoidable += tokens;
        }
    }

    Ok(ContextMetrics {
        scope_hash: scope_hash(scope),
        total_input_tokens: total,
        repeated_read_tokens: repeated,
        avoidable_tokens: avoidable,
        repeated_context_rate: ratio(repeated, total),
        avoidable_context_rate: ratio(avoidable, total),
    })
}

/// Append a `metric_snapshot` trend row (the pre-sale baseline artifact). The id
/// is content-addressed over the scope + values + time, so re-recording the same
/// snapshot is an idempotent no-op.
///
/// # Errors
/// Propagates store write errors.
pub async fn record_metric_snapshot(
    store: &impl CanonicalStore,
    m: &ContextMetrics,
    computed_at: i64,
) -> Result<()> {
    let id = sha256_id(
        "rift.metric.v1",
        &[
            m.scope_hash.as_bytes(),
            &m.total_input_tokens.to_le_bytes(),
            &m.repeated_read_tokens.to_le_bytes(),
            &m.avoidable_tokens.to_le_bytes(),
            &computed_at.to_le_bytes(),
        ],
    );
    store
        .insert_or_ignore(
            "metric_snapshot",
            &[
                "snapshot_id",
                "scope_hash",
                "total_input_tokens",
                "repeated_read_tokens",
                "avoidable_tokens",
                "computed_at",
            ],
            &[
                Value::text(id),
                Value::text(&m.scope_hash),
                Value::Int(i64::try_from(m.total_input_tokens).unwrap_or(i64::MAX)),
                Value::Int(i64::try_from(m.repeated_read_tokens).unwrap_or(i64::MAX)),
                Value::Int(i64::try_from(m.avoidable_tokens).unwrap_or(i64::MAX)),
                Value::Int(computed_at),
            ],
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::file_revision_id;
    use rift_store::{recall_migrations, SqliteStore};

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

    /// Insert a file read of `path@sha` for `tokens` at `ordinal` in `checkpoint`.
    fn put_read(
        store: &SqliteStore,
        checkpoint: &str,
        path: &str,
        sha: &str,
        tokens: i64,
        ordinal: i64,
    ) {
        let fr = file_revision_id(path, sha);
        block(store.insert_or_ignore(
            "n_file_revision",
            &["node_id", "path", "blob_oid", "content_sha256", "size"],
            &[
                Value::text(&fr),
                Value::text(path),
                Value::text(sha),
                Value::text(sha),
                Value::Int(0),
            ],
        ))
        .unwrap();
        let rid = format!("read-{checkpoint}-{ordinal}");
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
                Value::text(&rid),
                Value::text(checkpoint),
                Value::text("tc"),
                Value::text(&fr),
                Value::Int(tokens),
                Value::Int(ordinal),
                Value::Null,
                Value::Null,
            ],
        ))
        .unwrap();
    }

    #[test]
    fn second_read_of_same_hash_is_repeated() {
        let store = recall_store();
        put_checkpoint(&store, "c1", 1, None);
        put_read(&store, "c1", "a.rs", "h1", 100, 1);
        put_read(&store, "c1", "a.rs", "h1", 100, 2); // repeat
        let scope = Scope::default_for("repo", u32::MAX);
        let m = block(compute_metrics(&store, &scope, &BTreeMap::new())).unwrap();
        assert_eq!(m.total_input_tokens, 200);
        assert_eq!(m.repeated_read_tokens, 100);
        assert!((m.repeated_context_rate - 0.5).abs() < 1e-9);
    }

    #[test]
    fn snapshot_is_recorded_and_idempotent() {
        let store = recall_store();
        put_checkpoint(&store, "c1", 1, None);
        put_read(&store, "c1", "a.rs", "h1", 100, 1);
        let scope = Scope::default_for("repo", u32::MAX);
        let m = block(compute_metrics(&store, &scope, &BTreeMap::new())).unwrap();
        block(record_metric_snapshot(&store, &m, 0)).unwrap();
        block(record_metric_snapshot(&store, &m, 0)).unwrap(); // idempotent
        assert_eq!(block(store.count("metric_snapshot")).unwrap(), 1);
    }

    proptest::proptest! {
        /// `repeated ≤ avoidable ≤ total`, so both rates are in [0,1].
        #[test]
        fn repeated_le_avoidable_le_total(
            toks in proptest::collection::vec(1i64..500, 0..20)
        ) {
            let store = recall_store();
            put_checkpoint(&store, "c1", 1, None);
            for (i, t) in toks.iter().enumerate() {
                // alternate two paths so some reads repeat
                let p = if i % 2 == 0 { "a.rs" } else { "b.rs" };
                put_read(&store, "c1", p, "h", *t, i as i64 + 1);
            }
            // mark a.rs unchanged vs worktree so it contributes to avoidable
            let mut wt = BTreeMap::new();
            wt.insert("a.rs".to_string(), "h".to_string());
            let scope = Scope::default_for("repo", u32::MAX);
            let m = block(compute_metrics(&store, &scope, &wt)).unwrap();
            proptest::prop_assert!(m.repeated_read_tokens <= m.avoidable_tokens);
            proptest::prop_assert!(m.avoidable_tokens <= m.total_input_tokens);
            proptest::prop_assert!((0.0..=1.0).contains(&m.repeated_context_rate));
            proptest::prop_assert!((0.0..=1.0).contains(&m.avoidable_context_rate));
        }

        /// Metrics are invariant to the presence of out-of-scope (other-developer)
        /// data — the W14 containment guarantee at the metric layer.
        #[test]
        fn metrics_respect_scope_containment(extra in 1i64..400) {
            let store = recall_store();
            put_checkpoint(&store, "mine", 1, None);
            put_read(&store, "mine", "a.rs", "h1", 100, 1);
            let scope = Scope::default_for("repo", u32::MAX);
            let before = block(compute_metrics(&store, &scope, &BTreeMap::new())).unwrap();
            // add another developer's checkpoint + reads (out of scope)
            put_checkpoint(&store, "theirs", 2, Some("mallory"));
            put_read(&store, "theirs", "secret.rs", "hx", extra, 2);
            let after = block(compute_metrics(&store, &scope, &BTreeMap::new())).unwrap();
            proptest::prop_assert_eq!(before.total_input_tokens, after.total_input_tokens);
            proptest::prop_assert_eq!(before.avoidable_tokens, after.avoidable_tokens);
        }
    }
}

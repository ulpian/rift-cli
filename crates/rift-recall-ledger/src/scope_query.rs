//! The scope chokepoint (Stage 3 Phase 2 Step 6, W14 containment).
//!
//! [`in_scope_nodes`] is the **single** function every recall query funnels
//! through to bound itself to the current developer / last-N checkpoints. It
//! returns the in-scope checkpoint-id set; callers filter their rows by
//! [`ScopeSet::contains`], so no query can escape its declared scope without an
//! explicit [`rift_recall_types::CrossScopeGrant`].
//!
//! v1 scope axes: **developer** (`n_checkpoint.developer`; `NULL` = the current
//! developer) and the **checkpoint window** (last-N by walk ordinal). The branch
//! and repo axes exist in [`Scope`] but are no-ops here until the gix ancestry
//! walk lands (the whole `recall.db` is one repo); they never *widen* on
//! uncertainty.

use std::collections::BTreeSet;

use rift_core::Result;
use rift_recall_types::{CheckpointWindow, DeveloperScope, Scope};
use rift_store::CanonicalStore;

/// The resolved in-scope membership: the set of in-scope checkpoint ids.
#[derive(Clone, Debug, Default)]
pub struct ScopeSet {
    checkpoints: BTreeSet<String>,
}

impl ScopeSet {
    /// Whether `checkpoint_id` is in scope.
    #[must_use]
    pub fn contains(&self, checkpoint_id: &str) -> bool {
        self.checkpoints.contains(checkpoint_id)
    }

    /// Whether the scope is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
    }

    /// The number of in-scope checkpoints.
    #[must_use]
    pub fn len(&self) -> usize {
        self.checkpoints.len()
    }

    /// The in-scope checkpoint ids.
    #[must_use]
    pub fn checkpoint_ids(&self) -> &BTreeSet<String> {
        &self.checkpoints
    }
}

/// Whether a checkpoint owned by `dev` (`None` = current/unknown) is in scope.
fn developer_in_scope(scope: &Scope, dev: Option<&str>) -> bool {
    if scope.cross.developers() {
        return true; // explicit cross-developer grant
    }
    match &scope.developer {
        DeveloperScope::CurrentOnly => dev.is_none(),
        DeveloperScope::Specific(d) => dev == Some(d.as_str()),
    }
}

/// Resolve a [`Scope`] to its in-scope checkpoint set — the one membership
/// chokepoint. Applies the developer filter, then the last-N window (by ordinal).
///
/// # Errors
/// Propagates store read errors.
pub async fn in_scope_nodes(store: &impl CanonicalStore, scope: &Scope) -> Result<ScopeSet> {
    let rows = store
        .query(
            "SELECT node_id, ordinal, developer FROM n_checkpoint ORDER BY ordinal DESC",
            &[],
        )
        .await?;
    let mut kept: Vec<String> = Vec::new();
    for r in &rows {
        let id = r.get_text(0)?;
        let developer = r.get_opt_text(2)?;
        if developer_in_scope(scope, developer.as_deref()) {
            kept.push(id);
        }
    }
    // `kept` is already ordered newest-first; the window takes the most recent.
    let checkpoints: BTreeSet<String> = match scope.checkpoints {
        CheckpointWindow::LastN(n) => kept.into_iter().take(n as usize).collect(),
        CheckpointWindow::All => kept.into_iter().collect(),
    };
    Ok(ScopeSet { checkpoints })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_types::{CrossScopeGrant, DeveloperScope};
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

    #[test]
    fn current_developer_scope_excludes_other_developers() {
        let store = recall_store();
        put_checkpoint(&store, "mine", 1, None);
        put_checkpoint(&store, "theirs", 2, Some("mallory"));
        let scope = Scope::default_for("repo", 50);
        let set = block(in_scope_nodes(&store, &scope)).unwrap();
        assert!(set.contains("mine"));
        assert!(!set.contains("theirs"), "another developer is out of scope");
    }

    #[test]
    fn last_n_window_limits_checkpoints() {
        let store = recall_store();
        for i in 1..=5 {
            put_checkpoint(&store, &format!("c{i}"), i, None);
        }
        let mut scope = Scope::default_for("repo", 2);
        scope.developer = DeveloperScope::CurrentOnly;
        let set = block(in_scope_nodes(&store, &scope)).unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains("c5") && set.contains("c4"));
    }

    #[test]
    fn cross_developer_grant_widens_axis() {
        let store = recall_store();
        put_checkpoint(&store, "mine", 1, None);
        put_checkpoint(&store, "theirs", 2, Some("mallory"));
        let mut scope = Scope::default_for("repo", 50);
        scope.cross = CrossScopeGrant::grant("review", false, true, false);
        let set = block(in_scope_nodes(&store, &scope)).unwrap();
        assert!(set.contains("mine") && set.contains("theirs"));
    }

    proptest::proptest! {
        /// W14 keystone: over random multi-developer ledgers, a default
        /// (current-developer) scope never admits a checkpoint owned by another
        /// developer. `05-security §5.5`.
        #[test]
        fn query_never_returns_out_of_scope_rows(
            devs in proptest::collection::vec(proptest::option::of("dev[0-9]"), 1..15)
        ) {
            let store = recall_store();
            for (i, d) in devs.iter().enumerate() {
                put_checkpoint(&store, &format!("c{i}"), i as i64 + 1, d.as_deref());
            }
            let scope = Scope::default_for("repo", u32::MAX); // window-unbounded so only the dev axis filters
            let set = block(in_scope_nodes(&store, &scope)).unwrap();
            // every admitted checkpoint must be a current-developer (None) one
            for (i, d) in devs.iter().enumerate() {
                let admitted = set.contains(&format!("c{i}"));
                proptest::prop_assert_eq!(admitted, d.is_none());
            }
        }
    }
}

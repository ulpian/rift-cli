//! P3 S4 — the archival executor: decision → condense-or-record, serialized per
//! repo and deferred off the git critical path.
//!
//! A `true` [`ArchiveDecision`] runs [`condense`](crate::condense); a `false` one
//! still records an append-only `archive_decision` (the decision happened, even if
//! it was a no-op). Archival is **serialized per repo** by [`RepoSerializer`] so
//! two fast commits don't race two condensations of overlapping state, and the
//! git operation returns immediately because the caller (the hook entrypoint)
//! spawns this off the critical path.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rift_core::Result;
use rift_shadow_hooks::strategy::ArchiveDecision;
use rift_store::{record_archive_decision, ArchiveDecisionRow, CanonicalStore};
use tokio::sync::Mutex as AsyncMutex;

use crate::backend::ShadowBackend;
use crate::condense::{condense, decision_id, CheckpointId};

/// Run one archive decision. On `archive`, condenses and returns the new
/// checkpoint id; otherwise records the no-op decision and returns `None`.
///
/// # Errors
/// [`rift_core::RiftError`] if condense or the audit insert fails.
#[allow(clippy::too_many_arguments)]
pub async fn run_decision(
    backend: &impl ShadowBackend,
    store: &impl CanonicalStore,
    repo: &Path,
    session: &str,
    raw_capture: &rift_core::SecretBytes,
    decision: ArchiveDecision,
    commit_oid: &str,
    now_ms: i64,
    read_facts: Vec<crate::backend::ArchivedReadFact>,
) -> Result<Option<CheckpointId>> {
    if decision.archive {
        let id = condense(
            backend,
            store,
            repo,
            session,
            raw_capture,
            decision.strategy,
            decision.reason,
            commit_oid,
            now_ms,
            read_facts,
        )
        .await?;
        Ok(Some(id))
    } else {
        // A no-op decision is still an append-only fact (the decision happened).
        record_archive_decision(
            store,
            &ArchiveDecisionRow {
                decision_id: decision_id(session, decision.strategy, decision.reason, now_ms, None),
                session_id: Some(session.to_owned()),
                strategy: decision.strategy.as_str().to_owned(),
                reason: decision.reason.as_str().to_owned(),
                archived: false,
                checkpoint_id: None,
                decided_at: now_ms,
                schema_version: 1,
            },
        )
        .await?;
        Ok(None)
    }
}

/// Serializes archival per repo: two condensations for the same repo never
/// overlap (bounded by a per-repo async lock), while different repos proceed
/// concurrently. Cheap to clone (shared state behind an `Arc`).
#[derive(Default, Clone)]
pub struct RepoSerializer {
    locks: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
}

impl RepoSerializer {
    /// A fresh serializer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The per-repo lock (created on first use). A poisoned map is recovered, not
    /// panicked on — capture must never abort a developer's git op.
    fn lock_for(&self, repo_key: &str) -> Arc<AsyncMutex<()>> {
        let mut map = self
            .locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.entry(repo_key.to_owned()).or_default().clone()
    }

    /// Run `f` serialized against other work for the same `repo_key`.
    pub async fn run<F, Fut, T>(&self, repo_key: &str, f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let lock = self.lock_for(repo_key);
        let _guard = lock.lock().await;
        f().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::EntireBackend;
    use rift_shadow_entire::MockRunner;
    use rift_shadow_hooks::strategy::{ArchiveReason, StrategyKind};
    use rift_store::SqliteStore;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    #[tokio::test]
    async fn no_archive_decision_is_still_recorded() {
        let s = store();
        let runner = MockRunner::default();
        let backend = EntireBackend::new(&runner);
        let decision = ArchiveDecision {
            archive: false,
            reason: ArchiveReason::Commit,
            strategy: StrategyKind::ManualCommit,
        };
        let raw = rift_core::SecretBytes::new(Vec::new());
        let out = run_decision(
            &backend,
            &s,
            Path::new("/r"),
            "s",
            &raw,
            decision,
            "abc",
            1,
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(out.is_none());
        assert_eq!(s.count("archive_decision").await.unwrap(), 1);
        assert!(
            runner.calls.borrow().is_empty(),
            "no archive → no substrate call"
        );
    }

    #[tokio::test]
    async fn archive_decision_condenses_and_returns_id() {
        let s = store();
        let runner = MockRunner::default();
        let backend = EntireBackend::new(&runner);
        let decision = ArchiveDecision {
            archive: true,
            reason: ArchiveReason::Commit,
            strategy: StrategyKind::ManualCommit,
        };
        let raw = rift_core::SecretBytes::new(b"capture".to_vec());
        let id = run_decision(
            &backend,
            &s,
            Path::new("/r"),
            "s",
            &raw,
            decision,
            "abc123",
            1,
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(id.is_some());
        assert_eq!(runner.calls.borrow().len(), 1);
    }

    #[tokio::test]
    async fn serializer_prevents_overlap_for_same_repo() {
        let ser = RepoSerializer::new();
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));

        let job = |ser: RepoSerializer, inf: Arc<AtomicUsize>, mx: Arc<AtomicUsize>| async move {
            ser.run("same-repo", || async {
                let cur = inf.fetch_add(1, Ordering::SeqCst) + 1;
                mx.fetch_max(cur, Ordering::SeqCst);
                // give the scheduler a chance to interleave if NOT serialized
                tokio::task::yield_now().await;
                inf.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        };

        tokio::join!(
            job(ser.clone(), in_flight.clone(), max_seen.clone()),
            job(ser.clone(), in_flight.clone(), max_seen.clone()),
        );
        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            1,
            "same repo runs serialized"
        );
    }
}

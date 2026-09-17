//! Persist live commit diff-capture, and the post-commit archive+capture.
//!
//! Two pieces the bin's post-commit hook composes:
//!
//! [`persist_commit_capture`] writes the durable `C`-diff facts (`file_revision` +
//! `changed_hunk`, always) and the per-file `attribution` rollup (only when a
//! checkpoint anchors it). All writers are append-only + idempotent, so a re-fired
//! hook on the same commit is a no-op.
//!
//! [`archive_and_capture`] — when a strategy decides to archive — runs the live
//! backend archive ([`run_decision`]: the real
//! pinned-binary spawn behind the [`ShadowBackend`] seam) and persists the commit
//! capture keyed by the produced checkpoint.
//!
//! The diff bytes read from git here are content for *counts* (hunk shapes,
//! line tallies) — never a durable raw transcript; durable session bytes still
//! pass the one-way redaction gate in [`condense`](fn@crate::condense). Attribution at
//! the commit boundary is `unknown` (no per-turn writer history yet); the durable
//! `file_revision`/`changed_hunk` facts are exact regardless.

use std::path::Path;

use rift_core::{Result, SecretBytes};
use rift_git::BlobReader;
use rift_shadow_attribution::{capture_commit, CommitCapture};
use rift_shadow_hooks::strategy::ArchiveDecision;
use rift_store::{
    confidence_to_milli, record_attribution, record_attribution_hunk, record_changed_hunk,
    record_file_revision, AttributionHunkRow, AttributionRow, CanonicalStore, ChangedHunkRow,
    FileRevisionRow,
};

use crate::backend::ShadowBackend;
use crate::condense::CheckpointId;
use crate::executor::run_decision;

/// Persist a commit's diff-capture. Always writes `file_revision` + `changed_hunk`
/// (the exact `C`-diff structure, keyed by content so re-capture is a no-op). When
/// `checkpoint_id` is `Some`, also writes the per-file `attribution` rollup anchored
/// to that checkpoint. Returns the number of changed files persisted.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn persist_commit_capture(
    store: &impl CanonicalStore,
    capture: &CommitCapture,
    checkpoint_id: Option<&str>,
) -> Result<usize> {
    for f in &capture.files {
        let file_revision_id = f.head_revision.sha256().to_hex();
        record_file_revision(
            store,
            &FileRevisionRow {
                file_revision_id: file_revision_id.clone(),
                path: f.path.clone(),
                blob_oid: f.head_blob_oid.clone(),
                content_sha256: f.head_content_sha256.to_hex(),
                schema_version: 1,
            },
        )
        .await?;

        for hunk in &f.hunks {
            record_changed_hunk(
                store,
                &ChangedHunkRow {
                    hunk_id: f.hunk_id(&capture.commit_id, hunk).to_hex(),
                    commit_id: capture.commit_id.clone(),
                    file_revision_id: file_revision_id.clone(),
                    old_start: i64::from(hunk.old_start),
                    old_len: i64::from(hunk.old_len),
                    new_start: i64::from(hunk.new_start),
                    new_len: i64::from(hunk.new_len),
                    schema_version: 1,
                },
            )
            .await?;
        }

        if let Some(ckpt) = checkpoint_id {
            let a = &f.attribution;
            record_attribution(
                store,
                &AttributionRow {
                    checkpoint_id: ckpt.to_owned(),
                    file_revision_id: file_revision_id.clone(),
                    agent_added: i64::from(a.agent_lines),
                    human_added: i64::from(a.human_lines),
                    human_modified_agent: i64::from(a.human_modified_agent_lines),
                    agent_removed_by_human: i64::from(a.agent_removed_by_human),
                    confidence_milli: confidence_to_milli(f.confidence),
                    method_version: f.method_version.clone(),
                    supersedes_pk: None,
                    schema_version: 1,
                },
            )
            .await?;
            // The per-hunk detail the rollup sums (present once the turn
            // replay has enriched the capture).
            for hc in &f.hunk_categories {
                record_attribution_hunk(
                    store,
                    &AttributionHunkRow {
                        checkpoint_id: ckpt.to_owned(),
                        file_revision_id: file_revision_id.clone(),
                        hunk_ordinal: i64::from(hc.hunk_ordinal),
                        new_start: i64::from(hc.new_start),
                        new_len: i64::from(hc.new_len),
                        category: hc.category.clone(),
                        line_count: i64::from(hc.line_count),
                        confidence_milli: confidence_to_milli(f.confidence),
                        method_version: f.method_version.clone(),
                        schema_version: 1,
                    },
                )
                .await?;
            }
        }
    }
    Ok(capture.files.len())
}

/// Run the post-commit archive decision **and** persist the commit's diff-capture.
///
/// On `decision.archive`, runs [`run_decision`] (the live backend archive — the real
/// pinned-binary spawn behind the [`ShadowBackend`] seam) to produce a checkpoint,
/// then persists the `C`-diff capture (read through `reader`) anchored to it. On a
/// no-op decision, still records the durable `file_revision`/`changed_hunk` facts
/// (no checkpoint to anchor attribution to). Returns the checkpoint id if one was
/// produced.
///
/// # Errors
/// [`rift_core::RiftError`] if the archive, a blob read, or an insert fails.
#[allow(clippy::too_many_arguments)]
pub async fn archive_and_capture(
    reader: &impl BlobReader,
    backend: &impl ShadowBackend,
    store: &impl CanonicalStore,
    repo: &Path,
    commit_hex: &str,
    session: &str,
    raw_capture: &SecretBytes,
    decision: ArchiveDecision,
    now_ms: i64,
    packs: &rift_shadow_redact::RulePackSet,
) -> Result<Option<CheckpointId>> {
    // Compute the read-only capture first, so a backend archive failure never costs
    // us the durable `C`-diff facts.
    let capture = capture_commit(reader, commit_hex)?;
    archive_and_capture_with(
        &capture,
        backend,
        store,
        repo,
        commit_hex,
        session,
        raw_capture,
        decision,
        now_ms,
        Vec::new(),
        packs,
        // This convenience wrapper's callers carry no session cache to find a
        // transcript through; the worker path (`run_archive`) threads one —
        // and the same is true of subagent tasks.
        None,
        Vec::new(),
    )
    .await
}

/// [`archive_and_capture`] over a **precomputed** [`CommitCapture`] — for callers
/// that already diffed the commit (e.g. to evaluate the sensitive-path trigger)
/// and must not pay for the diff twice.
///
/// # Errors
/// [`rift_core::RiftError`] if the archive or an insert fails.
#[allow(clippy::too_many_arguments)]
pub async fn archive_and_capture_with(
    capture: &CommitCapture,
    backend: &impl ShadowBackend,
    store: &impl CanonicalStore,
    repo: &Path,
    commit_hex: &str,
    session: &str,
    raw_capture: &SecretBytes,
    decision: ArchiveDecision,
    now_ms: i64,
    read_facts: Vec<crate::backend::ArchivedReadFact>,
    packs: &rift_shadow_redact::RulePackSet,
    transcript: Option<crate::backend::ArchivedTranscript>,
    tasks: Vec<crate::backend::ArchivedTask>,
) -> Result<Option<CheckpointId>> {
    let archive = run_decision(
        backend,
        store,
        repo,
        session,
        raw_capture,
        decision,
        commit_hex,
        now_ms,
        read_facts,
        packs,
        transcript,
        tasks,
    )
    .await;
    // Persist the diff facts regardless; anchor attribution to the checkpoint only
    // if the archive produced one. Then surface any archive error to the caller
    // (swallowed at the never-block hook boundary) — the capture is already durable.
    let checkpoint = match &archive {
        Ok(c) => c.clone(),
        Err(_) => None,
    };
    persist_commit_capture(store, capture, checkpoint.as_deref()).await?;
    archive
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{CheckpointArchive, ShadowBackend};
    use rift_git::{MemoryBlobReader, TreeEntry};
    use rift_shadow_hooks::strategy::{ArchiveReason, StrategyKind};
    use rift_store::SqliteStore;

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    /// A backend that counts the archives it was asked for.
    #[derive(Default)]
    struct CountingBackend {
        calls: std::cell::RefCell<usize>,
    }
    impl ShadowBackend for CountingBackend {
        fn archive_checkpoint(&self, _archive: &CheckpointArchive) -> Result<()> {
            *self.calls.borrow_mut() += 1;
            Ok(())
        }
    }

    fn reader() -> MemoryBlobReader {
        MemoryBlobReader::new()
            .with_blob("A0", b"one\ntwo\n")
            .with_blob("A1", b"one\nTWO\n")
            .with_tree(
                "c0",
                vec![TreeEntry {
                    path: "a.txt".into(),
                    blob_oid: "A0".into(),
                }],
            )
            .with_tree(
                "c1",
                vec![TreeEntry {
                    path: "a.txt".into(),
                    blob_oid: "A1".into(),
                }],
            )
            .with_parent("c1", "c0")
    }

    #[tokio::test]
    async fn persist_writes_file_revision_and_hunk_idempotently() {
        let s = store();
        let capture = capture_commit(&reader(), "c1").unwrap();
        let n = persist_commit_capture(&s, &capture, None).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(s.count("file_revision").await.unwrap(), 1);
        assert_eq!(s.count("changed_hunk").await.unwrap(), 1);
        // no checkpoint → no attribution row
        assert_eq!(s.count("attribution").await.unwrap(), 0);
        // re-persisting the same commit is a true no-op (idempotent content keys)
        persist_commit_capture(&s, &capture, None).await.unwrap();
        assert_eq!(s.count("changed_hunk").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn persist_with_checkpoint_writes_attribution() {
        let s = store();
        let capture = capture_commit(&reader(), "c1").unwrap();
        persist_commit_capture(&s, &capture, Some("ckpt-abc"))
            .await
            .unwrap();
        assert_eq!(s.count("attribution").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn archive_and_capture_archives_and_persists_all() {
        let s = store();
        let backend = CountingBackend::default();
        let decision = ArchiveDecision {
            archive: true,
            reason: ArchiveReason::Commit,
            strategy: StrategyKind::ManualCommit,
        };
        let raw = SecretBytes::new(Vec::new());
        let ckpt = archive_and_capture(
            &reader(),
            &backend,
            &s,
            Path::new("/repo"),
            "c1",
            "01JSESSION",
            &raw,
            decision,
            1,
            &rift_shadow_redact::RulePackSet::builtin(),
        )
        .await
        .unwrap();
        assert!(ckpt.is_some(), "archive produced a checkpoint");
        // the backend was actually asked to write a shard
        assert_eq!(*backend.calls.borrow(), 1);
        // capture persisted + attribution anchored to the checkpoint
        assert_eq!(s.count("file_revision").await.unwrap(), 1);
        assert_eq!(s.count("changed_hunk").await.unwrap(), 1);
        assert_eq!(s.count("attribution").await.unwrap(), 1);
        assert_eq!(s.count("archive_decision").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn diff_facts_survive_a_backend_archive_failure() {
        /// A backend whose archive always fails (a git-write/integrity error).
        struct FailingBackend;
        impl ShadowBackend for FailingBackend {
            fn archive_checkpoint(&self, _archive: &CheckpointArchive) -> Result<()> {
                Err(rift_core::RiftError::storage("backend exploded"))
            }
        }

        let s = store();
        let decision = ArchiveDecision {
            archive: true,
            reason: ArchiveReason::Commit,
            strategy: StrategyKind::ManualCommit,
        };
        let raw = SecretBytes::new(Vec::new());
        let out = archive_and_capture(
            &reader(),
            &FailingBackend,
            &s,
            Path::new("/repo"),
            "c1",
            "01JSESSION",
            &raw,
            decision,
            1,
            &rift_shadow_redact::RulePackSet::builtin(),
        )
        .await;
        // The archive error is surfaced…
        assert!(out.is_err(), "backend failure propagates");
        // …but the durable C-diff facts were still recorded (capture-first).
        assert_eq!(s.count("file_revision").await.unwrap(), 1);
        assert_eq!(s.count("changed_hunk").await.unwrap(), 1);
        // no checkpoint → no attribution row
        assert_eq!(s.count("attribution").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn no_archive_decision_still_records_diff_facts() {
        let s = store();
        let backend = CountingBackend::default();
        let decision = ArchiveDecision {
            archive: false,
            reason: ArchiveReason::Commit,
            strategy: StrategyKind::ManualCommit,
        };
        let raw = SecretBytes::new(Vec::new());
        let ckpt = archive_and_capture(
            &reader(),
            &backend,
            &s,
            Path::new("/repo"),
            "c1",
            "01JSESSION",
            &raw,
            decision,
            1,
            &rift_shadow_redact::RulePackSet::builtin(),
        )
        .await
        .unwrap();
        assert!(ckpt.is_none(), "no-op decision → no checkpoint");
        assert_eq!(*backend.calls.borrow(), 0, "no archive → no shard written");
        // the C-diff facts are still durable (no attribution without a checkpoint)
        assert_eq!(s.count("file_revision").await.unwrap(), 1);
        assert_eq!(s.count("changed_hunk").await.unwrap(), 1);
        assert_eq!(s.count("attribution").await.unwrap(), 0);
    }
}

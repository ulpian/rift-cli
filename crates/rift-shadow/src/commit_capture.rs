//! Persist live commit diff-capture, and the post-commit archive+capture.
//!
//! Two pieces the bin's post-commit hook composes:
//!
//! [`persist_commit_capture`] writes the durable `C`-diff facts (`file_revision` +
//! `changed_hunk`, always) and the per-file `attribution` rollup (only when a
//! checkpoint anchors it). All writers are append-only + idempotent, so a re-fired
//! hook with the same completed attribution is a no-op. A changed or partial
//! existing attribution is refused without repairing historical rows.
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

use std::{collections::BTreeMap, path::Path};

use rift_core::{Result, SecretBytes};
use rift_git::BlobReader;
use rift_shadow_attribution::{capture_commit, CommitCapture};
use rift_shadow_hooks::strategy::ArchiveDecision;
use rift_store::{
    attribution_for_key, attribution_hunks_for_key, confidence_to_milli, record_attribution,
    record_attribution_hunk, record_changed_hunk, record_file_revision, AttributionHunkRow,
    AttributionRow, CanonicalStore, ChangedHunkRow, FileRevisionRow,
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
    let expected = expected_attributions(capture, checkpoint_id)?;
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
    }
    for (row, details) in expected.values() {
        persist_attribution(store, row, details).await?;
    }
    Ok(capture.files.len())
}

type ExpectedAttributions = BTreeMap<String, (AttributionRow, Vec<AttributionHunkRow>)>;

/// Detect contradictory duplicate keys before writing any capture facts.
fn expected_attributions(
    capture: &CommitCapture,
    checkpoint: Option<&str>,
) -> Result<ExpectedAttributions> {
    let mut expected = BTreeMap::new();
    let Some(checkpoint) = checkpoint else {
        return Ok(expected);
    };
    for f in &capture.files {
        let row = attribution_row(f, checkpoint);
        let mut details = BTreeMap::new();
        for h in &f.hunk_categories {
            let detail = AttributionHunkRow {
                checkpoint_id: checkpoint.to_owned(),
                file_revision_id: row.file_revision_id.clone(),
                hunk_ordinal: i64::from(h.hunk_ordinal),
                new_start: i64::from(h.new_start),
                new_len: i64::from(h.new_len),
                category: h.category.clone(),
                line_count: i64::from(h.line_count),
                confidence_milli: row.confidence_milli,
                method_version: row.method_version.clone(),
                schema_version: 1,
            };
            if let Some(prior) = details.insert(
                (detail.hunk_ordinal, detail.category.clone()),
                detail.clone(),
            ) {
                if prior != detail {
                    return Err(attribution_conflict(&row));
                }
            }
        }
        let value = (row.clone(), details.into_values().collect());
        if let Some(prior) = expected.insert(row.file_revision_id.clone(), value.clone()) {
            if prior != value {
                return Err(attribution_conflict(&row));
            }
        }
    }
    Ok(expected)
}

fn attribution_row(f: &rift_shadow_attribution::FileChange, checkpoint: &str) -> AttributionRow {
    let a = &f.attribution;
    AttributionRow {
        checkpoint_id: checkpoint.to_owned(),
        file_revision_id: f.head_revision.sha256().to_hex(),
        agent_added: i64::from(a.agent_lines),
        human_added: i64::from(a.human_lines),
        human_modified_agent: i64::from(a.human_modified_agent_lines),
        agent_removed_by_human: i64::from(a.agent_removed_by_human),
        confidence_milli: confidence_to_milli(f.confidence),
        method_version: f.method_version.clone(),
        supersedes_pk: None,
        schema_version: 1,
    }
}

fn attribution_conflict(row: &AttributionRow) -> rift_core::RiftError {
    rift_core::RiftError::storage(format!(
        "attribution conflict for checkpoint {} revision {}",
        row.checkpoint_id, row.file_revision_id
    ))
}

async fn validate_completed_attribution(
    store: &impl CanonicalStore,
    row: &AttributionRow,
    details: &[AttributionHunkRow],
) -> Result<()> {
    if attribution_for_key(store, &row.checkpoint_id, &row.file_revision_id)
        .await?
        .as_ref()
        != Some(row)
        || attribution_hunks_for_key(store, &row.checkpoint_id, &row.file_revision_id).await?
            != details
    {
        return Err(attribution_conflict(row));
    }
    Ok(())
}

async fn append_owned_attribution_details(
    store: &impl CanonicalStore,
    row: &AttributionRow,
    details: &[AttributionHunkRow],
) -> Result<()> {
    for detail in details {
        if !record_attribution_hunk(store, detail).await? {
            return Err(attribution_conflict(row));
        }
    }
    validate_completed_attribution(store, row, details).await
}

async fn persist_attribution(
    store: &impl CanonicalStore,
    row: &AttributionRow,
    details: &[AttributionHunkRow],
) -> Result<()> {
    let prior = attribution_for_key(store, &row.checkpoint_id, &row.file_revision_id).await?;
    if prior.is_none()
        && !attribution_hunks_for_key(store, &row.checkpoint_id, &row.file_revision_id)
            .await?
            .is_empty()
    {
        return Err(attribution_conflict(row));
    }
    if record_attribution(store, row).await? {
        append_owned_attribution_details(store, row, details).await
    } else {
        validate_completed_attribution(store, row, details).await
    }
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
        crate::condense::SessionFacts::default(),
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
    facts: crate::condense::SessionFacts,
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
        facts,
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

    fn recovery_capture(agent: bool) -> CommitCapture {
        use rift_shadow_attribution::{HunkAttribution, LineAttribution};
        let mut capture = capture_commit(&reader(), "c1").unwrap();
        let f = &mut capture.files[0];
        f.attribution = if agent {
            LineAttribution {
                agent_lines: 1,
                ..Default::default()
            }
        } else {
            LineAttribution {
                unknown_lines: 1,
                ..Default::default()
            }
        };
        f.hunk_categories = vec![HunkAttribution {
            hunk_ordinal: 0,
            new_start: 1,
            new_len: 1,
            category: if agent { "agent_added" } else { "unknown" }.into(),
            line_count: 1,
        }];
        f.method_version = if agent {
            "turn-replay@1"
        } else {
            "commit-diff@1"
        }
        .into();
        capture
    }

    async fn recovery_relations(s: &SqliteStore) -> String {
        use rift_store::CanonicalStore;
        let rollups = s
            .query(
                "SELECT * FROM attribution ORDER BY checkpoint_id, file_revision_id",
                &[],
            )
            .await
            .unwrap();
        let details = s
            .query(
                "SELECT * FROM attribution_hunk ORDER BY hunk_ordinal, category",
                &[],
            )
            .await
            .unwrap();
        format!("rollups={rollups:?}; details={details:?}")
    }

    #[tokio::test]
    async fn recovery_retry_cannot_mix_an_old_agent_rollup_with_unknown_detail() {
        let s = store();
        let original = recovery_capture(true);
        persist_commit_capture(&s, &original, Some("fixed-checkpoint"))
            .await
            .unwrap();
        let before = recovery_relations(&s).await;
        persist_commit_capture(&s, &original, Some("fixed-checkpoint"))
            .await
            .unwrap();
        assert_eq!(
            recovery_relations(&s).await,
            before,
            "identical retry must be a true no-op"
        );
        let retry =
            persist_commit_capture(&s, &recovery_capture(false), Some("fixed-checkpoint")).await;
        let after = recovery_relations(&s).await;
        eprintln!("same-key recovery: result={retry:?}; {after}");
        assert!(
            retry.is_err(),
            "conflicting retry must be refused before mixing immutable facts: {after}"
        );
        assert_eq!(
            after, before,
            "a refused retry must preserve both original relations"
        );
    }

    #[tokio::test]
    async fn recovery_state_interleaving_refuses_a_loser_until_the_owner_completes() {
        let s = store();
        let capture = recovery_capture(true);
        let expected = expected_attributions(&capture, Some("fixed-checkpoint")).unwrap();
        let (row, details) = expected.values().next().unwrap();
        assert!(
            record_attribution(&s, row).await.unwrap(),
            "winner owns fresh rollup"
        );
        // Pause the winner before details; the loser uses the actual public writer.
        assert!(
            persist_commit_capture(&s, &capture, Some("fixed-checkpoint"))
                .await
                .is_err()
        );
        assert_eq!(s.count("attribution_hunk").await.unwrap(), 0);
        append_owned_attribution_details(&s, row, details)
            .await
            .unwrap();
        let completed = recovery_relations(&s).await;
        persist_commit_capture(&s, &capture, Some("fixed-checkpoint"))
            .await
            .unwrap();
        assert_eq!(recovery_relations(&s).await, completed);
    }

    #[tokio::test]
    async fn recovery_same_totals_with_different_details_are_not_an_identical_retry() {
        let s = store();
        let capture = recovery_capture(true);
        persist_commit_capture(&s, &capture, Some("fixed-checkpoint"))
            .await
            .unwrap();
        let before = recovery_relations(&s).await;
        let mut changed = capture.clone();
        changed.files[0].hunk_categories[0].category = "unknown".into();
        assert!(
            persist_commit_capture(&s, &changed, Some("fixed-checkpoint"))
                .await
                .is_err()
        );
        assert_eq!(recovery_relations(&s).await, before);
    }

    #[tokio::test]
    async fn recovery_orphan_or_mixed_details_are_never_completed_or_repaired() {
        for state in ["orphan", "partial", "mixed"] {
            let s = store();
            let capture = recovery_capture(true);
            let expected = expected_attributions(&capture, Some("fixed-checkpoint")).unwrap();
            let (row, details) = expected.values().next().unwrap();
            if state != "orphan" {
                record_attribution(&s, row).await.unwrap();
            }
            if state != "partial" {
                record_attribution_hunk(&s, &details[0]).await.unwrap();
            }
            if state == "mixed" {
                let mut extra = details[0].clone();
                extra.category = "unknown".into();
                record_attribution_hunk(&s, &extra).await.unwrap();
            }
            let before = recovery_relations(&s).await;
            assert!(
                persist_commit_capture(&s, &capture, Some("fixed-checkpoint"))
                    .await
                    .is_err()
            );
            assert_eq!(recovery_relations(&s).await, before);
        }
    }

    #[tokio::test]
    async fn recovery_fresh_owner_refuses_colliding_or_extra_details() {
        for extra_category in [false, true] {
            let s = store();
            let expected =
                expected_attributions(&recovery_capture(true), Some("fixed-checkpoint")).unwrap();
            let (row, details) = expected.values().next().unwrap();
            assert!(record_attribution(&s, row).await.unwrap());
            let mut orphan = details[0].clone();
            if extra_category {
                orphan.category = "unknown".into();
            } else {
                orphan.line_count = 99;
            }
            record_attribution_hunk(&s, &orphan).await.unwrap();
            assert!(append_owned_attribution_details(&s, row, details)
                .await
                .is_err());
            assert_eq!(
                attribution_for_key(&s, &row.checkpoint_id, &row.file_revision_id)
                    .await
                    .unwrap(),
                Some(row.clone())
            );
            assert!(
                attribution_hunks_for_key(&s, &row.checkpoint_id, &row.file_revision_id)
                    .await
                    .unwrap()
                    .contains(&orphan)
            );
        }
    }

    #[tokio::test]
    async fn recovery_conflicting_duplicate_inputs_fail_before_capture_writes() {
        for duplicate_detail in [false, true] {
            let s = store();
            let mut capture = recovery_capture(true);
            if duplicate_detail {
                let mut hunk = capture.files[0].hunk_categories[0].clone();
                hunk.line_count = 99;
                capture.files[0].hunk_categories.push(hunk);
            } else {
                let mut other = capture.files[0].clone();
                other.attribution.agent_lines = 99;
                capture.files.push(other);
            }
            assert!(
                persist_commit_capture(&s, &capture, Some("fixed-checkpoint"))
                    .await
                    .is_err()
            );
            for table in [
                "file_revision",
                "changed_hunk",
                "attribution",
                "attribution_hunk",
            ] {
                assert_eq!(s.count(table).await.unwrap(), 0, "{table}");
            }
        }
        let s = store();
        let mut identical = recovery_capture(true);
        identical.files.push(identical.files[0].clone());
        persist_commit_capture(&s, &identical, Some("fixed-checkpoint"))
            .await
            .unwrap();
        assert_eq!(s.count("attribution").await.unwrap(), 1);
        assert_eq!(s.count("attribution_hunk").await.unwrap(), 1);
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

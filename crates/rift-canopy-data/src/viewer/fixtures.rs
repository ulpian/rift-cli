//! Test fixtures the viewer modules share: a bare catalogue entry, a richly
//! populated one, the text of a timeline item, and the repository and rows
//! the worker tests walk.
//!
//! A separate module because the same entry is walked by the catalogue, the
//! detail segments and the worker, and a fixture copied per module drifts.

use rift_canopy::{ViewerCheckpointRow, ViewerTimelineItem};
use rift_recall_substrate::catalogue::CatalogueIssue;
use rift_recall_substrate::{
    CheckpointCatalogueEntry, CheckpointOrigin, CheckpointStorageKind, Dialect, TimestampProvenance,
};
use rift_recall_types::checkpoint::CheckpointMetadata;

use super::catalogue::checkpoint_row;

pub(super) fn entry(id: &str, anchor: &str) -> CheckpointCatalogueEntry {
    CheckpointCatalogueEntry {
        checkpoint_id: id.to_owned(),
        origins: Vec::new(),
        chain_position: None,
        metadata: CheckpointMetadata::default(),
        metadata_present: true,
        recorded_at_ms: None,
        anchor_commit: Some(anchor.to_owned()),
        anchor_committed_ms: None,
        issues: Vec::new(),
    }
}

/// An entry with every optional fact populated, two origins, one issue, and
/// its metadata marked absent: the shape that exercises every summary line.
pub(super) fn rich_entry(anchor: &str) -> CheckpointCatalogueEntry {
    let mut rich = entry(&"c".repeat(64), anchor);
    rich.chain_position = Some(7);
    rich.recorded_at_ms = Some(0);
    rich.anchor_committed_ms = Some(1_000);
    rich.metadata.strategy = Some("manual".to_owned());
    rich.metadata.sessions = vec!["session-a".to_owned(), "session-b".to_owned()];
    rich.metadata.tasks = vec!["task-missing".to_owned(), "../unsafe".to_owned()];
    rich.metadata.merkle_root = Some("root".to_owned());
    rich.metadata.prev_hash = Some("previous".to_owned());
    rich.metadata.canon_version = Some(1);
    rich.metadata_present = false;
    rich.issues.push(CatalogueIssue {
        component: "metadata".to_owned(),
        detail: "partial".to_owned(),
    });
    rich.origins.extend([
        CheckpointOrigin {
            checkpoint_id: rich.checkpoint_id.clone(),
            dialect: Dialect::Rift,
            storage_ref: "refs/rift/checkpoints/main".to_owned(),
            kind: CheckpointStorageKind::Branch,
            tip_oid: Some(anchor.to_owned()),
        },
        CheckpointOrigin {
            checkpoint_id: rich.checkpoint_id.clone(),
            dialect: Dialect::Legacy,
            storage_ref: "refs/checkpoints/legacy".to_owned(),
            kind: CheckpointStorageKind::PerCheckpointRef,
            tip_oid: None,
        },
    ]);
    rich
}

pub(super) fn item_text(item: &ViewerTimelineItem) -> String {
    item.summary
        .iter()
        .chain(&item.body)
        .flat_map(|line| &line.spans)
        .map(|span| span.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A fresh sha1 repository with no commits: enough for the substrate to
/// open, and nothing for it to find.
pub(super) fn initialized_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q", "--object-format=sha1"])
        .current_dir(repo.path())
        .status()
        .unwrap()
        .success());
    repo
}

/// Unprojected rows with no time and no branch, one per entry.
pub(super) fn rows_for(entries: &[CheckpointCatalogueEntry]) -> Vec<ViewerCheckpointRow> {
    entries
        .iter()
        .map(|entry| {
            checkpoint_row(
                entry,
                None,
                TimestampProvenance::Unknown,
                Vec::new(),
                "unprojected",
                None,
            )
        })
        .collect()
}

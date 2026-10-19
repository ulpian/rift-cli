//! Intent resume over a real native tree: a persisted intent is authenticated
//! against the archived checkpoint and completes with one immutable receipt,
//! or refuses typed; nothing here can reach a backend, so nothing re-delegates.
use rift_core::RiftError;
use rift_recall_substrate::{CheckpointShard, GixSubstrate, SubstrateReader};
use rift_shadow::backend::{CheckpointArchive, NativeBackend, ShadowBackend};

use super::super::tests::{archive_for, batch, carrier};
use super::super::{Delegation, RecoveryBackend};
use super::*;
use crate::capture_recovery::eligibility::fixtures::Rig;

/// Writes nothing: the process "crashed" after the intent was durable.
struct Crash;

impl ShadowBackend for Crash {
    fn archive_checkpoint(&self, _: &CheckpointArchive) -> rift_core::Result<()> {
        Err(RiftError::validity("crashed before the native write"))
    }
}

/// The native tree with one artifact's bytes replaced.
struct Tampered(GixSubstrate);

impl SubstrateReader for Tampered {
    fn is_local_only(&self) -> bool {
        self.0.is_local_only()
    }
    fn checkpoints(&self) -> rift_core::Result<Vec<CheckpointShard>> {
        self.0.checkpoints()
    }
    fn blob(&self, id: &str) -> rift_core::Result<Option<Vec<u8>>> {
        self.0.blob(id)
    }
    fn read_artifact(&self, id: &str, rel: &str) -> rift_core::Result<Option<Vec<u8>>> {
        self.read_native_artifact(id, rel)
    }
    fn read_native_artifact(&self, id: &str, rel: &str) -> rift_core::Result<Option<Vec<u8>>> {
        let bytes = self.0.read_native_artifact(id, rel)?;
        if rel != BOUNDARY_GAPS_NAME {
            return Ok(bytes);
        }
        Ok(bytes.map(|mut b| {
            b.push(b'\n');
            b
        }))
    }
}

/// Archive the frozen carrier through the wrapper into `inner`.
fn archive_with(rig: &Rig, hold: &OwnHold<'_>, inner: &impl ShadowBackend) {
    let carrier = carrier();
    let backend = RecoveryBackend::new(inner, hold, carrier.clone(), batch(rig, hold)).unwrap();
    let archive = archive_for(&carrier, backend.artifact());
    let _ = backend.archive_checkpoint(&archive);
    assert!(matches!(
        backend.delegation(),
        Some(Delegation::Delegated { .. })
    ));
}

fn native(rig: &Rig) -> NativeBackend<rift_git::PlumbingGitWrite> {
    let repo = rift_git::discover(&rig.fixture.work_tree).unwrap();
    NativeBackend::new(rift_git::PlumbingGitWrite::new(repo))
}

fn receipt_path(rig: &Rig) -> std::path::PathBuf {
    rig.fixture.evidence.join(RECEIPT_NAME)
}

#[test]
fn an_archived_intent_resumes_to_one_immutable_receipt_bound_to_the_native_tree() {
    let rig = Rig::new();
    let hold = rig.admit();
    archive_with(&rig, &hold, &native(&rig));
    let intent_bytes = std::fs::read(rig.fixture.evidence.join(INTENT_NAME)).unwrap();
    let intent = ArchiveIntent::decode(&intent_bytes).unwrap();
    let reader = GixSubstrate::open(&rig.fixture.work_tree).unwrap();
    let receipt = resume_intent(&hold, &reader).unwrap();
    assert_eq!(receipt.checkpoint_id, intent.checkpoint_id);
    assert_eq!(receipt.shard_path, intent.shard_path);
    assert_eq!(receipt.gap_ids, intent.gap_ids);
    assert_eq!(receipt.gap_artifact_sha256, intent.gap_artifact_sha256);
    assert_eq!(
        receipt.intent_sha256,
        format!("{:x}", Sha256::digest(&intent_bytes))
    );
    let written = std::fs::read(receipt_path(&rig)).unwrap();
    assert_eq!(PublicationReceipt::decode(&written).unwrap(), receipt);
    assert_eq!(read_receipt(&hold), Ok(Some(receipt.clone())));
    // A second resume finds the same bytes and completes without a rewrite.
    assert_eq!(resume_intent(&hold, &reader), Ok(receipt));
    assert_eq!(std::fs::read(receipt_path(&rig)).unwrap(), written);
}

#[test]
fn an_intent_whose_archive_never_landed_is_owed_and_writes_no_receipt() {
    let rig = Rig::new();
    let hold = rig.admit();
    archive_with(&rig, &hold, &Crash);
    let reader = GixSubstrate::open(&rig.fixture.work_tree).unwrap();
    assert_eq!(resume_intent(&hold, &reader), Err(ResumeOwed::NotArchived));
    assert!(!receipt_path(&rig).exists());
    assert_eq!(read_receipt(&hold), Ok(None));
}

#[test]
fn a_native_tree_whose_gap_bytes_differ_from_the_intent_is_refused() {
    let rig = Rig::new();
    let hold = rig.admit();
    archive_with(&rig, &hold, &native(&rig));
    let reader = Tampered(GixSubstrate::open(&rig.fixture.work_tree).unwrap());
    assert_eq!(resume_intent(&hold, &reader), Err(ResumeOwed::GapMismatch));
    assert!(!receipt_path(&rig).exists());
}

#[test]
fn no_intent_on_disk_is_nothing_to_resume() {
    let rig = Rig::new();
    let hold = rig.admit();
    // A published gap batch, so the private namespace proves itself.
    batch(&rig, &hold);
    let reader = GixSubstrate::open(&rig.fixture.work_tree).unwrap();
    assert_eq!(resume_intent(&hold, &reader), Err(ResumeOwed::NoIntent));
}

#[test]
fn another_receipt_already_there_is_never_overwritten() {
    let rig = Rig::new();
    let hold = rig.admit();
    archive_with(&rig, &hold, &native(&rig));
    std::fs::write(receipt_path(&rig), b"{}").unwrap();
    let reader = GixSubstrate::open(&rig.fixture.work_tree).unwrap();
    assert_eq!(
        resume_intent(&hold, &reader),
        Err(ResumeOwed::ReceiptConflict)
    );
    assert_eq!(std::fs::read(receipt_path(&rig)).unwrap(), b"{}");
}

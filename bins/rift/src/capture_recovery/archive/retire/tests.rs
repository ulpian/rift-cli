//! The retirement intent over a real published recovery: it is durable before
//! any queue file is touched, names exactly the Cut's slots, and the per-slot
//! report reads each slot's state without changing anything.
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use rift_recall_substrate::GixSubstrate;

use super::super::resume::resume_intent;
use super::super::resume::tests::{archive_with, native};
use super::*;
use crate::capture_recovery::eligibility::fixtures::Rig;

/// The carrier archived natively and its publication receipt written.
fn published(rig: &Rig, hold: &OwnHold<'_>) {
    archive_with(rig, hold, &native(rig));
    let reader = GixSubstrate::open(&rig.fixture.work_tree).unwrap();
    resume_intent(hold, &reader).unwrap();
}

fn source(hold: &OwnHold<'_>, name: &str) -> PathBuf {
    hold.agent_queue_namespace()
        .unwrap()
        .queue()
        .path
        .join(name)
}

fn intent_path(rig: &Rig) -> PathBuf {
    rig.fixture.evidence.join(RETIRE_NAME)
}

fn written(hold: &OwnHold<'_>) -> RetireIntent {
    match write_retire_intent(hold).unwrap() {
        RetireOutcome::Published(intent) => intent,
        RetireOutcome::Resumed(_) => panic!("a fresh intent was expected"),
    }
}

#[test]
fn a_published_recovery_writes_one_retire_intent_naming_every_cut_slot_and_touches_no_queue_file() {
    let rig = Rig::new();
    let hold = rig.admit();
    published(&rig, &hold);
    let intent = written(&hold);
    let cut_bytes = std::fs::read(rig.fixture.evidence.join(CUT_NAME)).unwrap();
    let cut = CutManifest::decode(&cut_bytes).unwrap();
    assert_eq!(
        intent.cut_sha256,
        format!("{:x}", Sha256::digest(&cut_bytes))
    );
    assert_eq!(intent.slots.len(), 2);
    for (slot, entry) in intent.slots.iter().zip(&cut.entries) {
        assert_eq!(slot.pin_slot, entry.pin_slot);
        assert_eq!(slot.source_name, entry.source_name);
        assert_eq!(slot.pin_name, entry.pin_name);
        assert_eq!(slot.ticket_sha256, entry.ticket_sha256);
        assert!(source(&hold, &slot.source_name).exists());
    }
    let path = intent_path(&rig);
    let bytes = std::fs::read(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(RetireIntent::decode(&bytes).unwrap(), intent);
    // A retry finds the same bytes and resumes without a rewrite.
    assert_eq!(
        write_retire_intent(&hold),
        Ok(RetireOutcome::Resumed(intent))
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn without_a_publication_receipt_nothing_is_discharged_and_no_intent_is_written() {
    let rig = Rig::new();
    let hold = rig.admit();
    archive_with(&rig, &hold, &native(&rig));
    assert_eq!(write_retire_intent(&hold), Err(RetireOwed::NoReceipt));
    assert!(!intent_path(&rig).exists());
}

#[test]
fn another_retire_intent_already_there_is_never_overwritten() {
    let rig = Rig::new();
    let hold = rig.admit();
    published(&rig, &hold);
    std::fs::write(intent_path(&rig), b"{}").unwrap();
    assert_eq!(write_retire_intent(&hold), Err(RetireOwed::IntentConflict));
    assert_eq!(std::fs::read(intent_path(&rig)).unwrap(), b"{}");
}

#[test]
fn the_report_names_each_slot_retired_or_kept_with_its_typed_reason() {
    let rig = Rig::new();
    let hold = rig.admit();
    published(&rig, &hold);
    let intent = written(&hold);
    let report = retirement_report(&hold).unwrap();
    assert!(report
        .iter()
        .all(|slot| slot.state == SlotState::Kept(Kept::OriginalQueued)));
    let [first, second] = intent.slots.as_slice() else {
        panic!("two slots")
    };
    std::fs::remove_file(source(&hold, &first.source_name)).unwrap();
    // Another valid ticket as a new file under the second name: the first
    // slot's pinned bytes, never the second's pinned inode rewritten.
    let other = std::fs::read(rig.fixture.evidence.join(&first.pin_name)).unwrap();
    std::fs::remove_file(source(&hold, &second.source_name)).unwrap();
    std::fs::write(source(&hold, &second.source_name), other).unwrap();
    let report = retirement_report(&hold).unwrap();
    let state = |slot: &RetireSlot| {
        report
            .iter()
            .find(|row| row.pin_slot == slot.pin_slot)
            .map(|row| row.state)
    };
    assert_eq!(report.len(), 2);
    assert_eq!(state(first), Some(SlotState::Retired));
    // Whatever now sits under the name, it is never the pinned original.
    assert!(matches!(state(second), Some(SlotState::Kept(_))));
    assert!(!std::fs::read(intent_path(&rig)).unwrap().is_empty());
}

#[test]
fn the_report_without_a_retire_intent_is_owed() {
    let rig = Rig::new();
    let hold = rig.admit();
    published(&rig, &hold);
    assert_eq!(retirement_report(&hold), Err(RetireOwed::NoIntent));
    assert!(!intent_path(&rig).exists());
}

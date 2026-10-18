//! The published Cut as one checked core batch over real pinned tickets: its
//! evidence is the Cut file, its recorded time is chosen once and kept on
//! retry, and nothing but a Cut bound to this operation becomes a gap.
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use rift_core::boundary_gap::BoundaryEdge;
use sha2::{Digest as _, Sha256};

use super::*;
use crate::agent_queue::BoundaryEdge::{End, Start};
use crate::capture_recovery::cut::{pin_all, CUT_NAME};
use crate::capture_recovery::eligibility::fixtures::{payload, Rig};
use crate::capture_recovery::inventory::tests::enqueue;
use crate::capture_recovery::selection::select;

const RECORDED_AT: i64 = 1_760_000_000_000;

fn pinned(rig: &Rig, hold: &OwnHold<'_>) -> Pinned {
    let selected = rig
        .rt
        .block_on(select(&rig.store, hold, &rig.registry))
        .unwrap();
    pin_all(hold, selected).unwrap()
}

/// Two historical boundaries and no current turn: a Cut of two entries.
fn two_historical(rig: &Rig) {
    enqueue(&rig.fixture, "old-a", 1, Start);
    enqueue(&rig.fixture, "old-b", 2, End);
    rift_shadow::hooks::run_state::create_private_dir(&rig.sessions_dir).unwrap();
}

fn cut_path(rig: &Rig) -> PathBuf {
    rig.fixture.evidence.join(CUT_NAME)
}

fn gap_path(rig: &Rig) -> PathBuf {
    rig.fixture.evidence.join(GAP_NAME)
}

fn cut_sha256(pinned: &Pinned) -> String {
    match &pinned.cut {
        CutOutcome::Published { cut_sha256 } | CutOutcome::Resumed { cut_sha256 } => {
            cut_sha256.clone()
        }
        CutOutcome::NoHistoricalCut => panic!("a historical cut"),
    }
}

fn published(outcome: GapOutcome) -> GapBatch {
    let GapOutcome::Published(batch) = outcome else {
        panic!("published")
    };
    batch
}

#[test]
fn a_published_cut_becomes_one_batch_whose_evidence_is_the_cut_file_and_whose_record_binds_it() {
    let rig = Rig::new();
    let hold = rig.admit();
    two_historical(&rig);
    let pinned = pinned(&rig, &hold);
    let cut_sha256 = cut_sha256(&pinned);
    enqueue(&rig.fixture, "late", 3, Start);
    let batch = published(publish_gap(&hold, &pinned, || RECORDED_AT).unwrap());
    let manifest = CutManifest::decode(&std::fs::read(cut_path(&rig)).unwrap()).unwrap();
    assert_eq!(batch.evidence_digest().to_hex(), cut_sha256);
    assert_eq!(batch.ticket_count(), manifest.selected_count);
    assert_eq!(batch.ticket_count(), 2, "the late producer stays outside");
    assert_eq!(batch.repo_id().as_str(), manifest.repo_id);
    assert_eq!(batch.member_key().to_hex(), manifest.member_key);
    assert_eq!(batch.recorded_at(), RECORDED_AT);
    for (ticket, entry) in batch.tickets().iter().zip(&manifest.entries) {
        assert_ticket_is_its_entry(ticket, entry);
    }
    let bytes = std::fs::read(gap_path(&rig)).unwrap();
    assert!(!bytes.ends_with(b"\n"));
    let mode = std::fs::symlink_metadata(gap_path(&rig))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o600);
    let record = GapRecord::decode(&bytes).unwrap();
    assert_eq!(record.cut_sha256, cut_sha256);
    assert_eq!(record.intent_sha256, pinned.intent_sha256);
    assert_eq!(record.plan_sha256, manifest.plan_sha256);
    assert_eq!(record.operation_id, manifest.operation_id);
    assert_eq!(record.recorded_at, RECORDED_AT);
    assert_eq!(record.gap_id, batch.gap_id().to_hex());
    assert_eq!(record.selection_sha256, batch.selection_digest().to_hex());
    assert_eq!(record.ticket_count, 2);
}

/// Every safe fact of the ticket is its Cut entry's; nothing else is carried.
fn assert_ticket_is_its_entry(ticket: &GapTicket, entry: &PinnedEntry) {
    let known = &entry.known_boundary;
    assert_eq!(ticket.source_key().to_hex(), entry.source_key);
    assert_eq!(ticket.context_digest().to_hex(), entry.context_sha256);
    assert_eq!(ticket.ticket_digest().to_hex(), entry.ticket_sha256);
    assert_eq!(ticket.session_key().to_hex(), known.session_key);
    assert_eq!(ticket.turn_ordinal(), known.turn_ordinal);
    assert_eq!(
        ticket.boundary(),
        BoundaryEdge::parse(&known.boundary).unwrap()
    );
    assert_eq!(ticket.head_oid(), known.head_oid.as_deref());
    assert_eq!(ticket.captured_at(), known.captured_at);
    assert_eq!(ticket.queued_at(), known.queued_at);
    assert_eq!(ticket.attempts(), entry.attempts);
}

#[test]
fn a_retry_resumes_the_same_gap_with_the_first_recorded_time_and_never_reads_the_clock() {
    let rig = Rig::new();
    let hold = rig.admit();
    two_historical(&rig);
    let pinned = pinned(&rig, &hold);
    let first = published(publish_gap(&hold, &pinned, || RECORDED_AT).unwrap());
    let before = std::fs::read(gap_path(&rig)).unwrap();
    let again = publish_gap(&hold, &pinned, || panic!("the clock is read once")).unwrap();
    assert_eq!(again, GapOutcome::Resumed(first));
    assert_eq!(std::fs::read(gap_path(&rig)).unwrap(), before);
}

#[test]
fn no_historical_cut_publishes_no_gap_and_writes_no_record() {
    let rig = Rig::new();
    let hold = rig.admit();
    let body = payload("session_id", "cur");
    rig.ingest("claude-code", "SessionStart", &body, 10);
    rig.ingest("claude-code", "UserPromptSubmit", &body, 20);
    enqueue(&rig.fixture, "cur", 1, Start);
    rift_shadow::hooks::run_state::create_private_dir(&rig.sessions_dir).unwrap();
    let pinned = pinned(&rig, &hold);
    assert_eq!(pinned.cut, CutOutcome::NoHistoricalCut);
    let outcome = publish_gap(&hold, &pinned, || panic!("no clock without a cut")).unwrap();
    assert_eq!(outcome, GapOutcome::NoHistoricalCut);
    assert!(!gap_path(&rig).exists());
}

#[test]
fn a_cut_file_that_is_not_the_published_digest_refuses_and_writes_no_record() {
    let rig = Rig::new();
    let hold = rig.admit();
    two_historical(&rig);
    let mut pinned = pinned(&rig, &hold);
    pinned.cut = CutOutcome::Published {
        cut_sha256: format!("{:x}", Sha256::digest(b"another cut")),
    };
    let owed = publish_gap(&hold, &pinned, || RECORDED_AT).unwrap_err();
    assert_eq!(owed, GapOwed::CutMismatch);
    assert!(!gap_path(&rig).exists());
}

#[test]
fn a_record_bound_to_another_cut_or_with_an_edited_gap_id_refuses_and_is_never_overwritten() {
    let rig = Rig::new();
    let hold = rig.admit();
    two_historical(&rig);
    let pinned = pinned(&rig, &hold);
    published(publish_gap(&hold, &pinned, || RECORDED_AT).unwrap());
    let original = GapRecord::decode(&std::fs::read(gap_path(&rig)).unwrap()).unwrap();
    let other = format!("{:x}", Sha256::digest(b"other"));
    for (edited, expected) in [
        (
            GapRecord {
                cut_sha256: other.clone(),
                ..original.clone()
            },
            GapOwed::RecordConflict,
        ),
        (
            GapRecord {
                gap_id: other.clone(),
                ..original.clone()
            },
            GapOwed::RecordInvalid,
        ),
    ] {
        let bytes = edited.encode().unwrap();
        std::fs::remove_file(gap_path(&rig)).unwrap();
        std::fs::write(gap_path(&rig), &bytes).unwrap();
        let owed = publish_gap(&hold, &pinned, || panic!("never re-recorded")).unwrap_err();
        assert_eq!(owed, expected);
        assert_eq!(std::fs::read(gap_path(&rig)).unwrap(), bytes);
    }
}

#[test]
fn an_entry_with_a_noncanonical_head_or_a_negative_time_refuses_as_an_invalid_ticket() {
    let rig = Rig::new();
    let hold = rig.admit();
    two_historical(&rig);
    let pinned = pinned(&rig, &hold);
    let cut_sha256 = cut_sha256(&pinned);
    let manifest = CutManifest::decode(&std::fs::read(cut_path(&rig)).unwrap()).unwrap();
    assert!(batch_of(&manifest, &cut_sha256, RECORDED_AT).is_ok());
    let mut upper = manifest.clone();
    upper.entries[0].known_boundary.head_oid = Some("A".repeat(40));
    let mut negative = manifest.clone();
    negative.entries[1].known_boundary.captured_at = -1;
    for bad in [upper, negative] {
        assert_eq!(
            batch_of(&bad, &cut_sha256, RECORDED_AT).unwrap_err(),
            GapOwed::TicketInvalid
        );
    }
    assert_eq!(
        batch_of(&manifest, &cut_sha256, -1).unwrap_err(),
        GapOwed::BatchInvalid
    );
}

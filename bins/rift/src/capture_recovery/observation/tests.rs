//! The reserved observation is collected at most once: the attempt record is
//! durable before the collector runs, a rerun only reads back, and a spent
//! attempt that found nothing stays owed rather than collecting again.
use super::*;
use crate::agent_queue::BoundaryEdge::Start;
use crate::capture_recovery::eligibility::fixtures::Rig;
use crate::capture_recovery::inventory::tests::enqueue;
use crate::capture_recovery::selection::{Expectation, KnownBoundary};

fn reserved() -> ReservedObservation {
    let known_boundary = KnownBoundary {
        agent: "codex".to_owned(),
        native_session: "native-1".to_owned(),
        session_key: "session-key".to_owned(),
        turn_ordinal: 3,
        boundary: "start".to_owned(),
        head_oid: None,
        captured_at: 1_000,
        queued_at: 1_001,
    };
    let expectation = Expectation {
        pin_slot: 3,
        source_key: "source-key".to_owned(),
        source_name: "boundary-3.json".to_owned(),
        pin_name: "pin-3".to_owned(),
        dev: 1,
        ino: 2,
        mode: 0o600,
        raw_length: 10,
        ticket_sha256: "a".repeat(64),
        context_sha256: "b".repeat(64),
        attempts: 0,
        known_boundary,
    };
    ReservedObservation {
        expectation,
        event_key: "event-key".to_owned(),
        receipt_id: "c".repeat(64),
        cache_evidence_sha256: "d".repeat(64),
    }
}

fn reservation(observation: &ReservedObservation) -> Reservation<'_> {
    Reservation {
        operation_id: "fixture-op",
        plan_sha256: "plan",
        intent_sha256: "intent",
        observation,
    }
}

/// Counts each call and answers with the configured finding; the attempt
/// record must already be on disk when `collect` runs.
struct Counting {
    evidence: std::path::PathBuf,
    found: Collection,
    collects: u32,
    read_backs: u32,
}

impl Collector for Counting {
    fn collect(&mut self, _: &ReservedObservation) -> Collection {
        assert!(
            self.evidence.join(ATTEMPT_NAME).is_file(),
            "collected before the attempt was durable"
        );
        self.collects += 1;
        self.found
    }

    fn read_back(&mut self, _: &ReservedObservation) -> Collection {
        self.read_backs += 1;
        self.found
    }
}

fn counting(rig: &Rig, found: Collection) -> Counting {
    Counting {
        evidence: rig.fixture.evidence.clone(),
        found,
        collects: 0,
        read_backs: 0,
    }
}

#[test]
fn capture_recovery_reserved_observation_is_collected_once_and_a_rerun_only_reads_back() {
    let rig = Rig::new();
    // The hold reads its private directory through the live queue root.
    enqueue(&rig.fixture, "old-a", 1, Start);
    let hold = rig.admit();
    let observation = reserved();
    let mut collector = counting(&rig, Collection::Observed);
    let first = attempt_observation(&hold, Some(reservation(&observation)), &mut collector);
    assert_eq!(first, Ok(ObservationOutcome::Observed));
    let again = attempt_observation(&hold, Some(reservation(&observation)), &mut collector);
    assert_eq!(again, Ok(ObservationOutcome::Recognized));
    assert_eq!((collector.collects, collector.read_backs), (1, 1));
}

#[test]
fn capture_recovery_a_spent_attempt_that_found_nothing_stays_owed_and_never_collects_again() {
    let rig = Rig::new();
    // The hold reads its private directory through the live queue root.
    enqueue(&rig.fixture, "old-a", 1, Start);
    let hold = rig.admit();
    let observation = reserved();
    let mut collector = counting(&rig, Collection::Limited);
    let first = attempt_observation(&hold, Some(reservation(&observation)), &mut collector);
    assert_eq!(first, Err(ObservationOwed::Spent(Collection::Limited)));
    collector.found = Collection::Failed;
    let again = attempt_observation(&hold, Some(reservation(&observation)), &mut collector);
    assert_eq!(again, Err(ObservationOwed::Spent(Collection::Failed)));
    assert_eq!((collector.collects, collector.read_backs), (1, 1));
}

#[test]
fn capture_recovery_another_attempt_record_refuses_without_collecting() {
    let rig = Rig::new();
    // The hold reads its private directory through the live queue root.
    enqueue(&rig.fixture, "old-a", 1, Start);
    let hold = rig.admit();
    let observation = reserved();
    let mut collector = counting(&rig, Collection::Observed);
    attempt_observation(&hold, Some(reservation(&observation)), &mut collector).unwrap();
    let other = Reservation {
        intent_sha256: "another-intent",
        ..reservation(&observation)
    };
    let refused = attempt_observation(&hold, Some(other), &mut collector);
    assert_eq!(refused, Err(ObservationOwed::AttemptConflict));
    assert_eq!((collector.collects, collector.read_backs), (1, 0));
}

#[test]
fn capture_recovery_no_reservation_collects_nothing_and_writes_no_record() {
    let rig = Rig::new();
    // The hold reads its private directory through the live queue root.
    enqueue(&rig.fixture, "old-a", 1, Start);
    let hold = rig.admit();
    let mut collector = counting(&rig, Collection::Missing);
    assert_eq!(
        attempt_observation(&hold, None, &mut collector),
        Ok(ObservationOutcome::NoReservation)
    );
    assert!(!rig.fixture.evidence.join(ATTEMPT_NAME).exists());
    assert_eq!((collector.collects, collector.read_backs), (0, 0));
}

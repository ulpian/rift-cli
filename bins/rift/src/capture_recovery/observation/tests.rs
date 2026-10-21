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
    // The completion names the checkpoint, so the rerun reads nothing back.
    assert_eq!((collector.collects, collector.read_backs), (1, 0));
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

#[test]
fn capture_recovery_a_started_attempt_looks_its_boundary_up_before_collecting_again() {
    let rig = Rig::new();
    enqueue(&rig.fixture, "old-a", 1, Start);
    let hold = rig.admit();
    let observation = reserved();
    let mut collector = counting(&rig, Collection::Missing);
    let first = attempt_observation(&hold, Some(reservation(&observation)), &mut collector);
    assert_eq!(first, Err(ObservationOwed::Spent(Collection::Missing)));
    assert!(!rig.fixture.evidence.join(COLLECTED_NAME).exists());
    // The first collection did land after all: the lookup finds it.
    collector.found = Collection::Observed;
    let again = attempt_observation(&hold, Some(reservation(&observation)), &mut collector);
    assert_eq!(again, Ok(ObservationOutcome::Recognized));
    assert_eq!((collector.collects, collector.read_backs), (1, 1));
    let done: ObservationCollected =
        serde_json::from_slice(&std::fs::read(rig.fixture.evidence.join(COLLECTED_NAME)).unwrap())
            .unwrap();
    assert_eq!(done.checkpoint_id, Some(checkpoint_id(&observation)));
}

/// One line per collection audit under the checkpoint: its own start time.
fn audits(rig: &Rig, id: &str) -> Vec<i64> {
    let sql = "SELECT collection_started_at FROM turn_boundary_observation WHERE snapshot_id = ?1";
    let params = [rift_store::Value::text(id.to_owned())];
    let rows = rig.rt.block_on(rift_store::CanonicalStore::query(
        rig.store.inner(),
        sql,
        &params,
    ));
    rows.unwrap()
        .iter()
        .map(|row| row.get_i64(0).unwrap())
        .collect()
}

#[test]
fn capture_recovery_the_live_collector_leaves_one_fresh_checkpoint_and_a_rerun_adds_none() {
    let rig = Rig::new();
    enqueue(&rig.fixture, "old-a", 1, Start);
    let hold = rig.admit();
    let observation = reserved();
    let repo = rift_git::discover(&rig.fixture.work_tree).unwrap();
    let reader = rift_git::GixBlobReader::open(&rig.fixture.work_tree).unwrap();
    let mut collector = BoundaryCollector {
        rt: &rig.rt,
        store: rig.store.inner(),
        repo: &repo,
        reader: &reader,
    };
    let first = attempt_observation(&hold, Some(reservation(&observation)), &mut collector);
    assert_eq!(first, Ok(ObservationOutcome::Observed));
    let id = checkpoint_id(&observation);
    let stamped = audits(&rig, &id);
    // Stamped by the collection clock, never by the event's own instant.
    assert_eq!(stamped.len(), 1);
    assert!(stamped[0] > observation.expectation.known_boundary.captured_at);
    let again = attempt_observation(&hold, Some(reservation(&observation)), &mut collector);
    assert_eq!(again, Ok(ObservationOutcome::Recognized));
    assert_eq!(audits(&rig, &id), stamped);
}

/// One open turn with its boundary queued: the selection reserves it and the
/// pin loop pins it, as the hold does before it observes.
fn pinned_reservation(rig: &Rig, hold: &OwnHold<'_>) -> ReservedObservation {
    let body = crate::capture_recovery::eligibility::fixtures::payload("session_id", "cur");
    rig.ingest("claude-code", "SessionStart", &body, 10);
    rig.ingest("claude-code", "UserPromptSubmit", &body, 20);
    enqueue(&rig.fixture, "cur", 1, Start);
    let selected = rig
        .rt
        .block_on(crate::capture_recovery::selection::select(
            &rig.store,
            hold,
            &rig.registry,
        ))
        .unwrap();
    let pinned = crate::capture_recovery::cut::pin_all(hold, selected).unwrap();
    let mut observation = reserved();
    observation.expectation = pinned.reserved_pin.unwrap().expectation;
    observation
}

fn queued(hold: &OwnHold<'_>, observation: &ReservedObservation) -> std::path::PathBuf {
    let queue = hold.agent_queue_namespace().unwrap();
    queue
        .queue()
        .path
        .join(&observation.expectation.source_name)
}

fn recorded(rig: &Rig) -> ObservationCollected {
    let bytes = std::fs::read(rig.fixture.evidence.join(COLLECTED_NAME)).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn capture_recovery_the_reserved_ticket_is_removed_and_recorded_once_its_baseline_is_durable() {
    let rig = Rig::new();
    let hold = rig.admit();
    let observation = pinned_reservation(&rig, &hold);
    assert!(queued(&hold, &observation).is_file());
    let mut collector = counting(&rig, Collection::Observed);
    let first = attempt_observation(&hold, Some(reservation(&observation)), &mut collector);
    assert_eq!(first, Ok(ObservationOutcome::Observed));
    assert!(!queued(&hold, &observation).exists());
    assert_eq!(
        recorded(&rig).reserved_ticket,
        Some(ReservedTicket::Retired)
    );
    // A rerun finds the completion and touches nothing.
    let again = attempt_observation(&hold, Some(reservation(&observation)), &mut collector);
    assert_eq!(again, Ok(ObservationOutcome::Recognized));
    assert_eq!(
        recorded(&rig).reserved_ticket,
        Some(ReservedTicket::Retired)
    );
}

#[test]
fn capture_recovery_a_different_file_under_the_reserved_name_is_kept_and_recorded_typed() {
    let rig = Rig::new();
    let hold = rig.admit();
    let observation = pinned_reservation(&rig, &hold);
    let source = queued(&hold, &observation);
    std::fs::remove_file(&source).unwrap();
    std::fs::write(&source, b"a later delivery").unwrap();
    let mut collector = counting(&rig, Collection::Observed);
    let first = attempt_observation(&hold, Some(reservation(&observation)), &mut collector);
    assert_eq!(first, Ok(ObservationOutcome::Observed));
    assert_eq!(std::fs::read(&source).unwrap(), b"a later delivery");
    assert_eq!(
        recorded(&rig).reserved_ticket,
        Some(ReservedTicket::NameReused)
    );
}

#[test]
fn capture_recovery_a_baseline_collected_before_the_archive_is_read_back_and_never_collected_twice()
{
    let rig = Rig::new();
    enqueue(&rig.fixture, "old-a", 1, Start);
    let hold = rig.admit();
    let observation = reserved();
    let mut collector = counting(&rig, Collection::Observed);
    let first = collect_baseline(&hold, reservation(&observation), &mut collector);
    assert_eq!(first, Ok(checkpoint_id(&observation)));
    // A crash before the gap was published: the rerun reads the baseline's
    // recorded checkpoint back and leaves the completion to the last stage.
    let again = collect_baseline(&hold, reservation(&observation), &mut collector);
    assert_eq!(again, Ok(checkpoint_id(&observation)));
    assert!(!rig.fixture.evidence.join(COLLECTED_NAME).exists());
    let last = attempt_observation(&hold, Some(reservation(&observation)), &mut collector);
    assert_eq!(last, Ok(ObservationOutcome::Recognized));
    assert_eq!((collector.collects, collector.read_backs), (1, 2));
}

//! The recovery-only archive wrapper over real pinned tickets and the core
//! native backend: the intent is durable before the one delegation, and any
//! archive that is not exactly the frozen gap carrier is never delegated.
use std::cell::Cell;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use rift_shadow::backend::NativeBackend;
use sha2::{Digest as _, Sha256};

use super::run::{archive_recovered, RecoveryRun};
use super::*;
use crate::agent_queue::BoundaryEdge::{End, Start};
use crate::capture_recovery::cut::pin_all;
use crate::capture_recovery::eligibility::fixtures::Rig;
use crate::capture_recovery::gap::{publish_gap, GapOutcome};
use crate::capture_recovery::inventory::tests::enqueue;
use crate::capture_recovery::selection::select;

const AGENT: &str = "claude-code";

/// Delegates to the in-memory native backend and records whether the intent
/// was already on disk when the delegation arrived.
struct Spy {
    intent: PathBuf,
    calls: Cell<u32>,
    intent_was_there: Cell<bool>,
    native: NativeBackend,
}

impl ShadowBackend for Spy {
    fn archive_checkpoint(&self, archive: &CheckpointArchive) -> rift_core::Result<()> {
        self.calls.set(self.calls.get() + 1);
        self.intent_was_there.set(self.intent.exists());
        self.native.archive_checkpoint(archive)
    }
}

fn spy(rig: &Rig) -> Spy {
    Spy {
        intent: rig.fixture.evidence.join(INTENT_NAME),
        calls: Cell::new(0),
        intent_was_there: Cell::new(false),
        native: NativeBackend::in_memory(),
    }
}

/// Two historical boundaries pinned, cut and published as one batch.
fn batch(rig: &Rig, hold: &OwnHold<'_>) -> GapBatch {
    enqueue(&rig.fixture, "old-a", 1, Start);
    enqueue(&rig.fixture, "old-b", 2, End);
    rift_shadow::hooks::run_state::create_private_dir(&rig.sessions_dir).unwrap();
    let selected = rig
        .rt
        .block_on(select(&rig.store, hold, &rig.registry))
        .unwrap();
    let pinned = pin_all(hold, selected).unwrap();
    let GapOutcome::Published(batch) = publish_gap(hold, &pinned, || 1_760_000_000_000).unwrap()
    else {
        panic!("published")
    };
    batch
}

/// The ordinary audit fixture re-carried by `carrier` and re-derived.
fn archive_for(carrier: &Carrier, frozen: &FrozenBoundaryGaps) -> CheckpointArchive {
    let mut archive = rift_shadow::parity::boundary_gap_fixture_archive().unwrap();
    archive.boundary_gaps = Some(frozen.clone());
    archive.checkpoint_id = archive.derived_checkpoint_id().unwrap();
    assert_eq!(archive.sessions, vec![carrier.native_session.clone()]);
    archive
}

fn carrier() -> Carrier {
    let fixture = rift_shadow::parity::boundary_gap_fixture_archive().unwrap();
    let native = fixture.sessions.first().unwrap().clone();
    Carrier::new(AGENT, &native, &"a1".repeat(32), &"b2".repeat(32)).unwrap()
}

#[test]
fn a_matching_archive_is_delegated_once_and_only_after_its_intent_is_durable() {
    let rig = Rig::new();
    let hold = rig.admit();
    let batch = batch(&rig, &hold);
    let spy = spy(&rig);
    let carrier = carrier();
    let backend = RecoveryBackend::new(&spy, &hold, carrier.clone(), batch.clone()).unwrap();
    let archive = archive_for(&carrier, backend.artifact());
    backend.archive_checkpoint(&archive).unwrap();
    assert_eq!(spy.calls.get(), 1);
    assert!(spy.intent_was_there.get(), "the intent precedes delegation");
    let bytes = std::fs::read(&spy.intent).unwrap();
    assert!(!bytes.ends_with(b"\n"));
    let mode = std::fs::symlink_metadata(&spy.intent)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o600);
    let intent = ArchiveIntent::decode(&bytes).unwrap();
    assert_eq!(intent.cut_sha256, batch.evidence_digest().to_hex());
    assert_eq!(intent.gap_ids, vec![batch.gap_id().to_hex()]);
    assert_eq!(intent.attempt, 1);
    assert_eq!(intent.carrier_session_key, carrier.session_key.to_hex());
    assert_eq!(
        intent.carrier_session_key,
        rift_core::capture_session_row_id(AGENT, &carrier.native_session)
    );
    assert_eq!(intent.carrier_context_sha256, "a1".repeat(32));
    assert_eq!(intent.carrier_ticket_sha256, "b2".repeat(32));
    assert_eq!(intent.checkpoint_id, archive.checkpoint_id);
    assert_eq!(intent.shard_path, archive.shard_path());
    let artifact_sha256 = format!("{:x}", Sha256::digest(backend.artifact().as_bytes()));
    assert_eq!(intent.gap_artifact_sha256, artifact_sha256);
    assert_eq!(
        backend.delegation(),
        Some(Delegation::Delegated {
            intent_sha256: format!("{:x}", Sha256::digest(&bytes))
        })
    );
}

/// Each archive differs from the frozen carrier in one way the wrapper owns.
fn refused(case: impl FnOnce(&mut CheckpointArchive), owed: ArchiveOwed) {
    let rig = Rig::new();
    let hold = rig.admit();
    let batch = batch(&rig, &hold);
    let spy = spy(&rig);
    let carrier = carrier();
    let backend = RecoveryBackend::new(&spy, &hold, carrier.clone(), batch).unwrap();
    let mut archive = archive_for(&carrier, backend.artifact());
    case(&mut archive);
    assert!(backend.archive_checkpoint(&archive).is_err());
    assert_eq!(backend.delegation(), Some(Delegation::Refused(owed)));
    assert_eq!(spy.calls.get(), 0, "never delegated");
    assert!(!spy.intent.exists(), "no intent written");
}

#[test]
fn an_archive_without_the_frozen_gap_bytes_is_never_delegated() {
    refused(
        |archive| archive.boundary_gaps = None,
        ArchiveOwed::GapMismatch,
    );
}

#[test]
fn an_archive_that_names_another_session_or_agent_is_never_delegated() {
    refused(
        |archive| {
            let native = archive.sessions.first().unwrap().clone();
            archive.session_agents.insert(native, "codex".into());
        },
        ArchiveOwed::CarrierMismatch,
    );
    refused(
        |archive| archive.sessions.push("another-session".into()),
        ArchiveOwed::CarrierMismatch,
    );
}

#[test]
fn an_archive_whose_id_is_not_derived_from_its_own_root_is_never_delegated() {
    refused(
        |archive| archive.checkpoint_id = "ab".repeat(32),
        ArchiveOwed::IdMismatch,
    );
}

#[test]
fn an_intent_already_there_is_never_overwritten_and_never_delegated_again() {
    let rig = Rig::new();
    let hold = rig.admit();
    let batch = batch(&rig, &hold);
    let spy = spy(&rig);
    let carrier = carrier();
    let backend = RecoveryBackend::new(&spy, &hold, carrier.clone(), batch.clone()).unwrap();
    let archive = archive_for(&carrier, backend.artifact());
    backend.archive_checkpoint(&archive).unwrap();
    let before = std::fs::read(&spy.intent).unwrap();
    let again = RecoveryBackend::new(&spy, &hold, carrier.clone(), batch.clone()).unwrap();
    assert!(again.archive_checkpoint(&archive).is_err());
    assert_eq!(
        again.delegation(),
        Some(Delegation::Refused(ArchiveOwed::IntentExists))
    );
    let other = Carrier::new(
        AGENT,
        &carrier.native_session,
        &"c3".repeat(32),
        &"b2".repeat(32),
    )
    .unwrap();
    let third = RecoveryBackend::new(&spy, &hold, other, batch).unwrap();
    assert!(third.archive_checkpoint(&archive).is_err());
    assert_eq!(
        third.delegation(),
        Some(Delegation::Refused(ArchiveOwed::IntentConflict))
    );
    assert_eq!(spy.calls.get(), 1, "one delegation in all");
    assert_eq!(std::fs::read(&spy.intent).unwrap(), before);
}

#[test]
fn a_carrier_with_a_malformed_digest_or_agent_is_refused() {
    let native = "session-1";
    assert!(Carrier::new(AGENT, native, "nothex", &"b2".repeat(32)).is_err());
    assert!(Carrier::new(AGENT, native, &"a1".repeat(32), &"B2".repeat(32)).is_err());
    assert!(Carrier::new("", native, &"a1".repeat(32), &"b2".repeat(32)).is_err());
}

#[test]
fn the_shared_assembly_builds_the_carrier_archive_and_the_wrapper_delegates_it_once() {
    use rift_shadow::hooks::strategy::{ArchiveDecision, ArchiveReason, StrategyKind};
    let rig = Rig::new();
    let hold = rig.admit();
    let batch = batch(&rig, &hold);
    let spy = spy(&rig);
    let native = "carrier-native";
    rig.seed_turn(AGENT, native, 1, None);
    let carrier = Carrier::new(AGENT, native, &"a1".repeat(32), &"b2".repeat(32)).unwrap();
    let backend = RecoveryBackend::new(&spy, &hold, carrier, batch).unwrap();
    let repo = rift_git::discover(&rig.fixture.work_tree).unwrap();
    let run = RecoveryRun {
        rt: &rig.rt,
        store: rig.store.inner(),
        repo: &repo,
        commit: None,
        now: 1_760_000_000_000,
    };
    let decision = ArchiveDecision {
        archive: true,
        reason: ArchiveReason::Commit,
        strategy: StrategyKind::ManualCommit,
    };
    let id = archive_recovered(&run, &backend, decision).unwrap();
    assert!(id.is_some());
    assert_eq!(spy.calls.get(), 1);
    assert!(spy.intent_was_there.get());
    assert!(matches!(
        backend.delegation(),
        Some(Delegation::Delegated { .. })
    ));
}

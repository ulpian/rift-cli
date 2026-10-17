//! Selection over real queued tickets and real ingested evidence: one
//! reservation by the closed order, the rest historical in slot order, and
//! one durable intent — or the whole attempt owed with nothing published.
use std::os::unix::fs::PermissionsExt as _;

use rift_shadow::ids;
use sha2::{Digest as _, Sha256};

use super::*;
use crate::agent_queue::BoundaryEdge::{End, Start};
use crate::capture_recovery::eligibility::fixtures::{payload, Rig};
use crate::capture_recovery::inventory::tests::{enqueue, queue_dir};
use crate::capture_recovery::inventory::Refusal;

const CLAUDE: &str = "claude-code";

fn run(rig: &Rig, hold: &OwnHold<'_>) -> Result<Selected, Deferred> {
    rig.rt.block_on(select(&rig.store, hold, &rig.registry))
}

fn run_with(
    rig: &Rig,
    hold: &OwnHold<'_>,
    phase: &mut impl FnMut(Phase) -> Result<(), ()>,
) -> Result<Selected, Deferred> {
    rig.rt
        .block_on(select_with(&rig.store, hold, &rig.registry, phase))
}

/// A native session whose current turn is open at ordinal one: its queued
/// Start is eligible.
fn open_turn(rig: &Rig, native: &str) {
    let body = payload("session_id", native);
    rig.ingest(CLAUDE, "SessionStart", &body, 10);
    rig.ingest(CLAUDE, "UserPromptSubmit", &body, 20);
}

/// The member's sessions directory as the first hook leaves it: present, so a
/// session with no cache is "absent", not an unavailable tree.
fn ready_sessions(rig: &Rig) {
    rift_shadow::hooks::run_state::create_private_dir(&rig.sessions_dir).unwrap();
}

fn open_fds() -> usize {
    std::fs::read_dir("/dev/fd").unwrap().count()
}

fn intent_path(rig: &Rig) -> std::path::PathBuf {
    rig.fixture.evidence.join(INTENT_NAME)
}

#[test]
fn one_current_start_is_reserved_and_every_other_understood_boundary_is_historical_in_slot_order() {
    let rig = Rig::new();
    let hold = rig.admit();
    open_turn(&rig, "cur");
    enqueue(&rig.fixture, "cur", 1, Start);
    enqueue(&rig.fixture, "cur", 1, End);
    enqueue(&rig.fixture, "old-a", 1, Start);
    enqueue(&rig.fixture, "old-b", 2, End);
    let selected = run(&rig, &hold).unwrap();
    assert_eq!(selected.cut, Cut::Historical);
    assert_eq!(selected.publication, Publication::Fresh);
    let reserved = selected.reserved.as_ref().unwrap();
    assert_eq!(
        reserved
            .observation
            .expectation
            .known_boundary
            .native_session,
        "cur"
    );
    assert_eq!(
        reserved.observation.expectation.known_boundary.boundary,
        "start"
    );
    assert_eq!(reserved.observation.expectation.pin_slot, 3);
    assert_eq!(selected.retained.len(), 3);
    let keys: Vec<(String, u32, String)> = selected
        .retained
        .iter()
        .map(|r| {
            let k = &r.expectation.known_boundary;
            (k.session_key.clone(), k.turn_ordinal, k.boundary.clone())
        })
        .collect();
    let slots: Vec<u32> = selected.retained.iter().map(|r| r.slot).collect();
    assert_eq!(slots, vec![0, 1, 2]);
    let sorted = {
        let mut s = keys.clone();
        s.sort();
        s
    };
    assert_eq!(
        keys, sorted,
        "historical slots follow the closed lexical order"
    );
    assert!(selected
        .retained
        .iter()
        .any(|r| r.expectation.known_boundary.native_session == "cur"
            && r.expectation.known_boundary.boundary == "end"));
    assert_eq!(reserved.original.slot, 3);
    assert_eq!(
        crate::capture_recovery::files::physical_identity(
            &reserved.original.file.metadata().unwrap()
        )
        .ino,
        reserved.original.expectation.ino
    );
    let decoded = SelectionIntent::decode(&std::fs::read(intent_path(&rig)).unwrap()).unwrap();
    // The reservation is committed by the intent, never a historical member.
    assert!(decoded
        .historical
        .iter()
        .all(|e| e.source_name != reserved.observation.expectation.source_name));
}

/// The published record is the canonical bytes of the returned intent: its
/// file SHA is the returned digest, 0600, no trailing LF, decoding to the same
/// value, with the scope, counts and per-source identities as frozen.
#[test]
fn the_published_intent_is_the_canonical_record_of_the_frozen_selection() {
    let rig = Rig::new();
    let hold = rig.admit();
    open_turn(&rig, "cur");
    enqueue(&rig.fixture, "cur", 1, Start);
    enqueue(&rig.fixture, "old-a", 1, Start);
    enqueue(&rig.fixture, "old-b", 2, End);
    let selected = run(&rig, &hold).unwrap();
    let bytes = std::fs::read(intent_path(&rig)).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        selected.intent_sha256
    );
    assert_eq!(
        std::fs::metadata(intent_path(&rig))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
    assert!(!bytes.ends_with(b"\n"));
    let decoded = SelectionIntent::decode(&bytes).unwrap();
    assert_eq!(decoded, selected.intent);
    assert_eq!(decoded.inventory.tickets, 3);
    assert_eq!(decoded.inventory.entries_counted, 3);
    assert!(decoded.inventory.refused_by_reason.is_empty());
    assert_eq!(decoded.policy, POLICY);
    assert_eq!(
        decoded.scope.source_queue_dir,
        queue_dir(&rig.fixture).to_str().unwrap()
    );
    assert_eq!(
        decoded.scope.private_dir,
        rig.fixture.evidence.to_str().unwrap()
    );
    let member = crate::capture_recovery::plan::Plan::decode(&rig.fixture.plan_bytes)
        .unwrap()
        .member();
    for original in &selected.retained {
        let identity =
            crate::capture_recovery::files::physical_identity(&original.file.metadata().unwrap());
        assert_eq!(
            (identity.dev, identity.ino),
            (original.expectation.dev, original.expectation.ino)
        );
        assert_eq!(
            original.expectation.source_key,
            source_key(
                member,
                "fixture-op",
                original.slot,
                &original.expectation.source_name
            )
            .unwrap()
        );
        assert_eq!(
            original.expectation.pin_name,
            format!("pin-{:05}", original.slot)
        );
    }
}

#[test]
fn several_eligible_keys_choose_the_lexical_minimum_never_the_newest() {
    let rig = Rig::new();
    let hold = rig.admit();
    let natives = ["first-native", "second-native"];
    let rows: Vec<String> = natives
        .iter()
        .map(|n| ids::session_row_id(CLAUDE, n))
        .collect();
    // The newest session is deliberately the lexically GREATER row.
    let (older, newer) = if rows[0] < rows[1] {
        (natives[0], natives[1])
    } else {
        (natives[1], natives[0])
    };
    open_turn(&rig, older);
    open_turn(&rig, newer);
    enqueue(&rig.fixture, newer, 1, Start);
    enqueue(&rig.fixture, older, 1, Start);
    let selected = run(&rig, &hold).unwrap();
    let reserved = selected.reserved.as_ref().unwrap();
    assert_eq!(
        reserved
            .observation
            .expectation
            .known_boundary
            .native_session,
        older
    );
    assert_eq!(selected.retained.len(), 1);
    assert_eq!(
        selected.retained[0]
            .expectation
            .known_boundary
            .native_session,
        newer
    );
    assert_eq!(reserved.observation.receipt_id.len(), 64);
    assert_eq!(reserved.observation.event_key.len(), 64);
    let cache = std::fs::read(rig.sessions_dir.join(format!("{older}.json"))).unwrap();
    assert_eq!(
        reserved.observation.cache_evidence_sha256,
        format!("{:x}", Sha256::digest(&cache))
    );
}

#[test]
fn an_exact_five_way_tie_refuses_and_a_distinct_name_breaks_it() {
    let key = |name: &str| LexKey {
        session_row: "s".repeat(64),
        ordinal: 1,
        boundary: "start",
        context_sha256: "c".repeat(64),
        name: name.to_owned(),
    };
    assert_eq!(choose_reservation(&[]).unwrap(), None);
    assert_eq!(
        choose_reservation(&[(7, key("b.json")), (3, key("a.json"))]).unwrap(),
        Some(3)
    );
    assert_eq!(
        choose_reservation(&[(7, key("a.json")), (3, key("a.json"))]).unwrap_err(),
        Owed::ReservationTie
    );
}

#[test]
fn any_io_class_unavailability_on_one_candidate_defers_the_whole_selection_with_nothing_published()
{
    let rig = Rig::new();
    let hold = rig.admit();
    open_turn(&rig, "cur");
    enqueue(&rig.fixture, "cur", 1, Start);
    enqueue(&rig.fixture, "old-a", 1, Start);
    enqueue(&rig.fixture, "broken", 1, End);
    std::fs::create_dir(rig.sessions_dir.join("broken.json")).unwrap();
    let deferred = run(&rig, &hold).unwrap_err();
    assert_eq!(
        deferred.owed,
        Owed::CandidateUnavailable([(Reason::CacheNotRegular, 1)].into_iter().collect())
    );
    assert_eq!(deferred.counts.tickets, 3);
    assert!(!intent_path(&rig).exists());
}

#[test]
fn a_resource_failure_before_publication_leaves_no_intent_and_no_retained_descriptor() {
    let rig = Rig::new();
    let hold = rig.admit();
    enqueue(&rig.fixture, "old-a", 1, Start);
    enqueue(&rig.fixture, "old-a", 1, End);
    enqueue(&rig.fixture, "old-b", 1, Start);
    ready_sessions(&rig);
    let deferred = run_with(&rig, &hold, &mut |point| {
        if point == Phase::BeforeRetain(1) {
            Err(())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(deferred.owed, Owed::Inventory);
    assert_eq!(deferred.counts.refused.get(&Refusal::Resource), Some(&1));
    assert!(!intent_path(&rig).exists());
    let deferred = run_with(&rig, &hold, &mut |point| {
        if point == Phase::BeforePublish {
            Err(())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(deferred.owed, Owed::Resource);
    assert!(!intent_path(&rig).exists());
}

#[test]
fn the_intent_is_written_whole_then_synced_before_the_selection_returns() {
    let rig = Rig::new();
    let hold = rig.admit();
    for ordinal in 1..=3 {
        enqueue(&rig.fixture, "old", ordinal, Start);
    }
    ready_sessions(&rig);
    let mut phases = Vec::new();
    let selected = run_with(&rig, &hold, &mut |point| {
        phases.push(point);
        if point == Phase::Written {
            let bytes = std::fs::read(intent_path(&rig)).unwrap();
            assert!(SelectionIntent::decode(&bytes).is_ok(), "whole before sync");
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(selected.retained.len(), 3);
    let tail: Vec<Phase> = phases
        .iter()
        .copied()
        .filter(|p| !matches!(p, Phase::BeforeRetain(_)))
        .collect();
    assert_eq!(
        tail,
        vec![
            Phase::Inventoried,
            Phase::Classified,
            Phase::BeforePublish,
            Phase::Written,
            Phase::Synced
        ]
    );
    assert_eq!(
        phases
            .iter()
            .filter(|p| matches!(p, Phase::BeforeRetain(_)))
            .count(),
        3
    );
}

/// Descriptor accounting needs a quiet table: the test binary runs its other
/// tests on sibling threads of one process, so the counts are taken in a
/// child process running only this body.
#[test]
#[ignore = "run in a child process by `the_descriptor_peak_is_sources_plus_bounded_overhead_and_every_descriptor_is_released`"]
fn descriptor_accounting_in_a_quiet_process() {
    let rig = Rig::new();
    let hold = rig.admit();
    for ordinal in 1..=3 {
        enqueue(&rig.fixture, "old", ordinal, Start);
    }
    ready_sessions(&rig);
    // Bind the hold's lazily-opened capabilities before the baseline.
    drop(hold.agent_queue_namespace().unwrap());
    drop(hold.sessions_namespace().unwrap());
    let baseline = open_fds();
    let deferred = run_with(&rig, &hold, &mut |point| {
        if point == Phase::BeforeRetain(1) {
            Err(())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(deferred.owed, Owed::Inventory);
    assert_eq!(open_fds(), baseline, "a refused attempt retains nothing");
    let mut peak_small = 0;
    let selected = run_with(&rig, &hold, &mut |point| {
        if point == Phase::BeforePublish {
            peak_small = open_fds();
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(selected.retained.len(), 3);
    assert!(
        open_fds() >= baseline + 3,
        "one retained descriptor per source"
    );
    drop(selected);
    assert_eq!(
        open_fds(),
        baseline,
        "every retained descriptor is released"
    );
    std::fs::remove_file(intent_path(&rig)).unwrap();
    for ordinal in 4..=10 {
        enqueue(&rig.fixture, "old", ordinal, Start);
    }
    let mut peak_large = 0;
    let selected = run_with(&rig, &hold, &mut |point| {
        if point == Phase::BeforePublish {
            peak_large = open_fds();
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(selected.retained.len(), 10);
    assert_eq!(
        peak_large - peak_small,
        7,
        "one descriptor per source, nothing else grows"
    );
    assert!(
        peak_small - baseline <= 3 + 160,
        "bounded overhead: {}",
        peak_small - baseline
    );
    drop(selected);
    assert_eq!(open_fds(), baseline);
}

#[test]
fn the_descriptor_peak_is_sources_plus_bounded_overhead_and_every_descriptor_is_released() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "capture_recovery::selection::tests::descriptor_accounting_in_a_quiet_process",
            "--test-threads",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "quiet-process descriptor accounting failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn an_existing_equal_intent_resumes_and_a_different_one_refuses_without_overwrite() {
    let rig = Rig::new();
    let hold = rig.admit();
    enqueue(&rig.fixture, "old", 1, Start);
    ready_sessions(&rig);
    let first = run(&rig, &hold).unwrap();
    assert_eq!(first.publication, Publication::Fresh);
    let bytes = std::fs::read(intent_path(&rig)).unwrap();
    drop(first);
    let second = run(&rig, &hold).unwrap();
    assert_eq!(second.publication, Publication::Resumed);
    assert_eq!(second.retained.len(), 1);
    assert_eq!(std::fs::read(intent_path(&rig)).unwrap(), bytes);
    drop(second);
    enqueue(&rig.fixture, "old", 2, Start);
    let deferred = run(&rig, &hold).unwrap_err();
    assert_eq!(deferred.owed, Owed::IntentConflict);
    assert_eq!(
        std::fs::read(intent_path(&rig)).unwrap(),
        bytes,
        "never overwritten"
    );
    std::fs::remove_file(intent_path(&rig)).unwrap();
    std::os::unix::fs::symlink("elsewhere", intent_path(&rig)).unwrap();
    assert_eq!(run(&rig, &hold).unwrap_err().owed, Owed::IntentConflict);
}

#[test]
fn no_historical_cut_is_a_complete_inventory_with_nothing_to_gap_never_an_unknown_one() {
    let rig = Rig::new();
    let hold = rig.admit();
    queue_dir(&rig.fixture);
    let empty = run(&rig, &hold).unwrap();
    assert_eq!(empty.cut, Cut::NoHistoricalCut);
    assert!(empty.reserved.is_none());
    assert_eq!(empty.intent.inventory.entries_counted, 0);
    assert!(empty.intent.historical.is_empty());
    assert!(intent_path(&rig).exists());
    drop(empty);
    std::fs::remove_file(intent_path(&rig)).unwrap();
    open_turn(&rig, "cur");
    enqueue(&rig.fixture, "cur", 1, Start);
    let only_reserved = run(&rig, &hold).unwrap();
    assert_eq!(only_reserved.cut, Cut::NoHistoricalCut);
    assert!(only_reserved.reserved.is_some());
    assert_eq!(only_reserved.intent.inventory.tickets, 1);
    drop(only_reserved);
    std::fs::remove_file(intent_path(&rig)).unwrap();
    std::fs::write(queue_dir(&rig.fixture).join("stray.txt"), b"").unwrap();
    let deferred = run(&rig, &hold).unwrap_err();
    assert_eq!(deferred.owed, Owed::Inventory);
    assert_eq!(deferred.counts.refused.get(&Refusal::UnknownName), Some(&1));
    assert!(!intent_path(&rig).exists());
    let text = String::from_utf8(rig.fixture.plan_bytes.clone()).unwrap();
    assert!(crate::capture_recovery::plan::Plan::decode(
        text.replace(POLICY, "newest-session@0").as_bytes()
    )
    .is_err());
}

mod intent_tests {
    use super::super::intent::*;
    use super::super::POLICY;
    use sha2::{Digest as _, Sha256};

    fn entry(slot: u32, name: &str, native: &str) -> Expectation {
        Expectation {
            pin_slot: slot,
            source_key: format!("{slot:064x}"),
            source_name: name.to_owned(),
            pin_name: format!("pin-{slot:05}"),
            dev: 1,
            ino: 100 + u64::from(slot),
            mode: 0o600,
            raw_length: 512,
            ticket_sha256: "a".repeat(64),
            context_sha256: "b".repeat(64),
            attempts: 0,
            known_boundary: KnownBoundary {
                agent: "claude-code".into(),
                native_session: native.into(),
                session_key: "c".repeat(64),
                turn_ordinal: 1,
                boundary: "start".into(),
                head_oid: None,
                captured_at: 5,
                queued_at: 6,
            },
        }
    }

    fn intent(entries: Vec<Expectation>) -> SelectionIntent {
        SelectionIntent {
            schema_version: 1,
            operation_id: "op-1".into(),
            plan_sha256: "d".repeat(64),
            policy: POLICY.into(),
            scope: Scope {
                member_key: "e".repeat(64),
                repo_id: "repo".into(),
                canonical_git_dir: "/g/.git".into(),
                canonical_work_tree: "/g".into(),
                source_queue_dir: "/c/rift/f/run/m/agent-queue".into(),
                private_dir: "/p/op".into(),
                source_dir_identity: Identity { dev: 1, ino: 2 },
                private_dir_identity: Identity { dev: 1, ino: 3 },
                source_ancestry: vec![Identity { dev: 1, ino: 1 }, Identity { dev: 1, ino: 2 }],
                private_ancestry: vec![Identity { dev: 1, ino: 1 }, Identity { dev: 1, ino: 3 }],
            },
            inventory: InventoryCounts {
                entries_counted: u32::try_from(entries.len()).unwrap(),
                tickets: u32::try_from(entries.len()).unwrap(),
                understood_other: 0,
                ancillary: 0,
                refused_by_reason: std::collections::BTreeMap::new(),
            },
            historical: entries,
            reserved_observation: None,
        }
    }

    #[test]
    fn the_canonical_bytes_are_sorted_lf_free_and_the_file_sha_is_the_commitment_whatever_the_key_order(
    ) {
        let value = intent(vec![entry(0, "a.json", "n1"), entry(1, "b.json", "n2")]);
        let bytes = value.encode().unwrap();
        assert!(!bytes.ends_with(b"\n"));
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.starts_with(r#"{"historical":[{"attempts":0,"context_sha256""#));
        let shuffled = text
            .replacen("{\"historical\"", "{\"schema_version\":1,\"historical\"", 1)
            .replacen(",\"schema_version\":1", "", 1);
        assert_ne!(shuffled.as_bytes(), bytes.as_slice());
        let decoded = SelectionIntent::decode(shuffled.as_bytes()).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(decoded.encode().unwrap(), bytes);
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            format!("{:x}", Sha256::digest(decoded.encode().unwrap()))
        );
        let duplicate = text.replacen("\"policy\"", "\"policy\":1,\"policy\"", 1);
        assert!(SelectionIntent::decode(duplicate.as_bytes()).is_err());
        let unknown = text.replacen("{\"historical\"", "{\"extra\":1,\"historical\"", 1);
        assert!(SelectionIntent::decode(unknown.as_bytes()).is_err());
    }

    #[test]
    fn the_closed_checks_refuse_misordered_slots_duplicates_and_disagreeing_counts() {
        let mut misordered = intent(vec![entry(1, "a.json", "n1"), entry(0, "b.json", "n2")]);
        assert!(SelectionIntent::decode(&misordered.encode().unwrap()).is_err());
        misordered.historical.swap(0, 1);
        assert!(SelectionIntent::decode(&misordered.encode().unwrap()).is_ok());
        let same_name = intent(vec![entry(0, "a.json", "n1"), entry(1, "a.json", "n2")]);
        assert!(SelectionIntent::decode(&same_name.encode().unwrap()).is_err());
        let mut same_inode = intent(vec![entry(0, "a.json", "n1"), entry(1, "b.json", "n2")]);
        same_inode.historical[1].ino = same_inode.historical[0].ino;
        assert!(SelectionIntent::decode(&same_inode.encode().unwrap()).is_err());
        let mut counts = intent(vec![entry(0, "a.json", "n1")]);
        counts.inventory.tickets = 2;
        assert!(SelectionIntent::decode(&counts.encode().unwrap()).is_err());
        let mut policy = intent(vec![]);
        policy.policy = "newest".into();
        assert!(SelectionIntent::decode(&policy.encode().unwrap()).is_err());
    }

    #[test]
    fn the_header_entry_and_file_caps_refuse_before_any_whole_serialization() {
        let widest = entry(0, &format!("{}.json", "n".repeat(251)), &"s".repeat(200));
        let bytes = serde_json::to_vec(&widest).unwrap();
        assert!(bytes.len() > MAX_ENTRY_BYTES);
        assert!(
            intent(vec![widest]).encode().is_err(),
            "a maximal entry is over the entry cap"
        );
        let fits = entry(0, "a.json", "n1");
        assert!(serde_json::to_vec(&fits).unwrap().len() <= MAX_ENTRY_BYTES);
        let mut wide_header = intent(vec![]);
        for index in 0..6000 {
            wide_header
                .inventory
                .refused_by_reason
                .insert(format!("reason-{index:020}"), 1);
        }
        assert!(wide_header.encode().is_err(), "the header cap");
        let wide = "w".repeat(180);
        let many = intent(
            (0..12_000)
                .map(|slot| entry(slot, &format!("{wide}{slot}.json"), &"s".repeat(60)))
                .collect(),
        );
        let sized = many.measure().unwrap();
        assert!(sized.predicted_len().unwrap() > MAX_INTENT_BYTES);
        assert!(
            many.encode().is_err(),
            "the file cap, from the measured parts"
        );
        let few = intent(
            (0..3)
                .map(|slot| entry(slot, &format!("{slot}.json"), "n"))
                .collect(),
        );
        let sized = few.measure().unwrap();
        assert_eq!(sized.predicted_len().unwrap(), few.encode().unwrap().len());
    }
}

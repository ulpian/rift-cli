//! The pin loop and the Cut over real queued tickets: the intent durable before
//! the first link, one slot at a time, every earlier pin kept on refusal, a
//! restart that reopens only what exists, and one immutable Cut per intent.
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::PathBuf;

use sha2::{Digest as _, Sha256};

use super::*;
use crate::agent_queue::BoundaryEdge::{End, Start};
use crate::capture_recovery::eligibility::fixtures::{payload, Rig};
use crate::capture_recovery::inventory::tests::{boundary_job, enqueue, queue_dir};
use crate::capture_recovery::selection::{select, Cut, INTENT_NAME};

const CLAUDE: &str = "claude-code";

fn selected(rig: &Rig, hold: &OwnHold<'_>) -> Selected {
    rig.rt
        .block_on(select(&rig.store, hold, &rig.registry))
        .unwrap()
}

fn open_turn(rig: &Rig, native: &str) {
    let body = payload("session_id", native);
    rig.ingest(CLAUDE, "SessionStart", &body, 10);
    rig.ingest(CLAUDE, "UserPromptSubmit", &body, 20);
}

fn ready_sessions(rig: &Rig) {
    rift_shadow::hooks::run_state::create_private_dir(&rig.sessions_dir).unwrap();
}

fn historical(rig: &Rig, count: u32) {
    for ordinal in 1..=count {
        enqueue(&rig.fixture, "old", ordinal, Start);
    }
    ready_sessions(rig);
}

fn intent_path(rig: &Rig) -> PathBuf {
    rig.fixture.evidence.join(INTENT_NAME)
}

fn cut_path(rig: &Rig) -> PathBuf {
    rig.fixture.evidence.join(CUT_NAME)
}

fn pin_path(rig: &Rig, slot: u32) -> PathBuf {
    rig.fixture.evidence.join(format!("pin-{slot:05}"))
}

fn source_path(rig: &Rig, expectation: &Expectation) -> PathBuf {
    queue_dir(&rig.fixture).join(&expectation.source_name)
}

fn intent_on_disk(rig: &Rig) -> SelectionIntent {
    SelectionIntent::decode(&std::fs::read(intent_path(rig)).unwrap()).unwrap()
}

fn mode_of(path: &std::path::Path) -> u32 {
    std::fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

fn open_fds() -> usize {
    std::fs::read_dir("/dev/fd").unwrap().count()
}

/// Pin everything but stop before the Cut: the state a crash between the last
/// pin and the Cut leaves behind.
fn pin_all_but_cut(rig: &Rig, hold: &OwnHold<'_>) -> Vec<Expectation> {
    let selected = selected(rig, hold);
    let expectations: Vec<Expectation> = selected
        .retained
        .iter()
        .map(|r| r.expectation.clone())
        .collect();
    let owed = pin_all_with(hold, selected, &mut |point| {
        if point == Phase::BeforeCut {
            Err(())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(owed, Owed::Resource);
    assert!(!cut_path(rig).exists());
    expectations
}

#[test]
fn every_selected_original_is_pinned_in_slot_order_after_the_durable_intent_and_the_cut_commits_the_historical_ones(
) {
    let rig = Rig::new();
    let hold = rig.admit();
    open_turn(&rig, "cur");
    enqueue(&rig.fixture, "cur", 1, Start);
    enqueue(&rig.fixture, "cur", 1, End);
    enqueue(&rig.fixture, "old-a", 1, Start);
    enqueue(&rig.fixture, "old-b", 2, End);
    let selected = selected(&rig, &hold);
    assert_eq!(selected.cut, Cut::Historical);
    let intent = selected.intent.clone();
    let intent_sha256 = selected.intent_sha256.clone();
    let mut phases = Vec::new();
    let pinned = pin_all_with(&hold, selected, &mut |point| {
        phases.push(point);
        if point == Phase::BeforeLink(0) {
            let bytes = std::fs::read(intent_path(&rig)).unwrap();
            assert_eq!(format!("{:x}", Sha256::digest(&bytes)), intent_sha256);
            assert_eq!(SelectionIntent::decode(&bytes).unwrap(), intent);
            assert!(!pin_path(&rig, 0).exists(), "the intent is durable first");
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(
        phases,
        vec![
            Phase::BeforeLink(0),
            Phase::AfterLink(0),
            Phase::BeforeLink(1),
            Phase::AfterLink(1),
            Phase::BeforeLink(2),
            Phase::AfterLink(2),
            Phase::BeforeLink(3),
            Phase::AfterLink(3),
            Phase::BeforeCut,
        ]
    );
    assert_eq!(pinned.intent_sha256, intent_sha256);
    assert_eq!(pinned.pins.len(), 3);
    let reserved = pinned.reserved_pin.as_ref().unwrap();
    assert_eq!(reserved.expectation.pin_slot, 3);
    for receipt in pinned.pins.iter().chain(Some(reserved)) {
        assert_pin_aliases_its_original(&rig, &receipt.expectation);
    }
    assert_eq!(mode_of(&rig.fixture.evidence), 0o700);
    let manifest = assert_cut_is_published(&rig, &pinned, &intent_sha256);
    assert_eq!(manifest.plan_sha256, intent.plan_sha256);
    assert!(manifest
        .entries
        .iter()
        .all(|e| e.source_name != reserved.expectation.source_name));
    assert_eq!(manifest, CutManifest::from_intent(&intent, &intent_sha256));
}

/// The pin under the expectation's name is the original inode with its mode
/// and bytes, and the hard link outlives every handle.
fn assert_pin_aliases_its_original(rig: &Rig, expectation: &Expectation) {
    let pin = std::fs::symlink_metadata(pin_path(rig, expectation.pin_slot)).unwrap();
    assert_eq!(pin.ino(), expectation.ino, "the pin aliases the original");
    assert_eq!(pin.mode() & 0o7777, expectation.mode);
    assert_eq!(pin.mode() & 0o7777, 0o600);
    assert_eq!(pin.nlink(), 2);
    assert_eq!(
        std::fs::read(pin_path(rig, expectation.pin_slot)).unwrap(),
        std::fs::read(source_path(rig, expectation)).unwrap()
    );
}

/// The published Cut: 0600, LF-free, its file SHA the returned digest, bound
/// to the intent, three historical entries strictly sorted by source key.
fn assert_cut_is_published(rig: &Rig, pinned: &Pinned, intent_sha256: &str) -> CutManifest {
    let CutOutcome::Published { cut_sha256 } = &pinned.cut else {
        panic!("published")
    };
    let bytes = std::fs::read(cut_path(rig)).unwrap();
    assert_eq!(&format!("{:x}", Sha256::digest(&bytes)), cut_sha256);
    assert_eq!(mode_of(&cut_path(rig)), 0o600);
    assert!(!bytes.ends_with(b"\n"));
    let manifest = CutManifest::decode(&bytes).unwrap();
    assert_eq!(manifest.intent_sha256, intent_sha256);
    assert_eq!(manifest.selected_count, 3);
    assert_eq!(manifest.entries.len(), 3);
    let keys: Vec<&str> = manifest
        .entries
        .iter()
        .map(|e| e.source_key.as_str())
        .collect();
    assert!(keys.windows(2).all(|w| w[0] < w[1]), "sorted by source key");
    manifest
}

#[test]
fn a_source_mutated_between_the_intent_and_its_link_refuses_at_that_slot_and_keeps_every_earlier_pin(
) {
    let rig = Rig::new();
    let hold = rig.admit();
    historical(&rig, 3);
    let selected = selected(&rig, &hold);
    let second = selected.retained[1].expectation.clone();
    let owed = pin_all_with(&hold, selected, &mut |point| {
        if point == Phase::BeforeLink(1) {
            let path = source_path(&rig, &second);
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.push(b' ');
            std::fs::write(&path, bytes).unwrap();
        }
        Ok(())
    })
    .unwrap_err();
    assert_eq!(owed, Owed::RetainedMismatch(1));
    assert!(pin_path(&rig, 0).exists(), "the earlier pin is kept");
    assert!(!pin_path(&rig, 1).exists());
    assert!(!pin_path(&rig, 2).exists());
    assert!(!cut_path(&rig).exists());
}

#[test]
fn a_source_whose_name_is_gone_at_link_time_is_owed_as_missing_while_its_descriptor_still_proves_it(
) {
    let rig = Rig::new();
    let hold = rig.admit();
    historical(&rig, 3);
    let selected = selected(&rig, &hold);
    let second = selected.retained[1].expectation.clone();
    let owed = pin_all_with(&hold, selected, &mut |point| {
        if point == Phase::BeforeLink(1) {
            std::fs::remove_file(source_path(&rig, &second)).unwrap();
        }
        Ok(())
    })
    .unwrap_err();
    // `SourceMissing`, not `RetainedMismatch`: the retained descriptor was
    // inspected and still matched before the missing name was found.
    assert_eq!(owed, Owed::SourceMissing(1));
    assert!(pin_path(&rig, 0).exists());
    assert!(!pin_path(&rig, 1).exists());
    assert!(!cut_path(&rig).exists());
}

/// Descriptor accounting needs a quiet table, so the counts are taken in a
/// child process running only this body.
#[test]
#[ignore = "run in a child process by `each_slot_releases_its_original_descriptor_after_its_pin_validates`"]
fn descriptor_release_in_a_quiet_process() {
    let rig = Rig::new();
    let hold = rig.admit();
    historical(&rig, 4);
    drop(hold.agent_queue_namespace().unwrap());
    drop(hold.sessions_namespace().unwrap());
    let baseline = open_fds();
    let selected = selected(&rig, &hold);
    assert!(open_fds() >= baseline + 4);
    let mut at = std::collections::BTreeMap::new();
    let pinned = pin_all_with(&hold, selected, &mut |point| {
        if let Phase::BeforeLink(slot) = point {
            at.insert(slot, open_fds());
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(pinned.pins.len(), 4);
    assert_eq!(
        at[&0] - at[&3],
        3,
        "one original descriptor released per validated slot"
    );
    drop(pinned);
    assert_eq!(open_fds(), baseline, "nothing but the hard links remain");
}

#[test]
fn each_slot_releases_its_original_descriptor_after_its_pin_validates() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "capture_recovery::cut::tests::descriptor_release_in_a_quiet_process",
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
fn a_crash_after_every_pin_resumes_from_the_intent_alone_and_a_late_producer_stays_outside_the_cut()
{
    let rig = Rig::new();
    let hold = rig.admit();
    historical(&rig, 3);
    let expectations = pin_all_but_cut(&rig, &hold);
    for slot in 0..3 {
        assert!(pin_path(&rig, slot).exists());
    }
    drop(hold);
    // "Another process": a fresh admission and the intent read from disk.
    let hold = rig.admit();
    let intent = intent_on_disk(&rig);
    let resumed = resume_pins(&hold, &intent).unwrap();
    let CutOutcome::Published { cut_sha256 } = resumed.cut.clone() else {
        panic!("published on resume")
    };
    assert_eq!(resumed.pins.len(), 3);
    assert!(resumed.reserved_pin.is_none());
    assert_eq!(
        resumed
            .pins
            .iter()
            .map(|r| r.expectation.clone())
            .collect::<Vec<_>>(),
        expectations
    );
    let late = enqueue(&rig.fixture, "late", 9, Start);
    let late_before = std::fs::symlink_metadata(&late).unwrap();
    let again = resume_pins(&hold, &intent).unwrap();
    assert_eq!(
        again.cut,
        CutOutcome::Resumed {
            cut_sha256: cut_sha256.clone()
        }
    );
    let manifest = CutManifest::decode(&std::fs::read(cut_path(&rig)).unwrap()).unwrap();
    assert!(manifest
        .entries
        .iter()
        .all(|e| late.file_name().unwrap() != e.source_name.as_str()));
    let late_after = std::fs::symlink_metadata(&late).unwrap();
    assert_eq!(late_after.nlink(), 1, "never linked");
    assert_eq!(late_after.ino(), late_before.ino());
    assert!(!pin_path(&rig, 3).exists());
}

#[test]
fn a_missing_pin_after_restart_stays_owed_even_when_an_identical_same_name_source_exists() {
    let rig = Rig::new();
    let hold = rig.admit();
    historical(&rig, 2);
    let selected = selected(&rig, &hold);
    let second = selected.retained[1].expectation.clone();
    let owed = pin_all_with(&hold, selected, &mut |point| {
        if point == Phase::AfterLink(0) {
            Err(())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(owed, Owed::Resource);
    assert!(pin_path(&rig, 0).exists());
    assert!(!pin_path(&rig, 1).exists());
    // The second original is gone and an identical B takes its name.
    let path = source_path(&rig, &second);
    let (_, job) = crate::agent_queue::read_job(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    let replacement = crate::agent_queue::enqueue_in(&queue_dir(&rig.fixture), &job).unwrap();
    assert_eq!(replacement, path);
    let b = std::fs::symlink_metadata(&replacement).unwrap();
    assert_ne!(b.ino(), second.ino);
    drop(hold);
    let hold = rig.admit();
    let intent = intent_on_disk(&rig);
    assert_eq!(
        resume_pins(&hold, &intent).unwrap_err(),
        Owed::PinMissing(1)
    );
    assert!(!pin_path(&rig, 1).exists(), "never recreated");
    assert_eq!(
        std::fs::symlink_metadata(&replacement).unwrap().nlink(),
        1,
        "B is never linked"
    );
    assert!(!cut_path(&rig).exists());
}

#[test]
fn a_present_pin_that_fails_its_expectation_is_owed_as_a_mismatch() {
    for kind in ["copy", "mode"] {
        let rig = Rig::new();
        let hold = rig.admit();
        historical(&rig, 2);
        pin_all_but_cut(&rig, &hold);
        let pin = pin_path(&rig, 1);
        match kind {
            "copy" => {
                let copy = rig.fixture.evidence.join("copy");
                std::fs::copy(&pin, &copy).unwrap();
                std::fs::rename(&copy, &pin).unwrap();
            }
            _ => {
                std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o640)).unwrap();
            }
        }
        drop(hold);
        let hold = rig.admit();
        let intent = intent_on_disk(&rig);
        assert_eq!(
            resume_pins(&hold, &intent).unwrap_err(),
            Owed::PinMismatch(1),
            "{kind}"
        );
        assert!(!cut_path(&rig).exists(), "{kind}");
    }
}

#[test]
fn a_replaced_private_directory_or_parent_refuses_the_resume() {
    for kind in ["directory", "parent"] {
        let rig = Rig::new();
        let hold = rig.admit();
        historical(&rig, 2);
        pin_all_but_cut(&rig, &hold);
        drop(hold);
        let private = rig.fixture.evidence.clone();
        let names = ["plan.json", INTENT_NAME, "pin-00000", "pin-00001"];
        if kind == "directory" {
            let old = private.with_extension("old");
            std::fs::rename(&private, &old).unwrap();
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&private)
                .unwrap();
            for name in names {
                std::fs::hard_link(old.join(name), private.join(name)).unwrap();
            }
        } else {
            let parent = private.parent().unwrap().to_owned();
            let old = parent.with_extension("old");
            std::fs::rename(&parent, &old).unwrap();
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&parent)
                .unwrap();
            std::fs::rename(old.join("operation"), &private).unwrap();
            std::fs::remove_dir_all(&old).unwrap();
        }
        for name in names {
            assert!(private.join(name).exists(), "{kind}: {name}");
        }
        let hold = rig.admit();
        let intent = intent_on_disk(&rig);
        assert_eq!(
            resume_pins(&hold, &intent).unwrap_err(),
            Owed::ScopeMismatch,
            "{kind}"
        );
        assert!(!cut_path(&rig).exists(), "{kind}");
    }
}

#[test]
fn an_existing_different_cut_refuses_and_an_equal_one_resumes_without_overwrite() {
    let rig = Rig::new();
    let hold = rig.admit();
    historical(&rig, 2);
    let pinned = pin_all(&hold, selected(&rig, &hold)).unwrap();
    let CutOutcome::Published { cut_sha256 } = pinned.cut else {
        panic!("published")
    };
    let intent = intent_on_disk(&rig);
    assert_eq!(
        resume_pins(&hold, &intent).unwrap().cut,
        CutOutcome::Resumed {
            cut_sha256: cut_sha256.clone()
        }
    );
    let bytes = std::fs::read(cut_path(&rig)).unwrap();
    let mut other = CutManifest::decode(&bytes).unwrap();
    other.entries.pop();
    other.selected_count -= 1;
    std::fs::write(cut_path(&rig), other.encode().unwrap()).unwrap();
    assert_eq!(resume_pins(&hold, &intent).unwrap_err(), Owed::CutConflict);
    assert_eq!(
        std::fs::read(cut_path(&rig)).unwrap(),
        other.encode().unwrap(),
        "never overwritten"
    );
    std::fs::remove_file(cut_path(&rig)).unwrap();
    std::os::unix::fs::symlink("elsewhere", cut_path(&rig)).unwrap();
    assert_eq!(resume_pins(&hold, &intent).unwrap_err(), Owed::CutConflict);
}

#[test]
fn no_historical_cut_pins_the_reserved_source_and_writes_no_cut_file() {
    let rig = Rig::new();
    let hold = rig.admit();
    open_turn(&rig, "cur");
    enqueue(&rig.fixture, "cur", 1, Start);
    let selected = selected(&rig, &hold);
    assert_eq!(selected.cut, Cut::NoHistoricalCut);
    let pinned = pin_all(&hold, selected).unwrap();
    assert_eq!(pinned.cut, CutOutcome::NoHistoricalCut);
    assert!(pinned.pins.is_empty());
    let reserved = pinned.reserved_pin.unwrap();
    assert_eq!(reserved.expectation.pin_slot, 0);
    assert_eq!(
        std::fs::symlink_metadata(pin_path(&rig, 0)).unwrap().ino(),
        reserved.expectation.ino
    );
    assert!(!cut_path(&rig).exists());
    let intent = intent_on_disk(&rig);
    let resumed = resume_pins(&hold, &intent).unwrap();
    assert_eq!(resumed.cut, CutOutcome::NoHistoricalCut);
    assert!(resumed.reserved_pin.is_some());
    assert!(!cut_path(&rig).exists());
    drop(resumed);
    std::fs::remove_file(intent_path(&rig)).unwrap();
    std::fs::remove_file(pin_path(&rig, 0)).unwrap();
    std::fs::remove_file(queue_dir(&rig.fixture).join(&reserved.expectation.source_name)).unwrap();
    let empty = pin_all(&hold, self::selected(&rig, &hold)).unwrap();
    assert_eq!(empty.cut, CutOutcome::NoHistoricalCut);
    assert!(empty.pins.is_empty() && empty.reserved_pin.is_none());
    assert!(!cut_path(&rig).exists());
}

#[test]
fn a_selection_that_is_not_the_durable_intent_is_refused_before_any_link() {
    let rig = Rig::new();
    let hold = rig.admit();
    historical(&rig, 2);
    let mut selected = selected(&rig, &hold);
    selected.intent_sha256 = "0".repeat(64);
    assert_eq!(
        pin_all(&hold, selected).unwrap_err(),
        Owed::IntentUnavailable
    );
    assert!(!pin_path(&rig, 0).exists());
    let mut selected = self::selected(&rig, &hold);
    selected.retained.pop();
    assert_eq!(pin_all(&hold, selected).unwrap_err(), Owed::IntentMismatch);
    assert!(!pin_path(&rig, 0).exists());
    let _ = boundary_job;
}

mod manifest_tests {
    use super::super::*;
    use crate::capture_recovery::selection::intent::InventoryCounts;
    use crate::capture_recovery::selection::{Identity, KnownBoundary, Scope};

    fn expectation(slot: u32, name: &str, key: char) -> Expectation {
        Expectation {
            pin_slot: slot,
            source_key: key.to_string().repeat(64),
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
                native_session: format!("n{slot}"),
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
            policy: selection::POLICY.into(),
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

    fn manifest(entries: Vec<Expectation>) -> CutManifest {
        CutManifest::from_intent(&intent(entries), &"f".repeat(64))
    }

    #[test]
    fn the_canonical_bytes_are_sorted_by_source_key_lf_free_and_the_file_sha_is_the_commitment() {
        // Slot order is b then a; the Cut is keyed a then b.
        let value = manifest(vec![
            expectation(0, "b.json", 'b'),
            expectation(1, "a.json", 'a'),
        ]);
        assert_eq!(value.entries[0].pin_slot, 1);
        assert_eq!(value.entries[0].queue_root, "agent");
        assert_eq!(value.entries[0].file_type, "regular");
        assert_eq!(value.selected_count, 2);
        let bytes = value.encode().unwrap();
        assert!(!bytes.ends_with(b"\n"));
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.starts_with(r#"{"entries":[{"attempts":0,"context_sha256""#));
        let shuffled = text
            .replacen("{\"entries\"", "{\"schema_version\":1,\"entries\"", 1)
            .replacen(",\"schema_version\":1", "", 1);
        assert_ne!(shuffled.as_bytes(), bytes.as_slice());
        let decoded = CutManifest::decode(shuffled.as_bytes()).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(decoded.encode().unwrap(), bytes);
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            format!("{:x}", Sha256::digest(decoded.encode().unwrap()))
        );
        let duplicate = text.replacen("\"repo_id\"", "\"repo_id\":1,\"repo_id\"", 1);
        assert!(CutManifest::decode(duplicate.as_bytes()).is_err());
        let unknown = text.replacen("{\"entries\"", "{\"extra\":1,\"entries\"", 1);
        assert!(CutManifest::decode(unknown.as_bytes()).is_err());
    }

    #[test]
    fn the_reserved_observation_is_excluded_and_the_count_equals_the_entries() {
        let mut with_reserved = intent(vec![expectation(0, "a.json", 'a')]);
        with_reserved.reserved_observation = Some(selection::ReservedObservation {
            expectation: expectation(1, "r.json", 'c'),
            event_key: "1".repeat(64),
            receipt_id: "2".repeat(64),
            cache_evidence_sha256: "3".repeat(64),
        });
        with_reserved.inventory.tickets = 2;
        assert!(SelectionIntent::decode(&with_reserved.encode().unwrap()).is_ok());
        let cut = CutManifest::from_intent(&with_reserved, &"f".repeat(64));
        assert_eq!(cut.selected_count, 1);
        assert_eq!(cut.entries.len(), 1);
        assert_eq!(cut.entries[0].source_name, "a.json");
        assert!(cut.encode().is_ok());
        let mut counted = cut.clone();
        counted.selected_count = 2;
        assert!(counted.encode().is_err());
        assert!(CutManifest::decode(&cut.encode().unwrap()).is_ok());
    }

    #[test]
    fn duplicate_keys_names_slots_and_inode_aliases_refuse_before_any_bytes_exist() {
        let good = manifest(vec![
            expectation(0, "a.json", 'a'),
            expectation(1, "b.json", 'b'),
        ]);
        assert!(good.encode().is_ok());
        let mut alias = good.clone();
        alias.entries[1].source_ino = alias.entries[0].source_ino;
        assert!(alias.encode().is_err());
        let mut name = good.clone();
        name.entries[1].source_name = name.entries[0].source_name.clone();
        assert!(name.encode().is_err());
        let mut slot = good.clone();
        slot.entries[1].pin_slot = slot.entries[0].pin_slot;
        assert!(slot.encode().is_err());
        let mut key = good.clone();
        key.entries[1].source_key = key.entries[0].source_key.clone();
        assert!(key.encode().is_err());
        let mut unsorted = good.clone();
        unsorted.entries.swap(0, 1);
        assert!(unsorted.encode().is_err());
        let mut literal = good.clone();
        literal.entries[0].queue_root = "worker".into();
        assert!(literal.encode().is_err());
        let mut literal = good.clone();
        literal.entries[0].file_type = "symlink".into();
        assert!(literal.encode().is_err());
        let mut pin_name = good;
        pin_name.entries[0].pin_name = "../pin".into();
        assert!(pin_name.encode().is_err());
    }

    #[test]
    fn the_header_entry_and_file_caps_refuse_before_any_whole_serialization() {
        let widest = expectation(0, &format!("{}.json", "n".repeat(251)), 'a');
        let mut wide = PinnedEntry::of(&widest);
        wide.known_boundary.native_session = "s".repeat(200);
        assert!(serde_json::to_vec(&wide).unwrap().len() > MAX_PINNED_ENTRY_BYTES);
        let mut over = manifest(vec![]);
        over.entries.push(wide);
        over.selected_count = 1;
        assert!(
            over.encode().is_err(),
            "a maximal entry is over the entry cap"
        );
        let fits = PinnedEntry::of(&expectation(0, "a.json", 'a'));
        assert!(serde_json::to_vec(&fits).unwrap().len() <= MAX_PINNED_ENTRY_BYTES);
        let mut many = manifest(vec![]);
        for slot in 0..12_000u32 {
            let mut entry = expectation(slot, &format!("{}{slot}.json", "w".repeat(180)), 'a');
            entry.source_key = format!("{slot:064x}");
            entry.known_boundary.native_session = "s".repeat(60);
            many.entries.push(PinnedEntry::of(&entry));
        }
        many.selected_count = 12_000;
        let sized = many.measure().unwrap();
        assert!(sized.predicted_len().unwrap() > MAX_CUT_BYTES);
        assert!(
            many.encode().is_err(),
            "the file cap, from the measured parts"
        );
        let mut too_many = manifest(vec![]);
        for slot in 0..=8192u32 {
            let mut entry = expectation(slot, &format!("{slot}.json"), 'a');
            entry.source_key = format!("{slot:064x}");
            too_many.entries.push(PinnedEntry::of(&entry));
        }
        too_many.selected_count = 8193;
        assert!(too_many.measure().is_ok());
        assert!(too_many.encode().is_err(), "the core ceiling");
        let few = manifest(
            (0..3)
                .map(|slot| {
                    let mut entry = expectation(slot, &format!("{slot}.json"), 'a');
                    entry.source_key = format!("{slot:064x}");
                    entry
                })
                .collect(),
        );
        let sized = few.measure().unwrap();
        assert_eq!(sized.predicted_len().unwrap(), few.encode().unwrap().len());
    }
}

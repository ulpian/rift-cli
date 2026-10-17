//! The expectation-driven half of preservation: the retained descriptor is
//! the sample, the shared scope is opened once per operation, and an existing
//! pin reopens only through the frozen expectation it was made under.
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};

use super::*;
use crate::agent_queue::BoundaryEdge::Start;
use crate::capture_recovery::eligibility::fixtures::Rig;
use crate::capture_recovery::hold::OwnHold;
use crate::capture_recovery::inventory::tests::{enqueue, queue_dir};
use crate::capture_recovery::selection::{select, Identity, Selected};

fn selected(rig: &Rig, hold: &OwnHold<'_>) -> Selected {
    rig.rt
        .block_on(select(&rig.store, hold, &rig.registry))
        .unwrap()
}

fn scope_of(hold: &OwnHold<'_>) -> SharedScope {
    let queue = hold.agent_queue_namespace().unwrap();
    shared_scope(
        hold.owner(),
        &queue.queue().path,
        &queue.private().unwrap().path,
    )
    .unwrap()
}

fn ready_sessions(rig: &Rig) {
    rift_shadow::hooks::run_state::create_private_dir(&rig.sessions_dir).unwrap();
}

fn pin_path(rig: &Rig, expectation: &Expectation) -> PathBuf {
    rig.fixture.evidence.join(&expectation.pin_name)
}

fn source_path(rig: &Rig, expectation: &Expectation) -> PathBuf {
    queue_dir(&rig.fixture).join(&expectation.source_name)
}

fn three_historical(rig: &Rig) {
    for ordinal in 1..=3 {
        enqueue(&rig.fixture, "old", ordinal, Start);
    }
    ready_sessions(rig);
}

#[test]
fn the_retained_descriptor_is_the_sample_and_the_pin_aliases_the_original_inode() {
    let rig = Rig::new();
    let hold = rig.admit();
    three_historical(&rig);
    let selected = selected(&rig, &hold);
    let scope = scope_of(&hold);
    let mut originals = selected.retained.into_iter();
    let gone = originals.next().unwrap();
    let kept = originals.next().unwrap();
    // The name is gone; the retained descriptor still proves what it was.
    std::fs::remove_file(source_path(&rig, &gone.expectation)).unwrap();
    let frozen = inspect_retained(hold.owner(), &scope, &gone).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&frozen.raw_bytes)),
        gone.expectation.ticket_sha256
    );
    assert_eq!(frozen.identity.ino, gone.expectation.ino);
    let gone_pin = pin_path(&rig, &gone.expectation);
    assert!(preserve_selected(hold.owner(), &scope, gone, frozen).is_err());
    assert!(!gone_pin.exists(), "a hard link needs the name");
    let expectation = kept.expectation.clone();
    let frozen = inspect_retained(hold.owner(), &scope, &kept).unwrap();
    let live = preserve_selected(hold.owner(), &scope, kept, frozen).unwrap();
    verify_pin(hold.owner(), &live).unwrap();
    let pin = std::fs::symlink_metadata(pin_path(&rig, &expectation)).unwrap();
    assert_eq!(pin.ino(), expectation.ino);
    assert_eq!(pin.mode() & 0o7777, expectation.mode);
    assert_eq!(pin.mode() & 0o7777, 0o600);
    assert_eq!(
        live.original_identity(),
        (expectation.dev, expectation.ino, expectation.mode)
    );
    assert_eq!(
        std::fs::read(pin_path(&rig, &expectation)).unwrap(),
        std::fs::read(source_path(&rig, &expectation)).unwrap()
    );
    drop(live);
    assert_eq!(
        std::fs::symlink_metadata(pin_path(&rig, &expectation))
            .unwrap()
            .nlink(),
        2,
        "the private hard link outlives the transient handles"
    );
}

#[test]
fn a_retained_descriptor_that_disagrees_with_its_expectation_refuses_before_any_link() {
    for kind in ["bytes", "mode", "attempts", "inode"] {
        let rig = Rig::new();
        let hold = rig.admit();
        three_historical(&rig);
        let selected = selected(&rig, &hold);
        let scope = scope_of(&hold);
        let mut original = selected.retained.into_iter().next().unwrap();
        let path = source_path(&rig, &original.expectation);
        match kind {
            "bytes" => {
                let mut bytes = std::fs::read(&path).unwrap();
                bytes.push(b' ');
                std::fs::write(&path, bytes).unwrap();
            }
            "mode" => {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
            }
            "attempts" => original.expectation.attempts += 1,
            _ => original.expectation.ino += 1,
        }
        assert!(
            inspect_retained(hold.owner(), &scope, &original).is_err(),
            "{kind}"
        );
        assert!(!pin_path(&rig, &original.expectation).exists(), "{kind}");
    }
}

#[test]
fn reopen_expected_accepts_only_the_pin_that_aliases_the_original_inode() {
    let rig = Rig::new();
    let hold = rig.admit();
    three_historical(&rig);
    let selected = selected(&rig, &hold);
    let scope = scope_of(&hold);
    let original = selected.retained.into_iter().next().unwrap();
    let expectation = original.expectation.clone();
    let frozen = inspect_retained(hold.owner(), &scope, &original).unwrap();
    drop(preserve_selected(hold.owner(), &scope, original, frozen).unwrap());
    let reopened = reopen_expected(hold.owner(), &scope, &expectation).unwrap();
    let pin_bytes = std::fs::read(pin_path(&rig, &expectation)).unwrap();
    assert_eq!(
        reopened.selection().raw_sha256,
        <[u8; 32]>::from(Sha256::digest(&pin_bytes)),
        "the frozen selection is rebuilt from the pin's own bytes"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(&pin_bytes)),
        expectation.ticket_sha256
    );
    drop(reopened);
    let mut wrong = expectation.clone();
    wrong.ino += 1;
    assert!(reopen_expected(hold.owner(), &scope, &wrong).is_err());
    let mut wrong = expectation.clone();
    wrong.attempts += 1;
    assert!(reopen_expected(hold.owner(), &scope, &wrong).is_err());
    let pin = pin_path(&rig, &expectation);
    std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o640)).unwrap();
    assert!(
        reopen_expected(hold.owner(), &scope, &expectation).is_err(),
        "a changed mode"
    );
    std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(reopen_expected(hold.owner(), &scope, &expectation).is_ok());
    let copy = rig.fixture.evidence.join("copy");
    std::fs::copy(&pin, &copy).unwrap();
    std::fs::rename(&copy, &pin).unwrap();
    assert_eq!(
        std::fs::read(&pin).unwrap(),
        std::fs::read(source_path(&rig, &expectation)).unwrap()
    );
    assert!(
        reopen_expected(hold.owner(), &scope, &expectation).is_err(),
        "equal bytes on another inode"
    );
    std::fs::remove_file(&pin).unwrap();
    assert!(reopen_expected(hold.owner(), &scope, &expectation).is_err());
}

#[test]
fn the_shared_scope_binds_the_intent_scope_and_refuses_an_aliased_or_nested_private_directory() {
    let rig = Rig::new();
    let hold = rig.admit();
    three_historical(&rig);
    let selected = selected(&rig, &hold);
    let scope = scope_of(&hold);
    scope.recheck(hold.owner()).unwrap();
    scope.matches_scope(&selected.intent.scope).unwrap();
    let mut other = selected.intent.scope.clone();
    other.private_dir_identity = Identity {
        dev: other.private_dir_identity.dev,
        ino: other.private_dir_identity.ino + 1,
    };
    assert!(scope.matches_scope(&other).is_err());
    let mut other = selected.intent.scope.clone();
    other.source_ancestry.pop();
    assert!(scope.matches_scope(&other).is_err());
    let mut other = selected.intent.scope.clone();
    other.private_dir.push('x');
    assert!(scope.matches_scope(&other).is_err());
    let first = &selected.retained[0].expectation;
    assert!(scope.source_present(&first.source_name).unwrap());
    assert!(!scope.pin_present(&first.pin_name).unwrap());
    assert!(!scope.source_present("absent.json").unwrap());
    assert!(scope.source_present("../escape").is_err());
    let queue = queue_dir(&rig.fixture);
    assert!(shared_scope(hold.owner(), &queue, &queue).is_err());
    let nested = rig.fixture.work_tree.join("private");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&nested)
        .unwrap();
    assert!(shared_scope(hold.owner(), &queue, &nested).is_err());
    let other_rig = Rig::new();
    assert!(shared_scope(&other_rig.fixture.owner, &queue, &rig.fixture.evidence).is_err());
}

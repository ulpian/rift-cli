#![cfg(unix)]
use super::*;
use crate::capture_recovery::hold::admit_own_hold;
use crate::capture_recovery::test_support::Fixture;

fn legacy_job(f: &Fixture, attempts: u32) -> (PathBuf, Vec<u8>) {
    let job = crate::agent_queue::AgentJob {
        schema_version: 1,
        work_tree: f.work_tree.clone(),
        queued_at_ms: 1000,
        attempts,
        kind: crate::agent_queue::JobKind::MoveRunState,
    };
    let path =
        crate::agent_queue::enqueue_in(&RunRoot::AgentQueue.legacy_under(f.owner.git_dir()), &job)
            .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    (path, bytes)
}

fn unmark(f: &Fixture) {
    std::fs::remove_file(f.member.join(MOVED_MARKER)).unwrap();
}

fn assert_absent(path: &Path) {
    assert!(path
        .symlink_metadata()
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound));
}

#[test]
fn exact_own_hold_uses_normal_readiness_and_preserves_migrated_context_and_attempts() {
    let f = Fixture::new();
    unmark(&f);
    let (old, bytes) = legacy_job(&f, 2);
    let hold = std::fs::read(f.hold_path()).unwrap();
    let admitted = admit_own_hold(&f.owner, &f.evidence, "fixture-op").unwrap();
    assert!(super::super::prepare_owned(&f.owner).is_err());
    super::super::prepare_recovery(&admitted).unwrap();
    assert!(!old.exists());
    assert!(f.member.join(MOVED_MARKER).exists());
    let target = RunRoot::AgentQueue
        .under(&f.member)
        .join(old.file_name().unwrap());
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
    assert_eq!(crate::agent_queue::read_job(&target).unwrap().1.attempts, 2);
    let job = crate::agent_queue::read_job(&target).unwrap().1;
    assert!(crate::capture_queue::remove_owned(&target, &f.owner).is_err());
    assert!(
        crate::agent_queue::record_failure_owned(&target, &job, "fixture-failure", &f.owner)
            .is_err()
    );
    assert_eq!(std::fs::read(&target).unwrap(), bytes);
    assert_eq!(std::fs::read(f.hold_path()).unwrap(), hold);
}

fn permission_bits(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o7777
}

fn assert_exact_job_if_present(path: &Path, expected: &[u8], mode: u32, attempts: u32) -> bool {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(error) => panic!("fixture job could not be read: {error}"),
    };
    assert_eq!(bytes, expected);
    assert_eq!(permission_bits(path), mode);
    let value = crate::capture_queue::delivery::decode(&bytes).unwrap();
    let original = crate::capture_queue::delivery::decode(expected).unwrap();
    assert_eq!(value, original);
    assert_eq!(
        crate::capture_queue::delivery::context(&value).unwrap(),
        crate::capture_queue::delivery::context(&original).unwrap()
    );
    assert_eq!(
        crate::capture_queue::delivery::attempts(&value).unwrap(),
        attempts
    );
    true
}

#[test]
fn own_hold_does_not_bypass_legacy_owner_mover_stage_or_complete_context_conflicts() {
    for barrier in [
        "legacy-owner",
        "legacy-mover",
        "active-mover",
        "stage",
        "conflict",
    ] {
        let f = Fixture::new();
        unmark(&f);
        let (old, bytes) = legacy_job(&f, 2);
        let old_mode = permission_bits(&old);
        let hold = std::fs::read(f.hold_path()).unwrap();
        let stage = RunRoot::AgentQueue
            .legacy_under(f.owner.git_dir())
            .join("private.stage");
        let mut mover = None;
        let mut standing = None;
        let mut stage_mode = None;
        match barrier {
            "legacy-owner" => put(&old.parent().unwrap().join(".lock"), b"legacy"),
            "legacy-mover" => put(&f.run_root.join(MOVE_LOCK), b"legacy"),
            "active-mover" => mover = super::super::MoveLock::take(&f.run_root, NOW).unwrap(),
            "stage" => {
                put(&stage, b"unfinished");
                stage_mode = Some(permission_bits(&stage));
            }
            _ => {
                let mut job = crate::agent_queue::read_job(&old).unwrap().1;
                job.attempts = 1;
                let path =
                    crate::agent_queue::enqueue_in(&RunRoot::AgentQueue.under(&f.member), &job)
                        .unwrap();
                let bytes = std::fs::read(&path).unwrap();
                let mode = permission_bits(&path);
                standing = Some((path, bytes, mode));
            }
        }
        let admitted = admit_own_hold(&f.owner, &f.evidence, "fixture-op").unwrap();
        eprintln!("OWN_HOLD_BARRIER cause={barrier}");
        assert!(
            super::super::prepare_recovery(&admitted).is_err(),
            "{barrier}"
        );
        if barrier == "stage" {
            assert_eq!(std::fs::read(&stage).unwrap(), b"unfinished");
            assert_eq!(Some(permission_bits(&stage)), stage_mode);
            let target = RunRoot::AgentQueue
                .under(&f.member)
                .join(old.file_name().unwrap());
            let old_present = assert_exact_job_if_present(&old, &bytes, old_mode, 2);
            let new_present = assert_exact_job_if_present(&target, &bytes, old_mode, 2);
            eprintln!("OWN_HOLD_STAGE old_present={old_present} new_present={new_present}");
            assert!(old_present || new_present);
        } else {
            assert!(assert_exact_job_if_present(&old, &bytes, old_mode, 2));
        }
        if let Some((path, bytes, mode)) = standing {
            assert!(assert_exact_job_if_present(&path, &bytes, mode, 1));
        }
        assert_absent(&f.member.join(MOVED_MARKER));
        assert_eq!(std::fs::read(f.hold_path()).unwrap(), hold);
        drop(mover);
    }
}

#[test]
fn revoking_hold_after_actual_queue_publication_preserves_source_and_refuses_marker() {
    let f = Fixture::new();
    unmark(&f);
    let (old, bytes) = legacy_job(&f, 2);
    let admitted = admit_own_hold(&f.owner, &f.evidence, "fixture-op").unwrap();
    let path = f.hold_path();
    let _pause = install_migration_pause(MigrationPausePoint::QueuePublished, move || {
        std::fs::remove_file(&path).unwrap();
    });
    assert!(super::super::prepare_recovery(&admitted).is_err());
    assert_eq!(std::fs::read(&old).unwrap(), bytes);
    let target = RunRoot::AgentQueue
        .under(&f.member)
        .join(old.file_name().unwrap());
    assert_eq!(std::fs::read(target).unwrap(), bytes);
    assert_eq!(crate::agent_queue::read_job(&old).unwrap().1.attempts, 2);
    assert!(!f.member.join(MOVED_MARKER).exists());
}

#[test]
fn revoking_hold_after_session_publication_leaves_both_copies_and_no_marker() {
    let f = Fixture::new();
    unmark(&f);
    let old = RunRoot::Sessions
        .legacy_under(f.owner.git_dir())
        .join("session.json");
    put(&old, b"session-evidence");
    let admitted = admit_own_hold(&f.owner, &f.evidence, "fixture-op").unwrap();
    let path = f.hold_path();
    let _pause = install_migration_pause(MigrationPausePoint::SessionPublished, move || {
        std::fs::remove_file(&path).unwrap();
    });
    assert!(super::super::prepare_recovery(&admitted).is_err());
    assert_eq!(std::fs::read(old).unwrap(), b"session-evidence");
    assert_eq!(
        std::fs::read(RunRoot::Sessions.under(&f.member).join("session.json")).unwrap(),
        b"session-evidence"
    );
    assert!(!f.member.join(MOVED_MARKER).exists());
}

#[test]
fn revoking_the_hold_before_completion_does_not_publish_a_moved_marker() {
    let f = Fixture::new();
    unmark(&f);
    let admitted = admit_own_hold(&f.owner, &f.evidence, "fixture-op").unwrap();
    let path = f.hold_path();
    let _pause = install_migration_pause(MigrationPausePoint::BeforeMarker, move || {
        std::fs::remove_file(&path).unwrap();
    });
    assert!(super::super::prepare_recovery(&admitted).is_err());
    assert!(!f.member.join(MOVED_MARKER).exists());
}

#[test]
fn a_cold_unmoved_member_is_admitted_without_provisioning_then_migrates_normally() {
    for existing_run in [false, true] {
        let f = Fixture::cold();
        if existing_run {
            std::fs::create_dir_all(&f.run_root).unwrap();
        }
        assert_absent(&f.member);
        assert!(!f.member.join(MOVED_MARKER).exists());
        let (old, bytes) = legacy_job(&f, 2);
        let admitted = admit_own_hold(&f.owner, &f.evidence, "fixture-op").unwrap();
        assert_absent(&f.member);
        if existing_run {
            assert!(f.run_root.is_dir());
        } else {
            assert_absent(&f.run_root);
        }
        assert!(super::super::prepare_owned(&f.owner).is_err());
        assert_absent(&f.member);
        super::super::prepare_recovery(&admitted).unwrap();
        assert!(!old.exists());
        assert!(f.member.join(MOVED_MARKER).exists());
        let target = RunRoot::AgentQueue
            .under(&f.member)
            .join(old.file_name().unwrap());
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        assert_eq!(crate::agent_queue::read_job(&target).unwrap().1.attempts, 2);
        admitted.check_current().unwrap();
        let saved = f.member.with_file_name("fixture-saved-member");
        std::fs::rename(&f.member, &saved).unwrap();
        std::fs::create_dir(&f.member).unwrap();
        assert!(admitted.check_current().is_err());
        assert_eq!(
            std::fs::read(saved.join("agent-queue").join(target.file_name().unwrap())).unwrap(),
            bytes
        );
    }
}

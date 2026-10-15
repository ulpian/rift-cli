//! Execution failures are bounded only after healthy semantic admission.
use super::*;
use crate::agent_queue::{AgentJob, IntervalArchive, IntervalReason, JobKind};
use rift_store::CanonicalStore as _;

fn owed_interval(root: &Path) -> (rift_git::RepoPaths, AgentJob, std::path::PathBuf) {
    let (repo, condense) = super::tests::queue_session_end(root, "owed-interval");
    let owner = crate::capture_queue::acquire_owner(&repo.git_dir).unwrap();
    crate::capture_queue::remove_owned(&condense, &owner).unwrap();
    drop(owner);
    let cache = rift_shadow::hooks::SessionCache::new("owed-interval", "claude-code", "active", 1);
    rift_shadow::hooks::write_cache(
        &crate::family::run_state::sessions_dir(&repo.git_dir),
        &cache,
    )
    .unwrap();
    let job = AgentJob {
        schema_version: 1,
        work_tree: root.to_owned(),
        queued_at_ms: 1000,
        attempts: 0,
        kind: JobKind::Archive(IntervalArchive {
            session_id: "owed-interval".into(),
            agent: "claude-code".into(),
            reason: IntervalReason::SensitivePath,
        }),
    };
    let path = crate::agent_queue::enqueue(&repo.git_dir, &job).unwrap();
    (repo, job, path)
}

#[test]
fn an_admitted_interval_publication_failure_spends_exactly_three_attempts() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, job, path) = owed_interval(tmp.path());
    let store = rift_store::open_capture_projection(projection_db_at(tmp.path()).unwrap()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let JobKind::Archive(archive) = &job.kind else {
        panic!("archive fixture")
    };
    assert!(owed_interval_decision(&rt, &store, &repo, &job, archive)
        .unwrap()
        .is_some());
    let before = rt
        .block_on(store.query("SELECT * FROM checkpoint ORDER BY checkpoint_id", &[]))
        .unwrap();
    let obstruction = repo.common_dir.join("refs/heads/rift/checkpoints/v1.lock");
    std::fs::create_dir_all(obstruction.parent().unwrap()).unwrap();
    std::fs::write(&obstruction, b"publication-obstruction").unwrap();
    for attempt in 1..=crate::capture_queue::MAX_ATTEMPTS {
        drain_jobs(repo.git_dir.to_str(), LockWait::None);
        if attempt < crate::capture_queue::MAX_ATTEMPTS {
            assert_eq!(
                crate::agent_queue::read_job(&path).unwrap().1.attempts,
                attempt
            );
        } else {
            assert!(!path.exists());
        }
        assert_eq!(
            std::fs::read(&obstruction).unwrap(),
            b"publication-obstruction"
        );
        assert_eq!(
            rt.block_on(store.query("SELECT * FROM checkpoint ORDER BY checkpoint_id", &[]))
                .unwrap(),
            before
        );
    }
    assert!(crate::capture_queue::peek_errors(&repo.git_dir)
        .iter()
        .any(|note| note.contains("interval archive of session owed-interval")));
}

#[test]
fn a_healthy_owed_interval_archives_after_projection_ownership_is_released() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, _, path) = owed_interval(tmp.path());
    let db = projection_db_at(tmp.path()).unwrap();
    let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
    let before = std::fs::read(&path).unwrap();
    drain_jobs(repo.git_dir.to_str(), LockWait::None);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(crate::capture_queue::peek_errors(&repo.git_dir).is_empty());
    drop(guard);
    drain_jobs(repo.git_dir.to_str(), LockWait::None);
    assert!(!path.exists());
    let store = rift_store::open_capture_projection(&db).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert_eq!(rt.block_on(store.count("checkpoint")).unwrap(), 1);
}

#[test]
fn an_admitted_interval_recovers_on_the_next_drain_after_publication_unblocks() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, job, path) = owed_interval(tmp.path());
    let db = projection_db_at(tmp.path()).unwrap();
    let store = rift_store::open_capture_projection(&db).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let JobKind::Archive(archive) = &job.kind else {
        panic!("archive fixture")
    };
    assert!(owed_interval_decision(&rt, &store, &repo, &job, archive)
        .unwrap()
        .is_some());
    let obstruction = repo.common_dir.join("refs/heads/rift/checkpoints/v1.lock");
    std::fs::create_dir_all(obstruction.parent().unwrap()).unwrap();
    std::fs::write(&obstruction, b"publication-obstruction").unwrap();
    drain_jobs(repo.git_dir.to_str(), LockWait::None);
    let retry = crate::agent_queue::read_job(&path).unwrap().1;
    assert_eq!(retry.attempts, 1);
    assert_eq!(retry.kind, job.kind);
    assert_eq!(rt.block_on(store.count("checkpoint")).unwrap(), 0);
    std::fs::remove_file(&obstruction).unwrap();
    drain_jobs(repo.git_dir.to_str(), LockWait::None);
    assert!(!path.exists());
    assert_eq!(rt.block_on(store.count("checkpoint")).unwrap(), 1);
}

#[test]
fn exact_condense_context_redelivery_uses_its_existing_checkpoint_proof() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, path) = super::tests::queue_session_end(tmp.path(), "exact-redelivery");
    let original = crate::agent_queue::read_job(&path).unwrap().1;
    let first = drain_jobs(repo.git_dir.to_str(), LockWait::None);
    assert_eq!(first.len(), 1);
    assert!(!path.exists());
    let replay = crate::agent_queue::enqueue(&repo.git_dir, &original).unwrap();
    assert_eq!(replay, path);
    assert!(drain_jobs(repo.git_dir.to_str(), LockWait::None).is_empty());
    assert!(!replay.exists());
    let store = rift_store::open_capture_projection(projection_db_at(tmp.path()).unwrap()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert_eq!(rt.block_on(store.count("checkpoint")).unwrap(), 1);
}

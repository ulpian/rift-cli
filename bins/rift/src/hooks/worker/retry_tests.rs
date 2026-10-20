//! Execution failures are bounded only after healthy semantic admission.
use super::*;
use crate::agent_queue::{AgentJob, IntervalArchive, IntervalReason, JobKind};
use rift_store::CanonicalStore as _;

pub(super) fn owed_interval(root: &Path) -> (rift_git::RepoPaths, AgentJob, std::path::PathBuf) {
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
            interval_start_ms: None,
        }),
    };
    let path = crate::agent_queue::enqueue(&repo.git_dir, &job).unwrap();
    (repo, job, path)
}

#[test]
fn an_admitted_interval_publication_failure_spends_three_attempts_then_parks() {
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
    // L-0A Q5 / design §4.1: the sweep's drain ignores the backoff, so each
    // pass spends one attempt; the third parks the job — present AND parked,
    // with its backoff set — where it used to remove it.
    for attempt in 1..=crate::capture_queue::PARK_AFTER {
        drain_jobs(repo.git_dir.to_str(), LockWait::None);
        let (_, _, state) = crate::agent_queue::retry_states(&repo.git_dir).remove(0);
        assert_eq!(state.attempts, attempt);
        assert!(state.next_due_ms > 0);
        assert_eq!(state.parked, attempt == crate::capture_queue::PARK_AFTER);
        assert!(path.exists(), "parked, never removed");
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

/// The ordinary worker spawned with only work it cannot finish now: an
/// unproved historical condense (a legacy done marker proves nothing, so it
/// stays owed) and an interval archive whose publication is blocked. The
/// drain must end in bounded time and give the queue lock back, so recovery
/// and the next spawn can run; it must never sit on the lock re-selecting
/// the same jobs.
#[test]
fn a_worker_left_only_unprovable_and_failing_work_exits_and_frees_the_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, _job, archive) = owed_interval(tmp.path());
    let obstruction = repo.common_dir.join("refs/heads/rift/checkpoints/v1.lock");
    std::fs::create_dir_all(obstruction.parent().unwrap()).unwrap();
    std::fs::write(&obstruction, b"publication-obstruction").unwrap();
    let (_, condense) = super::tests::queue_session_end(tmp.path(), "unproved-end");
    crate::agent_queue::mark_condensed(&repo.git_dir, "unproved-end");
    let git_dir = repo.git_dir.to_str().unwrap().to_owned();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = drain_queues(Some(&git_dir), LockWait::Brief);
        let _ = done.send(());
    });
    assert!(
        finished
            .recv_timeout(std::time::Duration::from_mins(1))
            .is_ok(),
        "the worker kept the queue lock with nothing it could finish"
    );
    assert!(crate::capture_queue::acquire_owner(&repo.git_dir).is_ok());
    assert!(archive.exists(), "the failing job is kept, backed off");
    assert!(
        condense.exists(),
        "the unproved job stays owed for recovery"
    );
}

/// Live 2026-10-20: a worker that condensed once went on re-walking the whole
/// checkpoint branch after every later pass, each one opened by a job an
/// agent had just enqueued, so it never let go of the lock. A pass that
/// wrote no checkpoint must not level the projection.
#[test]
fn a_later_pass_that_writes_no_checkpoint_does_not_level_the_projection_again() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, condense) = super::tests::queue_session_end(tmp.path(), "levels-once");
    let (git_dir, work_tree) = (repo.git_dir.clone(), tmp.path().to_owned());
    let _pause = super::run_state_tests::before_release(move || {
        assert!(owe_search_index(&git_dir, &work_tree));
    });
    LEVELS.with(|levels| levels.set(0));

    let condensed = drain_queues(repo.git_dir.to_str(), LockWait::None).unwrap();

    assert_eq!(condensed.len(), 1);
    assert!(!condense.exists());
    assert_eq!(
        crate::agent_queue::job_file_count(&repo.git_dir),
        0,
        "the job that arrived between passes ran in a second pass"
    );
    assert_eq!(
        LEVELS.with(std::cell::Cell::get),
        1,
        "only the pass that condensed levels"
    );
}

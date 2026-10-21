//! A member whose run state an older binary left in the git dir: what a hook
//! reads, writes and asks for before the move, and the worker's move.

use std::path::{Path, PathBuf};

use crate::agent_queue::{AgentJob, JobKind};

use super::{run_agent_job, Landed};

/// A checkout with its family minted, and its git dir.
fn checkout() -> (tempfile::TempDir, PathBuf) {
    let checkout = tempfile::tempdir().unwrap();
    let initialized = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(checkout.path())
        .status()
        .unwrap();
    assert!(initialized.success());
    let git_dir = rift_git::discover(checkout.path()).unwrap().git_dir;
    crate::family::resolve_at(checkout.path()).unwrap();
    (checkout, git_dir)
}

/// The job the drain below consumes: one the schema refuses, so its run is
/// complete without a projection and leaves one note on the capture queue.
fn refused_job(work_tree: &Path) -> AgentJob {
    AgentJob {
        schema_version: 1,
        work_tree: work_tree.to_owned(),
        queued_at_ms: 1,
        attempts: 0,
        kind: JobKind::ToolResultTokens {
            tool_result_id: "d".repeat(64),
            session_id: "s-old".into(),
            tool_name: "Bash".into(),
            channel: "telepathy".into(),
            search: crate::search_measure::MeasuredSearch::Unmeasured,
            tree_key: None,
        },
    }
}

/// Seed the old tree only, as an older binary leaves it: one session cache,
/// and the refused job in the agent journal.
fn seed_old_tree(git_dir: &Path, work_tree: &Path) {
    let old_sessions = git_dir.join("rift-sessions");
    std::fs::create_dir_all(&old_sessions).unwrap();
    let cache = rift_shadow::hooks::SessionCache::new("s-old", "claude-code", "active", 5);
    std::fs::write(
        old_sessions.join("s-old.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
    // The job file as the journal names it, written by the journal's own
    // writer into a directory of its own, then placed in the old tree.
    let scratch = tempfile::tempdir().unwrap();
    let written = crate::agent_queue::enqueue_in(scratch.path(), &refused_job(work_tree)).unwrap();
    let old_queue = git_dir.join("rift-agent-queue");
    std::fs::create_dir_all(&old_queue).unwrap();
    std::fs::rename(&written, old_queue.join(written.file_name().unwrap())).unwrap();
}

fn move_jobs(git_dir: &Path) -> usize {
    crate::agent_queue::jobs(git_dir)
        .iter()
        .filter(|(_, job)| job.kind == JobKind::MoveRunState)
        .count()
}

/// Before the move a hook reads the old tree's session through the dual
/// read, journals one move job however often it resolves, and moves nothing.
#[test]
fn a_hook_on_an_unmoved_member_reads_through_and_asks_the_worker_once() {
    let (checkout, git_dir) = checkout();
    seed_old_tree(&git_dir, checkout.path());

    let sessions = crate::family::run_state::sessions_dir(&git_dir);
    crate::family::run_state::sessions_dir(&git_dir);
    assert!(!sessions.starts_with(&git_dir), "{}", sessions.display());
    let read = rift_shadow::hooks::read_cache(&sessions, "s-old").unwrap();
    assert_eq!(read.map(|c| c.updated_at_ms), Some(5));
    assert_eq!(move_jobs(&git_dir), 1);
    assert!(git_dir.join("rift-sessions").join("s-old.json").is_file());
    assert!(
        git_dir.join("rift-agent-queue").is_dir(),
        "a hook moves nothing"
    );
}

/// A hook's write before the move lands in the new tree and still stands
/// after the worker moves; the worker's move takes the rest and empties the
/// old tree; and the old-tree-only job was drained by the real worker step.
#[test]
fn run_state_seeded_only_in_the_old_tree_is_read_written_drained_and_moved_by_the_worker() {
    let (checkout, git_dir) = checkout();
    seed_old_tree(&git_dir, checkout.path());

    let sessions = crate::family::run_state::sessions_dir(&git_dir);
    let mut cache = rift_shadow::hooks::read_cache(&sessions, "s-old")
        .unwrap()
        .unwrap();
    cache.current_turn_ordinal = 3;
    rift_shadow::hooks::write_cache(&sessions, &cache).unwrap();
    assert!(
        sessions.join("s-old.json").is_file(),
        "written to the new tree"
    );

    let t = crate::hooktime::HookTimer::new("test", false);
    let jobs = crate::agent_queue::jobs(&git_dir);
    let (path, job) = jobs
        .iter()
        .find(|(_, job)| job.kind != JobKind::MoveRunState)
        .unwrap();
    assert_eq!(
        run_agent_job(
            &t,
            &git_dir,
            path,
            job,
            &crate::capture_queue::acquire_owner(&git_dir).unwrap()
        ),
        Landed::Nothing
    );
    assert_eq!(crate::capture_queue::peek_errors(&git_dir).len(), 1);

    let (path, job) = jobs
        .iter()
        .find(|(_, job)| job.kind == JobKind::MoveRunState)
        .unwrap();
    assert_eq!(
        run_agent_job(
            &t,
            &git_dir,
            path,
            job,
            &crate::capture_queue::acquire_owner(&git_dir).unwrap()
        ),
        Landed::Nothing
    );
    assert!(crate::agent_queue::jobs(&git_dir).is_empty(), "drained");
    let kept = rift_shadow::hooks::read_cache(&sessions, "s-old")
        .unwrap()
        .unwrap();
    assert_eq!(kept.current_turn_ordinal, 3, "the hook's write stands");
    assert!(!git_dir.join("rift-sessions").exists());
    assert!(!git_dir.join("rift-agent-queue").exists());
}

/// A hook never waits for a mover: with another mover's lock held and the
/// member unmarked, it resolves at once, reads through, and leaves the lock
/// and the old tree alone.
#[test]
fn a_hook_never_waits_for_a_mover_holding_the_lock() {
    let (checkout, git_dir) = checkout();
    seed_old_tree(&git_dir, checkout.path());
    let sessions = crate::family::run_state::sessions_dir(&git_dir);
    let run_root = sessions.parent().unwrap().parent().unwrap().to_path_buf();
    let lock = run_root.join(rift_shadow::hooks::run_state::MOVE_LOCK);
    std::fs::write(&lock, crate::hooks::now_ms().to_string()).unwrap();

    let again = crate::family::run_state::sessions_dir(&git_dir);
    assert_eq!(again, sessions);
    assert!(rift_shadow::hooks::read_cache(&again, "s-old")
        .unwrap()
        .is_some());
    assert!(lock.is_file() && git_dir.join("rift-sessions").is_dir());
}

thread_local! {
    static BEFORE_RELEASE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
pub(super) struct ReleasePause(Option<Box<dyn FnOnce()>>);
impl Drop for ReleasePause {
    fn drop(&mut self) {
        BEFORE_RELEASE.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}
pub(super) fn before_release(callback: impl FnOnce() + 'static) -> ReleasePause {
    ReleasePause(BEFORE_RELEASE.with(|slot| slot.replace(Some(Box::new(callback)))))
}
pub(super) fn pause_before_owner_release() {
    let callback = BEFORE_RELEASE.with(std::cell::RefCell::take);
    if let Some(callback) = callback {
        callback();
    }
}

#[test]
fn controlled_release_rechecks_new_delivery_without_repeating_deferred_work() {
    let (checkout, git_dir) = checkout();
    let deferred = AgentJob {
        schema_version: 1,
        work_tree: checkout.path().join("gone"),
        queued_at_ms: 1,
        attempts: 0,
        kind: JobKind::Condense {
            session_id: "deferred-c".into(),
            agent: "claude-code".into(),
            repo_id: "repo".into(),
        },
    };
    let c = crate::agent_queue::enqueue(&git_dir, &deferred).unwrap();
    let original = std::fs::read(&c).unwrap();
    let delivered = std::rc::Rc::new(std::cell::RefCell::new(None));
    let observed = delivered.clone();
    let held_dir = git_dir.clone();
    let root = checkout.path().to_owned();
    let pause = before_release(move || {
        assert!(crate::capture_queue::acquire_owner(&held_dir).is_err());
        let b = AgentJob {
            schema_version: 1,
            work_tree: root,
            queued_at_ms: 2,
            attempts: 0,
            kind: JobKind::ReadTokens {
                tool_call_id: "b".repeat(64),
                rel_path: "absent.txt".into(),
                content_sha256: "c".repeat(64),
                blob_oid: "d".repeat(40),
                lines: None,
                schema_version: 1,
            },
        };
        let path = crate::agent_queue::enqueue(&held_dir, &b).unwrap();
        assert_eq!(crate::agent_queue::read_job(&path).unwrap().1, b);
        *observed.borrow_mut() = Some(path);
    });
    super::drain_jobs(git_dir.to_str(), super::LockWait::None);
    drop(pause);
    let b = delivered
        .borrow()
        .clone()
        .expect("actual B publication before release");
    assert!(
        !b.exists(),
        "post-release pass executes B's explicit unmeasured decline"
    );
    assert_eq!(std::fs::read(&c).unwrap(), original);
    assert_eq!(crate::agent_queue::read_job(&c).unwrap().1.attempts, 0);
    assert_eq!(crate::agent_queue::jobs(&git_dir).len(), 1);
}

#[test]
fn globally_unready_migration_skips_the_actual_worker_outer_tail() {
    use rift_store::CanonicalStore as _;
    let tmp = tempfile::tempdir().unwrap();
    let (repo, job) = super::tests::queue_session_end(tmp.path(), "blocked-tail");
    let sessions = crate::family::run_state::sessions_dir(&repo.git_dir);
    let cache = sessions.join("blocked-tail.json");
    let usage = sessions.join("blocked-tail.usage");
    std::fs::write(&usage, b"unsettled-usage-evidence").unwrap();
    let before: Vec<_> = [&cache, &usage, &job]
        .into_iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect();
    let store =
        rift_store::open_capture_projection(crate::hooks::projection_db_at(tmp.path()).unwrap())
            .unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let checkpoints = rt
        .block_on(store.query("SELECT * FROM checkpoint ORDER BY checkpoint_id", &[]))
        .unwrap();
    let barrier = repo.git_dir.join("rift-capture-queue/.lock");
    std::fs::create_dir_all(barrier.parent().unwrap()).unwrap();
    std::fs::write(&barrier, b"legacy-ownership-unresolved").unwrap();
    assert!(
        super::drain_queues(repo.git_dir.to_str(), super::LockWait::None)
            .unwrap()
            .is_empty()
    );
    for (path, bytes) in [&cache, &usage, &job].into_iter().zip(before) {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    assert_eq!(
        rt.block_on(store.query("SELECT * FROM checkpoint ORDER BY checkpoint_id", &[]))
            .unwrap(),
        checkpoints
    );
    assert_eq!(
        std::fs::read(&barrier).unwrap(),
        b"legacy-ownership-unresolved"
    );
}

/// A job published while a worker drains, with recovery asking for the
/// queue: the worker lets go at the pass boundary instead of taking the new
/// job, recovery takes the owner lock, and once recovery releases it the
/// next drain takes the job.
#[test]
fn a_draining_worker_yields_to_a_recovery_request_and_the_next_drain_takes_what_arrived() {
    let (checkout, git_dir) = checkout();
    let first = crate::agent_queue::enqueue(&git_dir, &refused_job(checkout.path())).unwrap();
    let delivered = std::rc::Rc::new(std::cell::RefCell::new(None));
    let observed = delivered.clone();
    let held_dir = git_dir.clone();
    let root = checkout.path().to_owned();
    let pause = before_release(move || {
        let mut late = refused_job(&root);
        late.queued_at_ms = 2;
        if let JobKind::ToolResultTokens { tool_result_id, .. } = &mut late.kind {
            *tool_result_id = "e".repeat(64);
        }
        *observed.borrow_mut() = Some(crate::agent_queue::enqueue(&held_dir, &late).unwrap());
        let request = crate::capture_queue::request::request_path(&held_dir);
        std::fs::write(request, b"1").unwrap();
    });
    super::drain_jobs(git_dir.to_str(), super::LockWait::None);
    drop(pause);
    assert!(!first.exists(), "the pass in flight finished its job");
    let late = delivered.borrow().clone().unwrap();
    assert!(late.exists(), "the worker let go before the next pass");

    let recovery = crate::capture_queue::acquire_owner(&git_dir).unwrap();
    std::fs::remove_file(crate::capture_queue::request::request_path(&git_dir)).unwrap();
    super::drain_jobs(git_dir.to_str(), super::LockWait::None);
    assert!(
        late.exists(),
        "no drain runs while recovery holds the queue"
    );
    drop(recovery);
    super::drain_jobs(git_dir.to_str(), super::LockWait::None);
    assert!(
        !late.exists(),
        "the drain after recovery takes what arrived"
    );
}

thread_local! {
    static AFTER_JOB: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
pub(super) struct JobPause(Option<Box<dyn FnOnce()>>);
impl Drop for JobPause {
    fn drop(&mut self) {
        AFTER_JOB.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}
pub(super) fn after_first_job(callback: impl FnOnce() + 'static) -> JobPause {
    JobPause(AFTER_JOB.with(|slot| slot.replace(Some(Box::new(callback)))))
}
pub(super) fn pause_after_job() {
    let callback = AFTER_JOB.with(std::cell::RefCell::take);
    if let Some(callback) = callback {
        callback();
    }
}

/// One pass over many owed jobs outlasts the wait `--fix` gives it, so a
/// request placed after the first job stops the pass there: the job in
/// flight finished, the rest are still queued untouched, and the lock is free
/// for recovery.
#[test]
fn a_recovery_request_placed_mid_pass_stops_the_pass_after_the_job_in_flight() {
    let (checkout, git_dir) = checkout();
    let queued: Vec<_> = (0..5u8)
        .map(|n| {
            let mut job = refused_job(checkout.path());
            job.queued_at_ms = i64::from(n) + 1;
            if let JobKind::ToolResultTokens { tool_result_id, .. } = &mut job.kind {
                *tool_result_id = format!("{n:064}");
            }
            crate::agent_queue::enqueue(&git_dir, &job).unwrap()
        })
        .collect();
    let before: Vec<_> = queued
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect();
    let held_dir = git_dir.clone();
    let pause = after_first_job(move || {
        let request = crate::capture_queue::request::request_path(&held_dir);
        std::fs::write(request, b"1").unwrap();
    });
    super::drain_jobs(git_dir.to_str(), super::LockWait::None);
    drop(pause);
    let left: Vec<_> = queued.iter().filter(|path| path.exists()).collect();
    assert_eq!(left.len(), 4, "only the job in flight ran");
    for (path, bytes) in queued.iter().zip(&before) {
        if path.exists() {
            assert_eq!(
                &std::fs::read(path).unwrap(),
                bytes,
                "a queued job stays untouched"
            );
        }
    }
    let recovery = crate::capture_queue::acquire_owner(&git_dir);
    assert!(recovery.is_ok(), "the worker released the owner lock");
}

//! The worker's own tests, moved out of `worker.rs` by concern
//! (a move only): the drain, the condense, the interval archive and the
//! typed-defer controls, over real `git init` fixtures.

use super::*;
use crate::agent_queue::{AgentJob, IntervalArchive, IntervalReason, JobKind};

/// ★ The job that counts a read binds the outline of what was read and
/// caches it for the read gate, before anything about the count can fail.
#[test]
fn the_read_count_job_binds_and_caches_the_outline_of_what_was_read() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    rift_git::test_support::fake_repo(root).unwrap();
    let body = "fn alpha() {\n    1\n}\n\nfn beta() {\n    2\n}\n";
    std::fs::write(root.join("lib.rs"), body).unwrap();
    let sha = rift_recall_substrate::worktree::sha256_hex(body.as_bytes());
    let job = AgentJob {
        schema_version: 1,
        work_tree: root.to_path_buf(),
        queued_at_ms: 1_700_000_000_000,
        attempts: 0,
        kind: JobKind::ReadTokens {
            tool_call_id: "0".repeat(64),
            rel_path: "lib.rs".to_owned(),
            content_sha256: sha.clone(),
            blob_oid: rift_recall_substrate::worktree::git_blob_oid(body.as_bytes()),
            lines: None,
            schema_version: 1,
        },
    };
    // Whatever becomes of the count row, the outline is already bound.
    let _ = run_job_kind(
        &crate::hooktime::HookTimer::from_env("test"),
        &root.join(".git"),
        &job,
        &crate::capture_queue::acquire_owner(&root.join(".git")).unwrap(),
        &crate::capture_queue::delivery::context(&serde_json::to_value(&job).unwrap()).unwrap(),
    );
    let common_dir = rift_git::discover(root).unwrap().common_dir;
    let cached =
        crate::hooks::gate::outline_cache_dir(&common_dir).join(format!("{sha}-e3p1r1.outline"));
    let text = std::fs::read_to_string(&cached).expect("the outline is cached");
    assert!(
        text.starts_with("rift-outline 1\nclass code\nshape 7 "),
        "{text}"
    );
    assert!(
        text.contains(" alpha\n") && text.contains(" beta\n"),
        "{text}"
    );
}

#[test]
fn a_session_condense_without_a_recall_projection_archives_once() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, path) = queue_session_end(checkout.path(), "s-absent");
    let recall = crate::family::recall_db(checkout.path()).unwrap();
    let git_dir = repo.git_dir.to_str().unwrap();
    let condensed = drain_jobs(Some(git_dir), LockWait::None);
    assert_eq!(condensed.len(), 1);
    assert_eq!(condensed[0].0, "s-absent");
    assert!(!path.exists());
    assert!(crate::capture_queue::peek_errors(&repo.git_dir).is_empty());
    assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
    // The condense never opens recall; the store exists afterwards only
    // because the checkpoint it wrote asked for one recall ingest.
    assert!(recall.exists());
    assert_eq!(crate::agent_queue::job_file_count(&repo.git_dir), 0);
}

#[test]
fn a_busy_interval_archive_preserves_its_typed_cause_and_existing_retry_budget() {
    let checkout = tempfile::tempdir().unwrap();
    fixture_git(checkout.path(), &["init", "--quiet"]);
    fixture_git(
        checkout.path(),
        &["commit", "--quiet", "--allow-empty", "-m", "fixture"],
    );
    let repo = rift_git::discover(checkout.path()).unwrap();
    let db = crate::family::projection_db(checkout.path()).unwrap();
    let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
    let archive = IntervalArchive {
        session_id: "s-interval".into(),
        agent: "claude-code".into(),
        reason: IntervalReason::Schedule,
        interval_start_ms: None,
    };
    let job = AgentJob {
        schema_version: 1,
        work_tree: checkout.path().to_owned(),
        queued_at_ms: 1,
        attempts: 0,
        kind: JobKind::Archive(archive.clone()),
    };
    let timer = crate::hooktime::HookTimer::new("test", false);
    assert!(matches!(
        interval_archive_job(&timer, &job, &archive),
        Err(rift_core::RiftError::ProjectionBusy { .. })
    ));
    let path = crate::agent_queue::enqueue(&repo.git_dir, &job).unwrap();
    let before = std::fs::read(&path).unwrap();
    let git_dir = repo.git_dir.to_str().unwrap();
    for _ in 0..crate::capture_queue::PARK_AFTER + 2 {
        assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(crate::capture_queue::peek_errors(&repo.git_dir).is_empty());
    }
    drop(guard);
    assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(crate::agent_queue::read_job(&path).unwrap().1.attempts, 0);
    assert!(crate::capture_queue::peek_errors(&repo.git_dir)
        .iter()
        .any(|note| note.contains("interval completion lacks exact delivery proof")));
}

#[test]
fn busy_attribution_keeps_the_existing_job_even_after_the_retry_budget() {
    let checkout = tempfile::tempdir().unwrap();
    let initialized = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(checkout.path())
        .status()
        .unwrap();
    assert!(initialized.success());
    let repo = rift_git::discover(checkout.path()).unwrap();
    let db = crate::family::recall_db(checkout.path()).unwrap();
    let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
    let git_dir = repo.git_dir.to_str().unwrap();
    for attempts in [0, crate::capture_queue::PARK_AFTER, 7] {
        let job = AgentJob {
            schema_version: 1,
            work_tree: checkout.path().to_owned(),
            queued_at_ms: 1,
            attempts,
            kind: JobKind::AttributeServe(crate::agent_queue::ServeClaim {
                session: "s-1".into(),
                window: None,
                call_id: "a".repeat(32),
                audit_id: "b".repeat(64),
            }),
        };
        let path = crate::agent_queue::enqueue(&repo.git_dir, &job).unwrap();
        let before = std::fs::read(&path).unwrap();
        for _ in 0..crate::capture_queue::PARK_AFTER + 2 {
            assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        assert!(crate::capture_queue::peek_errors(&repo.git_dir).is_empty());
        assert!(
            !db.exists(),
            "a busy no-source gap must not mint a database"
        );
    }
    drop(guard);
    let (path, _) = crate::agent_queue::jobs(&repo.git_dir).pop().unwrap();
    assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
    assert!(!path.exists(), "the next worker can complete the same job");
    assert!(!db.exists(), "legitimate absence remains an omission");
    assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
    assert!(crate::agent_queue::jobs(&repo.git_dir).is_empty());
}

#[test]
fn busy_is_classified_by_its_typed_cause_through_context() {
    #[derive(Debug)]
    struct Context(rift_core::RiftError);
    impl std::fmt::Display for Context {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "capture job failed: {}", self.0)
        }
    }
    impl std::error::Error for Context {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }
    let busy = Context(rift_core::RiftError::ProjectionBusy {
        projection: "projection.db".into(),
        reason: "replacement owns its marker".into(),
    });
    assert!(projection_busy(&busy));
    assert!(!projection_busy(&rift_core::RiftError::Other(
        busy.to_string()
    )));
}

#[test]
fn repeated_busy_commit_drains_keep_the_job_then_record_its_facts_once() {
    let checkout = tempfile::tempdir().unwrap();
    fixture_git(checkout.path(), &["init", "--quiet"]);
    std::fs::write(checkout.path().join("file.txt"), b"captured line\n").unwrap();
    fixture_git(checkout.path(), &["add", "file.txt"]);
    fixture_git(checkout.path(), &["commit", "--quiet", "-m", "fixture"]);
    let commit = fixture_git(checkout.path(), &["rev-parse", "HEAD"]);
    let repo = rift_git::discover(checkout.path()).unwrap();
    let db = crate::family::projection_db(checkout.path()).unwrap();
    let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
    let entry = crate::capture_queue::QueueEntry {
        schema_version: 1,
        commit: commit.trim().into(),
        work_tree: checkout.path().to_owned(),
        session_id: None,
        agent: None,
        session_state: None,
        queued_at_ms: 1,
        attempts: 0,
    };
    let path = crate::capture_queue::enqueue(&repo.git_dir, &entry).unwrap();
    let before = std::fs::read(&path).unwrap();
    let git_dir = repo.git_dir.to_str().unwrap();
    for _ in 0..crate::capture_queue::PARK_AFTER + 2 {
        assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(crate::capture_queue::peek_errors(&repo.git_dir).is_empty());
    }
    drop(guard);
    assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
    assert!(!path.exists());
    assert!(drain_jobs(Some(git_dir), LockWait::None).is_empty());
    let store = rift_store::SqliteStore::open_read_only(&db).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert_eq!(
        rt.block_on(rift_store::CanonicalStore::count(&store, "file_revision"))
            .unwrap(),
        1
    );
    assert!(crate::capture_queue::entries(&repo.git_dir).is_empty());
}

#[test]
fn a_drained_session_end_condense_leaves_its_checkpoint_in_the_projection() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, path) = queue_session_end(checkout.path(), "s-level");
    let git_dir = repo.git_dir.to_str().unwrap();
    let condensed = drain_queues(Some(git_dir), LockWait::None).unwrap();
    assert_eq!(condensed.len(), 1);
    assert!(!path.exists());
    let checkpoint = condensed[0].1.clone();
    let db = crate::family::projection_db(checkout.path()).unwrap();
    let store = rift_store::SqliteStore::open_read_only(&db).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let rows = |sql: &str| {
        rt.block_on(rift_store::CanonicalStore::query_opt_i64(
            &store,
            sql,
            &[rift_store::Value::text(checkpoint.clone())],
        ))
        .unwrap()
    };
    assert_eq!(
        rows("SELECT COUNT(*) FROM checkpoint WHERE checkpoint_id = ?1"),
        Some(1)
    );
    assert_eq!(
        rows("SELECT COUNT(*) FROM session_in_checkpoint WHERE checkpoint_id = ?1"),
        Some(1)
    );
    let watermarks = rt
        .block_on(rift_store::CanonicalStore::count(
            &store,
            "ingest_watermark",
        ))
        .unwrap();
    assert!(watermarks >= 1, "the levelling records where it got to");
}

#[test]
fn a_busy_projection_defers_the_condense_and_its_levelling_without_spending_an_attempt() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, path) = queue_session_end(checkout.path(), "s-busy");
    let db = crate::family::projection_db(checkout.path()).unwrap();
    let guard = rift_store::ProjectionReplacementGuard::acquire(&db).unwrap();
    let before = std::fs::read(&path).unwrap();
    let git_dir = repo.git_dir.to_str().unwrap();
    for _ in 0..crate::capture_queue::PARK_AFTER + 2 {
        assert!(drain_queues(Some(git_dir), LockWait::None)
            .unwrap()
            .is_empty());
        assert_eq!(level_after_condense(&repo.git_dir), Levelled::Deferred);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(crate::agent_queue::jobs(&repo.git_dir).len(), 1);
        assert!(crate::capture_queue::peek_errors(&repo.git_dir).is_empty());
    }
    drop(guard);
    let condensed = drain_queues(Some(git_dir), LockWait::None).unwrap();
    assert_eq!(condensed.len(), 1);
    assert!(!path.exists());
    let store = rift_store::SqliteStore::open_read_only(&db).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert_eq!(
        rt.block_on(rift_store::CanonicalStore::count(&store, "checkpoint"))
            .unwrap(),
        1
    );
}

/// A committed checkout with one session in `ending` and its session-end
/// condense journaled, as the `SessionEnd` hook leaves them.
pub(super) fn queue_session_end(
    root: &Path,
    session: &str,
) -> (rift_git::RepoPaths, std::path::PathBuf) {
    fixture_git(root, &["init", "--quiet"]);
    fixture_git(
        root,
        &["commit", "--quiet", "--allow-empty", "-m", "fixture"],
    );
    let repo = rift_git::discover(root).unwrap();
    let family = crate::family::resolve_at(root).unwrap();
    let cache = rift_shadow::hooks::SessionCache::new(session, "claude-code", "ending", 1);
    rift_shadow::hooks::write_cache(
        &crate::family::run_state::sessions_dir(&repo.git_dir),
        &cache,
    )
    .unwrap();
    let job = AgentJob {
        schema_version: 1,
        work_tree: root.to_owned(),
        queued_at_ms: 1,
        attempts: 0,
        kind: JobKind::Condense {
            session_id: session.into(),
            agent: "claude-code".into(),
            repo_id: family.family_id.to_string(),
        },
    };
    let path = crate::agent_queue::enqueue(&repo.git_dir, &job).unwrap();
    (repo, path)
}

pub(super) fn fixture_git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=Fixture", "-c", "user.email=fixture-id"])
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_DATE", "2026-10-01T12:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-10-01T12:00:00Z")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// A missing checkout leaves an unchanged delivery without an execution attempt.
#[test]
fn a_gone_interval_checkout_stays_owed_without_spending_an_attempt() {
    let git_dir = tempfile::tempdir().unwrap();
    let job = AgentJob {
        schema_version: 1,
        work_tree: git_dir.path().join("gone"),
        queued_at_ms: 1,
        attempts: 0,
        kind: JobKind::Archive(IntervalArchive {
            session_id: "s-1".into(),
            agent: "claude-code".into(),
            reason: IntervalReason::Schedule,
            interval_start_ms: None,
        }),
    };
    let path = crate::agent_queue::enqueue(git_dir.path(), &job).unwrap();
    let before = std::fs::read(&path).unwrap();
    let t = crate::hooktime::HookTimer::new("test", false);
    assert_eq!(
        run_agent_job(
            &t,
            git_dir.path(),
            &path,
            &job,
            &crate::capture_queue::acquire_owner(git_dir.path()).unwrap()
        ),
        Landed::Nothing
    );
    let (_, kept) = crate::agent_queue::jobs(git_dir.path()).pop().unwrap();
    assert_eq!(kept.attempts, 0);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let notes = crate::capture_queue::peek_errors(git_dir.path());
    assert!(
        notes
            .iter()
            .any(|n| n.contains("job repository is unavailable")),
        "{notes:?}"
    );
}

/// A session with everything the final archive cleans up: a subagent
/// claim marker, a spooled original, and the two sidecars.
fn seed_session_leftovers(repo: &rift_git::RepoPaths, session: &str) -> [std::path::PathBuf; 5] {
    let sessions = crate::family::run_state::sessions_dir(&repo.git_dir);
    let claims = sessions.join(format!("{session}.subagents"));
    std::fs::create_dir_all(&claims).unwrap();
    std::fs::write(claims.join("agent-1.json"), b"{}").unwrap();
    let spool = repo
        .common_dir
        .join("rift-family")
        .join("spool")
        .join(session);
    std::fs::create_dir_all(&spool).unwrap();
    std::fs::write(spool.join("toolu_1.original"), b"spooled").unwrap();
    let transcript = sessions.join(format!("{session}.transcript"));
    std::fs::write(&transcript, b"{}").unwrap();
    let usage = sessions.join(format!("{session}.usage"));
    std::fs::write(&usage, b"{}").unwrap();
    let frontier = sessions.join(format!("{session}.turn-seal"));
    std::fs::write(&frontier, b"{}").unwrap();
    [claims, spool, transcript, usage, frontier]
}

/// The final archive (a session-end condense) removes the claims, the spool
/// and the three sidecars (transcript, usage, turn-seal frontier), and keeps the `condensed-` marker and the cache; an
/// interval archive of a live session — a seal that is not final — removes
/// nothing.
#[test]
fn the_final_archive_removes_claims_spool_and_sidecars_and_a_turn_seal_removes_nothing() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, path) = queue_session_end(checkout.path(), "s-final");
    let leftovers = seed_session_leftovers(&repo, "s-final");
    let git_dir = repo.git_dir.to_str().unwrap();
    assert_eq!(drain_jobs(Some(git_dir), LockWait::None).len(), 1);
    assert!(!path.exists());
    for left in &leftovers {
        assert!(
            !left.exists(),
            "{} is gone after the final archive",
            left.display()
        );
    }
    assert!(
        crate::agent_queue::is_condensed(&repo.git_dir, "s-final"),
        "the proof marker stays"
    );
    let sessions = crate::family::run_state::sessions_dir(&repo.git_dir);
    assert!(
        sessions.join("s-final.json").exists(),
        "the cache is retained as ended"
    );

    // Not final: an interval archive of a live session leaves everything.
    let interval = tempfile::tempdir().unwrap();
    let (repo, _job, path) = super::retry_tests::owed_interval(interval.path());
    let leftovers = seed_session_leftovers(&repo, "owed-interval");
    drain_jobs(repo.git_dir.to_str(), LockWait::None);
    assert!(!path.exists(), "the interval archive landed");
    for left in &leftovers {
        assert!(
            left.exists(),
            "{} survives a seal that is not final",
            left.display()
        );
    }
}

/// S9 condition 1: a cache that reads `active` after the completion is the
/// re-activation window, and the cleanup must not run — claims, spool and
/// sidecars stay — however the condense came out.
#[test]
fn a_cache_that_reads_active_after_completion_keeps_claims_spool_and_sidecars() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, _path) = queue_session_end(checkout.path(), "s-reactivated");
    let sessions = crate::family::run_state::sessions_dir(&repo.git_dir);
    let cache = rift_shadow::hooks::SessionCache::new("s-reactivated", "claude-code", "active", 2);
    rift_shadow::hooks::write_cache(&sessions, &cache).unwrap();
    let leftovers = seed_session_leftovers(&repo, "s-reactivated");
    drain_jobs(repo.git_dir.to_str(), LockWait::None);
    for left in &leftovers {
        assert!(
            left.exists(),
            "{} is kept for the reactivated session",
            left.display()
        );
    }
    assert_ne!(
        rift_shadow::hooks::session_cache::read_cache(&sessions, "s-reactivated")
            .unwrap()
            .unwrap()
            .state,
        "ended"
    );
}

/// An interval archive lands with the anchor absent on an unborn HEAD,
/// and `Ok(true)` means a real publication.
#[test]
fn an_interval_archive_lands_without_a_commit() {
    let checkout = tempfile::tempdir().unwrap();
    fixture_git(checkout.path(), &["init", "--quiet"]);
    let repo = rift_git::discover(checkout.path()).unwrap();
    let cache = rift_shadow::hooks::SessionCache::new("s-unborn", "claude-code", "active", 1);
    rift_shadow::hooks::write_cache(
        &crate::family::run_state::sessions_dir(&repo.git_dir),
        &cache,
    )
    .unwrap();
    let archive = IntervalArchive {
        session_id: "s-unborn".into(),
        agent: "claude-code".into(),
        reason: IntervalReason::SensitivePath,
        interval_start_ms: None,
    };
    let job = AgentJob {
        schema_version: 1,
        work_tree: checkout.path().to_owned(),
        queued_at_ms: 1_000,
        attempts: 0,
        kind: JobKind::Archive(archive.clone()),
    };
    let timer = crate::hooktime::HookTimer::new("test", false);
    assert!(
        archive_if_still_owed(&timer, &job, &archive).unwrap(),
        "the interval archive landed, anchorless"
    );
    let shards = rift_git::resolve_commit(&repo, "refs/heads/rift/checkpoints/v1").unwrap();
    assert!(
        shards.is_some(),
        "a checkpoint exists on an unborn repository"
    );
}

/// A `Due` drain before the due instant spends no attempt; the repair road
/// (`drain_jobs`, `Everything`) runs the same job and spends one.
#[test]
fn a_second_drain_before_the_due_instant_spends_no_attempt() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, job, path) = super::retry_tests::owed_interval(tmp.path());
    let owner = crate::capture_queue::acquire_owner(&repo.git_dir).unwrap();
    let (value, _) = crate::agent_queue::read_job(&path).unwrap();
    let stem = crate::agent_queue::logical_stem(&job).unwrap();
    let far = crate::capture_queue::delivery::RetryState {
        attempts: 1,
        next_due_ms: i64::MAX,
        parked: false,
    };
    crate::capture_queue::delivery::replace_retry_state(&path, &stem, &value, far, &owner).unwrap();
    drop(owner);
    let obstruction = repo.common_dir.join("refs/heads/rift/checkpoints/v1.lock");
    std::fs::create_dir_all(obstruction.parent().unwrap()).unwrap();
    std::fs::write(&obstruction, b"publication-obstruction").unwrap();

    let before = std::fs::read(&path).unwrap();
    assert!(drain_queues(repo.git_dir.to_str(), LockWait::None)
        .unwrap()
        .is_empty());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "a not-due job is skipped whole"
    );
    assert!(crate::capture_queue::peek_errors(&repo.git_dir).is_empty());
    drain_jobs(repo.git_dir.to_str(), LockWait::None);
    let (_, _, state) = crate::agent_queue::retry_states(&repo.git_dir).remove(0);
    assert_eq!(
        state.attempts, 2,
        "the repair road ran it and it failed once more"
    );
}

/// Two `SessionEnd`s for one session publish one delivery when their context
/// is the same, and two when it changed. One condense lands; the
/// changed-context delivery found after it carries no proof of its own, so it
/// stays owed with its typed reason and spends no attempt (the legacy arm of discharge by
/// fact, until capture recovery admits it) — never a second checkpoint.
#[test]
fn two_session_ends_for_one_session_publish_one_condense_delivery() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, path) = queue_session_end(checkout.path(), "s-twice");
    let (_, job) = crate::agent_queue::read_job(&path).unwrap();
    assert_eq!(
        crate::agent_queue::enqueue(&repo.git_dir, &job).unwrap(),
        path
    );
    assert_eq!(
        crate::agent_queue::jobs(&repo.git_dir).len(),
        1,
        "same context, one file"
    );
    let later = AgentJob {
        queued_at_ms: job.queued_at_ms + 1,
        ..job.clone()
    };
    let second = crate::agent_queue::enqueue(&repo.git_dir, &later).unwrap();
    assert_ne!(second, path, "a changed instant is a second delivery");
    assert_eq!(crate::agent_queue::jobs(&repo.git_dir).len(), 2);

    let condensed = drain_jobs(repo.git_dir.to_str(), LockWait::None);
    assert_eq!(condensed.len(), 1, "one condense lands: {condensed:?}");
    assert!(!path.exists(), "the first delivery is done and removed");
    assert!(second.exists(), "the second has no proof of its own: owed");
    let (_, _, state) = crate::agent_queue::retry_states(&repo.git_dir).remove(0);
    assert_eq!(state.attempts, 0, "a typed defer spends no attempt");
    let notes = crate::capture_queue::peek_errors(&repo.git_dir);
    assert!(
        notes.iter().any(|n| n.contains("lacks delivery proof")),
        "and it says why: {notes:?}"
    );
}

/// Discharge by fact: a delivery of the final condense found again **after** it landed — the
/// same context republished, as a hook retried by its client would — is
/// discharged by its fact: the exact-context completion and the checkpoint
/// it names exist, so the file is removed, no second checkpoint is minted and
/// no attempt is spent. (The turn seal joins this rule with its own job.)
#[test]
fn a_seal_or_condense_job_found_after_the_final_archive_is_discharged_by_its_fact() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, path) = queue_session_end(checkout.path(), "s-again");
    let (_, job) = crate::agent_queue::read_job(&path).unwrap();
    let condensed = drain_jobs(repo.git_dir.to_str(), LockWait::None);
    assert_eq!(condensed.len(), 1, "{condensed:?}");
    assert!(!path.exists());
    let tip = |repo: &rift_git::RepoPaths| {
        std::process::Command::new("git")
            .args(["rev-parse", "refs/heads/rift/checkpoints/v1"])
            .current_dir(&repo.work_tree)
            .output()
            .unwrap()
            .stdout
    };
    let before = tip(&repo);

    let again = crate::agent_queue::enqueue(&repo.git_dir, &job).unwrap();
    assert_eq!(again, path, "the same context is the same file");
    assert!(drain_jobs(repo.git_dir.to_str(), LockWait::None).is_empty());
    assert!(!again.exists(), "discharged by its fact, not left owed");
    assert_eq!(tip(&repo), before, "no second checkpoint");
    assert!(crate::agent_queue::retry_states(&repo.git_dir).is_empty());
}

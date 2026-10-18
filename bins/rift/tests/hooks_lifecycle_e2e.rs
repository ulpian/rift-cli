//! End-to-end: the session lifecycle around the archive — a `SessionEnd`
//! whose archive fails defers instead of closing, a condense never needs a
//! commit (an unborn HEAD closes on an anchorless checkpoint), the sweep
//! finishes what a hook could not, a crashed session is finalized beside its
//! live neighbour, and doctor's deferral line names the true cause.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;
use std::process::Command;

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    agent_queue_of, checkpoint_ref_exists, checkpoint_shards, doctor_fix_json, doctor_json, git_in,
    now_ms, rift_json, run_rift_in, run_rift_stdout, seed_live_session_repo,
    seed_live_session_repo_unborn, seed_session, session_phase, sessions_leg, shard_blob,
    shard_path_of, sync_env, sync_env_with_data,
};

/// "Crashed-session auto-finalize (doctor hook)" **and**
/// "concurrent sessions same base commit", which are one fixture: two sessions
/// live in one worktree against one base commit, one abandoned by a dead agent
/// and one current. A doctor pass must finalize exactly the dead one and leave
/// the other alone. Nothing else in the product can reach a crashed session —
/// it fires no more hooks — so without this pass it stayed `active` forever and
/// kept being offered to every later archive as the session to attribute to.
#[test]
fn doctor_finalizes_the_crashed_session_and_leaves_its_live_neighbour() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    // The seeded repo already carries one live session; remove it so the two
    // below are the whole population.
    std::fs::remove_file(common::seed_sessions_dir(&repo.join(".git")).join("e2e-live-1.json"))
        .unwrap();
    // The idle window is 24 h, so "now" and "the epoch" are unambiguous
    // regardless of when this test runs.
    seed_session(&repo, "crashed-1", 0);
    seed_session(&repo, "current-1", now_ms());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);

    let doc = doctor_fix_json(&repo, &env);
    let sessions = sessions_leg(&doc);
    assert_eq!(
        sessions["finalized"],
        serde_json::json!(["crashed-1"]),
        "exactly the abandoned session is finalized: {sessions}"
    );
    assert_eq!(
        sessions["live"],
        serde_json::json!(["current-1"]),
        "a session inside its idle window is untouched: {sessions}"
    );

    // The finalized session is closed in the cache, and the live one is not.
    assert_eq!(session_phase(&repo, "crashed-1"), "ended");
    assert_eq!(session_phase(&repo, "current-1"), "active");

    // Finalizing condensed real work: a checkpoint exists on the chain ref.
    assert!(
        checkpoint_ref_exists(&repo),
        "the finalized session condensed to a checkpoint"
    );

    // The sweep is idempotent: a second pass finds one live session and
    // finalizes nothing.
    let doc2 = doctor_fix_json(&repo, &env);
    let s2 = sessions_leg(&doc2);
    assert_eq!(s2["finalized"], serde_json::json!([]));
    assert_eq!(s2["live"], serde_json::json!(["current-1"]));
}

/// `doctor --fix` on a repository with **no commit** finalizes the crashed
/// session: the sweep archives with the anchor absent, so there is no
/// deferral line at all (L-0A §1.1; Q78 — a condense never needs a commit).
///
/// This used to pin the opposite — `condense still owed … no commit to anchor
/// to` — when an unborn HEAD was the one deferral cause left after the wrapper
/// route was deleted (2026-08-11). The negative assertions are kept
/// deliberately as resurrection guards: an absent binary is not a deferral
/// cause, so nothing may ever tell a developer to install one.
#[test]
fn the_doctor_fix_on_an_unborn_repo_finalizes_without_a_deferral_line() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-q", "."]);
    seed_session(&repo, "crashed-1", 0);

    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = sync_env_with_data(&cache, &data);
    let (code, stdout) = run_rift_stdout(&repo, &["doctor", "--fix"], &env);
    assert_eq!(code, 0, "doctor reports, it never fails: {stdout}");
    assert!(
        !stdout.contains("condense still owed"),
        "an unborn HEAD is not a deferral cause: {stdout}"
    );
    assert!(
        !stdout.contains("no commit to anchor"),
        "the retired cause is never written: {stdout}"
    );
    assert!(
        !stdout.contains("rift setup") && !stdout.to_lowercase().contains("not provisioned"),
        "nothing may send a developer to install a capture backend: there is \
         none, and there is nothing to install: {stdout}"
    );
    let doc = doctor_fix_json(&repo, &env);
    let sessions = sessions_leg(&doc);
    assert_eq!(
        sessions["condense_deferred"],
        serde_json::json!([]),
        "nothing deferred: {sessions}"
    );
    assert_eq!(session_phase(&repo, "crashed-1"), "ended");
    assert!(
        checkpoint_ref_exists(&repo),
        "the finalize condensed to an anchorless checkpoint"
    );
    assert!(
        !Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success(),
        "the user's HEAD is still unborn: nothing moved it"
    );
}

/// The unborn-HEAD sibling below never reaches the archive guard
/// (`drive_post_ingest` returns before `RequestCondense` when there is no
/// commit), so replacing the guard with a tautology left 197 tests green.
/// This test reaches it: a repo WITH a commit whose archive FAILS (the git
/// object store is made unwritable), driven through a real `SessionEnd`. The
/// property is the docstring's own claim — a `SessionEnd` that archived
/// nothing must not reach `ended`.
#[test]
#[cfg(unix)]
fn a_failing_archive_with_a_commit_present_still_defers_the_close() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("XDG_DATA_HOME", data.to_str().unwrap()),
        ("LOCALAPPDATA", data.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    // The archive path must WANT to run (a commit exists) and then FAIL
    // (nothing can write an object). chmod the object store read-only.
    let objects = repo.join(".git").join("objects");
    let restore: Vec<(std::path::PathBuf, u32)> = walkdir_dirs(&objects)
        .into_iter()
        .map(|d| {
            let mode = std::fs::metadata(&d).unwrap().permissions().mode();
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o555)).unwrap();
            (d, mode)
        })
        .collect();

    let body = r#"{"schema":1,"session":"e2e-live-1","turn":1,"payload":{"reason":"exit"}}"#;
    let code = run_rift_in(&repo, &["hooks", "claude-code", "SessionEnd"], body, &env);

    // Restore FIRST so a failing assertion cannot strand a read-only tempdir.
    for (d, mode) in restore {
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    assert_eq!(code, 0, "a capture hook never fails the agent, even here");
    assert_ne!(
        session_phase(&repo, "e2e-live-1"),
        "ended",
        "a SessionEnd whose archive FAILED must not close the session — \
         this is the guard the unborn-HEAD tests cannot reach"
    );
    // The failure is recorded as retry state on the job, not as a removal:
    // one attempt spent, a backoff set, not yet parked (L-0A §4.1).
    let (job, state) = only_condense_job(&repo);
    assert_eq!(job["attempts"], 1, "{job}");
    assert_eq!(state.attempts, 1);
    assert!(state.next_due_ms > 0, "a backoff is recorded: {job}");
    assert!(!state.parked, "one failure does not park: {job}");
}

/// A journaled job's retry state as the file carries it (`attempts`,
/// `next_due_ms`, `parked` are top-level siblings; absent reads as zero and
/// false), and the whole document for the assertion message.
fn only_condense_job(repo: &Path) -> (serde_json::Value, RetryState) {
    let dir = agent_queue_of(repo);
    let mut jobs: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "json")
                && p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("condense-"))
        })
        .collect();
    assert_eq!(jobs.len(), 1, "one condense job: {jobs:?}");
    let job: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(jobs.remove(0)).unwrap()).unwrap();
    let state = RetryState {
        attempts: job["attempts"].as_u64().unwrap_or(0),
        next_due_ms: job["next_due_ms"].as_i64().unwrap_or(0),
        parked: job["parked"].as_bool().unwrap_or(false),
    };
    (job, state)
}

/// The retry state the two sibling members spell.
#[derive(Debug, PartialEq, Eq)]
struct RetryState {
    attempts: u64,
    next_due_ms: i64,
    parked: bool,
}

/// Every directory under `root`, including itself — the minimal walker the
/// permission plant needs, no dependency added.
#[cfg(unix)]
fn walkdir_dirs(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let mut i = 0;
    while i < out.len() {
        if let Ok(rd) = std::fs::read_dir(&out[i]) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    out.push(p);
                }
            }
        }
        i += 1;
    }
    out
}

/// A condense never needs a commit, end to end through the real binary: a
/// `SessionEnd` on an **unborn HEAD** closes the session at once, on a
/// checkpoint under `refs/heads/rift/checkpoints/v1` whose metadata names no
/// `commit`, and the user's HEAD stays unborn (L-0A §1.1, Q78/Q81).
///
/// This test used to pin the opposite: the unborn HEAD was, after the wrapper
/// route died (2026-08-11), the one condition left under which the archive
/// "genuinely could not work around" a missing anchor, and a `SessionEnd`
/// then landed in `ending` for the sweep to rescue once a commit existed. The
/// anchor was never part of the id, so the record now lands without it; the
/// rule that still binds is the one the failed-archive test above keeps — a
/// `SessionEnd` whose archive did not mint a checkpoint must not reach `ended`.
#[test]
fn a_session_end_without_a_commit_closes_with_a_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo_unborn(tmp.path());
    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = sync_env_with_data(&cache, &data);

    let body = r#"{"schema":1,"session":"e2e-live-1","turn":1,"payload":{"reason":"exit"}}"#;
    assert_eq!(
        run_rift_in(&repo, &["hooks", "claude-code", "SessionEnd"], body, &env),
        0,
        "a capture hook never fails the agent"
    );

    assert_eq!(
        session_phase(&repo, "e2e-live-1"),
        "ended",
        "the condense landed, so the session closed at the hook: no sweep needed"
    );
    assert!(
        checkpoint_ref_exists(&repo),
        "an anchorless checkpoint exists on the checkpoint branch"
    );
    let metadata: serde_json::Value =
        serde_json::from_str(&shard_blob(&repo, &shard_path_of(&repo, "metadata.json"))).unwrap();
    assert!(
        metadata.get("commit").is_none(),
        "the shard names no anchor — the member is absent, never empty: {metadata}"
    );
    assert!(
        !Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success(),
        "the user's HEAD is still unborn: capture never commits for the user"
    );
    let dir = agent_queue_of(&repo);
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("condense-"))
        .collect();
    assert!(
        left.is_empty(),
        "the condense job was removed by its success"
    );
}

/// The other half of the same rule, and the regression guard for it: a
/// `SessionEnd` whose archive **does** run still closes the session on the spot,
/// exactly as before. Deferral is the exception the absent checkpoint earns, not
/// the new normal — without this pinned, "leave it `ending`" could quietly become
/// the only path and every session would need a sweep to close.
#[test]
fn a_session_end_that_archived_still_closes_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    let body = r#"{"schema":1,"session":"e2e-live-1","turn":1,"payload":{"reason":"exit"}}"#;
    assert_eq!(
        run_rift_in(&repo, &["hooks", "claude-code", "SessionEnd"], body, &env),
        0
    );
    assert_eq!(
        session_phase(&repo, "e2e-live-1"),
        "ended",
        "a condense that landed closes the session at the hook, no sweep needed"
    );
    assert!(
        !checkpoint_shards(&repo).is_empty(),
        "and it closed on a checkpoint that exists"
    );
}

/// The sweep when the condense **cannot** land because a worker holds the
/// member's lock — the case the crashed-session pass meets while a worker is
/// mid-drain, and the one that carried a measured regression.
///
/// Two things are pinned here, and they are the same defect seen from two ends.
/// (1) A deferred session must never become the repository's archive target:
/// `live_sessions` counts `ending` as live and returns most-recently-updated
/// first, so a sweep that stamped the cache would hand the *dead* session to the
/// next post-commit, and that attribution is durable. (2) A deferred session
/// must still be reachable: `Ending` has no `IdleTimeout` edge and `post-commit`
/// only ever runs the strategy decision — never a pending `RequestCondense` — so
/// the sweep is the only retry there is. Without it, `ending` is a terminal
/// state in practice: counted live forever, condensed by nothing.
///
/// The condense is made deterministically unable to land by **holding the
/// member's queue lock** from this test, rather than assumed unable. It used
/// to be an unborn HEAD; a condense no longer needs a commit (L-0A §1.1), so
/// the repository here stays unborn throughout and the retry lands an
/// anchorless checkpoint — the new property, proven on the same fixture.
#[test]
fn a_deferred_sweep_keeps_the_live_session_first_and_retries_later() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo_unborn(tmp.path());
    std::fs::remove_file(common::seed_sessions_dir(&repo.join(".git")).join("e2e-live-1.json"))
        .unwrap();
    seed_session(&repo, "crashed-1", 0);
    seed_session(&repo, "current-1", now_ms());
    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = sync_env_with_data(&cache, &data);

    let held = hold_owner_lock(&repo.join(".git"));
    let first_doc = doctor_fix_json(&repo, &env);
    let first = sessions_leg(&first_doc);
    assert_eq!(
        first["condense_deferred"],
        serde_json::json!(["crashed-1"]),
        "a worker holds the lock: the session defers, it is not closed on a \
         condense that never happened: {first}"
    );
    assert_eq!(
        first["condense_deferred_causes"],
        serde_json::json!({ "crashed-1": "worker-holds-lock" }),
        "{first}"
    );
    assert_eq!(first["finalized"], serde_json::json!([]));
    assert_eq!(first["live"], serde_json::json!(["current-1"]));
    // A held lock defers before the machine is asked, so the dead session is
    // neither timed out nor stamped: it reads as it was left.
    assert_eq!(session_phase(&repo, "crashed-1"), "active");

    // THE REGRESSION: the deferred session is still live (it is `ending`), but
    // it must not be the one the next archive attributes against.
    let live = rift_shadow::hooks::session_cache::live_sessions(&hooks_fixture::sessions_of(&repo))
        .unwrap();
    let order: Vec<&str> = live.iter().map(|c| c.session_id.as_str()).collect();
    assert_eq!(
        order,
        vec!["current-1", "crashed-1"],
        "the developer's live session stays the repo's archive target"
    );

    // THE HOLE UNDERNEATH IT: release the lock, and the next sweep must finish
    // the condense it owes — nothing else will — with HEAD still unborn.
    drop(held);
    let second_doc = doctor_fix_json(&repo, &env);
    let second = sessions_leg(&second_doc);
    assert_eq!(
        second["finalized"],
        serde_json::json!(["crashed-1"]),
        "the sweep retried the condense it owed: {second}"
    );
    assert_eq!(second["live"], serde_json::json!(["current-1"]));
    assert_eq!(session_phase(&repo, "crashed-1"), "ended");
    assert!(
        checkpoint_ref_exists(&repo),
        "the retry condensed real work, anchorless"
    );
}

/// A worker's capture-queue owner lock under `git_dir`, held until dropped:
/// the open-file lock the drain and the sweep both take, so a sweep beside it
/// defers to "the worker" exactly as it would beside a real one.
fn hold_owner_lock(git_dir: &Path) -> std::fs::File {
    let path = std::fs::canonicalize(git_dir)
        .unwrap()
        .join(".rift-capture-owner.lock");
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path).unwrap();
    file.try_lock().unwrap();
    file
}

/// An archive that ran against a real commit and minted no checkpoint defers
/// with its own cause, never the lock's: the checkpoint branch's namespace is
/// blocked by a plain file, so the write fails while HEAD is there to anchor.
#[test]
fn an_archive_that_mints_nothing_defers_with_its_own_cause() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    seed_session(&repo, "crashed-1", 0);
    std::fs::write(
        repo.join(".git").join("refs").join("heads").join("rift"),
        "",
    )
    .unwrap();
    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = sync_env_with_data(&cache, &data);

    let doc = doctor_fix_json(&repo, &env);
    let sessions = sessions_leg(&doc);
    assert_eq!(
        sessions["condense_deferred_causes"],
        serde_json::json!({ "crashed-1": "archive-produced-nothing" }),
        "{sessions}"
    );
    let (code, human) = run_rift_stdout(&repo, &["doctor", "--fix"], &env);
    assert_eq!(code, 0, "{human}");
    let deferred: Vec<&str> = human.lines().filter(|l| l.contains("deferred")).collect();
    assert_eq!(deferred.len(), 1, "one line per cause present: {human}");
    assert!(deferred[0].contains("archive-produced-nothing"), "{human}");
    assert!(
        deferred[0].contains("`rift doctor --fix` retries"),
        "{human}"
    );
    assert!(!deferred[0].contains("queue lock"), "{human}");

    // The same blocked namespace through the hook road: the live session's
    // `SessionEnd` journals a condense whose archive fails, and the job is
    // kept with its retry state — one attempt, a backoff, not parked.
    let body = r#"{"schema":1,"session":"e2e-live-1","turn":1,"payload":{"reason":"exit"}}"#;
    assert_eq!(
        run_rift_in(&repo, &["hooks", "claude-code", "SessionEnd"], body, &env),
        0
    );
    assert_ne!(session_phase(&repo, "e2e-live-1"), "ended");
    let (job, state) = only_condense_job(&repo);
    assert_eq!(state.attempts, 1, "{job}");
    assert!(state.next_due_ms > 0 && !state.parked, "{job}");
}

/// A blocked checkpoint namespace, so every archive of the session fails.
fn block_checkpoint_namespace(repo: &Path) {
    std::fs::write(
        repo.join(".git").join("refs").join("heads").join("rift"),
        "",
    )
    .unwrap();
}

/// Every file under `dir` with its bytes, sorted: what report mode must leave
/// exactly as it found it.
fn queue_listing(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap_or_default(),
            )
        })
        .collect();
    out.sort();
    out
}

/// The report's lines that name parked work.
fn parked_lines(report: &str) -> Vec<String> {
    report
        .lines()
        .filter(|l| l.contains("parked"))
        .map(str::to_owned)
        .collect()
}

/// Three execution failures **park** the condense job: it stays in the
/// journal with `parked: true` and its backoff, the ordinary worker runs it
/// no more, and report-only `rift doctor` names the parked session with its
/// last note — twice, identically, publishing, renaming and moving nothing
/// (R2; CFX-7: `peek_errors`, never `take_errors`).
#[test]
fn three_failures_park_the_job_and_doctor_names_it_twice_identically() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    block_checkpoint_namespace(&repo);
    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = sync_env_with_data(&cache, &data);

    let body = r#"{"schema":1,"session":"e2e-live-1","turn":1,"payload":{"reason":"exit"}}"#;
    assert_eq!(
        run_rift_in(&repo, &["hooks", "claude-code", "SessionEnd"], body, &env),
        0
    );
    assert_eq!(only_condense_job(&repo).1.attempts, 1);
    // The repair road ignores the backoff: two more `--fix` passes, two more
    // failures, and the third parks.
    let _ = doctor_fix_json(&repo, &env);
    assert_eq!(only_condense_job(&repo).1.attempts, 2);
    let _ = doctor_fix_json(&repo, &env);
    let (job, state) = only_condense_job(&repo);
    assert_eq!(state.attempts, 3, "{job}");
    assert!(state.parked, "the third failure parks: {job}");
    assert!(state.next_due_ms > 0, "{job}");
    assert_ne!(session_phase(&repo, "e2e-live-1"), "ended");

    let queue_dir = agent_queue_of(&repo);
    let before = queue_listing(&queue_dir);
    let (code, first) = run_rift_stdout(&repo, &["doctor"], &env);
    assert_eq!(code, 0, "{first}");
    let (code, second) = run_rift_stdout(&repo, &["doctor"], &env);
    assert_eq!(code, 0, "{second}");
    assert_eq!(
        parked_lines(&first),
        parked_lines(&second),
        "a second report prints the same parked lines"
    );
    assert!(
        first.contains(
            "agent jobs:     1 owed (1 parked after 3 attempts — retry with \
                        `rift doctor --fix` or `rift session retry <session>`; 0 due later)"
        ),
        "{first}"
    );
    assert!(
        first.contains("parked:       session e2e-live-1 — ")
            && first.contains("parked after 3 attempts"),
        "the parked session is named with its last note: {first}"
    );
    assert_eq!(
        queue_listing(&queue_dir),
        before,
        "report mode published, renamed and moved nothing"
    );
    let doc = doctor_json(&repo, &env);
    let jobs = doc
        .pointer("/capture_queue/agent_jobs")
        .or_else(|| doc.pointer("/data/capture_queue/agent_jobs"))
        .expect("the agent-jobs leg");
    assert_eq!(jobs["parked"][0]["session"], "e2e-live-1", "{jobs}");
}

/// `rift session retry` resets the parked job's backoff and park, keeps its
/// attempt count, and drains it: with the namespace unblocked the archive
/// lands, the session closes and the job is gone.
#[test]
fn rift_session_retry_unparks_and_the_unblocked_archive_lands() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    block_checkpoint_namespace(&repo);
    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = sync_env_with_data(&cache, &data);
    let body = r#"{"schema":1,"session":"e2e-live-1","turn":1,"payload":{"reason":"exit"}}"#;
    assert_eq!(
        run_rift_in(&repo, &["hooks", "claude-code", "SessionEnd"], body, &env),
        0
    );
    let _ = doctor_fix_json(&repo, &env);
    let _ = doctor_fix_json(&repo, &env);
    assert!(only_condense_job(&repo).1.parked);

    // A retry with the namespace still blocked unparks, fails again and the
    // history is kept: attempts climb to four, the job is parked again.
    let retried = rift_json(
        &repo,
        &["--json", "session", "retry", "--session", "e2e-live-1"],
        &env,
    );
    assert_eq!(
        retried["unparked"],
        serde_json::json!(["e2e-live-1"]),
        "{retried}"
    );
    let (job, state) = only_condense_job(&repo);
    assert_eq!(
        state.attempts, 4,
        "attempts are history, never reset: {job}"
    );
    assert!(state.parked, "{job}");

    std::fs::remove_file(repo.join(".git").join("refs").join("heads").join("rift")).unwrap();
    let retried = rift_json(
        &repo,
        &["--json", "session", "retry", "--session", "e2e-live-1"],
        &env,
    );
    assert_eq!(
        retried["unparked"],
        serde_json::json!(["e2e-live-1"]),
        "{retried}"
    );
    assert_eq!(retried["landed"][0]["session"], "e2e-live-1", "{retried}");
    assert_eq!(session_phase(&repo, "e2e-live-1"), "ended");
    assert!(checkpoint_ref_exists(&repo));
    let left: Vec<_> = std::fs::read_dir(agent_queue_of(&repo))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("condense-"))
        .collect();
    assert!(left.is_empty(), "the job was removed by its success");
}

/// A worker killed mid-wait leaves the job untouched — no attempt spent, no
/// marker — and the next worker takes it. The worker is this test's own
/// child, killed by the pid the test spawned; it is held off the queue by the
/// owner lock this test holds, so the kill lands while it waits.
#[test]
fn a_killed_worker_leaves_the_job_and_the_next_worker_takes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = sync_env_with_data(&cache, &data);
    let git_dir = repo.join(".git");

    // Journal the condense without draining it: the lock is held, so the
    // hook's own worker finds the queue busy and leaves.
    let held = hold_owner_lock(&git_dir);
    let body = r#"{"schema":1,"session":"e2e-live-1","turn":1,"payload":{"reason":"exit"}}"#;
    assert_eq!(
        run_rift_in(&repo, &["hooks", "claude-code", "SessionEnd"], body, &env),
        0
    );
    let (job_before, state) = only_condense_job(&repo);
    assert_eq!(state.attempts, 0);
    assert_ne!(session_phase(&repo, "e2e-live-1"), "ended");

    let mut worker = common::rift_cmd(&repo)
        .args(["hooks", "worker", "--git-dir", git_dir.to_str().unwrap()])
        .envs(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // The worker waits briefly for the lock this test holds; kill it there.
    std::thread::sleep(std::time::Duration::from_millis(300));
    worker.kill().unwrap();
    let _ = worker.wait().unwrap();

    let (job_after, state) = only_condense_job(&repo);
    assert_eq!(job_after, job_before, "a killed worker rewrote nothing");
    assert_eq!(state.attempts, 0);
    drop(held);

    let (code, _) = run_rift_stdout(
        &repo,
        &["hooks", "worker", "--git-dir", git_dir.to_str().unwrap()],
        &env,
    );
    assert_eq!(code, 0);
    assert_eq!(session_phase(&repo, "e2e-live-1"), "ended");
    assert!(checkpoint_ref_exists(&repo));
}

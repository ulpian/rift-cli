//! End-to-end: **a stale-session sweep never archives beside the worker that
//! holds its member's capture-queue lock.**
//!
//! The detached worker condenses a queued commit under the member's lock; a
//! sweep condenses an idle session against the same HEAD. Those are two
//! archivers of one commit, and two archivers that each read the chain before
//! either appends leave it with a checkpoint at two positions. The sweep's
//! answer is to condense only under the member's lock, and to leave a member
//! whose lock is held to the worker holding it, whose own tail sweep reaches
//! the same sessions.
//!
//! The fixture holds the actual open-file owner lock, runs the candidate CLI
//! sweep beside it, then drops the descriptor and runs the worker to completion.
//! This controls the interleaving without a detached-process timing guess; it
//! does not prove a handoff from an older installed image.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    doctor_fix_json, git_in, rev_parse, rift_json, run_rift_stdout, seed_live_session_repo,
    seed_session, session_phase, sessions_leg, sync_env, LIVE_SESSION,
};

/// A session marker's stamp far outside every idle floor: the session the
/// sweep exists to finalize.
const LONG_AGO_MS: i64 = 1_783_000_000_000;

/// A worker's capture-queue lock under `git_dir`, held until dropped.
struct HeldLock {
    _file: std::fs::File,
}

impl HeldLock {
    fn take(git_dir: &Path, _cache: &Path) -> Self {
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
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            options.share_mode(3);
        }
        let file = options.open(path).unwrap();
        file.try_lock().unwrap();
        Self { _file: file }
    }
}

/// Plant the queue entry `post-commit` leaves for the detached worker: HEAD,
/// condensed for `session`.
fn queue_head(git_dir: &Path, cache: &Path, work_tree: &Path, session: &str) {
    let commit = rev_parse(work_tree, "HEAD");
    let dir = common::capture_queue_dir(git_dir, cache);
    std::fs::create_dir_all(&dir).unwrap();
    let entry = serde_json::json!({
        "schema_version": 1,
        "commit": commit,
        "work_tree": work_tree.display().to_string(),
        "session_id": session,
        "agent": "claude-code",
        "session_state": "active",
        "queued_at_ms": LONG_AGO_MS,
        "attempts": 0,
    });
    std::fs::write(
        dir.join(format!("{commit}.json")),
        serde_json::to_vec(&entry).unwrap(),
    )
    .unwrap();
}

/// The real detached worker over `git_dir`, run to completion.
fn run_worker(cwd: &Path, git_dir: &Path, cache: &Path) {
    let args = ["hooks", "worker", "--git-dir", git_dir.to_str().unwrap()];
    let (code, out) = run_rift_stdout(cwd, &args, &sync_env(cache));
    assert_eq!(code, 0, "the worker never fails: {out}");
}

/// The checkpoint ids the chain ref records, position 1 first.
fn chain_ids(repo: &Path) -> Vec<String> {
    if !has_ref(repo, "refs/rift/chain/v1") {
        return Vec::new();
    }
    let log = git_in(
        repo,
        &[
            "log",
            "--first-parent",
            "--reverse",
            "--format=%B",
            "refs/rift/chain/v1",
        ],
    );
    log.lines()
        .filter_map(|l| l.trim().strip_prefix("Rift-Chain-Checkpoint:"))
        .map(|id| id.trim().to_owned())
        .collect()
}

/// The checkpoint ids the checkpoint branch carries, oldest first, read with
/// git alone so the comparison does not go through the code under test.
fn branch_ids(repo: &Path) -> Vec<String> {
    if !has_ref(repo, "refs/heads/rift/checkpoints/v1") {
        return Vec::new();
    }
    let log = git_in(
        repo,
        &[
            "log",
            "--first-parent",
            "--reverse",
            "--root",
            "--name-only",
            "--format=",
            "refs/heads/rift/checkpoints/v1",
        ],
    );
    log.lines()
        .filter_map(|p| p.strip_suffix("/metadata.json"))
        .map(|d| d.split('/').collect())
        .collect()
}

fn has_ref(repo: &Path, name: &str) -> bool {
    !git_in(repo, &["for-each-ref", name]).trim().is_empty()
}

/// The chain holds each of the branch's checkpoints once, in the branch's
/// order, and holds at least one.
fn assert_chain_is_the_branch(repo: &Path) {
    let chain = chain_ids(repo);
    let mut unique = chain.clone();
    unique.sort();
    unique.dedup();
    assert!(!chain.is_empty(), "the worker archived the commit");
    assert_eq!(
        unique.len(),
        chain.len(),
        "one position per checkpoint: {chain:?}"
    );
    assert_eq!(chain, branch_ids(repo), "the chain is the branch's order");
}

/// The doctor's sweep, run while the worker holds the member's lock, archives
/// nothing and defers the idle session; the worker, once it runs, condenses
/// the commit and the session, and the chain records the checkpoint once.
#[test]
fn a_doctor_sweep_beside_a_worker_holding_the_lock_leaves_the_commit_to_that_worker() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let git_dir = repo.join(".git");
    seed_session(&repo, LIVE_SESSION, LONG_AGO_MS);
    queue_head(&git_dir, &cache, &repo, LIVE_SESSION);

    let lock = HeldLock::take(&git_dir, &cache);
    let doc = doctor_fix_json(&repo, &sync_env(&cache));
    let sessions = sessions_leg(&doc);
    assert_eq!(
        sessions["condense_deferred"],
        serde_json::json!([LIVE_SESSION]),
        "the session is left to the worker: {sessions}"
    );
    assert_eq!(sessions["finalized"], serde_json::json!([]), "{sessions}");
    assert!(
        chain_ids(&repo).is_empty(),
        "nothing was archived beside the worker"
    );
    assert_eq!(session_phase(&repo, LIVE_SESSION), "active");

    drop(lock);
    run_worker(&repo, &git_dir, &cache);
    assert_eq!(session_phase(&repo, LIVE_SESSION), "ended");
    assert_chain_is_the_branch(&repo);
}

/// The deferral says which cause it was, alone: a scripted caller reading the
/// lock-held reason knows to wait for the worker, not to look for a failure.
#[test]
fn a_doctor_sweep_beside_a_held_lock_names_the_lock_as_the_only_cause() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    seed_session(&repo, LIVE_SESSION, LONG_AGO_MS);

    let lock = HeldLock::take(&repo.join(".git"), &cache);
    let doc = doctor_fix_json(&repo, &sync_env(&cache));
    let sessions = sessions_leg(&doc);
    assert_eq!(
        sessions["condense_deferred_causes"],
        serde_json::json!({ LIVE_SESSION: "worker-holds-lock" }),
        "{sessions}"
    );
    let (code, human) = run_rift_stdout(&repo, &["doctor", "--fix"], &sync_env(&cache));
    assert_eq!(code, 0, "{human}");
    let deferred: Vec<&str> = human
        .lines()
        .filter(|line| line.trim_start().starts_with("deferred:"))
        .collect();
    assert_eq!(deferred.len(), 1, "one line per cause present: {human}");
    let line = deferred[0];
    assert!(line.contains("worker-holds-lock"), "{line}");
    assert!(line.contains("nothing is lost"), "{line}");
    assert!(
        !line.contains("anchor") && !line.contains("archive"),
        "the line names the lock alone: {line}"
    );
    drop(lock);
}

/// A targeted stop waits briefly for a worker, and one that outlasts the wait
/// leaves the session deferred in the verb's usual shape, never finalized.
#[test]
fn a_targeted_stop_beside_a_held_lock_defers_the_session_in_the_usual_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let git_dir = repo.join(".git");
    seed_session(&repo, LIVE_SESSION, LONG_AGO_MS);

    common::provision_here(&repo, &cache);

    let lock = HeldLock::take(&git_dir, &cache);
    let args = ["session", "stop", LIVE_SESSION, "--json"];
    let doc = rift_json(&repo, &args, &sync_env(&cache));
    assert_eq!(doc["deferred"], serde_json::json!([LIVE_SESSION]), "{doc}");
    assert_eq!(
        doc["deferred_causes"],
        serde_json::json!({ LIVE_SESSION: "worker-holds-lock" }),
        "{doc}"
    );
    assert_eq!(doc["finalized"], serde_json::json!([]), "{doc}");
    assert_eq!(doc["already_ended"], serde_json::json!([]), "{doc}");
    assert!(
        chain_ids(&repo).is_empty(),
        "nothing was archived beside the worker"
    );
    drop(lock);
}

/// A worker's tail sweep covers the whole family, but a sibling whose own
/// worker holds the sibling's lock is left to that worker; once released,
/// the sibling's worker finalizes its session.
#[test]
fn a_tail_sweep_leaves_a_sibling_whose_own_worker_holds_its_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let main = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    std::fs::remove_file(
        common::seed_sessions_dir(&main.join(".git")).join(format!("{LIVE_SESSION}.json")),
    )
    .unwrap();
    let member = tmp.path().join("member");
    let member_str = member.to_str().unwrap();
    git_in(
        &main,
        &["worktree", "add", "-q", "-b", "member", member_str],
    );
    std::fs::create_dir_all(member.join(".rift")).unwrap();
    let admin = main.join(".git").join("worktrees").join("member");
    seed_member_session(&admin, "member-idle");

    let lock = HeldLock::take(&admin, &cache);
    run_worker(&main, &main.join(".git"), &cache);
    assert_eq!(member_phase(&admin, &cache, "member-idle"), "active");
    assert!(
        chain_ids(&main).is_empty(),
        "nothing was archived beside the worker"
    );

    drop(lock);
    run_worker(&member, &admin, &cache);
    assert_eq!(member_phase(&admin, &cache, "member-idle"), "ended");
    assert_chain_is_the_branch(&main);
}

/// An idle session in a linked member's own journal.
fn seed_member_session(admin: &Path, id: &str) {
    let sessions = common::seed_sessions_dir(admin);
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join(format!("{id}.json")),
        format!(
            r#"{{"schema":1,"session_id":"{id}","state":"active","agent":"claude-code",
                "current_turn_ordinal":1,"turn_open":false,"compaction_boundary":null,
                "updated_at_ms":{LONG_AGO_MS},"native_id_map":{{}}}}"#
        ),
    )
    .unwrap();
}

/// The state of one session in a linked member's journal.
fn member_phase(admin: &Path, cache: &Path, id: &str) -> String {
    let sessions = common::sessions_dir(admin, cache);
    let raw = std::fs::read_to_string(common::read_path(&sessions, &format!("{id}.json"))).unwrap();
    serde_json::from_str::<serde_json::Value>(&raw).unwrap()["state"]
        .as_str()
        .unwrap()
        .to_owned()
}

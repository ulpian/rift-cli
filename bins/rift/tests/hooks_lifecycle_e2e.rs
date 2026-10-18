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
    checkpoint_ref_exists, checkpoint_shards, doctor_fix_json, git_in, now_ms, run_rift_in,
    run_rift_stdout, seed_live_session_repo, seed_live_session_repo_unborn, seed_session,
    session_phase, sessions_leg, sync_env, sync_env_with_data,
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

/// Rewind (or advance) a session cache's last-touch stamp on disk. The sweep's
/// input is this recorded number, never the clock, so a fixture can age a
/// session by a day without sleeping for one or reaching for a fake clock.
fn set_session_touched(repo: &Path, id: &str, updated_at_ms: i64) {
    let path = common::read_path(&hooks_fixture::sessions_of(repo), &format!("{id}.json"));
    let mut cache: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    cache["updated_at_ms"] = serde_json::json!(updated_at_ms);
    std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();
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

/// The lifecycle rule settled 2026-08-07, end to end through the real binary: a
/// `SessionEnd` whose archive could not run must land in `ending`, not `ended`.
///
/// The hole this closes was the worst kind — ordinary inputs, no error, total
/// loss. When the archive could not run, the hook path used to close the session
/// anyway. `Ended` is left only by the session's own next hook, and an agent
/// that is gone sends none, so that session's work could never be condensed by
/// anything afterwards: not the sweep, not doctor. Everything the agent did in
/// it was captured and then orphaned.
///
/// The rescue is the other half of the rule and is what makes deferral honest
/// rather than merely cautious: the session is still `ending`, so the moment the
/// repository has an archive it can reach, the sweep finishes the condense the
/// hook could not.
///
/// **How the deferral is reproduced changed with the wrapper.** It used to be a
/// repository pinned to the wrapper route on a machine with no capture backend
/// installed; that route and that backend are gone (2026-08-11). The condition
/// left is the one the archive genuinely cannot work around: an **unborn HEAD**
/// — no commit to anchor a checkpoint to. Same property, reproduced by the same
/// mechanism the product will actually meet.
#[test]
fn a_session_end_that_could_not_archive_defers_instead_of_closing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo_unborn(tmp.path());
    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("XDG_DATA_HOME", data.to_str().unwrap()),
        ("LOCALAPPDATA", data.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    let body = r#"{"schema":1,"session":"e2e-live-1","turn":1,"payload":{"reason":"exit"}}"#;
    assert_eq!(
        run_rift_in(&repo, &["hooks", "claude-code", "SessionEnd"], body, &env),
        0,
        "a capture hook never fails the agent, deferral or not"
    );

    // Nothing was archived…
    let refs = Command::new("git")
        .args(["for-each-ref", "refs/heads/rift/checkpoints/v1"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&refs.stdout).trim().is_empty(),
        "nothing to anchor to: no checkpoint exists"
    );
    // …so the session is NOT closed. `ended` would be a false record of the one
    // fact the phase exists to carry, and an unrecoverable one.
    assert_eq!(
        session_phase(&repo, "e2e-live-1"),
        "ending",
        "a SessionEnd that archived nothing leaves the session sweep-retryable"
    );

    // A day passes with the agent gone, and the repository gains the commit the
    // archive needs. The sweep must finish what the hook could not.
    git_in(&repo, &["commit", "-q", "-m", "seed"]);
    set_session_touched(&repo, "e2e-live-1", 0);
    // `--fix` because the sweep is doctor's repair arm: the rescue is a repair, and bare doctor
    // only reports it as stuck.
    let (code, stdout) = run_rift_stdout(&repo, &["--json", "doctor", "--fix"], &env);
    assert_eq!(code, 0, "doctor reports, it never fails: {stdout}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let sessions = doc
        .pointer("/sessions")
        .or_else(|| doc.pointer("/data/sessions"))
        .expect("doctor json carries a sessions leg");
    assert_eq!(
        sessions["finalized"],
        serde_json::json!(["e2e-live-1"]),
        "the sweep rescued the condense the hook deferred: {sessions}"
    );
    assert_eq!(session_phase(&repo, "e2e-live-1"), "ended");
    let refs = Command::new("git")
        .args(["for-each-ref", "refs/heads/rift/checkpoints/v1"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&refs.stdout).trim().is_empty(),
        "the rescue condensed real work"
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
}

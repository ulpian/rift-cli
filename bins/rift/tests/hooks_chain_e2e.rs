//! End-to-end: the chain ref and the soak — a condensation accrues one
//! witnessed verify and a bare verify accrues none, levelling cannot launder a
//! planted row, a divergence vetoes permanently, and two archives record two
//! positions under one genesis.
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
    archive_head, doctor_json, git_in, run_rift_capture, run_rift_capture_in,
    seed_live_session_repo, seed_session, sync_env,
};

/// The soak half: a real commit that condenses accrues one
/// `verification_run` row, and `rift doctor` says the count out loud.
///
/// The verify runs in the DETACHED worker, after git has returned, so this
/// costs the hook budget nothing — asserted here only in that the hook still
/// exits 0. What is asserted directly is the thing the kill commit is gated on:
/// the counter moves by itself, from an ordinary commit, with nobody
/// remembering to run anything.
///
/// **HALF OF A PAIR — DO NOT DELETE EITHER HALF.** This test and
/// [`five_bare_verifies_are_not_soak_evidence`] are one measurement, and
/// neither is a measurement alone. This one says the counter MOVES when work
/// is condensed; that one says it does NOT move when the operator merely runs
/// `rift verify`. Un-scope `SoakEvidence::gather` from witnessed runs and this
/// test stays green while that one reddens — which is the only reason either
/// number means anything. A reader who finds one of them "redundant" and
/// removes it does not weaken the suite by one test; they silently restore
/// the whole defect the witness row closes, because a counter that moves is
/// indistinguishable from a counter that moves for the wrong reason.
#[test]
fn a_condensed_commit_accrues_one_clean_verify_and_doctor_reports_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    // Re-stamp the session as FRESH. The seeder's fixed timestamp is idle past
    // the sweep threshold, so the `doctor` call below would finalize it, condense
    // it, and accrue a row of its own — and this test would pass with the drain
    // accrual deleted. It did: that is how this line came to exist.
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    seed_session(&repo, "e2e-live-1", now);
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    let (code, stderr) = run_rift_capture(
        &repo,
        &[
            "hooks",
            "git",
            "post-commit",
            "--git-dir",
            repo.join(".git").to_str().unwrap(),
        ],
        Some(""),
        &env,
    );
    assert_eq!(code, 0, "the hook never fails the git op: {stderr}");

    let (code, after) = run_rift_capture_in(&repo, &["doctor", "--json"], "", &env);
    assert_eq!(code, 0, "doctor --json: {after}");
    let v: serde_json::Value = serde_json::from_str(&after).expect("doctor emits json");
    let soak = &v["soak"];
    assert_eq!(soak["readable"], serde_json::json!(true), "{after}");
    assert_eq!(soak["target"], serde_json::json!(20), "the soak threshold");
    assert!(
        soak["total_runs"].as_i64().unwrap_or(0) >= 1,
        "a condensed commit recorded a WITNESSED verification run: {after}"
    );
    assert!(
        soak["consecutive_clean"].as_i64().unwrap_or(0) >= 1,
        "and it was clean: {after}"
    );
    // The run is evidence because it names the condensation it
    // verified. Without the witness row the counter reads zero here — which is
    // exactly what a bare `rift verify` produces, and the test below plants one.
    assert!(
        soak["distinct_checkpoints"].as_i64().unwrap_or(0) >= 1,
        "the run witnessed a real checkpoint: {after}"
    );
    // Not soaked on one run: absence of 20 is not evidence of safety.
    assert_eq!(soak["soaked"], serde_json::json!(false), "{after}");
}

/// **The calibration the witness row exists for: a bare `rift verify` is not
/// soak evidence, however many times it is run.**
///
/// `verification_run` has three writers and `rift verify` is one of them, so
/// before the witness row twenty invocations of the operator's own diagnostic
/// filled the soak — on a repository that had condensed nothing. This plants
/// exactly that: five verifies against a repository with a live session and no
/// condense, and the counter must not move.
///
/// **HALF OF A PAIR — DO NOT DELETE EITHER HALF.** The other half is
/// [`a_condensed_commit_accrues_one_clean_verify_and_doctor_reports_it`]: it
/// condenses and the counter moves, this one verifies five times and it does
/// not. Restoring the un-scoped reader reddens THIS test and leaves that one
/// green — measured, exit 101, 2026-08-18 — which is what makes the two of them
/// a measurement rather than an assertion. Deleting either one leaves a suite
/// that still passes and no longer distinguishes a soak from a diagnostic.
#[test]
fn five_bare_verifies_are_not_soak_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    for _ in 0..5 {
        // WHICH verdict is not the point — measured, this fixture verifies
        // CLEAN (exit 0) — the ROW it writes is, and it writes one either way.
        //
        // But that it reached the verify path at all IS the point, and this
        // assertion used to be `!out.is_empty() || out.is_empty()` — a tautology
        // wearing a message that claimed a check. It mattered here more than
        // most places: the assertion below is `total_runs == 0`, which passes
        // just as happily if `rift verify` never ran. So this pins a real VERIFY
        // verdict — 0 clean, 1 tamper, 3 redaction, 5 structure — which a usage
        // error (2) or an internal failure (70) could not produce.
        let (code, out) = run_rift_capture_in(&repo, &["verify"], "", &env);
        assert!(
            matches!(code, 0 | 1 | 3 | 5),
            "verify reached a verdict rather than failing before it: exit {code}, {out}"
        );
    }

    // Bare `rift doctor` — no `--fix`, so its own sweep condenses nothing and
    // cannot contribute a witness of its own.
    let (code, after) = run_rift_capture_in(&repo, &["doctor", "--json"], "", &env);
    assert_eq!(code, 0, "doctor --json: {after}");
    let v: serde_json::Value = serde_json::from_str(&after).expect("doctor emits json");
    let soak = &v["soak"];
    assert_eq!(
        soak["total_runs"],
        serde_json::json!(0),
        "five bare verifies witnessed nothing, so the soak counted nothing: {after}"
    );
    assert_eq!(soak["consecutive_clean"], serde_json::json!(0), "{after}");
    assert_eq!(
        soak["distinct_checkpoints"],
        serde_json::json!(0),
        "{after}"
    );
    assert_eq!(soak["soaked"], serde_json::json!(false), "{after}");
}

/// The second condense site, found by measurement rather than by reading:
/// `rift doctor`'s idle-session sweep **condenses**, and before accrual was
/// wired here it accrued nothing.
///
/// That mattered twice over. The sweep's checkpoint went uncounted, and because
/// the sweep also *closes* the session, the next commit had nothing left to
/// archive and accrued nothing either — so running the command that DISPLAYS
/// the soak counter was enough to stop the counter moving.
#[test]
fn doctors_own_sweep_accrues_a_verify_instead_of_swallowing_one() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    // `--fix` because the sweep is doctor's repair arm: the sweep is doctor's REPAIR, and bare
    // doctor reports without repairing.
    let (code, out) = run_rift_capture_in(&repo, &["doctor", "--fix", "--json"], "", &env);
    assert_eq!(code, 0, "doctor: {out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("doctor emits json");

    // The sweep really did condense — otherwise this test proves nothing.
    let refs = Command::new("git")
        .args(["for-each-ref", "refs/heads/rift/checkpoints/v1"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&refs.stdout).contains("rift/checkpoints/v1"),
        "doctor's sweep archived a checkpoint"
    );

    // …and the same invocation reports the row it just earned.
    assert!(
        v["soak"]["total_runs"].as_i64().unwrap_or(0) >= 1,
        "doctor's own condense accrued a verify: {out}"
    );
    // And the verify NAMES what it condensed. The sweep runs
    // one verify for every session it finalizes, so this is the witness half of
    // the same row — without it the accrual is indistinguishable from doctor
    // having simply run `verify` on itself.
    assert!(
        v["soak"]["distinct_checkpoints"].as_i64().unwrap_or(0) >= 1,
        "the sweep's verify witnessed the checkpoint it minted: {out}"
    );
}

/// **The one property the levelling must not break, and until now nothing
/// could redden when it did: verify must not launder a projection into
/// agreeing with the source it is about to be compared against.**
///
/// `run_at_with` levels before comparing, which is only safe
/// because `record_checkpoint_if_new` is `insert_or_ignore`
/// (`rift-store/src/integrity.rs`) — append-only means levelling can ADD a
/// missing row and can never overwrite a present one. Change that single call
/// to `INSERT OR REPLACE`, or add any "heal the projection" step, and the whole
/// `ProjectionVsSource` comparison silently becomes a tautology: verify would
/// fill the projection from git and then report that it matches git.
///
/// Nothing in the suite drove that path. `verify.rs`'s unit tests call
/// `verify_under` directly, `e2e.rs` uses the library verify over an in-memory
/// store, and the divergence test beside this one plants a *divergence* row,
/// not a *checkpoint* row — so the sentence in `run_at_with` claiming the
/// laundering is forbidden was an assertion with nothing behind it. This is the
/// test that makes it a measurement: a GENUINE checkpoint id carrying a WRONG
/// merkle root, planted into an empty migrated projection, and verify must come
/// back TAMPER rather than quietly overwriting it.
#[test]
fn levelling_cannot_launder_a_planted_checkpoint_into_agreement() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache_a = tmp.path().join("cache-a");
    // A real condensation, so git holds a real shard with a real checkpoint id.
    archive_head(&repo, &sync_env(&cache_a));

    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let repo_id = rift_git::repo_id_for(&repo).unwrap();
    // The GENUINE id, read from the projection the real condensation built.
    let genuine = rt.block_on(first_checkpoint_id(&cache_a, &repo_id));
    // A SECOND, empty cache: migrated, then given that genuine id with a merkle
    // root that is not the one git will re-derive.
    let cache_b = tmp.path().join("cache-b");
    rt.block_on(plant_disagreeing_checkpoint(&cache_b, &repo_id, &genuine));

    let (code, out) = run_rift_capture_in(&repo, &["--json", "verify"], "", &sync_env(&cache_b));
    assert_eq!(
        code, 1,
        "levelling must not overwrite the planted row into agreement: {out}"
    );
    // `verify` prints its repo/projection lines before the JSON document, so the
    // payload starts at the first `{` rather than at byte zero.
    let json_start = out
        .find('{')
        .expect("verify emits a json document after its repo/projection lines");
    let v: serde_json::Value = serde_json::from_str(&out[json_start..]).expect("verify emits json");
    assert_eq!(
        v["verify"],
        serde_json::json!("tamper"),
        "and the verdict is tamper, not a laundered ok: {out}"
    );
    assert!(
        v["detail"]
            .as_str()
            .is_some_and(|d| d.contains(&genuine[..8])),
        "naming the checkpoint it disagreed about: {out}"
    );
}

/// The one checkpoint id the projection under `cache` recorded for `repo_id`.
async fn first_checkpoint_id(cache: &Path, repo_id: &rift_core::RepoId) -> String {
    let store =
        rift_store::open_capture_projection(rift_store::projection_db_under(cache, repo_id))
            .unwrap();
    let rows = rift_store::CanonicalStore::query(
        &store,
        "SELECT checkpoint_id FROM checkpoint LIMIT 1",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "the condensation recorded a checkpoint");
    rows[0].get_text(0).unwrap()
}

/// A fresh, migrated projection under `cache` holding `genuine` with a merkle
/// root git will never re-derive — the row levelling must not overwrite.
async fn plant_disagreeing_checkpoint(cache: &Path, repo_id: &rift_core::RepoId, genuine: &str) {
    let store =
        rift_store::open_capture_projection(rift_store::projection_db_under(cache, repo_id))
            .unwrap();
    rift_store::record_checkpoint_if_new(
        &store,
        &rift_store::CheckpointRecord {
            checkpoint_id: rift_core::Sha256Id::parse_hex(genuine).unwrap(),
            strategy: "manual-commit".to_owned(),
            created_at: 1,
            merkle_root: rift_core::Sha256Id::parse_hex(&"f".repeat(64)).unwrap(),
            prev: None,
            schema_version: 1,
        },
    )
    .await
    .unwrap();
}

/// **The divergence veto has to reach the USER, not just the source: one
/// recorded divergence vetoes that projection's soak permanently.**
///
/// `verification_divergence` is append-only and nothing supersedes a row, so
/// this is not a state that waiting fixes — the projection has to be rebuilt
/// from git. A counter that simply refuses to clear, with the reason available
/// only by reading `soak.rs`, is the same silence the pending-marker line was
/// added to end.
///
/// The divergence is PLANTED directly into the sandbox projection rather than
/// provoked, because provoking a real one means corrupting a checkpoint, and
/// what is under test here is the REPORTING, not the detection.
#[test]
fn a_recorded_divergence_says_it_vetoed_the_soak_and_says_it_is_permanent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    // A real condensation first, so the streak is genuinely non-zero and the
    // veto is what stops the soak rather than an empty log.
    archive_head(&repo, &env);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(plant_unwitnessed_divergence(&cache, &repo));

    let v = doctor_json(&repo, &env);
    assert_eq!(
        v["soak"]["divergence_veto"],
        serde_json::json!(true),
        "the veto is machine-readable: {v}"
    );
    assert_eq!(v["soak"]["soaked"], serde_json::json!(false), "{v}");

    // …and a human is TOLD, including that it is permanent.
    let (code, human) = run_rift_capture_in(&repo, &["doctor"], "", &env);
    assert_eq!(code, 0, "doctor: {human}");
    assert!(
        human.contains("VETOED"),
        "the human output names the veto: {human}"
    );
    assert!(
        human.contains("permanent") && human.contains("rift rebuild"),
        "and says it is permanent and how to clear it: {human}"
    );
}

/// PLANT the divergence, on a run id that witnessed nothing — the case that
/// was decided, and the one an earlier revision excused.
async fn plant_unwitnessed_divergence(cache: &Path, repo: &Path) {
    let repo_id = rift_git::repo_id_for(repo).unwrap();
    let db = rift_store::projection_db_under(cache, &repo_id);
    let store = rift_store::open_capture_projection(db).unwrap();
    rift_store::record_verification_run(
        &store,
        &rift_store::VerificationRunRow {
            run_id: "planted-bare-verify".to_owned(),
            branch: "rift/checkpoints/v1".to_owned(),
            exit_code: 1,
            checked_at: 1,
            outcome_sha256: "ab".repeat(32),
            schema_version: 1,
        },
    )
    .await
    .unwrap();
    rift_store::record_verification_divergence(
        &store,
        &rift_store::VerificationDivergenceRow {
            run_id: "planted-bare-verify".to_owned(),
            ordinal: 0,
            checkpoint_id: "cd".repeat(32),
            layer: "chain".to_owned(),
            site: "ProjectionVsSource".to_owned(),
            detail: "planted".to_owned(),
            schema_version: 1,
        },
    )
    .await
    .unwrap();
}

/// The shard reaches git; its POSITION has to reach git too.
///
/// Two commits, not one. One entry proves only that something was written —
/// it is the SECOND that shows where a new entry attaches, and "attaches to the
/// head" versus "starts again at genesis" is the whole defect this ref exists
/// to end (a second genesis in the projection, from a chain derived by sorting
/// content hashes).
#[test]
fn two_native_archives_record_two_chain_positions_and_one_genesis() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);

    // First archive: the repository's genesis position.
    let stderr = archive_head(&repo, &env);
    assert!(!stderr.contains("chain"), "quiet on success: {stderr}");
    let after_one = git_in(&repo, &["rev-list", "--count", "refs/rift/chain/v1"]);
    assert_eq!(after_one.trim(), "1", "one archive, one position");

    // Second archive: it must attach to the head.
    commit_with_live_session(&repo, "chain", 2);
    let stderr = archive_head(&repo, &env);
    assert!(!stderr.contains("chain"), "quiet on success: {stderr}");
    assert_eq!(
        git_in(&repo, &["rev-list", "--count", "refs/rift/chain/v1"]).trim(),
        "2",
        "the second archive appended rather than replacing"
    );
    assert_eq!(
        git_in(
            &repo,
            &["rev-list", "--max-parents=0", "refs/rift/chain/v1"]
        )
        .lines()
        .count(),
        1,
        "EXACTLY ONE genesis: the second entry is parented on the first"
    );

    // The positions are the branch's checkpoints, in the branch's own order.
    let branch_ids = branch_checkpoint_ids(&repo);
    assert_eq!(branch_ids.len(), 2, "two checkpoints on the branch");
    assert_eq!(
        chain_entry_ids(&repo),
        branch_ids,
        "the chain records the branch's checkpoints, in the branch's order"
    );
}

/// Planted twice by an adversarial review before this held: the chain step's marker rode the
/// queue entry's own `.error` file, and `accrue_soak` overwrote that same file
/// moments later — a chain refusal makes the soak verify unclean BY
/// CONSTRUCTION, so the operator only ever read "soak verify is not clean",
/// never WHY. The chain marker now rides the repository channel (`mark_note`,
/// key "chain"); this asserts both lines coexist after the clobber shape.
#[test]
fn a_chain_refusal_and_the_soak_verdict_both_survive_to_be_read() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);

    // Two archived checkpoints, chain healthy.
    archive_head(&repo, &env);
    commit_with_live_session(&repo, "clob", 2);
    archive_head(&repo, &env);
    // Erase the order: the next archive sees history it must not backfill.
    git_in(&repo, &["update-ref", "-d", "refs/rift/chain/v1"]);
    commit_with_live_session(&repo, "clob", 3);
    archive_head(&repo, &env);

    let qdir = repo.join(".git").join("rift-capture-queue");
    let chain_note = std::fs::read_to_string(qdir.join("chain.error"))
        .or_else(|_| std::fs::read_to_string(qdir.join("chain")))
        .expect("the chain refusal has its own file on the repository channel");
    assert!(
        chain_note.contains("rift chain init"),
        "the chain marker says WHAT to run: {chain_note}"
    );
    // And the soak verdict is still where it always was — on the entry.
    let soak_lines = entry_error_notes(&qdir);
    assert!(
        soak_lines.iter().any(|l| l.contains("soak verify")),
        "the soak verdict coexists instead of clobbering: {soak_lines:?}"
    );
}

/// A commit with its own live session, so each one condenses: session
/// `e2e-<label>-<n>` is planted in the journal and `a.txt` changes, then git
/// commits under an explicit identity.
fn commit_with_live_session(repo: &Path, label: &str, n: u32) {
    std::fs::write(
        repo.join(".git")
            .join("rift-sessions")
            .join(format!("e2e-{label}-{n}.json")),
        format!(
            "{{\"schema\":1,\"session_id\":\"e2e-{label}-{n}\",\"state\":\"active\",\
             \"agent\":\"claude-code\",\"current_turn_ordinal\":1,\"turn_open\":true,\
             \"compaction_boundary\":null,\"updated_at_ms\":1783000000000,\
             \"native_id_map\":{{}}}}"
        ),
    )
    .unwrap();
    std::fs::write(repo.join("a.txt"), format!("{label} {n}\n")).unwrap();
    for args in [
        vec!["add", "a.txt"],
        vec![
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=T",
            "commit",
            "-q",
            "-m",
            "next",
        ],
    ] {
        assert!(Command::new("git")
            .args(&args)
            .current_dir(repo)
            .status()
            .unwrap()
            .success());
    }
}

/// Every `.error` marker under the queue dir except the chain's own — the
/// per-entry channel the soak verdict rides.
fn entry_error_notes(qdir: &Path) -> Vec<String> {
    std::fs::read_dir(qdir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let is_error = Path::new(&name)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("error"));
            if is_error && !name.starts_with("chain") {
                Some(std::fs::read_to_string(&p).unwrap_or_default())
            } else {
                None
            }
        })
        .collect()
}

/// The checkpoint ids the branch's own first-parent history carries, oldest
/// first — read with git alone, so the assertion does not go through the code
/// under test.
fn branch_checkpoint_ids(repo: &Path) -> Vec<String> {
    let out = Command::new("git")
        .args([
            "log",
            "--first-parent",
            "--reverse",
            "--root",
            "--name-only",
            "--format=",
            "refs/heads/rift/checkpoints/v1",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|p| p.strip_suffix("/metadata.json"))
        .map(|d| d.split('/').collect())
        .collect()
}

/// The checkpoint ids the chain ref records, in its own first-parent order.
fn chain_entry_ids(repo: &Path) -> Vec<String> {
    let out = Command::new("git")
        .args([
            "log",
            "--first-parent",
            "--reverse",
            "--format=%B",
            "refs/rift/chain/v1",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().strip_prefix("Rift-Chain-Checkpoint:"))
        .map(|id| id.trim().to_owned())
        .collect()
}

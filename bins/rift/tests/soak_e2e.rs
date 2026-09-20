//! **The verify-green soak, driven end to end through the real binary.**
//!
//! The standing rule: *no gate may wait on real usage, real commits
//! or wall-clock time; deterministic unit tests, frozen fixtures, or faithful
//! simulation THROUGH THE REAL BINARY stand in.* This file is that simulation,
//! and it is deliberately a **permanent test rather than a one-off script**:
//! what was owed was never twenty rows in one projection — it was evidence that
//! the machinery which accrues them works, and evidence that expires the moment
//! somebody breaks the accrual is not evidence at all.
//!
//! What is real here, and what is fixture:
//!
//! * **Real:** the `rift` binary (spawned through `common::rift_cmd`, the one
//!   place allowed to name it — and the name it uses is cargo's bin-path env
//!   for the crate under test, so the binary is always the one this very run
//!   just built; the stale-binary hazard is absent by construction rather than
//!   avoided by discipline); `git`; the post-commit hook; the condense; the
//!   archive onto `refs/heads/rift/checkpoints/v1`; the verify the worker runs
//!   after each condense; the `verification_run` / `verification_witness` rows
//!   it writes into a real projection; `rift doctor --json` reading them back
//!   through the soak reader.
//! * **Fixture:** the repository (a tempdir), and the session journal entries,
//!   written exactly as the hooks suites' shared seeder
//!   (`common/hooks_fixture.rs`) writes them —
//!   one live session per cycle, so each condensation covers a distinct session
//!   set and therefore mints a **distinct checkpoint id**. That distinctness is
//!   the point: a checkpoint id is `from_root_sessions(sessions, merkle_root)`
//!   and carries no commit oid, so twenty commits over one unchanged session
//!   would mint one id twenty times and must not soak.
//!
//! **Determinism:** no wall clock, no RNG, no network. Commit timestamps and
//! session `updated_at_ms` are pinned constants; the git identity is pinned;
//! `RIFT_CACHE_DIR`, `RIFT_CONFIG_DIR` and `RIFT_DEVELOPER` are pinned into the
//! tempdir so the run cannot read or write the developer's own projection,
//! registry or keychain (a soak leg that runs `verify` or
//! `doctor` against an unpinned cache inflates the very counter it certifies).
//!
//! **Both arms.** Reaching twenty is half a measurement. The other half is that
//! the gate still refuses when it should: the same fixture then has one real
//! divergence introduced — the canonical branch rewound behind the projection —
//! and `soaked` must go back to false and stay there.
//!
//! Measured 2026-08-24 (the run is printed, not just asserted — pass
//! `-- --nocapture`), 43.1 s warm / 60.3 s cold on the author's machine:
//!
//! ```text
//! after 20 condensations: total_runs 20, consecutive_clean 20,
//!                         distinct_checkpoints 20, divergences 0, soaked TRUE
//! after the divergence:   total_runs 20, consecutive_clean 20,
//!                         distinct_checkpoints 20, divergences 1, soaked FALSE
//!                         (layer CheckpointRoot, site GitVsRecomputed)
//! ```
//!
//! The assertions are `>=` on the counts and `==` on `soaked`, deliberately:
//! the exact counts above are what this machine produced, while what the gate
//! actually claims is "twenty was reachable and the veto still bites".
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

mod common;
use common::rift_cmd;

/// `SOAK_MIN_CONSECUTIVE_CLEAN`, restated here so the driver's own
/// count is not read from the code it is testing.
const TARGET: i64 = 20;

/// Fixed author/committer epoch for the first commit; each cycle adds one
/// second. Never the wall clock.
const BASE_EPOCH: i64 = 1_780_000_000;

/// Fixed `updated_at_ms` for a seeded session. Idle relative to "now" by
/// construction, which is fine: every condensation here is driven by the
/// post-commit hook, never by an idle sweep.
const SESSION_MS: i64 = 1_783_000_000_000;

/// `git` in `dir`, hermetic and time-pinned: no system or global config, a
/// fixed identity, fixed author/committer dates, and no inherited re-targeting
/// environment.
fn git(dir: &Path, epoch: i64, args: &[&str]) {
    let stamp = format!("{epoch} +0000");
    let st = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
        .env("GIT_AUTHOR_NAME", "Soak")
        .env("GIT_AUTHOR_EMAIL", "soak@example.invalid")
        .env("GIT_COMMITTER_NAME", "Soak")
        .env("GIT_COMMITTER_EMAIL", "soak@example.invalid")
        .env("GIT_AUTHOR_DATE", &stamp)
        .env("GIT_COMMITTER_DATE", &stamp)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

/// `git` in `dir` returning stdout (for reading refs back).
fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stderr(Stdio::null())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// One live native-hook session in the repository's journal — the same JSON
/// `common/hooks_fixture.rs`'s seeder writes, with the id and stamp supplied.
fn seed_session(repo: &Path, id: &str, updated_at_ms: i64) {
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join(format!("{id}.json")),
        format!(
            r#"{{"schema":1,"session_id":"{id}","state":"active","agent":"claude-code",
                "current_turn_ordinal":1,"turn_open":false,"compaction_boundary":null,
                "updated_at_ms":{updated_at_ms},"native_id_map":{{}}}}"#
        ),
    )
    .unwrap();
}

/// Run the real `rift` through the hermetic chokepoint; return exit code and
/// stdout.
fn rift(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = rift_cmd(cwd);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"").unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// `rift doctor --json`'s `soak` object.
fn soak(repo: &Path, env: &[(&str, &str)]) -> serde_json::Value {
    let (code, out) = rift(repo, &["doctor", "--json"], env);
    assert_eq!(code, 0, "doctor --json exits 0: {out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("doctor emits json");
    v["soak"].clone()
}

/// **The soak, driven: `TARGET` real condensations, then the veto.**
///
/// Each cycle is one ordinary developer commit with a live agent session: seed
/// the session, commit, run the post-commit hook the way git would. The hook
/// condenses, archives a checkpoint shard, and runs the soak's verify against
/// it — the whole accrual path, in-process order, no step simulated.
///
/// `RIFT_CAPTURE_SYNC=1` makes the capture run in-line instead of in the
/// detached worker. That is the one behavioural difference from a real commit,
/// and it is a *scheduling* difference: the same code runs, on the same
/// arguments, in the same order. Detaching would make the test race the worker.
#[test]
fn twenty_real_condensations_soak_the_gate_and_a_divergence_vetoes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let cache = tmp.path().join("cache");
    let config = tmp.path().join("config");
    let git_dir = repo.join(".git");
    let env: Vec<(&str, &str)> = vec![
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_CONFIG_DIR", config.to_str().unwrap()),
        ("RIFT_DEVELOPER", "soak@example.invalid"),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    git(&repo, BASE_EPOCH, &["init", "-q"]);

    for i in 0..TARGET {
        let epoch = BASE_EPOCH + i;
        let name = format!("f{i}.txt");
        std::fs::write(repo.join(&name), format!("cycle {i}\n")).unwrap();
        // The session is live BEFORE the commit, as an agent's would be, and is
        // distinct per cycle so the condensation covers a distinct session set.
        seed_session(&repo, &format!("soak-{i}"), SESSION_MS + i);
        git(&repo, epoch, &["add", &name]);
        git(&repo, epoch, &["commit", "-q", "-m", &format!("cycle {i}")]);
        let (code, out) = rift(
            &repo,
            &[
                "hooks",
                "git",
                "post-commit",
                "--git-dir",
                git_dir.to_str().unwrap(),
            ],
            &env,
        );
        assert_eq!(
            code, 0,
            "the hook never fails the git op (cycle {i}): {out}"
        );
    }

    let ev = soak(&repo, &env);
    // Printed, not merely asserted: a driver that says what it measured lets a
    // ledger quote real numbers instead of the assertion's lower bound
    // (`cargo test … -- --nocapture`).
    eprintln!("SOAK AFTER {TARGET} CONDENSATIONS: {ev}");
    assert_eq!(ev["readable"], serde_json::json!(true), "{ev}");
    assert_eq!(ev["target"], serde_json::json!(TARGET), "{ev}");
    assert_eq!(
        ev["projections"],
        serde_json::json!(1),
        "structurally one since the family store: {ev}"
    );
    assert_sandbox_holds_the_projection(&cache);
    assert_eq!(
        ev["total_divergences"],
        serde_json::json!(0),
        "nothing diverged over {TARGET} clean cycles: {ev}"
    );
    let clean = ev["consecutive_clean"].as_i64().unwrap_or(0);
    let distinct = ev["distinct_checkpoints"].as_i64().unwrap_or(0);
    assert!(
        clean >= TARGET,
        "{TARGET} condensations accrued {clean} consecutive clean witnessed runs: {ev}"
    );
    assert!(
        distinct >= TARGET,
        "…over {distinct} distinct checkpoints — one per cycle is what makes \
         them {TARGET} condensations rather than one condensed {TARGET} times: {ev}"
    );
    assert_eq!(
        ev["soaked"],
        serde_json::json!(true),
        "the soak clears on real accrual: {ev}"
    );

    // ---- the other arm: one real divergence, and the gate refuses again ----
    //
    // Rewinding the canonical checkpoint branch by one shard leaves the
    // projection holding a checkpoint the source no longer has. That is a
    // genuine `ProjectionVsSource` finding, recorded by the same `rift verify`
    // an operator runs — not a row poked into a table.
    let head = git_out(&repo, &["rev-parse", "refs/heads/rift/checkpoints/v1"]);
    assert!(!head.is_empty(), "the cycles archived a real branch");
    git(
        &repo,
        BASE_EPOCH,
        &[
            "update-ref",
            "refs/heads/rift/checkpoints/v1",
            &format!("{head}^"),
        ],
    );
    let (_code, _out) = rift(&repo, &["verify"], &env);

    let ev = soak(&repo, &env);
    eprintln!("SOAK AFTER THE PLANTED DIVERGENCE: {ev}");
    assert!(
        ev["total_divergences"].as_i64().unwrap_or(0) >= 1,
        "the rewind was found: {ev}"
    );
    assert_eq!(
        ev["divergence_veto"],
        serde_json::json!(true),
        "and it is reported as a veto rather than buried: {ev}"
    );
    assert_eq!(
        ev["soaked"],
        serde_json::json!(false),
        "one divergence vetoes {TARGET} clean condensations, \
         permanently — `verification_divergence` is append-only: {ev}"
    );
}

/// The sandbox-leak guard, REPLANTED on something measuring: `projections` in
/// the doctor JSON became a compiled constant when the
/// family store landed, so asserting it can no longer catch a leak. What can:
/// the soak's condensations must have written their projection UNDER the
/// pinned RIFT_CACHE_DIR — an env pin that stopped working would land the
/// store in the developer's real cache while the constant stayed green.
fn assert_sandbox_holds_the_projection(cache: &std::path::Path) {
    let dbs = walkdir_projections(cache);
    assert!(
        !dbs.is_empty(),
        "the soak condensed but no projection.db exists under the sandbox \
         cache {} — the store leaked outside the pin",
        cache.display()
    );
}

/// Every `projection.db` under `root`, however the cache keys its directories.
fn walkdir_projections(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "projection.db") {
                out.push(path);
            }
        }
    }
    out
}

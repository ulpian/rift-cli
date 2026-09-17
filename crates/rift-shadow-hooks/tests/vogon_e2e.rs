//! The fake-agent end-to-end.
//!
//! `vogon` plays a whole scripted Claude Code session against the **real**
//! `rift` binary, and this asserts what landed in the repository's own
//! projection. It is the first **merge-blocking** test that drives
//! `PreToolUse`, `SubagentStart`, `SubagentStop` or `PreCompact` through a
//! process: `bins/rift/tests/hooks_e2e.rs` drives **five** — `SessionStart`,
//! `UserPromptSubmit`, `PostToolUse`, `Stop`, `SessionEnd` — so a fan-out or a
//! lifecycle transition only the other four reach was, until now, asserted
//! in-process or not at all. (`PreToolUse` alone is also spawned by
//! `benches/native-baseline`, which is its own workspace and is not built by CI; the
//! precise scoping is on `vogon`'s module doc.)
//!
//! **Hermetic by construction.** The repository is a tempdir, every child's cwd
//! is pinned to it, `RIFT_CACHE_DIR`/`XDG_DATA_HOME` redirect the projection and
//! the data root into the same tempdir, and `GIT_*` re-targeting is scrubbed by
//! the driver. This suite must never be able to write into the repository it
//! runs in — that class already put one fixture session into this repository's
//! immutable checkpoint history (see `bins/rift/tests/common/mod.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The `rift` binary this suite drives, **rebuilt from the current source
/// before it is driven**.
///
/// `CARGO_BIN_EXE_*` only names bins of *this* package, and the binary under
/// test belongs to `bins/rift`, so it can only be found as vogon's sibling in
/// the same target profile directory. Finding it is not enough, and the first
/// draft of this suite learned that the expensive way: a defect planted in
/// `dispatch` left all four tests **green**, because `cargo test -p
/// rift-shadow-hooks` rebuilds this crate but has no reason to relink a binary
/// belonging to another package — so the suite drove a stale `rift` compiled
/// from the code before the plant. That is this fleet's recorded
/// stale-`target/debug/rift` hazard, and a calibration that cannot redden is
/// worse than no calibration.
///
/// So the build is unconditional — and it goes to **its own target directory**,
/// which is not an optimization but the second half of the same correctness
/// argument. Measured: building into the shared `target/` recompiled
/// `rift-core`, `rift-agent`, `rift-recall-types` and `rift-hashchain` *during*
/// the workspace test run, because the gate builds `--all-features` and a
/// package-scoped `cargo build -p rift` does not — so the two invocations fight
/// over the same fingerprints and every later `cargo test --workspace
/// --all-features` pays to undo this one. A separate directory cannot collide
/// with the outer build's artifacts, cannot relink a binary another suite is
/// spawning, and is always current. The cost is one build of the binary the
/// first time and a cargo no-op on every run after it, because the directory
/// persists — stated as a shape rather than a duration, since every wall-clock
/// number taken on this machine today is void (the Gatekeeper-assessment wedge
/// the ledger records at 2026-08-16T14:40Z was live while they were taken).
///
/// Building rather than skipping also keeps this leg free of a per-item opt-out
/// — an `#[ignore]`, a silent skip or an early `return` is exactly what a gate
/// of last resort may not have.
fn rift_bin() -> PathBuf {
    static BIN: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        // `<workspace>/target/vogon-e2e`, derived from where cargo put vogon
        // (`<workspace>/target/<profile>/vogon`) so it follows a redirected
        // `CARGO_TARGET_DIR` instead of guessing at the repository layout.
        let vogon = PathBuf::from(env!("CARGO_BIN_EXE_vogon"));
        let profile_dir = vogon.parent().expect("vogon has a parent dir");
        let profile = profile_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("debug")
            .to_owned();
        let target = profile_dir
            .parent()
            .expect("a target dir above the profile dir")
            .join("vogon-e2e");
        let mut build = Command::new(env!("CARGO"));
        build.args(["build", "-p", "rift", "--bin", "rift"]);
        if profile == "release" {
            build.arg("--release");
        }
        let status = build
            .arg("--target-dir")
            .arg(&target)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status()
            .expect("spawn cargo to build the binary under test");
        assert!(status.success(), "cargo build -p rift --bin rift failed");
        let exe = target
            .join(&profile)
            .join(format!("rift{}", std::env::consts::EXE_SUFFIX));
        assert!(
            exe.is_file(),
            "the rift binary is not at {} after building it",
            exe.display()
        );
        exe
    })
    .clone()
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A hermetic repository with one commit and the `src/` directory the script's
/// tool payloads name.
fn seed_repo(tmp: &Path) -> PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("src").join("vogon_1.rs"), "fn one() {}\n").unwrap();
    std::fs::write(repo.join("src").join("vogon_2.rs"), "fn two() {}\n").unwrap();
    git(&repo, &["add", "--", "src"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=seed",
            "-c",
            "user.email=seed@rift.invalid",
            "commit",
            "-q",
            "-m",
            "seed",
        ],
    );
    repo
}

/// Redirect a child's projection, data root and keychain into `sandbox`.
///
/// Pinned per leg: a run of this suite must never touch
/// the developer's real projection, and a `rift verify` it provoked must never
/// land in the real soak counter.
fn hermetic<'a>(cmd: &'a mut Command, sandbox: &Path) -> &'a mut Command {
    cmd.env("RIFT_CACHE_DIR", sandbox.join("cache"))
        .env("XDG_DATA_HOME", sandbox.join("data"))
        .env("LOCALAPPDATA", sandbox.join("data"))
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_CAPTURE_SYNC", "1")
        .current_dir(sandbox)
}

/// Run `vogon drive` against the real binary, hermetically. Returns its stdout
/// summary line.
fn drive(repo: &Path, sandbox: &Path, session: &str, extra: &[&str]) -> String {
    let rift = rift_bin();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vogon"));
    cmd.args(["drive", "--rift"])
        .arg(&rift)
        .arg("--repo")
        .arg(repo)
        .args(["--turns", "2", "--session", session])
        .args(extra);
    let out = hermetic(&mut cmd, sandbox)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "vogon drive exited {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The same scripted session `drive` plays, as `(verb, payload)` — read from
/// `vogon script`, so a redelivery here is byte-identical to what the driver
/// sent rather than a second hand-written copy of it.
///
/// **`--repo` is passed here for the same reason `drive` passes it**, and
/// leaving it out is not a cosmetic difference: the payload carries `cwd` and
/// absolute tool paths, the idempotency key hashes the payload, and an omitted
/// `--repo` therefore produces a DIFFERENT key for what looks like the same
/// hook. The first draft of this suite omitted it and read the resulting new
/// rows as a dedup failure in the product; measured, the product was right and
/// the flags were not.
fn script(repo: &Path, session: &str) -> Vec<(String, String)> {
    let out = Command::new(env!("CARGO_BIN_EXE_vogon"))
        .args(["script", "--turns", "2", "--session", session])
        .arg("--repo")
        .arg(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "vogon script failed");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            (
                v["verb"].as_str().unwrap().to_owned(),
                v["payload"].to_string(),
            )
        })
        .collect()
}

/// Deliver one hook to the real binary, exactly as the driver does.
fn deliver(repo: &Path, sandbox: &Path, verb: &str, payload: &str) {
    let mut cmd = Command::new(rift_bin());
    cmd.args(["hooks", "claude-code", verb])
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hermetic(&mut cmd, sandbox).current_dir(repo);
    let mut child = cmd.spawn().unwrap();
    use std::io::Write as _;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "a hook never fails");
}

/// `(column 0, column 1)` for a query against this repository's own projection.
///
/// Both columns are read as TEXT, so a caller counting rows must `CAST(… AS
/// TEXT)`: a bare `count(*)` reads back as the empty string here, and a pair of
/// empty strings compares equal to any other pair of empty strings. The first
/// draft of this suite did exactly that and its replay assertion passed
/// vacuously — the [`census`] helper exists so no caller can repeat it.
fn rows(cache: &Path, repo: &Path, sql: &str) -> Vec<(String, String)> {
    let id = rift_git::repo_id_for(repo).unwrap();
    let db = rift_store::projection_db_under(cache, &id);
    assert!(db.exists(), "no projection at {}", db.display());
    let store = rift_store::open_capture_projection(db).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let rows = rt
        .block_on(rift_store::CanonicalStore::query(&store, sql, &[]))
        .unwrap();
    rows.iter()
        .map(|r| {
            (
                r.get_text(0).unwrap_or_default(),
                r.get_text(1).unwrap_or_default(),
            )
        })
        .collect()
}

/// `kind → count` over every canonical event this repository captured. Never
/// empty when anything was captured, which is what makes an equality assertion
/// over it non-vacuous.
fn census(cache: &Path, repo: &Path) -> Vec<(String, String)> {
    let census = rows(
        cache,
        repo,
        "SELECT kind, CAST(count(*) AS TEXT) FROM captured_event GROUP BY kind ORDER BY kind",
    );
    assert!(!census.is_empty(), "nothing at all was captured");
    census
}

/// The whole point: every verb the script fires leaves a **recorded** receipt,
/// including the five that no process had ever driven, and the canonical events
/// they fan out to are exactly the ones the adapter promises.
///
/// The expected multiset is written out here rather than imported from the
/// driver, deliberately: two independent statements of the same fact catch a
/// change to either side, where a shared constant would agree with itself.
#[test]
fn a_scripted_session_records_every_verb_including_the_four_never_driven() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let summary = drive(&repo, tmp.path(), "vogon-e2e-1", &[]);
    let summary: serde_json::Value = serde_json::from_str(summary.trim()).unwrap();
    assert_eq!(summary["hooks"], 19, "the driven script changed: {summary}");

    let cache = tmp.path().join("cache");
    let receipts = rows(
        &cache,
        &repo,
        "SELECT verb, outcome FROM agent_hook_event ORDER BY verb",
    );
    assert!(!receipts.is_empty(), "no hook receipts at all");
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for (verb, outcome) in &receipts {
        assert_eq!(
            outcome, "recorded",
            "{verb} landed as {outcome:?}, not a recorded capture"
        );
        *seen.entry(verb.clone()).or_default() += 1;
    }
    // The four this suite exists for, plus the five the old harness drove.
    for verb in [
        "PreToolUse",
        "SubagentStart",
        "SubagentStop",
        "PreCompact",
        "SessionStart",
        "UserPromptSubmit",
        "PostToolUse",
        "Stop",
        "SessionEnd",
    ] {
        assert!(
            seen.contains_key(verb),
            "no recorded receipt for {verb}; got {seen:?}"
        );
    }

    let kinds = census(&cache, &repo);
    let counted: BTreeMap<&str, &str> = kinds
        .iter()
        .map(|(k, n)| (k.as_str(), n.as_str()))
        .collect();
    // Two turns; the subagent rides turn 2 and the compaction follows turn 1.
    for (kind, count) in [
        ("session_start", "1"),
        ("prompt_submit", "2"),
        ("tool_call", "5"),
        ("file_edit", "2"),
        ("tool_result", "5"),
        ("turn_end", "2"),
        ("subagent_start", "1"),
        ("subagent_end", "1"),
        ("session_compaction", "1"),
        ("session_end", "1"),
    ] {
        assert_eq!(
            counted.get(kind).copied(),
            Some(count),
            "{kind}: expected {count}, got {counted:?}"
        );
    }
}

/// Re-delivering ONE hook is a no-op, not a second set of rows. The conformance
/// kit proves each adapter maps deterministically and
/// `every_registered_adapter_is_replay_safe_through_dispatch` proves the
/// stamping keys it stably; this proves the two compose through the shipped
/// binary and the store's idempotent writes — the only place a redelivered turn
/// could actually be double-counted.
///
/// Every hook of the last turn is redelivered, not just a convenient one: the
/// property is about the class, and the fan-out verbs (`PreToolUse` on a
/// file-mutating tool writes two canonical events from one delivery) are
/// exactly where a per-hook key could collide or move.
///
/// **The turn-opening prompt is excluded, and that is a fact about the product
/// rather than a convenience.** The idempotency key carries `turn_ordinal`, and
/// `effective_turn` (`rift-shadow/src/bridge.rs:480-496`) opens the *next*
/// ordinal for a `PromptSubmit` arriving on a closed turn. So a prompt
/// redelivered after its turn ended is not a redelivery at all — it is the
/// agent asking for a new turn, and it correctly gets one. Everything that does
/// not open a turn re-derives the identical key and dedups — measured here over
/// ten hooks including the two-event `PreToolUse` fan-out, with the census
/// unchanged in every kind.
#[test]
fn redelivering_the_last_turns_hooks_writes_no_second_row() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    drive(&repo, tmp.path(), "vogon-e2e-2", &[]);
    let before = census(&cache, &repo);
    let steps = script(&repo, "vogon-e2e-2");
    let opened = steps
        .iter()
        .rposition(|(verb, _)| verb == "UserPromptSubmit")
        .expect("the script opens at least one turn");
    let tail = steps.get(opened + 1..).unwrap_or_default();
    assert!(
        tail.iter().any(|(verb, _)| verb == "PreToolUse"),
        "the redelivered tail must include a fan-out verb"
    );
    for (verb, payload) in tail {
        deliver(&repo, tmp.path(), verb, payload);
    }
    assert_eq!(
        before,
        census(&cache, &repo),
        "a redelivered hook wrote new canonical events"
    );
}

/// **Resuming a session id is not a redelivery**, and the distinction is
/// measured rather than assumed — the first draft of the test above asserted
/// the opposite and was wrong.
///
/// A second `vogon drive` under the same session id continues the *live* cache
/// (`state: ended`, `current_turn_ordinal: 2`), so its prompts open turns 3 and
/// 4 rather than re-opening 1 and 2. The idempotency key carries
/// `turn_ordinal`, so those are genuinely new events. That is the product
/// behaving correctly: an agent that resumes a conversation is doing more work,
/// not redelivering old work.
#[test]
fn resuming_a_session_id_opens_new_turns_rather_than_duplicating_old_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let turns = |cache: &Path| {
        rows(
            cache,
            &repo,
            "SELECT CAST(count(DISTINCT turn_ordinal) AS TEXT), '' FROM captured_event",
        )
    };
    drive(&repo, tmp.path(), "vogon-e2e-4", &[]);
    // Three, not two: `SessionStart` arrives before any prompt has opened a
    // turn, so it is stamped with the honest ordinal 0 rather than being
    // back-dated into turn 1.
    assert_eq!(turns(&cache).first().map(|r| r.0.as_str()), Some("3"));
    drive(&repo, tmp.path(), "vogon-e2e-4", &[]);
    assert_eq!(
        turns(&cache).first().map(|r| r.0.as_str()),
        Some("5"),
        "a resumed session must open new turn ordinals, not re-open old ones"
    );
}

/// The commit beat: a real `git commit` between the last turn and `SessionEnd`,
/// with the session still live, condenses the session onto the native checkpoint
/// ref. This is the `session → turns → tools → subagent → compaction → commit`
/// walk end to end, in one process tree.
#[test]
fn the_commit_beat_condenses_the_live_session_onto_the_checkpoint_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let install = Command::new(rift_bin())
        .args(["hook", "install"])
        .current_dir(&repo)
        .env("RIFT_CACHE_DIR", tmp.path().join("cache"))
        .env("XDG_DATA_HOME", tmp.path().join("data"))
        .env("RIFT_NO_KEYCHAIN", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "hook install: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    drive(&repo, tmp.path(), "vogon-e2e-3", &["--commit"]);
    let refs = Command::new("git")
        .args(["for-each-ref", "--format=%(refname)", "refs/heads/rift/"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let refs = String::from_utf8_lossy(&refs.stdout).into_owned();
    assert!(
        refs.contains("refs/heads/rift/checkpoints/v1"),
        "the commit beat condensed nothing; refs = {refs:?}"
    );
}

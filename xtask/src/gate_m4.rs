//! `xtask gate-m4` — the scripted M4 gate (PRD-M4 "Testing Decisions").
//!
//! Drives the **real** `rift` binary headless over a hermetic scratch repo:
//! register → an *unseeded* frame proving every column-3 section renders its
//! honest placeholder (no demo seed anywhere) → seed captured sessions,
//! token facts, served `memd.*` events, and a live session cache → a frame
//! with **all six sections live** → a projection rebuild (epoch bump) with
//! *different* facts landed → a frame proving the fresh scoped read (no
//! stale numbers survive). The mid-run half of the rebuild story — the live
//! loop re-bootstrapping through the epoch poller without a restart — is
//! carried by `rift-canopy-data/tests/live_sections.rs` against the real
//! bus + loop; this script proves it end-to-end at the binary seam.
//!
//! Run `cargo run -p xtask -- gate-m4`. The dogfood half — bare `rift` on
//! this repository showing the same sections from real captured history —
//! is reported (never failed: what history exists by gate day is evidence,
//! not a check; see the M4 map's fog note).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rift_store::CanonicalStore as _;
use rift_store::Value;

/// One gate step outcome.
fn violation(v: &mut Vec<String>, ok: bool, what: &str) {
    if !ok {
        v.push(what.to_owned());
    }
}

/// The scratch environment: a temp repo + hermetic Rift dirs.
struct Scratch {
    _keep: tempfile::TempDir,
    repo: PathBuf,
    cache: PathBuf,
    config: PathBuf,
    rift: PathBuf,
}

impl Scratch {
    fn env(&self) -> Vec<(String, String)> {
        vec![
            ("RIFT_CACHE_DIR".into(), self.cache.display().to_string()),
            ("RIFT_CONFIG_DIR".into(), self.config.display().to_string()),
            ("RIFT_NO_PROVISION".into(), "1".into()),
            ("RIFT_HEADLESS".into(), "1".into()),
            // The OS keychain would pop an ACL prompt for a fresh binary.
            ("RIFT_NO_KEYCHAIN".into(), "1".into()),
            // Give the headless bus a generous window to deliver the feeds.
            ("RIFT_HEADLESS_WAIT_MS".into(), "4000".into()),
            ("GIT_AUTHOR_NAME".into(), "Gate".into()),
            ("GIT_AUTHOR_EMAIL".into(), "gate@rift.local".into()),
            ("GIT_COMMITTER_NAME".into(), "Gate".into()),
            ("GIT_COMMITTER_EMAIL".into(), "gate@rift.local".into()),
        ]
    }

    /// Run `rift <args>` in the scratch repo; return (exit_code, stdout+err).
    fn rift(&self, args: &[&str]) -> (i32, String) {
        run_cmd(&self.rift, args, &self.repo, &self.env())
    }

    /// The scratch repo's projection database path (created by seeding).
    fn projection_db(&self) -> Result<PathBuf, String> {
        Ok(rift_store::projection_db_under(
            &self.cache,
            &repo_id(&self.repo)?,
        ))
    }

    /// The scratch repo's recall database path.
    fn recall_db(&self) -> Result<PathBuf, String> {
        Ok(rift_store::recall_db_under(
            &self.cache,
            &repo_id(&self.repo)?,
        ))
    }
}

/// The repo id the binary derives (pinned formula: leaf-hash of the canonical
/// path, first 32 hex — see `rift_git::repo_id_for`).
fn repo_id(dir: &Path) -> Result<rift_core::RepoId, String> {
    let canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let hex = rift_hashchain::leaf_hash(canon.to_string_lossy().as_bytes()).to_hex();
    rift_core::RepoId::new(hex.get(..32).unwrap_or(&hex)).map_err(|e| format!("repo id: {e}"))
}

fn run_cmd(program: &Path, args: &[&str], cwd: &Path, env: &[(String, String)]) -> (i32, String) {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let Ok(out) = cmd.output() else {
        return (-1, String::new());
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

/// Build the `rift` binary and return its path.
fn build_rift(root: &Path) -> Result<PathBuf, String> {
    let status = Command::new("cargo")
        .args(["build", "-p", "rift"])
        .current_dir(root)
        .status()
        .map_err(|e| format!("cargo build: {e}"))?;
    if !status.success() {
        return Err("cargo build -p rift failed".into());
    }
    let target =
        std::env::var("CARGO_TARGET_DIR").map_or_else(|_| root.join("target"), PathBuf::from);
    let bin = target
        .join("debug")
        .join(if cfg!(windows) { "rift.exe" } else { "rift" });
    if bin.exists() {
        Ok(bin)
    } else {
        Err(format!("built binary not found at {}", bin.display()))
    }
}

/// A short-lived runtime for the seeding writes.
fn rt() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|e| format!("runtime: {e}"))
}

/// Seed one captured session + a usage-bearing model call into the projection.
fn seed_projection(
    db: &Path,
    call_id: &str,
    t: i64,
    classes: (i64, i64, i64, i64),
) -> Result<(), String> {
    let store =
        rift_store::open_capture_projection(db).map_err(|e| format!("open projection: {e}"))?;
    let rt = rt()?;
    // The session row (idempotent per id; ignore a duplicate on re-seed).
    let _ = rt.block_on(store.insert_or_ignore(
        "session",
        &[
            "session_id",
            "agent",
            "model_primary",
            "repo_id",
            "started_at",
            "schema_version",
        ],
        &[
            Value::text("gate-session"),
            Value::text("claude-code"),
            Value::text("claude-fable-5"),
            Value::text("gate-repo"),
            Value::Int(1),
            Value::Int(1),
        ],
    ));
    rt.block_on(store.insert(
        "model_call",
        &[
            "call_id",
            "turn_id",
            "model",
            "started_at",
            "schema_version",
        ],
        &[
            Value::text(call_id),
            Value::text("t1"),
            Value::text("claude-fable-5"),
            Value::Int(t),
            Value::Int(1),
        ],
    ))
    .map_err(|e| format!("seed model_call: {e}"))?;
    rt.block_on(store.insert(
        "token_usage",
        &[
            "call_id",
            "uncached_input",
            "cached_input",
            "cache_creation",
            "output",
            "counter_method",
            "schema_version",
        ],
        &[
            Value::text(call_id),
            Value::Int(classes.0),
            Value::Int(classes.1),
            Value::Int(classes.2),
            Value::Int(classes.3),
            Value::text("provider_reported"),
            Value::Int(1),
        ],
    ))
    .map_err(|e| format!("seed token_usage: {e}"))?;
    Ok(())
}

/// Seed served `memd.*` audit events into the recall projection.
fn seed_recall(db: &Path, events: &[(i64, i64)]) -> Result<(), String> {
    if let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("recall dir: {e}"))?;
    }
    let store = rift_store::open_migrated(db, &rift_store::recall_migrations())
        .map_err(|e| format!("open recall.db: {e}"))?;
    let rt = rt()?;
    for (i, (t, avoided)) in events.iter().enumerate() {
        rt.block_on(store.insert(
            "memd_audit_event",
            &[
                "audit_id",
                "verb",
                "handle",
                "reason",
                "tokens_returned",
                "tokens_avoided",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text(format!("gate-a{i}-{t}")),
                Value::text("memd.summary_served"),
                Value::text("h"),
                Value::Null,
                Value::Int(10),
                Value::Int(*avoided),
                Value::Int(*t),
                Value::Int(1),
            ],
        ))
        .map_err(|e| format!("seed memd audit event: {e}"))?;
    }
    Ok(())
}

/// Write a live per-session cache entry (what the capture hooks maintain).
fn seed_session_cache(repo: &Path) -> Result<(), String> {
    let dir = repo.join(".git").join("rift-sessions");
    std::fs::create_dir_all(&dir).map_err(|e| format!("sessions dir: {e}"))?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0));
    let body = format!(
        r#"{{"schema":1,"session_id":"gate-session","state":"active","agent":"claude-code",
            "current_turn_ordinal":2,"turn_open":true,"compaction_boundary":null,
            "updated_at_ms":{now_ms},"native_id_map":{{}}}}"#
    );
    std::fs::write(dir.join("gate-session.json"), body)
        .map_err(|e| format!("session cache: {e}"))?;
    Ok(())
}

/// The six live-section markers asserted on a seeded frame.
const LIVE_MARKERS: [&str; 6] = [
    "token usage  Σ",
    "input | read | output  read",
    "nic-gauge  ",
    "recall savings  Σ",
    "recall mcp calls  Σ",
    "threads  1 active",
];

/// The six placeholder markers asserted on the unseeded frame.
const PLACEHOLDER_MARKERS: [&str; 6] = [
    "token usage  (no token activity yet)",
    // The breakdown placeholder truncates at the column edge on a 100-wide
    // frame; match the stable prefix.
    "input | read | output  (no token",
    "(no model calls yet)",
    "recall savings  (recall off",
    "recall mcp calls  (recall off",
    "threads  (no active sessions)",
];

/// Run the gate. Returns the violation list (empty = pass).
///
/// # Errors
/// A `String` when the environment cannot even be built (cargo/binary).
pub fn run(root: &Path) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    let rift = build_rift(root)?;

    let keep = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let repo = keep.path().join("gate-repo");
    std::fs::create_dir_all(repo.join(".git")).map_err(|e| format!("scratch repo: {e}"))?;
    let scratch = Scratch {
        repo: repo.clone(),
        cache: keep.path().join("cache"),
        config: keep.path().join("config"),
        rift,
        _keep: keep,
    };

    // ---- register the scratch repo -----------------------------------------
    let (code, out) = scratch.rift(&["setup"]);
    violation(
        &mut v,
        code == 0,
        &format!("rift setup failed ({code}): {out}"),
    );

    // ---- frame 0: NO seed anywhere → every section is its placeholder ------
    let (code, frame0) = scratch.rift(&[]);
    violation(&mut v, code == 0, "headless dashboard (frame 0) failed");
    for marker in PLACEHOLDER_MARKERS {
        violation(
            &mut v,
            frame0.contains(marker),
            &format!("frame 0 must show the honest placeholder {marker:?}, got:\n{frame0}"),
        );
    }

    // ---- seed: captured tokens + served memd events + a live session -------
    seed_projection(
        &scratch.projection_db()?,
        "c1",
        1_000,
        (5_000, 48_000, 3_300, 9_100),
    )?;
    seed_projection(
        &scratch.projection_db()?,
        "c2",
        60_000,
        (20_000, 100_000, 0, 4_000),
    )?;
    seed_recall(&scratch.recall_db()?, &[(1_000, 500), (30_000, 1_500)])?;
    seed_session_cache(&scratch.repo)?;

    // ---- frame 1: all six sections live -------------------------------------
    let (code, frame1) = scratch.rift(&[]);
    violation(&mut v, code == 0, "headless dashboard (frame 1) failed");
    for marker in LIVE_MARKERS {
        violation(
            &mut v,
            frame1.contains(marker),
            &format!("frame 1 must render {marker:?} live, got:\n{frame1}"),
        );
    }
    // The seeded totals themselves (proving real data, not a demo seed).
    violation(
        &mut v,
        frame1.contains("Σ 189K"),
        "frame 1 token total Σ 189K",
    );
    violation(
        &mut v,
        frame1.contains("Σ 2.0K saved"),
        "frame 1 savings Σ 2.0K",
    );
    // The known model reads a percentage; the live session shows Running.
    violation(&mut v, frame1.contains("% "), "frame 1 nic percentage");
    violation(
        &mut v,
        frame1.contains("●"),
        "frame 1 Running glyph from the session cache",
    );

    // ---- rebuild mid-run: epoch bump + different facts → fresh read ---------
    rift_store::rebuild(&scratch.projection_db()?).map_err(|e| format!("rebuild: {e}"))?;
    seed_projection(&scratch.projection_db()?, "n1", 2_000, (300, 0, 0, 100))?;
    let (code, frame2) = scratch.rift(&[]);
    violation(&mut v, code == 0, "headless dashboard (frame 2) failed");
    violation(
        &mut v,
        frame2.contains("token usage  Σ 400"),
        &format!("frame 2 must show the fresh post-rebuild total (Σ 400), got:\n{frame2}"),
    );
    violation(
        &mut v,
        !frame2.contains("Σ 189K"),
        "frame 2 must not serve the pre-rebuild total (stale data survived the rebuild)",
    );

    Ok(v)
}

/// The dogfood half: run the freshly-built binary headless on `root` (this
/// repository) with the developer's real environment and report what the six
/// sections showed. Never fails the gate — the observations are the point.
#[must_use]
pub fn dogfood(root: &Path) -> String {
    let target =
        std::env::var("CARGO_TARGET_DIR").map_or_else(|_| root.join("target"), PathBuf::from);
    let bin = target
        .join("debug")
        .join(if cfg!(windows) { "rift.exe" } else { "rift" });
    let mut cmd = Command::new(bin);
    cmd.current_dir(root)
        .env("RIFT_HEADLESS", "1")
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_NO_PROVISION", "1")
        .env("RIFT_HEADLESS_WAIT_MS", "4000")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let Ok(out) = cmd.output() else {
        return "dogfood: could not run the binary".into();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut report = String::from("dogfood (bare `rift` on this repository):\n");
    for line in text.lines() {
        // The section titles sit mid-line (column 3 of the frame).
        if let Some(idx) = line.find("· ") {
            let l = line.get(idx..).unwrap_or("").trim_end_matches(['│', ' ']);
            report.push_str("  ");
            report.push_str(l);
            report.push('\n');
        } else if line.contains('●') || line.contains("RIFT — Canopy") {
            // The running-project rows + title give the observations context.
            report.push_str("  ");
            report.push_str(line.trim_end_matches(['│', ' ']).trim_start());
            report.push('\n');
        }
    }
    if report.lines().count() == 1 {
        report.push_str("  (no section lines found — is a project registered?)\n");
    }
    report
}

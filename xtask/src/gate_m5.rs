//! `xtask gate-m5` — the scripted M5 gate (PRD-M5 "Testing Decisions").
//!
//! Five legs, per the revised PRD's gate text:
//!
//! 1. **Liveness** — a hermetic scratch repo renders LIVE column-3 frames
//!    with **zero injected requests** (the loop's own startup prime is the
//!    only subscribe; `bins/rift` is grep-clean of hardcoded `SetActive`),
//!    and the selection follows the launch cwd.
//! 2. **Palette** — every grammar verb across all four tiers drives typed
//!    effects and a recording dispatcher (the rift-canopy suites built for
//!    exactly that: the tier-table mapping walk, the mock-dispatcher loop
//!    tests, the confirm gate, the structural no-spawn scan), plus the
//!    parse-fuzz-target-in-CI check.
//! 3. **Memory dogfood on THIS repository** — recall enabled → `.mcp.json`
//!    provisioned → checkpoints present → ingest populated → verified
//!    serving → the memory surface shows captured reads (headline numbers
//!    reported).
//! 4. **Full3d** — the budget/downgrade suite passes and the criterion
//!    bench exists (its numbers are the recorded evidence; the ladder's
//!    behavior is asserted under the injected slow clock).
//! 5. **Coverage bars** — ≥85% line for rift-canopy + rift-canopy-data,
//!    computed via `cargo llvm-cov` when available (recorded), else the CI
//!    wiring that enforces the bars is verified.
//!
//! Run `cargo run -p xtask -- gate-m5`. gate-m4 stays intact as the M4
//! regression harness.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One gate step outcome.
fn violation(v: &mut Vec<String>, ok: bool, what: &str) {
    if !ok {
        v.push(what.to_owned());
    }
}

fn run_cmd(program: &str, args: &[&str], cwd: &Path, env: &[(String, String)]) -> (i32, String) {
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

/// Recursively scan `dir`'s `.rs` files for a token in CODE (comments allowed).
fn scan_code_for(dir: &Path, token: &str, hits: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_code_for(&path, token, hits);
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains(token) {
                hits.push(format!("{}:{}", path.display(), i + 1));
            }
        }
    }
}

/// Leg 1 — liveness on a hermetic scratch repo: seeded token facts render
/// live with ZERO injected requests, and the selection follows the launch cwd.
fn leg_liveness(root: &Path, rift: &Path, v: &mut Vec<String>) -> Result<(), String> {
    // Structural half: no hardcoded SetActive in the binary's sources (the
    // shared prime in rift-canopy is the only startup path).
    let mut hits = Vec::new();
    scan_code_for(&root.join("bins/rift/src"), "SetActive", &mut hits);
    violation(
        v,
        hits.is_empty(),
        &format!("liveness: hardcoded SetActive in bins/rift: {hits:?}"),
    );

    // Behavioral half: two scratch repos registered; launch from the SECOND —
    // the frame must select it (cwd follow) and render its seeded facts live
    // without any injected subscribe (the binary has no such flag at all).
    let tmp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let cache = tmp.path().join("cache");
    let config = tmp.path().join("config");
    let env: Vec<(String, String)> = vec![
        ("RIFT_CACHE_DIR".into(), cache.display().to_string()),
        ("RIFT_CONFIG_DIR".into(), config.display().to_string()),
        ("RIFT_NO_PROVISION".into(), "1".into()),
        ("RIFT_HEADLESS".into(), "1".into()),
        ("RIFT_NO_KEYCHAIN".into(), "1".into()),
        ("RIFT_HEADLESS_WAIT_MS".into(), "4000".into()),
    ];
    let mk_repo = |name: &str| -> Result<PathBuf, String> {
        let repo = tmp.path().join(name);
        std::fs::create_dir_all(&repo).map_err(|e| format!("mkdir: {e}"))?;
        let (code, out) = run_cmd("git", &["init", "-q"], &repo, &[]);
        if code != 0 {
            return Err(format!("git init: {out}"));
        }
        Ok(repo)
    };
    let first = mk_repo("first")?;
    let second = mk_repo("second")?;
    for repo in [&first, &second] {
        let (code, out) = run_cmd(
            &rift.display().to_string(),
            &["setup", "--no-provision"],
            repo,
            &env,
        );
        if code != 0 {
            return Err(format!("setup failed: {out}"));
        }
    }
    // Seed a usage-bearing fact into the SECOND repo's projection.
    let canon = std::fs::canonicalize(&second).unwrap_or_else(|_| second.clone());
    let hex = rift_hashchain::leaf_hash(canon.to_string_lossy().as_bytes()).to_hex();
    let repo_id =
        rift_core::RepoId::new(hex.get(..32).unwrap_or(&hex)).map_err(|e| format!("{e}"))?;
    let db = rift_store::projection_db_under(&cache, &repo_id);
    seed_token_fact(&db, "gate-c1", 1_000, (500, 6_000, 0, 700))?;

    let (code, frame) = run_cmd(&rift.display().to_string(), &[], &second, &env);
    violation(v, code == 0, "liveness: bare rift (headless) exits 0");
    violation(
        v,
        frame.contains("> ● second")
            || frame.contains("> ○ second")
            || frame.contains("> ◐ second"),
        &format!("liveness: the launch-cwd repo is selected: {frame}"),
    );
    violation(
        v,
        frame.contains("token usage  Σ"),
        "liveness: the token section rendered LIVE with zero injected requests",
    );
    violation(
        v,
        !frame.contains("no token activity yet"),
        "liveness: the seeded repo shows data, not the placeholder",
    );
    Ok(())
}

/// Seed one captured model call + token usage (the same shape gate-m4 seeds).
fn seed_token_fact(
    db: &Path,
    call_id: &str,
    t: i64,
    classes: (i64, i64, i64, i64),
) -> Result<(), String> {
    use rift_store::{CanonicalStore as _, Value};
    let store =
        rift_store::open_capture_projection(db).map_err(|e| format!("open projection: {e}"))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
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

/// Leg 2 — the palette: the canopy suites that drive every verb through
/// typed effects and a recording dispatcher, the confirm gate, and the
/// structural no-spawn proof; plus the fuzz target's CI wiring.
fn leg_palette(root: &Path, v: &mut Vec<String>) {
    let (code, out) = run_cmd(
        "cargo",
        &[
            "test",
            "-p",
            "rift-canopy",
            "--lib",
            "--",
            "dispatch::",
            "command::",
            "app::tests::read_only_verb_crosses_the_dispatcher_and_lands_in_the_overlay",
            "app::tests::mutating_verb_dispatches_only_after_confirm_and_reloads",
            "app::tests::cancelled_confirm_dispatches_nothing",
        ],
        root,
        &[],
    );
    violation(
        v,
        code == 0,
        &format!("palette: grammar/dispatcher/confirm suites: {out}"),
    );
    let (code, out) = run_cmd(
        "cargo",
        &["test", "-p", "rift-canopy", "--test", "no_spawn"],
        root,
        &[],
    );
    violation(
        v,
        code == 0,
        &format!("palette: structural no-spawn: {out}"),
    );

    // The parse fuzz target is wired into CI's smoke and the nightly matrix.
    let ci = std::fs::read_to_string(root.join(".github/workflows/ci.yml")).unwrap_or_default();
    violation(
        v,
        ci.contains("palette_parse"),
        "palette: palette_parse in the CI fuzz smoke",
    );
    let nightly = std::fs::read_to_string(root.join(".github/workflows/fuzz-nightly.yml"))
        .unwrap_or_default();
    violation(
        v,
        nightly.contains("palette_parse"),
        "palette: palette_parse in the nightly fuzz matrix",
    );
    violation(
        v,
        root.join("fuzz/fuzz_targets/palette_parse.rs").exists(),
        "palette: the fuzz target exists",
    );
}

/// Leg 3 — the memory dogfood on THIS repository, read through the real
/// binary's own diagnosis: recall enabled, provisioned, checkpoints, ingested
/// reads, servable daemon — then the memory surface's real reads.
fn leg_memory_dogfood(root: &Path, rift: &Path, v: &mut Vec<String>) -> Result<String, String> {
    let (code, out) = run_cmd(
        &rift.display().to_string(),
        &["doctor", "--json"],
        root,
        &[],
    );
    violation(v, code == 0, "dogfood: rift doctor exits 0");
    let doc: serde_json::Value =
        serde_json::from_str(&out).map_err(|e| format!("doctor json: {e}: {out}"))?;
    let empty = serde_json::Value::Null;
    let chain = doc.get("memory_chain").unwrap_or(&empty);
    let checkpoints = chain
        .get("checkpoints")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let ingested = chain
        .get("ingested_reads")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    violation(v, checkpoints > 0, "dogfood: checkpoints present");
    violation(
        v,
        chain
            .get("recall_enabled")
            .and_then(serde_json::Value::as_bool)
            == Some(true),
        "dogfood: rift-total-recall enabled here",
    );
    violation(
        v,
        chain.get("mcp_json").and_then(serde_json::Value::as_str) == Some("provisioned"),
        "dogfood: .mcp.json provisioned",
    );
    violation(
        v,
        ingested > 0,
        "dogfood: recall ingest populated (reads > 0)",
    );
    violation(
        v,
        chain
            .get("memd")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|p| !p.is_empty()),
        "dogfood: rift-memd servable (binary present)",
    );
    // Verified serving + captured reads on the memory surface.
    let (code, hotspots) = run_cmd(
        &rift.display().to_string(),
        &["memory", "cost-hotspots"],
        root,
        &[],
    );
    violation(v, code == 0, "dogfood: cost-hotspots serves");
    violation(
        v,
        !hotspots.contains("no captured reads"),
        &format!("dogfood: the memory surface shows captured reads: {hotspots}"),
    );
    Ok(format!(
        "  checkpoints: {checkpoints}; ingested reads: {ingested}\n  cost-hotspots:\n{}",
        hotspots
            .lines()
            .take(6)
            .map(|l| format!("    {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

/// Leg 4 — Full3d: the budget/downgrade + mesh suites, and the bench exists.
fn leg_full3d(root: &Path, v: &mut Vec<String>) {
    let (code, out) = run_cmd(
        "cargo",
        &[
            "test",
            "-p",
            "rift-canopy",
            "--lib",
            "--",
            "splash::director::",
            "splash::mesh3d::",
        ],
        root,
        &[],
    );
    violation(
        v,
        code == 0,
        &format!("full3d: director/mesh suites: {out}"),
    );
    violation(
        v,
        root.join("crates/rift-canopy/benches/splash_frame.rs")
            .exists(),
        "full3d: the criterion frame-budget bench exists",
    );
}

/// Leg 5 — the coverage bars: computed when `cargo llvm-cov` is available
/// (recorded in the output); otherwise the CI wiring that enforces them is
/// verified (xtask check-coverage with the ≥85% floor).
fn leg_coverage(root: &Path, v: &mut Vec<String>) -> String {
    let probe = run_cmd("cargo", &["llvm-cov", "--version"], root, &[]);
    if probe.0 == 0 {
        let (code, out) = run_cmd(
            "cargo",
            &[
                "llvm-cov",
                "--summary-only",
                "--json",
                "-p",
                "rift-canopy",
                "-p",
                "rift-canopy-data",
            ],
            root,
            &[],
        );
        if code == 0 {
            // The combined stream carries test progress lines around the JSON
            // document — stream-parse the first value from the first brace.
            let json_start = out.find('{').unwrap_or(0);
            let mut stream =
                serde_json::Deserializer::from_str(out.get(json_start..).unwrap_or("{}"))
                    .into_iter::<serde_json::Value>();
            if let Some(Ok(doc)) = stream.next() {
                let pct = doc
                    .get("data")
                    .and_then(|d| d.get(0))
                    .and_then(|d| d.get("totals"))
                    .and_then(|t| t.get("lines"))
                    .and_then(|l| l.get("percent"))
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                violation(
                    v,
                    pct >= 85.0,
                    &format!("coverage: rift-canopy(+data) line {pct:.1}% < 85%"),
                );
                return format!("  measured line coverage (canopy crates): {pct:.1}%");
            }
        }
        let tail: String = out
            .chars()
            .rev()
            .take(400)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        violation(
            v,
            false,
            &format!("coverage: llvm-cov run failed (exit {code}): …{tail}"),
        );
        return String::new();
    }
    // No local llvm-cov: the CI gate must be carrying the bars.
    let ci = std::fs::read_to_string(root.join(".github/workflows/ci.yml")).unwrap_or_default();
    violation(
        v,
        ci.contains("check-coverage"),
        "coverage: CI runs xtask check-coverage",
    );
    "  coverage: enforced in CI (xtask check-coverage; no local llvm-cov)".into()
}

/// Run every gate leg; returns human-readable violations (empty = pass).
///
/// # Errors
/// A setup failure (not a gate violation) as a string.
pub fn run(root: &Path) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    let rift = build_rift(root)?;
    leg_liveness(root, &rift, &mut v)?;
    leg_palette(root, &mut v);
    let dogfood = leg_memory_dogfood(root, &rift, &mut v)?;
    leg_full3d(root, &mut v);
    let coverage = leg_coverage(root, &mut v);
    if v.is_empty() {
        println!("gate-m5 evidence:\n{dogfood}\n{coverage}");
    }
    Ok(v)
}

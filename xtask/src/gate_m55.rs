//! `xtask gate-m55` — the scripted M5.5 gate (PRD-M5.5 "Testing Decisions").
//!
//! Five legs, per the PRD's gate text:
//!
//! 1. **Tier** — the style-aware goldens render across all four capability
//!    tiers (truecolor / 256 / mono / ASCII) for the load-bearing surfaces
//!    (header, rail, each card, each column-3 panel, tiles, the donut). Driven
//!    by the rift-canopy `style_goldens` + `snapshots` suites, which serialize
//!    per-cell fg/bg/attrs and assert the mono/ASCII floors carry no colour.
//! 2. **Donut** — radial-on-SUBCELL + the horizontal-bar degrade + the
//!    unknown-model indicator, plus the arc's monotonic-coverage property and
//!    the band-colour steps at the gauge crate's boundaries (49/50, 94/95).
//! 3. **Data** — the activity + panel-metrics feeds against fixture
//!    projections (empty days, day boundaries, missing projections, disabled
//!    recall), and the savings %'s no-drift-vs-ledger property.
//! 4. **Launcher** — `e` and the repository button through the recording mock
//!    (the loop tests), the extended no-spawn structural proof, and the bin's
//!    argv-only property over adversarial `$EDITOR`/path values.
//! 5. **Coverage bars** — ≥85% line for rift-canopy + rift-canopy-data,
//!    computed via `cargo llvm-cov` when available (recorded), else the CI
//!    wiring that enforces the bars is verified.
//!
//! Run `cargo run -p xtask -- gate-m55`. The earlier gates stay intact.

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
    for (k, val) in env {
        cmd.env(k, val);
    }
    let Ok(out) = cmd.output() else {
        return (-1, String::new());
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

/// Run one named test binary in a canopy crate with `INSTA_UPDATE=no` (a colour
/// or layout regression fails, never silently updates a golden).
fn cargo_test(root: &Path, pkg: &str, args: &[&str]) -> (i32, String) {
    let mut full = vec!["test", "-p", pkg];
    full.extend_from_slice(args);
    run_cmd(
        "cargo",
        &full,
        root,
        &[("INSTA_UPDATE".into(), "no".into())],
    )
}

/// Leg 1 — the per-tier style goldens render green (the serializer proves the
/// mono/ASCII floors carry no colour; the goldens pin every load-bearing
/// surface at all four tiers).
fn leg_tier(root: &Path, v: &mut Vec<String>) {
    let (code, out) = cargo_test(root, "rift-canopy", &["--test", "style_goldens"]);
    violation(
        v,
        code == 0,
        &format!(
            "tier: style_goldens (per-tier serializer + goldens) failed:\n{}",
            tail(&out)
        ),
    );
    let (code, out) = cargo_test(root, "rift-canopy", &["--test", "snapshots"]);
    violation(
        v,
        code == 0,
        &format!("tier: snapshots (glyph goldens) failed:\n{}", tail(&out)),
    );
    // The serializer + theme resolution live-tested (NO_COLOR-is-a-property).
    let (code, _) = cargo_test(root, "rift-canopy", &["--lib", "theme::"]);
    violation(v, code == 0, "tier: theme resolution property tests failed");
}

/// Leg 2 — the donut: radial-on-SUBCELL + bar-degrade + the unknown indicator,
/// plus the arc/band properties.
fn leg_donut(root: &Path, v: &mut Vec<String>) {
    // The dedicated donut goldens (radial + degrade) and the direct assertions.
    let (code, out) = cargo_test(root, "rift-canopy", &["--test", "style_goldens", "donut"]);
    violation(
        v,
        code == 0,
        &format!(
            "donut: radial/degrade/unknown goldens failed:\n{}",
            tail(&out)
        ),
    );
    // The arc + system-panel unit properties (monotonic coverage, band steps).
    let (code, _) = cargo_test(root, "rift-canopy", &["--lib", "donut"]);
    violation(
        v,
        code == 0,
        "donut: arc coverage / band property tests failed",
    );
    let (code, _) = cargo_test(root, "rift-canopy", &["--lib", "system_panel"]);
    violation(v, code == 0, "donut: system-panel unit tests failed");
}

/// Leg 3 — the data plane: the activity + metrics feeds and the savings %
/// no-drift property against fixture projections.
fn leg_data(root: &Path, v: &mut Vec<String>) {
    let (code, out) = cargo_test(root, "rift-canopy-data", &["--lib", "feeds::"]);
    violation(
        v,
        code == 0,
        &format!(
            "data: feed fixture tests (activity/metrics/savings) failed:\n{}",
            tail(&out)
        ),
    );
    // The reducer routes the two new feeds + resets on selection change.
    let (code, _) = cargo_test(root, "rift-canopy", &["--lib", "activity_feed"]);
    violation(v, code == 0, "data: activity-feed reducer test failed");
    let (code, _) = cargo_test(root, "rift-canopy", &["--lib", "metrics_feed"]);
    violation(v, code == 0, "data: metrics-feed reducer test failed");
}

/// Leg 4 — the editor launcher: the `e`/button loop tests, the extended
/// no-spawn structural proof, and the bin's argv-only property.
fn leg_launcher(root: &Path, v: &mut Vec<String>) {
    let (code, _) = cargo_test(root, "rift-canopy", &["--test", "no_spawn"]);
    violation(
        v,
        code == 0,
        "launcher: the no-spawn structural proof failed",
    );
    let (code, out) = cargo_test(root, "rift-canopy", &["--lib", "editor"]);
    violation(
        v,
        code == 0,
        &format!(
            "launcher: `e`/button loop + reducer tests failed:\n{}",
            tail(&out)
        ),
    );
    // The bin is a binary target (no lib), so its unit tests run without `--lib`.
    let (code, out) = cargo_test(root, "rift", &["editor_argv"]);
    violation(
        v,
        code == 0,
        &format!(
            "launcher: the bin's argv-only property failed:\n{}",
            tail(&out)
        ),
    );
}

/// Build the `rift` binary and return its path (proves the bin compiles with
/// the launcher wired).
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

/// Leg 5 — the coverage bars (≥85% line for the two canopy crates): computed
/// via `cargo llvm-cov` when available, else the CI wiring is verified. Scoped
/// to the **library** targets — the bulk of the surface, and deterministic
/// (the `live_sections` integration test uses the real macOS FSEvents watcher,
/// which is flaky under llvm-cov's parallel instrumentation; its own gate leg
/// and the CI matrix cover it directly).
fn leg_coverage(root: &Path, v: &mut Vec<String>) -> String {
    let probe = run_cmd("cargo", &["llvm-cov", "--version"], root, &[]);
    if probe.0 == 0 {
        let (code, out) = run_cmd(
            "cargo",
            &[
                "llvm-cov",
                "--summary-only",
                "--json",
                "--lib",
                "-p",
                "rift-canopy",
                "-p",
                "rift-canopy-data",
            ],
            root,
            &[("INSTA_UPDATE".into(), "no".into())],
        );
        if code == 0 {
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
        violation(
            v,
            false,
            &format!(
                "coverage: llvm-cov run failed (exit {code}): …{}",
                tail(&out)
            ),
        );
        return String::new();
    }
    let ci = std::fs::read_to_string(root.join(".github/workflows/ci.yml")).unwrap_or_default();
    violation(
        v,
        ci.contains("check-coverage"),
        "coverage: CI runs xtask check-coverage",
    );
    "  coverage: enforced in CI (xtask check-coverage; no local llvm-cov)".into()
}

/// The last ~500 chars of a command's combined output (for a compact evidence
/// tail in a violation message).
fn tail(out: &str) -> String {
    out.chars()
        .rev()
        .take(500)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

/// Run every gate leg; returns human-readable violations (empty = pass).
///
/// # Errors
/// A setup failure (not a gate violation) as a string.
pub fn run(root: &Path) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    // The bin must build with the launcher wired before the legs run.
    let _rift = build_rift(root)?;
    leg_tier(root, &mut v);
    leg_donut(root, &mut v);
    leg_data(root, &mut v);
    leg_launcher(root, &mut v);
    let coverage = leg_coverage(root, &mut v);
    if v.is_empty() {
        println!("gate-m55 evidence:\n{coverage}");
    }
    Ok(v)
}

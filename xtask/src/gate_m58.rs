//! `xtask gate-m58` — the scripted M5.8 gate (PRD-M5.8 "Testing Decisions").
//!
//! Three legs, per the PRD's gate text (retire the editor + data-driven layout
//! — ADRs 0032/0033):
//!
//! 1. **Removal** — no `EditorLauncher` / `OpenEditor` / `editor_launch` /
//!    `EditorButton` / *Open in $EDITOR* symbol remains in the crate/bin source,
//!    and the `no_spawn` structural proof passes (the dashboard is back to zero
//!    spawns).
//! 2. **Seam** — a `LayoutModel` drives `layout::solve` such that the default
//!    model tiles byte-for-byte identically to the shipped `[20,40,40]` grid
//!    (`split_n ≡ split_3` + `solve_model(default) ≡ solve`), an arbitrary model
//!    still tiles exactly, and the non-editor snapshot goldens are byte-stable
//!    (`INSTA_UPDATE=no`) — the no-behavior-change proof.
//! 3. **Coverage bars** — ≥85% line across the canopy surface (rift-canopy +
//!    rift-canopy-data libraries, as gate-m55/m56/m57), via `cargo llvm-cov` when
//!    available (recorded), else the CI wiring is verified.
//!
//! Run `cargo run -p xtask -- gate-m58`. The earlier gates stay intact.

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

/// The `N` in `test result: ok. N passed` (0 when absent) — so a leg can prove
/// its filters matched tests (a typo'd filter that runs zero tests still exits 0,
/// which must not read as a pass).
fn passed_count(out: &str) -> usize {
    out.lines()
        .filter_map(|l| l.split_once("test result: ok. "))
        .filter_map(|(_, rest)| rest.split_once(" passed"))
        .filter_map(|(n, _)| n.trim().parse::<usize>().ok())
        .sum()
}

/// Run a set of `--lib` test-name filters (OR semantics) with `INSTA_UPDATE=no`,
/// asserting the run is green **and** matched at least one test (guards filter
/// drift from silently passing).
fn require_lib(root: &Path, v: &mut Vec<String>, filters: &[&str], label: &str) {
    let mut full = vec!["test", "-p", "rift-canopy", "--lib", "--"];
    full.extend_from_slice(filters);
    let (code, out) = run_cmd(
        "cargo",
        &full,
        root,
        &[("INSTA_UPDATE".into(), "no".into())],
    );
    violation(
        v,
        code == 0,
        &format!("{label}: tests failed:\n{}", tail(&out)),
    );
    violation(
        v,
        passed_count(&out) > 0,
        &format!("{label}: filters {filters:?} matched no tests (filter drift)"),
    );
}

/// Run a named integration-test binary (optionally with a name filter).
fn require_test_bin(
    root: &Path,
    v: &mut Vec<String>,
    bin: &str,
    filter: Option<&str>,
    label: &str,
) {
    let mut full = vec!["test", "-p", "rift-canopy", "--test", bin];
    if let Some(f) = filter {
        full.push(f);
    }
    let (code, out) = run_cmd(
        "cargo",
        &full,
        root,
        &[("INSTA_UPDATE".into(), "no".into())],
    );
    violation(
        v,
        code == 0,
        &format!("{label}: {bin} failed:\n{}", tail(&out)),
    );
}

/// Leg 1 — the editor hand-off is gone (no lingering symbol) and the no-spawn
/// proof passes.
fn leg_removal(root: &Path, v: &mut Vec<String>) {
    // No retired editor symbol survives anywhere in the crate/bin source. grep
    // exits 0 when it MATCHES, so a match is the violation. (xtask/ and docs/ are
    // excluded — this gate names the symbols in strings, and the ADRs record the
    // retirement.)
    let (code, out) = run_cmd(
        "grep",
        &[
            "-rIn",
            "--include=*.rs",
            "-e",
            "EditorLauncher",
            "-e",
            "OpenEditor",
            "-e",
            "editor_launch",
            "-e",
            "EditorButton",
            "-e",
            "Open in $EDITOR",
            "crates",
            "bins",
        ],
        root,
        &[],
    );
    violation(
        v,
        code != 0,
        &format!(
            "removal: a retired editor symbol survives in source:\n{}",
            tail(&out)
        ),
    );
    // The dashboard spawns nothing reachable from the grammar (the sole sanctioned
    // spawn is gone; the proof strengthens to "zero spawns").
    require_test_bin(root, v, "no_spawn", None, "removal");
}

/// Leg 2 — the data-driven layout is a no-op at the default: the fidelity pins
/// hold and the non-editor goldens are byte-stable.
fn leg_seam(root: &Path, v: &mut Vec<String>) {
    require_lib(
        root,
        v,
        &[
            // The fidelity pins (ADR-0033): default model ≡ shipped grid.
            "split_n_matches_split_3_for_the_default",
            "solve_model_default_equals_solve",
            // Exact-tiling for an arbitrary arrangement + the module's own tests.
            "arbitrary_model_tiles_the_body_exactly",
            "split_n_tiles_exactly_for_arbitrary_columns",
            "layout_model::",
            // The layout solver's own exact-tiling proofs still hold.
            "tiling_is_exact_and_gapless",
            "columns_abut_with_no_gap_or_overlap",
        ],
        "seam",
    );
    // The dashboard goldens are byte-stable under the refactor (INSTA_UPDATE=no):
    // the default model renders exactly as before.
    require_test_bin(root, v, "snapshots", None, "seam");
    require_test_bin(root, v, "style_goldens", None, "seam");
}

/// Leg 3 — the coverage bar (≥85% line across the canopy crates' libraries, as
/// gate-m55/m56/m57), via `cargo llvm-cov` when available, else the CI wiring is
/// verified.
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
                    &format!("coverage: canopy crates line {pct:.1}% < 85%"),
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

/// The last ~500 chars of a command's combined output (a compact evidence tail).
fn tail(out: &str) -> String {
    out.chars()
        .rev()
        .take(500)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

/// Build the `rift` binary (proves the removal + the layout model compile in).
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

/// Run every gate leg; returns human-readable violations (empty = pass).
///
/// # Errors
/// A setup failure (not a gate violation) as a string.
pub fn run(root: &Path) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    let _rift = build_rift(root)?;
    leg_removal(root, &mut v);
    leg_seam(root, &mut v);
    let coverage = leg_coverage(root, &mut v);
    if v.is_empty() {
        println!("gate-m58 evidence:\n{coverage}");
    }
    Ok(v)
}

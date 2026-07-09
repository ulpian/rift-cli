//! `xtask gate-m59` — the scripted M5.9 gate (PRD-M5.9 "Testing Decisions").
//!
//! Three legs, per the PRD's gate text (the panel editor — ADRs 0034/0035):
//!
//! 1. **Editor** — the header toggle and the `e` key enter `Mode::PanelEditor`;
//!    the keymap-derivation proof covers the editor context (every hinted key is
//!    live); grips and column dividers resolve where they are drawn (pixels ==
//!    targets); and the scoped mouse drag holds the idle invariant (normal-mode
//!    Drag/Up are inert and non-dirtying).
//! 2. **Persistence** — the `LayoutModel` codec round-trips (a malformed/forward
//!    blob decodes to the default); the canopy store appends a `canopy_layout_event`
//!    and folds the latest (a reset supersedes to the default); the layout table
//!    carries its append-only guards and the DDL snapshot matches.
//! 3. **Coverage bars** — ≥85% line across the canopy surface (rift-canopy +
//!    rift-canopy-data libraries), via `cargo llvm-cov` when available (recorded),
//!    else the CI wiring is verified.
//!
//! Run `cargo run -p xtask -- gate-m59`. The earlier gates stay intact.

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

/// The `N` in `test result: ok. N passed` (0 when absent).
fn passed_count(out: &str) -> usize {
    out.lines()
        .filter_map(|l| l.split_once("test result: ok. "))
        .filter_map(|(_, rest)| rest.split_once(" passed"))
        .filter_map(|(n, _)| n.trim().parse::<usize>().ok())
        .sum()
}

/// Run `--lib` test-name filters in package `pkg` (OR semantics) with
/// `INSTA_UPDATE=no`, asserting green **and** that at least one test matched.
fn require_lib(root: &Path, v: &mut Vec<String>, pkg: &str, filters: &[&str], label: &str) {
    let mut full = vec!["test", "-p", pkg, "--lib", "--"];
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
        &format!("{label}: {pkg} tests failed:\n{}", tail(&out)),
    );
    violation(
        v,
        passed_count(&out) > 0,
        &format!("{label}: filters {filters:?} matched no tests in {pkg} (filter drift)"),
    );
}

/// Leg 1 — the editor mode, grips, and the scoped-drag idle invariant.
fn leg_editor(root: &Path, v: &mut Vec<String>) {
    require_lib(
        root,
        v,
        "rift-canopy",
        &[
            // The keymap-derivation proof (now covers HintContext::PanelEditor).
            "every_hint_key_is_handled_by_the_reducer",
            // Grips/dividers resolve where drawn (pixels == targets).
            "panel_editor_grips_and_dividers_resolve_where_drawn",
            // The scoped-drag idle invariant + the drag/drop path.
            "normal_mode_drag_and_up_are_inert_and_do_not_dirty",
            "edit_mode_drag_without_a_live_pickup_is_inert",
            "a_grip_press_drag_release_moves_a_unit",
            // The pure reposition ops (conserve the multiset, stay valid).
            "editor::",
        ],
        "editor",
    );
}

/// Leg 2 — the codec, the store round-trip/reset, and the table guards.
fn leg_persistence(root: &Path, v: &mut Vec<String>) {
    require_lib(
        root,
        v,
        "rift-canopy",
        &["codec_round_trips_the_default_and_rejects_garbage"],
        "persistence",
    );
    require_lib(
        root,
        v,
        "rift-canopy-data",
        &[
            "set_layout_appends_and_reads_back_the_latest_with_reset",
            "layout_for_unknown_path_is_a_noop",
        ],
        "persistence",
    );
    // The append-only guards on the new table + the frozen-v1 / additive-v2 split.
    require_lib(
        root,
        v,
        "rift-store",
        &[
            "canopy_migrations_apply_and_rerun_cleanly",
            "every_canopy_migration_is_additive",
        ],
        "persistence",
    );
    // The canopy DDL snapshot (now three tables) stays locked (INSTA_UPDATE=no).
    let (code, out) = run_cmd(
        "cargo",
        &[
            "test",
            "-p",
            "rift-store",
            "--test",
            "schema_snapshot",
            "canopy",
        ],
        root,
        &[("INSTA_UPDATE".into(), "no".into())],
    );
    violation(
        v,
        code == 0,
        &format!("persistence: canopy DDL snapshot drift:\n{}", tail(&out)),
    );
}

/// Leg 3 — the coverage bar (≥85% line across the canopy crates' libraries).
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

/// Build the `rift` binary (proves the editor + persistence compile in).
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
    leg_editor(root, &mut v);
    leg_persistence(root, &mut v);
    let coverage = leg_coverage(root, &mut v);
    if v.is_empty() {
        println!("gate-m59 evidence:\n{coverage}");
    }
    Ok(v)
}

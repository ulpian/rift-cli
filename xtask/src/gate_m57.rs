//! `xtask gate-m57` — the scripted M5.7 gate (PRD-M5.7 "Testing Decisions").
//!
//! Three legs, per the PRD's gate text (charts + scroll — ADRs 0030/0031):
//!
//! 1. **Axis** — the activity and token-usage charts render a Y-tick gutter
//!    (min/mid/max humanized), an X label row, and a legend key; the ticks total
//!    over the empty case (axis + a real `0` baseline); and every axis glyph
//!    lands outside the plot rect (labels are chrome, bars are data). The radial
//!    donut/gauge/nic carry no axis — their goldens prove it.
//! 2. **Scroll** — a track/thumb renders on each overflowing region (modal body,
//!    projects rail, col2 card stack, col3 panel stack) and NOT on a fitting one;
//!    the thumb derives from `(offset, viewport, content_len)` and its drawn cells
//!    equal its hit-region; the graphs offset moves content and clamps to the
//!    fully-expanded overflow ("j/k scroll" is live — the regression the old inert
//!    code fails).
//! 3. **Coverage bars** — ≥85% line across the canopy surface (rift-canopy +
//!    rift-canopy-data libraries, as gate-m55/m56), via `cargo llvm-cov` when
//!    available (recorded), else the CI wiring is verified.
//!
//! Run `cargo run -p xtask -- gate-m57`. The earlier gates stay intact.

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

/// Leg 1 — the axis frame on the two cartesian charts + the radial-has-no-axis
/// goldens.
fn leg_axis(root: &Path, v: &mut Vec<String>) {
    require_lib(
        root,
        v,
        &[
            "axis::",
            "a_quiet_week_still_draws_the_axis_and_a_zero_baseline",
            "activity_card_renders_bars_legend_and_placeholder",
        ],
        "axis",
    );
    // The axis + activity + token goldens; and the donut/gauge goldens that carry
    // no axis (radial widgets are excluded — ADR-0030).
    require_test_bin(root, v, "snapshots", Some("axis"), "axis");
    require_test_bin(root, v, "style_goldens", Some("donut"), "axis");
}

/// Leg 2 — the scrollbar primitive + the applied offsets + the region bars.
fn leg_scroll(root: &Path, v: &mut Vec<String>) {
    require_lib(
        root,
        v,
        &[
            "no_scrollbar_when_content_fits",
            "thumb_tracks_top_middle_bottom",
            "drawn_cells_equal_the_geometry",
            "scrollbar_degrades_on_ascii",
            "a_short_column_scrolls_the_expanded_panels",
            "scrolling_over_the_graphs_column_scrolls_its_viewport",
            "scroll_offset_always_in_range_after_resize",
            "the_graphs_scrollbar_resolves_where_it_is_drawn",
            "a_fitting_column_shows_no_scrollbar",
            "a_long_rail_shows_a_scrollbar",
        ],
        "scroll",
    );
    // The overflowing-modal-body golden shows the bar.
    require_test_bin(root, v, "snapshots", Some("overlay_truncated"), "scroll");
}

/// Leg 3 — the coverage bar (≥85% line across the canopy crates' libraries, as
/// gate-m55/m56), via `cargo llvm-cov` when available, else the CI wiring is
/// verified. Scoped to the library targets — deterministic (the FSEvents
/// integration test is flaky under llvm-cov and is covered by the CI matrix).
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

/// Build the `rift` binary (proves the charts + scroll compile into the bin).
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
    leg_axis(root, &mut v);
    leg_scroll(root, &mut v);
    let coverage = leg_coverage(root, &mut v);
    if v.is_empty() {
        println!("gate-m57 evidence:\n{coverage}");
    }
    Ok(v)
}

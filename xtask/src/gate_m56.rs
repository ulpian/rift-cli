//! `xtask gate-m56` — the scripted M5.6 gate (PRD-M5.6 "Testing Decisions").
//!
//! Five legs, per the PRD's gate text (the modal shell — ADRs 0026–0029):
//!
//! 1. **Geometry** — every modal is a centered 80/80 box clamped up to the
//!    readable minimum and degraded to near-full-screen on the narrow tiers, and
//!    the drawn close X cell equals its `OverlayClose` hit-region (the shared
//!    `modal_rect`/`modal_close_rect` geometry). Driven by the hit/view modal
//!    tests and the overlay goldens (`snapshots`).
//! 2. **Focus / close** — every `Overlay` variant's controls are reachable by
//!    Tab in a stable order, the X closes by click and by Tab-then-Enter, Esc
//!    closes every dismissable variant (Ctrl-C exits even from one), and the
//!    default focus is the confirm/close/primary control (ADR-0027).
//! 3. **Flash** — every clickable target arms the press-pulse on click *and* on
//!    keyboard activation, decays by `dt`, and snaps under reduced motion; the
//!    selection pulse stays distinct (ADR-0028). The `is_button` completeness
//!    test fails if a button lands without a flash.
//! 4. **Routing** — an error opens an `Alert`, a mutating command a confirm, a
//!    success a `Notice` that auto-dismisses by `dt`, and the first-run wizard a
//!    modal; the header/hint/command bars and splash are chrome, and the
//!    closed-grammar + no-spawn proofs re-run green (ADR-0029/0015/0021/0025).
//! 5. **Coverage bars** — ≥85% line across the canopy surface (rift-canopy +
//!    rift-canopy-data libraries, as gate-m55), computed via `cargo llvm-cov`
//!    when available (recorded), else the CI wiring is verified.
//!
//! Run `cargo run -p xtask -- gate-m56`. The earlier gates stay intact.

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
/// its filters actually matched tests (a typo'd filter that runs zero tests
/// still exits 0, which must not read as a pass).
fn passed_count(out: &str) -> usize {
    out.lines()
        .filter_map(|l| l.split_once("test result: ok. "))
        .filter_map(|(_, rest)| rest.split_once(" passed"))
        .filter_map(|(n, _)| n.trim().parse::<usize>().ok())
        .sum()
}

/// Run a set of `--lib` test-name filters (OR semantics) with `INSTA_UPDATE=no`,
/// asserting the run is green **and** matched at least one test (guards against
/// filter drift silently passing).
fn require_lib(root: &Path, v: &mut Vec<String>, filters: &[&str], label: &str) {
    let mut full = vec!["test", "-p", "rift-canopy", "--lib", "--"];
    full.extend_from_slice(filters);
    let (code, out) = run_cmd("cargo", &full, root, &[("INSTA_UPDATE".into(), "no".into())]);
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
fn require_test_bin(root: &Path, v: &mut Vec<String>, bin: &str, filter: Option<&str>, label: &str) {
    let mut full = vec!["test", "-p", "rift-canopy", "--test", bin];
    if let Some(f) = filter {
        full.push(f);
    }
    let (code, out) = run_cmd("cargo", &full, root, &[("INSTA_UPDATE".into(), "no".into())]);
    violation(
        v,
        code == 0,
        &format!("{label}: {bin} failed:\n{}", tail(&out)),
    );
}

/// Leg 1 — geometry: the shared centered-80/80 `modal_rect` + the close-X
/// pixels-equal-targets discipline, across the overlay family + the goldens.
fn leg_geometry(root: &Path, v: &mut Vec<String>) {
    require_lib(
        root,
        v,
        &[
            "modal_rect",
            "close_x",
            "overlay_owns_its_region",
            "results_overlay_has_no_buttons",
            "wizard_primary_button_resolves",
            "primary_rect_is_bottom_left",
        ],
        "geometry",
    );
    // The overlay goldens pin the inset box + the close X at real sizes.
    require_test_bin(root, v, "snapshots", Some("overlay"), "geometry");
}

/// Leg 2 — focus / close: the focus ring, the universal X/Esc close, and the
/// hard-quit escape hatch across every modal variant.
fn leg_focus_close(root: &Path, v: &mut Vec<String>) {
    require_lib(
        root,
        v,
        &[
            "tab_cycles_the_modal_focus_ring",
            "clicking_the_close_x_dismisses",
            "esc_dismisses_an_alert",
            "wizard_primary_enter",
            "ctrl_c_quits",
            "every_hint_key_is_handled",
        ],
        "focus/close",
    );
}

/// Leg 3 — flash: click + keyboard activation arm the press-pulse on every
/// button, decay by `dt`, snap under reduced motion; the selection pulse is
/// distinct.
fn leg_flash(root: &Path, v: &mut Vec<String>) {
    require_lib(
        root,
        v,
        &[
            "every_button_target_flashes",
            "keyboard_activation_of_a_modal_button",
            "a_button_press_flashes",
            "reduced_motion_snaps",
            "a_row_select_uses_the_selection_pulse",
            "a_pressed_retry_chip",
        ],
        "flash",
    );
}

/// Leg 4 — routing: error→Alert, mutating→confirm, success→Notice (auto-dismiss),
/// wizard→modal, plus the closed-grammar + no-spawn structural proofs.
fn leg_routing(root: &Path, v: &mut Vec<String>) {
    require_lib(
        root,
        v,
        &[
            "a_soft_miss_opens_a_warning_alert",
            "add_without_a_launch_cwd_opens_an_error_alert",
            "open_notice_arms",
            "open_alert_does_not_arm",
            "an_empty_registry_raises_the_wizard",
            "removing_the_last_project_re_raises",
            "the_wizard_primary_click",
            "severity",
            "unset_editor_becomes_an_alert",
            "add_project_effect_invokes_on_add",
        ],
        "routing",
    );
    // The closed-grammar + no-spawn proof (buttons are triggers, not capabilities).
    require_test_bin(root, v, "no_spawn", None, "routing");
}

/// Leg 5 — the coverage bar (≥85% line across the canopy surface), computed via
/// `cargo llvm-cov` when available, else the CI wiring is verified. Measured over
/// the **canopy crates' libraries** (rift-canopy + rift-canopy-data, as
/// gate-m55): rift-canopy's own view/hit/state code is exercised by both its
/// unit tests and rift-canopy-data's dashboard tests, so the single-crate
/// `--lib` figure blinds to the golden-driven surface. Scoped to the library
/// targets — deterministic (the FSEvents integration test is flaky under
/// llvm-cov's parallel instrumentation and is covered by the CI matrix).
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
            &format!("coverage: llvm-cov run failed (exit {code}): …{}", tail(&out)),
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

/// Build the `rift` binary (proves the modal shell compiles into the bin).
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
    leg_geometry(root, &mut v);
    leg_focus_close(root, &mut v);
    leg_flash(root, &mut v);
    leg_routing(root, &mut v);
    let coverage = leg_coverage(root, &mut v);
    if v.is_empty() {
        println!("gate-m56 evidence:\n{coverage}");
    }
    Ok(v)
}

//! `xtask gate-m512` — the scripted M5.12 gate (PRD-M5.12 "Testing Decisions").
//!
//! Five legs, per the PRD's gate text (the profile hub — ADRs 0044/0045/0046):
//!
//! 1. **Launch** — a store-backed launch renders the identity button from
//!    frame one (the regression class every `set_profiles`-direct test
//!    missed); the width sweep proves identity visibility is monotonic
//!    (shrink → drop whole — never a hidden band); a pinned launch shows the
//!    pin, wears its theme, and writes nothing; the session's first write
//!    carries the deferred selected-sync.
//! 2. **Store** — migration v5 adds `canopy_profile_startup_event` with
//!    generated guards (the DDL snapshot holds all eight tables); duplicate
//!    clones effective membership + arrangements + `Some` theme slots and
//!    never writes `selected`; the pin refuses dead profiles and launch
//!    resolution is a pure read; the editor's first default-profile edit
//!    materialises the catalogue; every `PersistOp` stays insert-only.
//! 3. **Suite** — hub browse / settings / members / naming / confirm work
//!    keyboard-first with total click parity (chips, `[ + new profile ]`,
//!    the close `[x]` on every stage — the ADR-0046 sweep); the picker
//!    targets its profile (persists always, re-themes only when active);
//!    every hinted key in every stage-aware context is live.
//! 4. **Palette** — `/profile duplicate` and `/profile startup` (incl. the
//!    bare unpin) parse and map onto the same ops; guards and misses stay
//!    soft alerts.
//! 5. **Coverage bars** — ≥85% line across the canopy surface (rift-canopy +
//!    rift-canopy-data libraries), via `cargo llvm-cov` when available
//!    (recorded), else the CI wiring is verified.
//!
//! Run `cargo run -p xtask -- gate-m512`. The earlier gates stay intact.

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

/// Leg 1 — launch: the store-backed regression vehicle, the monotonic width
/// sweep, and the pinned-session semantics (pure-read launch + deferred sync).
fn leg_launch(root: &Path, v: &mut Vec<String>) {
    require_lib(
        root,
        v,
        "rift-canopy-data",
        &["a_store_backed_launch_renders_the_identity_button_on_frame_one"],
        "launch",
    );
    require_lib(
        root,
        v,
        "rift-canopy",
        &[
            "identity_button_draws_exactly_on_its_span",
            "identity_visibility_is_monotonic_across_widths",
            "identity_name_shrinks_before_the_button_hides",
            "identity_clips_away_whole_on_a_narrow_header",
            "an_unwired_cluster_keeps_the_four_item_geometry",
            "a_pinned_launch_shows_the_pinned_profile_and_writes_nothing",
            "a_pinned_sessions_first_write_syncs_then_the_view_stays",
        ],
        "launch",
    );
}

/// Leg 2 — the store: migration v5 + guards + the eight-table DDL snapshot,
/// the duplicate clone semantics, the pin's write guards + pure-read launch
/// resolution, and the insert-only exhaustiveness proof.
fn leg_store(root: &Path, v: &mut Vec<String>) {
    require_lib(
        root,
        v,
        "rift-store",
        &[
            "canopy_migrations_apply_and_rerun_cleanly",
            "every_canopy_migration_is_additive",
            "every_canopy_table_rejects_update_and_delete",
        ],
        "store",
    );
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
        &format!("store: canopy DDL snapshot drift:\n{}", tail(&out)),
    );
    let snap = std::fs::read_to_string(
        root.join("crates/rift-store/tests/snapshots/schema_snapshot__canopy_projection_ddl.snap"),
    )
    .unwrap_or_default();
    violation(
        v,
        snap.contains("-- table canopy_profile_startup_event")
            && snap.contains("canopy_profile_startup_event_no_update")
            && snap.contains("canopy_profile_startup_event_no_delete"),
        "store: canopy_profile_startup_event (or its guards) missing from the DDL snapshot",
    );
    require_lib(
        root,
        v,
        "rift-canopy-data",
        &[
            "duplicate_clones_membership_layouts_and_theme_in_full",
            "duplicate_of_the_default_freezes_the_catalogue_and_legacy_layout_fallback",
            "duplicate_refuses_invalid_duplicate_names_and_dead_sources",
            "duplicate_and_source_diverge_freely_after_the_clone",
            "startup_pin_supersedes_unpins_and_refuses_dead_profiles",
            "launch_projects_resolves_a_live_pin_and_a_dead_pin_falls_back",
            "default_membership_edit_from_the_editor_materializes_first",
        ],
        "store",
    );
    require_lib(
        root,
        v,
        "rift-canopy",
        &[
            "persist_ops_are_insert_only",
            "startup_fold_is_latest_wins_and_null_unpins",
            "launch_resolution_prefers_a_live_pin_and_falls_back",
            "resolved_launch_profile_is_always_live",
        ],
        "store",
    );
}

/// Leg 3 — the suite: the hub-and-spoke grammar, total click parity (the
/// ADR-0046 close-affordance sweep included), the members editor through the
/// seam, the targeted picker, and hint liveness across the stage contexts.
fn leg_suite(root: &Path, v: &mut Vec<String>) {
    require_lib(
        root,
        v,
        "rift-canopy",
        &[
            "settings_opens_from_key_chip_and_enter_dispatches_rows",
            "duplicate_from_settings_names_then_emits_full_clone_op",
            "startup_toggle_pins_and_unpins",
            "delete_from_settings_confirms_then_returns_to_browse",
            "members_editor_removes_reorders_and_adds_for_a_non_active_profile",
            "members_chips_mirror_the_keyboard",
            "entering_members_loads_through_the_loader_and_lands_the_lists",
            "picker_for_a_non_active_profile_persists_without_retheming",
            "picker_close_x_returns_to_the_menu",
            "menu_close_x_closes_the_mode_not_an_overlay",
            "every_modal_surface_carries_the_close_affordance",
            "settings_chips_and_new_button_hit_their_drawn_cells",
            "settings_page_lists_rows_with_the_pin_state",
            "every_hint_key_is_handled_by_the_reducer",
        ],
        "suite",
    );
}

/// Leg 4 — the palette: the two new verbs parse (incl. the bare unpin) and
/// map onto the same ops; guards and misses stay soft alerts.
fn leg_palette(root: &Path, v: &mut Vec<String>) {
    require_lib(
        root,
        v,
        "rift-canopy",
        &[
            "profile_duplicate_and_startup_parse_including_bare_startup",
            "profile_palette_verbs_map_onto_the_menu_ops",
            "profile_palette_guards_and_misses_alert_softly",
        ],
        "palette",
    );
}

/// Leg 5 — the coverage bar (≥85% line across the canopy crates' libraries).
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

/// Build the `rift` binary (proves the hub + pin + verbs compile in).
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
    leg_launch(root, &mut v);
    leg_store(root, &mut v);
    leg_suite(root, &mut v);
    leg_palette(root, &mut v);
    let cov = leg_coverage(root, &mut v);
    println!("gate-m512 evidence:");
    if !cov.is_empty() {
        println!("{cov}");
    }
    Ok(v)
}

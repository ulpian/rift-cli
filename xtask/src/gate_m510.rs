//! `xtask gate-m510` — the scripted M5.10 gate (PRD-M5.10 "Testing Decisions").
//!
//! Three legs, per the PRD's gate text (the profile plane — ADRs 0036/0037):
//!
//! 1. **Profile plane** — the three canonical tables register with generated
//!    append-only guards and fold latest-wins; the default profile is seeded on
//!    first launch (a second launch appends nothing) and its rename is an
//!    ordinary superseding event; creating a profile copies the creating
//!    profile's project list; remove refuses the active and the last profile;
//!    the legacy per-project layout stays readable as the default profile's
//!    fallback; a switch swaps projects and arrangements and the active profile
//!    is restored on relaunch; the insert-only exhaustiveness proof covers the
//!    profile ops; the canopy DDL snapshot (six tables) stays locked.
//! 2. **Menu** — the identity button renders on the exact cells its hit target
//!    resolves (truncation and whole-clip included) and goes inert under an
//!    overlay; the button click and the `p` key enter `Mode::ProfileMenu`;
//!    switch/create/rename/delete work keyboard-first with an inline confirm;
//!    the `/profile` palette verbs map onto the same ops; every hinted menu key
//!    is proven live by the keymap-derivation test; a switch reloads the whole
//!    view through the one profile loader seam.
//! 3. **Coverage bars** — ≥85% line across the canopy surface (rift-canopy +
//!    rift-canopy-data libraries), via `cargo llvm-cov` when available
//!    (recorded), else the CI wiring is verified.
//!
//! Run `cargo run -p xtask -- gate-m510`. The earlier gates stay intact.

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

/// Leg 1 — the profile plane: tables + guards, folds, seed, copy-on-create,
/// removal guards, legacy fallback, and switch/relaunch restore.
fn leg_profile_plane(root: &Path, v: &mut Vec<String>) {
    // Migration v3 applies additively; guards generate; the registry stays whole.
    require_lib(
        root,
        v,
        "rift-store",
        &[
            "canopy_migrations_apply_and_rerun_cleanly",
            "every_canopy_migration_is_additive",
            "every_canopy_table_rejects_update_and_delete",
            "every_canopy_db_table_is_registered_or_infra",
        ],
        "profile-plane",
    );
    // The pure folds: latest-valid profiles, active-is-always-live, membership.
    require_lib(
        root,
        v,
        "rift-canopy",
        &["profile::", "persist_ops_are_insert_only"],
        "profile-plane",
    );
    // The store behaviors the PRD names, over the real append-only db.
    require_lib(
        root,
        v,
        "rift-canopy-data",
        &[
            "seed_happens_once_and_folds_to_an_active_default",
            "rename_the_default_is_an_ordinary_superseding_event",
            "create_copies_the_creating_profiles_effective_list",
            "create_refuses_invalid_and_duplicate_names",
            "remove_refuses_the_active_and_the_last_profile",
            "select_switches_the_view_and_survives_a_relaunch",
            "default_shows_the_catalogue_until_its_first_membership_edit",
            "default_falls_back_to_the_legacy_layout_and_other_profiles_do_not",
            "add_project_joins_an_explicit_view_but_not_the_catalogue_fallback",
            "profile_reorder_swaps_within_the_view_only",
        ],
        "profile-plane",
    );
    // The canopy DDL snapshot (now six tables) stays locked (INSTA_UPDATE=no).
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
        &format!("profile-plane: canopy DDL snapshot drift:\n{}", tail(&out)),
    );
    let snap = std::fs::read_to_string(
        root.join("crates/rift-store/tests/snapshots/schema_snapshot__canopy_projection_ddl.snap"),
    )
    .unwrap_or_default();
    for table in [
        "canopy_profile_event",
        "canopy_profile_project_event",
        "canopy_profile_layout_event",
    ] {
        violation(
            v,
            snap.contains(&format!("-- table {table}")),
            &format!("profile-plane: {table} missing from the DDL snapshot"),
        );
        violation(
            v,
            snap.contains(&format!("{table}_no_update"))
                && snap.contains(&format!("{table}_no_delete")),
            &format!("profile-plane: {table} guards missing from the DDL snapshot"),
        );
    }
}

/// Leg 2 — the menu: the identity span discipline, the mode entries, the
/// grammar (keyboard + palette), and hint liveness.
fn leg_menu(root: &Path, v: &mut Vec<String>) {
    require_lib(
        root,
        v,
        "rift-canopy",
        &[
            // The identity button: drawn cells == hit targets, truncation,
            // whole-clip, active wash, sanitisation, inert-under-overlay.
            "identity_button_draws_exactly_on_its_span",
            "identity_name_truncates_with_an_ellipsis_at_the_budget",
            "identity_clips_away_whole_on_a_narrow_header",
            "identity_hides_until_the_profile_plane_is_wired",
            "identity_reads_active_while_the_menu_is_open_and_flashes_on_press",
            "hostile_profile_names_are_sanitised_before_render",
            "identity_button_resolves_on_row_zero_and_goes_inert_under_an_overlay",
            // The menu mode: entries, browse/switch, naming, the inline confirm.
            "p_opens_the_menu_on_the_active_row_and_esc_closes",
            "menu_cursor_moves_and_clamps",
            "enter_in_browse_switches",
            "naming_create_validates_then_emits",
            "naming_rename_prefills_and_emits",
            "naming_esc_steps_back_to_browse",
            "delete_needs_the_inline_confirm_and_guards_refuse",
            "delete_refuses_the_last_profile",
            // Menu clicks: rows hit their drawn cells; identity click toggles.
            "profile_menu_rows_hit_their_drawn_cells_and_the_body_is_inert",
            "identity_click_toggles_the_menu_and_row_click_switches",
            // The state flows: one op + the whole-view reload per act.
            "menu_switch_emits_select_plus_reload_and_closes",
            "menu_create_rename_delete_emit_their_ops_with_reload",
            "wired_remove_and_reorder_are_profile_scoped",
            // Palette parity: /profile maps onto the same typed ops.
            "profile_palette_verbs_map_onto_the_menu_ops",
            "profile_palette_guards_and_misses_alert_softly",
            // Hint liveness: the derivation test covers HintContext::ProfileMenu.
            "every_hint_key_is_handled_by_the_reducer",
            // The switch-reload through the one loader seam.
            "a_profile_switch_reloads_the_whole_view_through_the_loader_seam",
        ],
        "menu",
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

/// Build the `rift` binary (proves the profile plane + menu compile in).
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
    leg_profile_plane(root, &mut v);
    leg_menu(root, &mut v);
    let coverage = leg_coverage(root, &mut v);
    if v.is_empty() {
        println!("gate-m510 evidence:\n{coverage}");
    }
    Ok(v)
}

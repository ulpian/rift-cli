//! `xtask gate-m511` — the scripted M5.11 gate (PRD-M5.11 "Testing Decisions").
//!
//! Three legs, per the PRD's gate text (profile theming — ADRs 0038/0039):
//!
//! 1. **Theme** — with no overrides every tier's render is byte-identical to
//!    Rift dark (the existing style goldens pass unchanged, `INSTA_UPDATE=no`);
//!    the five slots repaint their roles and semantic colours never move; theme
//!    events supersede per (profile, slot), NULL resets, and the theme restores
//!    on relaunch and on switch; the canopy DDL snapshot (seven tables) stays
//!    locked; and **the gate grep** fails on any bare themable colour constant
//!    referenced outside the theme module (the seam cannot silently re-leak —
//!    and the grep is itself tested on fixtures).
//! 2. **Picker** — the ten presets render as name + swatch (plus the reset
//!    row); keyboard and click both select; a selection applies live in the
//!    same frame; reset returns a slot to the default; every hinted picker key
//!    is live (the keymap-derivation proof).
//! 3. **Coverage bars** — ≥85% line across the canopy surface (rift-canopy +
//!    rift-canopy-data libraries), via `cargo llvm-cov` when available
//!    (recorded), else the CI wiring is verified.
//!
//! Run `cargo run -p xtask -- gate-m511`. The earlier gates stay intact.

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

// ---- the gate grep (ADR-0038 §4) ---------------------------------------------

/// The themable constants no draw module may name directly — every call site
/// asks the [`Theme`] handle for a role instead. Assembled at runtime so this
/// scanner can never match itself.
fn themable_needles() -> Vec<String> {
    [
        "SURFACE",
        "BORDER_IDLE",
        "BODY",
        "TOKEN_LO",
        "TOKEN_HI",
        "RECALL_CALLS",
    ]
    .iter()
    .map(|stem| match *stem {
        "SURFACE" => format!("{stem}_BG"),
        "BODY" => format!("{stem}_FG"),
        other => other.to_string(),
    })
    .collect()
}

/// The production half of a source file: everything before the first
/// `#[cfg(test)]` marker (tests may name constants to assert against them).
fn production_half(source: &str) -> &str {
    source
        .split(concat!("#[cfg", "(test)]"))
        .next()
        .unwrap_or(source)
}

/// Scan one file's production text for bare themable-constant references.
/// Returns `file:line` findings — the seam-leak evidence.
fn themable_leaks_in(name: &str, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, line) in production_half(source).lines().enumerate() {
        let code = line.split("//").next().unwrap_or(line);
        for needle in themable_needles() {
            if code.contains(&needle) {
                out.push(format!("{name}:{} names {needle}", i + 1));
            }
        }
    }
    out
}

/// Walk `rift-canopy/src` (the draw modules), skipping the theme module itself
/// (where the constants live) and the splash (exempt — it plays before the
/// profile context exists and stays Rift dark; ADR-0038 §3).
fn leg_grep(root: &Path, v: &mut Vec<String>) {
    let src = root.join("crates/rift-canopy/src");
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "splash") {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs")
                || path.file_name().is_some_and(|n| n == "theme.rs")
            {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let rel = path
                .strip_prefix(&src)
                .unwrap_or(&path)
                .display()
                .to_string();
            for leak in themable_leaks_in(&rel, &source) {
                violation(v, false, &format!("grep: bare themable constant — {leak}"));
            }
        }
    }
}

/// Leg 1 — the runtime theme: byte-identity, slots, fixed semantics, the
/// store's supersede/reset/restore, and the seam grep.
fn leg_theme(root: &Path, v: &mut Vec<String>) {
    // The theme handle's own proofs.
    require_lib(
        root,
        v,
        "rift-canopy",
        &[
            "no_overrides_resolve_to_the_rift_dark_constants",
            "every_slot_override_is_consulted_first",
            "semantic_colours_never_move_under_a_full_override",
            "hex_colours_round_trip_and_reject_garbage",
            "the_ten_presets_are_named_rgb_and_slot_keys_round_trip",
        ],
        "theme",
    );
    // Byte-identity + the overridden set + the floor-tier proof: the whole
    // style-golden suite must pass with updates OFF.
    let (code, out) = run_cmd(
        "cargo",
        &["test", "-p", "rift-canopy", "--test", "style_goldens"],
        root,
        &[("INSTA_UPDATE".into(), "no".into())],
    );
    violation(
        v,
        code == 0,
        &format!(
            "theme: style goldens drifted (byte-identity broken):\n{}",
            tail(&out)
        ),
    );
    // The store: supersede per (profile, slot), NULL reset, writer guards.
    require_lib(
        root,
        v,
        "rift-canopy-data",
        &[
            "theme_events_supersede_per_slot_and_fold_latest_wins",
            "theme_writer_refuses_unknown_slots_and_malformed_hex",
        ],
        "theme",
    );
    // Migration v4 + guards + the seven-table DDL snapshot.
    require_lib(
        root,
        v,
        "rift-store",
        &[
            "canopy_migrations_apply_and_rerun_cleanly",
            "every_canopy_migration_is_additive",
            "every_canopy_table_rejects_update_and_delete",
        ],
        "theme",
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
        &format!("theme: canopy DDL snapshot drift:\n{}", tail(&out)),
    );
    let snap = std::fs::read_to_string(
        root.join("crates/rift-store/tests/snapshots/schema_snapshot__canopy_projection_ddl.snap"),
    )
    .unwrap_or_default();
    violation(
        v,
        snap.contains("-- table canopy_profile_theme_event")
            && snap.contains("canopy_profile_theme_event_no_update")
            && snap.contains("canopy_profile_theme_event_no_delete"),
        "theme: canopy_profile_theme_event (or its guards) missing from the DDL snapshot",
    );
    // Restore on switch/relaunch: the loader-seam reload carries the theme.
    require_lib(
        root,
        v,
        "rift-canopy",
        &["a_profile_switch_reloads_the_whole_view_through_the_loader_seam"],
        "theme",
    );
    // The seam grep (ADR-0038 §4).
    leg_grep(root, v);
}

/// Leg 2 — the picker: name + swatch rows, keyboard + click selection, live
/// apply, reset, and hint liveness.
fn leg_picker(root: &Path, v: &mut Vec<String>) {
    require_lib(
        root,
        v,
        "rift-canopy",
        &[
            "picker_lists_presets_as_name_beside_a_swatch",
            "picker_slot_stage_shows_current_values",
            "c_in_the_menu_opens_the_picker_and_esc_steps_all_the_way_back",
            "picker_cursors_move_and_clamp_in_both_stages",
            "enter_on_a_preset_applies_and_the_reset_row_maps_to_none",
            "a_picker_selection_rethemes_the_same_frame_and_persists_one_event",
            "picker_clicks_mirror_the_keyboard",
            "colour_picker_rows_hit_their_drawn_cells",
            "every_hint_key_is_handled_by_the_reducer",
        ],
        "picker",
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

/// Build the `rift` binary (proves the theme + picker compile in).
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
    leg_theme(root, &mut v);
    leg_picker(root, &mut v);
    let coverage = leg_coverage(root, &mut v);
    if v.is_empty() {
        println!("gate-m511 evidence:\n{coverage}");
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The "grep is itself tested" proof (PRD-M5.11): a fixture with a bare
    /// themable constant outside the theme module fails the leg.
    #[test]
    fn a_bare_themable_constant_fails_the_grep() {
        let fixture = "fn draw() {\n    let bg = crate::theme::SURFACE_BG;\n}\n";
        let leaks = themable_leaks_in("view.rs", fixture);
        assert_eq!(leaks.len(), 1);
        assert!(leaks[0].contains("view.rs:2"));
    }

    #[test]
    fn a_role_accessor_passes_the_grep() {
        let fixture = "fn draw(theme: Theme) {\n    let bg = theme.surface_bg();\n}\n";
        assert!(themable_leaks_in("view.rs", fixture).is_empty());
    }

    #[test]
    fn test_half_references_are_exempt() {
        let fixture = concat!(
            "fn draw(theme: Theme) { let _ = theme.body(); }\n",
            "#[cfg",
            "(test)]\n",
            "mod tests {\n    use crate::theme::SURFACE_BG;\n}\n"
        );
        assert!(
            themable_leaks_in("bars.rs", fixture).is_empty(),
            "tests may name constants to assert against them"
        );
    }

    #[test]
    fn comment_only_references_are_exempt() {
        let fixture = "fn draw() {} // falls back to SURFACE_BG\n";
        assert!(themable_leaks_in("chrome.rs", fixture).is_empty());
    }

    #[test]
    fn the_live_tree_passes_the_grep() {
        // The real draw modules are clean TODAY — the leg holds on this repo.
        let root = crate::workspace_root();
        let mut v = Vec::new();
        leg_grep(&root, &mut v);
        assert!(v.is_empty(), "seam leaks: {v:?}");
    }
}

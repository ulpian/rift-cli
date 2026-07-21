//! `xtask gate-m516` — the destination-explorer gate (ADR-0052).
//!
//! Pins the four things that would rot silently: the ADR-0046 parity
//! registration (the hand-listed fixtures compile clean when a surface is
//! forgotten), the mirror discipline (every click is a key's twin, resolved
//! through the pure hit-test), the listing contract (one comparator, the cap
//! with its disclosure, folded errors, symlink classification), and the funnel
//! (select reuses `PersistOp::AddProject`; `/add` is untouched). Plus the
//! structural rule that keeps the seam honest — `rift-canopy` never reads the
//! filesystem. `INSTA_UPDATE=no` throughout: a gate never rewrites a golden.

use std::path::Path;
use std::process::{Command, Stdio};

fn violation(out: &mut Vec<String>, ok: bool, message: impl Into<String>) {
    if !ok {
        out.push(message.into());
    }
}

fn command(root: &Path, args: &[&str]) -> (i32, String) {
    let Ok(output) = Command::new("cargo")
        .args(args)
        .current_dir(root)
        .env("INSTA_UPDATE", "no")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    else {
        return (-1, String::new());
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.code().unwrap_or(-1), text)
}

fn tail(text: &str) -> String {
    text.chars()
        .rev()
        .take(700)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

/// Run one named library test, requiring that it matched **and** passed — the
/// anti-drift guard: a renamed test would otherwise pass as "0 tests run".
fn require_lib(root: &Path, violations: &mut Vec<String>, package: &str, filter: &str, leg: &str) {
    let args = vec!["test", "-p", package, "--lib", "--", filter, "--exact"];
    let (code, output) = command(root, &args);
    violation(
        violations,
        code == 0 && output.contains("1 passed"),
        format!(
            "{leg}: {package}::{filter} failed or drifted (renamed tests must be \
             re-pinned here, never silently dropped):\n{}",
            tail(&output)
        ),
    );
}

/// Parity (ADR-0046 §5 / ADR-0052 §8): the explorer is registered in every
/// hand-listed fixture, so forgetting the surface turns CI red instead of
/// leaving it unguarded.
fn leg_parity(root: &Path, violations: &mut Vec<String>) {
    for filter in [
        "state::tests::every_modal_surface_carries_the_close_affordance",
        "keymap::tests::every_hint_key_is_handled_by_the_reducer",
        "state::tests::every_button_target_flashes_on_click",
        "hit::tests::explorer_pixels_equal_targets_on_every_drawn_affordance",
        "view::tests::explorer_draws_the_open_folder_its_rows_and_both_affordances",
        "view::tests::explorer_chips_degrade_rather_than_vanish_on_narrow_tiers",
        "view::tests::the_dotfiles_toggle_has_a_drawn_chip_at_every_width_the_surface_draws",
    ] {
        require_lib(root, violations, "rift-canopy", filter, "parity");
    }
    // The hint table must cover exactly the 19 live contexts — the count is
    // part of the fixture, so a new mode cannot slip in unhinted. The
    // MEMBERSHIP is checked inside the `ALL` literal itself: a bare
    // `contains("HintContext::Explorer")` would be satisfied by the enum
    // declaration, the `context()` arm, or the `hints()` arm, none of which
    // is the fixture.
    let keymap =
        std::fs::read_to_string(root.join("crates/rift-canopy/src/keymap.rs")).unwrap_or_default();
    let all_list = keymap
        .split_once("const ALL: [HintContext; 19] = [")
        .and_then(|(_, rest)| rest.split_once("];"))
        .map(|(body, _)| body.to_owned());
    violation(
        violations,
        all_list
            .as_deref()
            .is_some_and(|body| body.contains("HintContext::Explorer")),
        "parity: the hint-liveness fixture is no longer a 19-context ALL list \
         containing HintContext::Explorer (a renamed count or a dropped \
         membership both leave the surface unguarded)",
    );
    // Every act the explorer's hint table teaches must have a pointer twin
    // (ADR-0046, PRD story 8) — the direction the liveness test cannot see.
    let hit =
        std::fs::read_to_string(root.join("crates/rift-canopy/src/hit.rs")).unwrap_or_default();
    for target in [
        "ExplorerRow",
        "ExplorerUp",
        "ExplorerSelect",
        "ExplorerTypePath",
        "ExplorerDotfiles",
        "ExplorerClose",
    ] {
        violation(
            violations,
            hit.contains(&format!("Target::{target}")) || hit.contains(&format!("    {target}")),
            format!(
                "parity: {target} left the target set — an act the explorer's \
                 hints teach would have no click twin"
            ),
        );
    }
}

/// Mirrors (ADR-0046): row-click ≡ Enter, up ≡ ⌫, select ≡ `s`, type-path ≡
/// `i`, dotfiles ≡ `.`, X ≡ Esc — proven through the pure hit-test, plus the
/// wheel's clamps and the scrolled row mapping (ADR-0052 §7). Every act the
/// hint table teaches is in here; that is what makes the leg a parity proof
/// rather than a sample.
fn leg_mirrors(root: &Path, violations: &mut Vec<String>) {
    for filter in [
        "state::tests::explorer_clicks_mirror_their_keys",
        "state::tests::a_file_row_click_only_moves_the_cursor",
        "state::tests::the_explorer_wheel_clamps_at_both_ends_and_yields_to_an_overlay",
        "state::tests::keyboard_movement_follows_the_cursor_without_stealing_a_free_offset",
        "hit::tests::a_scrolled_explorer_maps_the_viewport_row_back_to_its_visible_index",
        "hit::tests::the_explorer_scrollbar_resolves_where_it_is_drawn_and_only_on_overflow",
    ] {
        require_lib(root, violations, "rift-canopy", filter, "mirrors");
    }
}

/// The listing contract (ADR-0052 §3): total, deterministic, bounded, honest —
/// and sorted by the ONE exported comparator, so the walker and the state can
/// never drift apart.
fn leg_listing(root: &Path, violations: &mut Vec<String>) {
    for filter in [
        "dirlist::tests::a_mixed_directory_lists_dirs_first_and_keeps_dot_entries",
        "dirlist::tests::an_unreadable_path_folds_into_the_error_and_never_throws",
        "dirlist::tests::a_huge_directory_is_capped_and_discloses_the_overflow",
    ] {
        require_lib(root, violations, "rift-canopy-data", filter, "listing");
    }
    // Symlink classification is only expressible where symlinks are (the two
    // fixtures are `#[cfg(unix)]`); on Windows the leg simply has less to pin.
    if cfg!(unix) {
        for filter in [
            "dirlist::tests::a_symlink_to_a_directory_counts_as_a_directory",
            "dirlist::tests::a_broken_symlink_is_listed_as_a_plain_row_never_dropped",
        ] {
            require_lib(root, violations, "rift-canopy-data", filter, "listing");
        }
    }
    for filter in [
        "explorer::tests::sort_puts_directories_first_then_case_insensitive_names",
        "explorer::tests::sort_tie_breaks_on_the_exact_name_deterministically",
        "explorer::tests::apply_failure_sets_only_the_error_and_stays_put",
        "explorer::tests::visible_filters_dot_entries_without_dropping_them",
    ] {
        require_lib(root, violations, "rift-canopy", filter, "listing");
    }
}

/// The seam's structural rules (ADR-0052 §2/§3, ADR-0009): the render crate
/// performs no filesystem I/O, the walker lives in the data plane, and there is
/// exactly one comparator.
fn leg_seam(root: &Path, violations: &mut Vec<String>) {
    // `rift-canopy` never reads a directory: the explorer requests, the loop
    // walks. A `read_dir` anywhere in the render crate means the seam was
    // bypassed and reducer purity is gone.
    let mut offenders = Vec::new();
    let mut stack = vec![root.join("crates/rift-canopy/src")];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs")
                && std::fs::read_to_string(&path)
                    .unwrap_or_default()
                    .contains("read_dir")
            {
                offenders.push(path.display().to_string());
            }
        }
    }
    violation(
        violations,
        offenders.is_empty(),
        format!(
            "seam: rift-canopy reads the filesystem (the reducer must only \
             request through Effects.list_dir): {}",
            offenders.join(", ")
        ),
    );
    let dirlist = std::fs::read_to_string(root.join("crates/rift-canopy-data/src/dirlist.rs"))
        .unwrap_or_default();
    violation(
        violations,
        dirlist.contains("sort_entries(&mut entries)") && dirlist.contains("MAX_ENTRIES"),
        "seam: the walker no longer sorts with the exported comparator / the \
         shared cap (two orderings can then drift across the seam)",
    );
    violation(
        violations,
        !dirlist.contains(".sort_by(") && !dirlist.contains(".sort("),
        "seam: the walker grew its own sort — there is exactly one comparator, \
         exported from rift-canopy::explorer",
    );
    let explorer = std::fs::read_to_string(root.join("crates/rift-canopy/src/explorer.rs"))
        .unwrap_or_default();
    violation(
        violations,
        explorer.contains("pub fn sort_entries") && explorer.contains("pub const MAX_ENTRIES"),
        "seam: the one comparator / cap left the explorer module",
    );
    // The mode's close X is its own target, never the overlay plane's.
    let view =
        std::fs::read_to_string(root.join("crates/rift-canopy/src/view.rs")).unwrap_or_default();
    violation(
        violations,
        view.contains("Target::ExplorerClose"),
        "seam: the explorer's close X no longer routes to the MODE's own target \
         (ADR-0046: never Target::OverlayClose)",
    );
}

/// The funnel (ADR-0052 §5/§6): select reuses the typed flow's own action, all
/// three affordances arrive through one door, and `/add` is untouched.
fn leg_funnel(root: &Path, violations: &mut Vec<String>) {
    for filter in [
        "state::tests::selecting_a_destination_registers_it_and_add_is_untouched",
        "state::tests::every_add_project_affordance_arrives_through_the_one_door",
        "state::tests::an_explorer_list_action_folds_to_the_lister_seam",
        "input::tests::select_registers_the_open_folder_and_returns_to_normal_first",
        "input::tests::entering_the_explorer_seeds_home_then_the_launch_cwd_then_root",
    ] {
        require_lib(root, violations, "rift-canopy", filter, "funnel");
    }
    for filter in [
        "app::tests::the_lister_seam_is_invoked_once_and_lands_before_the_next_frame",
        "app::tests::a_failed_listing_lands_as_an_inline_error_through_the_loop",
    ] {
        require_lib(root, violations, "rift-canopy", filter, "funnel");
    }
}

fn leg_coverage(root: &Path, violations: &mut Vec<String>) -> String {
    if command(root, &["llvm-cov", "--version"]).0 != 0 {
        let ci = std::fs::read_to_string(root.join(".github/workflows/ci.yml")).unwrap_or_default();
        violation(
            violations,
            ci.contains("check-coverage"),
            "coverage: CI coverage enforcement is missing",
        );
        return "  coverage: enforced in CI (local cargo-llvm-cov unavailable)".into();
    }
    let mut evidence = Vec::new();
    for package in ["rift-canopy", "rift-canopy-data"] {
        // ALL targets, like CI's `cargo llvm-cov --workspace --all-features`.
        // Measuring `--lib` alone would report `rift-canopy` at ~76% purely
        // because its three integration-test files (snapshots, style goldens,
        // no-spawn) exercise a large slice of the render layer — a gate must
        // use the same instrument as the threshold it enforces.
        let args = vec![
            "llvm-cov",
            "--summary-only",
            "--json",
            "--all-features",
            "-p",
            package,
        ];
        let (code, output) = command(root, &args);
        if code != 0 {
            violation(
                violations,
                false,
                format!("coverage: {package} llvm-cov failed:\n{}", tail(&output)),
            );
            continue;
        }
        let json = output.get(output.find('{').unwrap_or(0)..).unwrap_or("{}");
        let pct = serde_json::Deserializer::from_str(json)
            .into_iter::<serde_json::Value>()
            .next()
            .and_then(Result::ok)
            .and_then(|doc| {
                doc.pointer("/data/0/totals/lines/percent")
                    .and_then(serde_json::Value::as_f64)
            })
            .unwrap_or(0.0);
        violation(
            violations,
            pct >= 85.0,
            format!("coverage: {package} line coverage {pct:.1}% < 85%"),
        );
        evidence.push(format!("  measured line coverage ({package}): {pct:.1}%"));
    }
    evidence.join("\n")
}

/// Run the five M5.16 gate legs.
///
/// # Errors
/// Returns an error only when gate setup itself cannot be completed; ordinary
/// test, structural, or coverage failures are returned as violations.
pub fn run(root: &Path) -> Result<Vec<String>, String> {
    let mut violations = Vec::new();
    leg_parity(root, &mut violations);
    leg_mirrors(root, &mut violations);
    leg_listing(root, &mut violations);
    leg_seam(root, &mut violations);
    leg_funnel(root, &mut violations);
    let coverage = leg_coverage(root, &mut violations);
    println!("gate-m516 evidence:");
    if !coverage.is_empty() {
        println!("{coverage}");
    }
    Ok(violations)
}

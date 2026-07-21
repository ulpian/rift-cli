//! `xtask gate-m515` — the Recall savings-in-dollars gate (ADR-0051).
//!
//! Pins the honesty contract so it cannot quietly regress: the tilde is
//! always present, a positive saving never renders as zero, an unpriced
//! model produces no amount, an unresolvable basis produces no chip, money
//! stays integer micro-USD behind the one authority, the breakdown's slices
//! and subtotals compose the headline exactly, and the chip's drawn cells
//! are its click target (one solver). `INSTA_UPDATE=no` throughout — a gate
//! never rewrites a golden.

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

fn require_test(
    root: &Path,
    violations: &mut Vec<String>,
    package: &str,
    target: &[&str],
    filter: &str,
    leg: &str,
) {
    let mut args = vec!["test", "-p", package];
    args.extend_from_slice(target);
    args.extend_from_slice(&["--", filter, "--exact"]);
    let (code, output) = command(root, &args);
    violation(
        violations,
        code == 0 && output.contains("1 passed"),
        format!(
            "{leg}: {package}::{filter} failed or drifted:\n{}",
            tail(&output)
        ),
    );
}

/// Honesty (ADR-0051 §3/§5): the tilde is unconditional, a positive saving
/// never renders as zero, absence stays absent at every rung, money never
/// animates, and the modal discloses the whole basis.
fn leg_honesty(root: &Path, violations: &mut Vec<String>) {
    for filter in [
        "tests::money_always_reads_as_an_estimate",
        "tests::a_positive_saving_never_renders_as_zero",
        "tests::rendering_is_marked_and_never_fakes_a_zero",
        "tests::an_unpriced_model_yields_none_never_zero",
        "tests::exact_rendering_quotes_the_catalog_without_the_tilde",
    ] {
        require_test(
            root,
            violations,
            "rift-shadow-cost",
            &["--lib"],
            filter,
            "honesty",
        );
    }
    require_test(
        root,
        violations,
        "rift-canopy-data",
        &["--lib"],
        "feeds::tests::usd_absent_when_nothing_is_priced_or_no_capture",
        "honesty",
    );
    for filter in [
        "col3::tests::usd_chip_absent_without_an_honest_figure_or_room",
        "col3::tests::money_is_a_headline_never_tweened",
        "col3::tests::breakdown_lines_disclose_the_whole_basis",
        "state::tests::clicking_the_cost_chip_opens_the_breakdown_modal",
        "state::tests::a_stray_cost_click_without_a_figure_opens_nothing",
    ] {
        require_test(
            root,
            violations,
            "rift-canopy",
            &["--lib"],
            filter,
            "honesty",
        );
    }
}

/// Money (ADR-0051 §4): integer micro-USD behind the one authority — the
/// structural shape, not just behavior.
fn leg_money(root: &Path, violations: &mut Vec<String>) {
    let cost = std::fs::read_to_string(root.join("crates/rift-shadow-cost/src/lib.rs"))
        .unwrap_or_default();
    violation(
        violations,
        cost.contains("pub fn avoided_tokens_micros")
            && cost.contains("pub fn partition_largest_remainder")
            && cost.contains("pub fn fmt_usd_estimate")
            && cost.contains("saturating_mul"),
        "money: the single conversion/partition/format authority drifted out of rift-shadow-cost",
    );
    let feeds = std::fs::read_to_string(root.join("crates/rift-canopy-data/src/feeds.rs"))
        .unwrap_or_default();
    violation(
        violations,
        feeds.contains("avoided_tokens_micros(")
            && feeds.contains("partition_largest_remainder(")
            && feeds.contains("effective_prices("),
        "money: the data plane no longer routes through the money authority + \
         the production price resolver",
    );
    violation(
        violations,
        !feeds.contains("price_for("),
        "money: the dead in-memory PriceTable is being consulted (a second \
         price authority)",
    );
    // The render layer never formats or computes money: labels arrive
    // pre-rendered from the data plane.
    let col3 =
        std::fs::read_to_string(root.join("crates/rift-canopy/src/col3.rs")).unwrap_or_default();
    let state = std::fs::read_to_string(root.join("crates/rift-canopy/src/state/mod.rs"))
        .unwrap_or_default();
    violation(
        violations,
        !col3.contains("fmt_usd_estimate(") && !state.contains("fmt_usd_estimate("),
        "money: the render layer is formatting money (labels must be \
         pre-rendered by the authority in the data plane)",
    );
    require_test(
        root,
        violations,
        "rift-shadow-cost",
        &["--lib"],
        "tests::pathological_token_counts_saturate_rather_than_wrap",
        "money",
    );
}

/// Arithmetic (ADR-0051 §2): slices sum to the avoided total, subtotals
/// compose the headline, the money multiplies the NET figure.
fn leg_arithmetic(root: &Path, violations: &mut Vec<String>) {
    for filter in [
        "tests::partition_sums_to_the_total_and_is_deterministic",
        "tests::partition_always_sums_to_the_total",
        "tests::breakdown_rows_account_for_every_token",
    ] {
        require_test(
            root,
            violations,
            "rift-shadow-cost",
            &["--lib"],
            filter,
            "arithmetic",
        );
    }
    for filter in [
        "feeds::tests::usd_full_mix_composes_the_headline",
        "feeds::tests::usd_partial_coverage_prices_what_it_can",
        "feeds::tests::usd_no_mix_falls_back_to_most_frequent_model",
        "feeds::tests::usd_values_net_avoided_not_gross",
        "feeds::tests::usd_breakdown_ladder_edges",
    ] {
        require_test(
            root,
            violations,
            "rift-canopy-data",
            &["--lib"],
            filter,
            "arithmetic",
        );
    }
}

/// Parity (ADR-0051 §7 / ADR-0046): one solver feeds the draw and the
/// hit-test, the golden shows the chip, and the modal-surface fixtures the
/// Results reuse leans on still pass.
fn leg_parity(root: &Path, violations: &mut Vec<String>) {
    for filter in [
        "col3::tests::usd_chip_draws_exactly_where_the_shared_solver_says",
        "hit::tests::recall_cost_chip_wins_over_its_section_exactly_on_its_cells",
        "keymap::tests::every_hint_key_is_handled_by_the_reducer",
        "state::tests::every_modal_surface_carries_the_close_affordance",
    ] {
        require_test(
            root,
            violations,
            "rift-canopy",
            &["--lib"],
            filter,
            "parity",
        );
    }
    require_test(
        root,
        violations,
        "rift-canopy",
        &["--test", "snapshots"],
        "golden_recall_usd_chip",
        "parity",
    );
}

/// Seed (ADR-0051 §8): the models the toolchain runs are priced — migration
/// v8's append is present, bitemporal, and leaves the M1 rows untouched.
fn leg_seed(root: &Path, violations: &mut Vec<String>) {
    require_test(
        root,
        violations,
        "rift-store",
        &["--lib"],
        "migrate::tests::current_model_prices_are_seeded_and_bitemporal",
        "seed",
    );
    require_test(
        root,
        violations,
        "rift-store",
        &["--lib"],
        "migrate::tests::shipped_capture_migrations_are_byte_stable",
        "seed",
    );
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
    let groups: [(&str, &[&str]); 2] = [
        ("money authority", &["rift-shadow-cost"]),
        ("savings data plane", &["rift-canopy-data"]),
    ];
    let mut evidence = Vec::new();
    for (label, packages) in groups {
        let mut args = vec!["llvm-cov", "--summary-only", "--json", "--lib"];
        for package in packages {
            args.extend_from_slice(&["-p", package]);
        }
        let (code, output) = command(root, &args);
        if code != 0 {
            violation(
                violations,
                false,
                format!("coverage: {label} llvm-cov failed:\n{}", tail(&output)),
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
            format!("coverage: {label} line coverage {pct:.1}% < 85%"),
        );
        evidence.push(format!("  measured line coverage ({label}): {pct:.1}%"));
    }
    evidence.join("\n")
}

/// Run the five M5.15 gate legs.
///
/// # Errors
/// Returns an error only when gate setup itself cannot be completed; ordinary
/// test, structural, or coverage failures are returned as violations.
pub fn run(root: &Path) -> Result<Vec<String>, String> {
    let mut violations = Vec::new();
    leg_honesty(root, &mut violations);
    leg_money(root, &mut violations);
    leg_arithmetic(root, &mut violations);
    leg_parity(root, &mut violations);
    leg_seed(root, &mut violations);
    let coverage = leg_coverage(root, &mut violations);
    println!("gate-m515 evidence:");
    if !coverage.is_empty() {
        println!("{coverage}");
    }
    Ok(violations)
}

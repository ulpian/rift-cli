//! `xtask gate-m514` — the scripted Truthful Canopy System-panel gate.

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

fn leg_identity(root: &Path, violations: &mut Vec<String>) {
    require_test(
        root,
        violations,
        "rift-core",
        &["--lib"],
        "id::tests::capture_session_identity_derivation_is_pinned",
        "identity",
    );
    require_test(
        root,
        violations,
        "rift-store",
        &["--lib"],
        "migrate::tests::v7_identity_backfill_derives_production_session_id_append_only",
        "identity",
    );
    require_test(
        root,
        violations,
        "rift-store",
        &["--test", "immutability"],
        "every_db_table_is_registered_or_infra",
        "identity",
    );
    require_test(
        root,
        violations,
        "rift-canopy-data",
        &["--lib"],
        "feeds::tests::unresolved_live_identity_never_falls_back",
        "identity",
    );
}

fn leg_observation(root: &Path, violations: &mut Vec<String>) {
    require_test(
        root,
        violations,
        "rift-agent-claude-code",
        &["--lib"],
        "tests::main_context_extraction_is_opt_in_and_separate",
        "observation",
    );
    require_test(
        root,
        violations,
        "rift-shadow",
        &["--lib"],
        "project::tests::main_context_is_append_only_and_separate_from_turn_spend",
        "observation",
    );
    require_test(
        root,
        violations,
        "rift",
        &["--bins"],
        "hooks::tests::claude_transcript_enrichment_separates_context_from_turn_spend",
        "observation",
    );
}

fn leg_feed(root: &Path, violations: &mut Vec<String>) {
    for filter in [
        "feeds::tests::system_uses_main_context_not_huge_aggregate_spend",
        "feeds::tests::system_aggregates_all_live_sessions_and_counts_terminal_invocations",
        "feeds::tests::system_unknown_model_shows_used_without_percentage",
        "feeds::tests::system_holds_completed_context_when_current_session_has_no_sample",
    ] {
        require_test(
            root,
            violations,
            "rift-canopy-data",
            &["--lib"],
            filter,
            "feed",
        );
    }
    require_test(
        root,
        violations,
        "rift-canopy-data",
        &["--test", "live_sections"],
        "system_counts_hold_while_idle_and_move_on_capture",
        "feed",
    );
}

fn leg_render(root: &Path, violations: &mut Vec<String>) {
    for filter in [
        "col3::tests::system_panel_donut_bands_and_unknown_indicator",
        "col3::tests::system_panel_hints_the_activity_scope",
        "col3::tests::system_metrics_render_placeholders_for_absent_sources",
        "col3::tests::active_session_badge_pluralizes_honestly",
    ] {
        require_test(
            root,
            violations,
            "rift-canopy",
            &["--lib"],
            filter,
            "render",
        );
    }
    require_test(
        root,
        violations,
        "rift-canopy",
        &["--test", "snapshots"],
        "golden_system_scope_and_responsive_states",
        "render",
    );

    let col3 =
        std::fs::read_to_string(root.join("crates/rift-canopy/src/col3.rs")).unwrap_or_default();
    violation(
        violations,
        ![
            "Smart",
            "Capable",
            "Thinking",
            "Strained",
            "Dumb",
            "session · idle",
        ]
        .iter()
        .any(|copy| col3.contains(copy)),
        "render: legacy intelligence/idle copy remains in col3",
    );
    let feeds = std::fs::read_to_string(root.join("crates/rift-canopy-data/src/feeds.rs"))
        .unwrap_or_default();
    violation(
        violations,
        feeds.contains("FROM main_context_observation")
            && feeds.contains("FROM memd_call_event")
            && feeds.contains("pub fn compute_system"),
        "render: System feed drifted from observation/call-event authorities",
    );
    let state = std::fs::read_to_string(root.join("crates/rift-canopy/src/state/mod.rs"))
        .unwrap_or_default();
    violation(
        violations,
        state.contains("System {")
            && !state.contains("FeedPayload::Nic(")
            && !state.contains("FeedPayload::Metrics("),
        "render: independent Nic/Metrics payloads reappeared",
    );
    violation(
        violations,
        !col3.contains("ThreadsView")
            && !col3.contains("capture_per_min")
            && !col3.contains("tokens_per_min")
            && !col3.contains("recall_per_hour"),
        "render: capture-session thread or legacy session-rate model reappeared",
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
        (
            "System panel/data plane",
            &["rift-canopy", "rift-canopy-data"],
        ),
        (
            "adapter/context gauge",
            &["rift-agent-claude-code", "rift-nic-gauge"],
        ),
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

/// Run the five M5.14 gate legs.
///
/// # Errors
/// Returns an error only when gate setup itself cannot be completed; ordinary
/// test, structural, or coverage failures are returned as violations.
pub fn run(root: &Path) -> Result<Vec<String>, String> {
    let mut violations = Vec::new();
    leg_identity(root, &mut violations);
    leg_observation(root, &mut violations);
    leg_feed(root, &mut violations);
    leg_render(root, &mut violations);
    let coverage = leg_coverage(root, &mut violations);
    println!("gate-m514 evidence:");
    if !coverage.is_empty() {
        println!("{coverage}");
    }
    Ok(violations)
}

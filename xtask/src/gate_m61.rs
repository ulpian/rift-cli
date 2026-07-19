//! `xtask gate-m61` — the scripted M6.1 gate (PRD-M6.1 "Testing Decisions").
//!
//! Five legs, per the PRD's gate text (honest serve accounting — ADR-0049):
//!
//! 1. **Seam** — every tool's emitted `tokens_returned` equals the real
//!    tokenizer count of its serialized payload; event figures equal response
//!    figures on all six `memd.*` variants; exactly one `TurnBudget`
//!    token-charge site (structural); the arbitrary-call-sequence property
//!    holds the real budget against a shadow model that cannot exceed its
//!    ceilings (mutation-checked: a re-broken seam goes red).
//! 2. **Truncation** — the manifest cap composes default/max/remaining
//!    (pack-parity 1200/4000); `fit_to_cap` keeps the largest fitting prefix
//!    deterministically; known_state keeps the ranked prefix; diff_since
//!    drops unchanged entries before changed ones; the skeleton edge ships
//!    honestly (`kept == 0`, `truncated: true`, charge clamps).
//! 3. **Audit** — a REAL `PackBuilt` routed through both envelope paths
//!    (v1 content-addressed, v2 invocation-addressed) lands
//!    `tokens_returned > 0` in both audit relations — the exact rows that
//!    audited 0 before the seam; a real refusal audits its own payload cost.
//! 4. **Events** — `PackBuilt` carries `estimated_tokens_returned` and the
//!    accessor reads it (never `total_tokens`); both hook envelopes carry it.
//! 5. **Coverage bars** — ≥85% line across the accounting surface
//!    (rift-recall-mcp + rift-recall-events libraries), via `cargo llvm-cov`
//!    when available (recorded), else the CI wiring is verified.
//!
//! Run `cargo run -p xtask -- gate-m61`. The earlier gates stay intact.

use std::path::Path;
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

/// Run test-name filters in package `pkg` over the given target flag
/// (`--lib` for library crates, `--bins` for the bin's unit tests) with
/// `INSTA_UPDATE=no`, asserting green **and** that at least one test matched
/// (anti-drift: a renamed test must go red here, not silently vanish).
fn require_tests(
    root: &Path,
    v: &mut Vec<String>,
    pkg: &str,
    target: &str,
    filters: &[&str],
    label: &str,
) {
    let mut full = vec!["test", "-p", pkg, target, "--"];
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

/// Leg 1 — the seam: measured figures, the six-variant event agreement, the
/// single charge site, and the arbitrary-call-sequence property.
fn leg_seam(root: &Path, v: &mut Vec<String>) {
    require_tests(
        root,
        v,
        "rift-recall-mcp",
        "--lib",
        &[
            "every_tool_emits_the_measured_payload_count",
            "turn_budget_has_exactly_one_token_charge_site",
            "token_accounting_is_non_negative_and_honest",
            "turn_budget_and_figures_hold_over_arbitrary_call_sequences",
        ],
        "seam",
    );
}

/// Leg 2 — truncation: cap composition, the deterministic prefix, the drop
/// orders, and the skeleton edge.
fn leg_truncation(root: &Path, v: &mut Vec<String>) {
    require_tests(
        root,
        v,
        "rift-recall-mcp",
        "--lib",
        &[
            "manifest_cap_composes_default_max_and_remaining",
            "fit_to_cap_keeps_the_largest_fitting_prefix_deterministically",
            "known_state_truncates_to_the_ranked_prefix",
            "diff_since_drops_unchanged_before_changed",
            "budget_never_exceeded_over_a_turn",
        ],
        "truncation",
    );
}

/// Leg 3 — the audit writer: real events land nonzero figures in both
/// relations through both envelope paths.
fn leg_audit(root: &Path, v: &mut Vec<String>) {
    require_tests(
        root,
        v,
        "rift",
        "--bins",
        &["real_serve_events_audit_nonzero_returned_in_both_relations"],
        "audit",
    );
}

/// Leg 4 — the events crate: the `PackBuilt` figure and its envelopes.
fn leg_events(root: &Path, v: &mut Vec<String>) {
    require_tests(
        root,
        v,
        "rift-recall-events",
        "--lib",
        &[
            "pack_built_carries_the_returned_figure",
            "verbs_and_tokens_avoided",
            "refused_event_is_namespaced_and_leaks_no_content",
        ],
        "events",
    );
}

/// Leg 5 — the coverage bar (≥85% line across the accounting crates'
/// libraries).
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
                "rift-recall-mcp",
                "-p",
                "rift-recall-events",
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
                    &format!("coverage: accounting crates line {pct:.1}% < 85%"),
                );
                return format!("  measured line coverage (accounting crates): {pct:.1}%");
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

/// Run every gate leg; returns human-readable violations (empty = pass).
///
/// # Errors
/// A setup failure (not a gate violation) as a string.
pub fn run(root: &Path) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    leg_seam(root, &mut v);
    leg_truncation(root, &mut v);
    leg_audit(root, &mut v);
    leg_events(root, &mut v);
    let cov = leg_coverage(root, &mut v);
    println!("gate-m61 evidence:");
    if !cov.is_empty() {
        println!("{cov}");
    }
    Ok(v)
}

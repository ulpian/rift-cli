//! `xtask gate-m6` — the scripted M6 gate (PRD-M6 "Testing Decisions"; M6-11).
//!
//! Proves the milestone's gate text: *a context pack is trust/policy-shaped,
//! section-ordered, budget-bound, and byte-identical on re-serve from an
//! input key; and the harness yields a go/no-go with measured P10/P50/P90
//! savings meeting the threshold without degrading
//! success/test-pass/human-mod.* Four legs, each with the anti-drift
//! matched-a-test guard:
//!
//! 1. **pack** — the ADR-0016 structural no-bypass sweeps; placement
//!    invariants (order, unchanged = hashes/summaries only, sensitive =
//!    exact-or-handle, closed cost vocabulary, tail ⊆ top); the injection
//!    corpus through the pack path (zero forbidden spans, both trust routes);
//!    the M6-04 budget model (hard ceiling, `BudgetTooSmall` fail-loud,
//!    `min(pack, turn_remaining)` composition, post-render re-measure);
//!    byte-identity across runs + the golden pack snapshot.
//! 2. **eval** — the deterministic served suite clears the v1 gate; the
//!    seeded regression is NoGo; reconciliation flags the planted
//!    over-estimate against the REAL emitted figure; P10/P50/P90 present and
//!    ordered; the committed go/no-go report snapshot reproduces
//!    (`INSTA_UPDATE=no`); an `eval_run` row lands append-only (ADR-0042).
//! 3. **defaults** — the versioned `EvalDefaults` artifact exists with
//!    sweep-config + code provenance (ADR-0043 #5: existence + provenance,
//!    never values — the pre-sweep conservative-fallback artifact satisfies
//!    this leg until the owner-keyed paid run replaces it).
//! 4. **coverage** — ≥ 85% line across rift-recall-pack / rift-recall-eval /
//!    rift-recall-mcp via `cargo llvm-cov` when available; the ≥95%
//!    security-crates list is byte-stable.
//!
//! Run `cargo run -p xtask -- gate-m6`. The earlier gates stay intact.

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

/// Run test-name filters in `pkg` over the given target flag with
/// `INSTA_UPDATE=no`, asserting green **and** at least one match
/// (anti-drift: a renamed test goes red here, never silently vanishes).
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

/// Leg 1 — the pack: sealed constructors, placement invariants, the
/// injection corpus, the budget model, byte identity + the golden snapshot.
fn leg_pack(root: &Path, v: &mut Vec<String>) {
    require_tests(
        root,
        v,
        "rift-recall-pack",
        "--lib",
        &[
            "sections_are_in_placement_order",
            "unchanged_refuses_exact_content",
            "known_unchanged_is_hashes_only",
            "sensitive_summary_downgrades_to_handle",
            "cost_note_vocabulary_is_closed",
            "tail_is_subset_of_top",
            "critical_facts_appear_top_and_tail",
            "red_team_corpus_never_leaks_through_a_rendered_pack",
            "red_team_control_char_path_cannot_forge_a_directive_line",
            "red_team_untrusted_bytes_cannot_become_a_critical_fact",
            "untrusted_payloads_never_render_verbatim",
            "never_exceeds_hard_ceiling",
            "budget_too_small_fails_loud",
            "effective_cap_is_min_with_turn_remaining",
            "measured_total_never_exceeds_effective_cap",
            "degradation_swaps_whole_entries_never_truncates",
            "same_inputs_reserve_byte_identically",
            "anchors_reserved_under_pressure",
        ],
        "pack",
    );
    require_tests(
        root,
        v,
        "rift-recall-mcp",
        "--lib",
        &[
            "no_bypass_serve_paths_use_sealed_constructors_only",
            "no_serve_path_builds_four_state_inline",
            "red_team_hostile_candidate_path_is_never_rendered",
            "red_team_untrusted_exact_never_echoes_the_payload",
            "context_pack_is_input_keyed_and_budget_bounded",
            "typed_pack_reserves_byte_identically_across_runs",
            "golden_pack_snapshot_is_stable",
        ],
        "pack",
    );
}

/// Leg 2 — the eval: Go on the served suite, NoGo on the seeded regression,
/// the REAL-figure reconciliation flag, ordered percentiles, the committed
/// report snapshot, and the append-only `eval_run` row.
fn leg_eval(root: &Path, v: &mut Vec<String>) {
    require_tests(
        root,
        v,
        "rift-recall-eval",
        "--lib",
        &[
            "served_suite_clears_the_gate",
            "served_replay_reconciles_the_real_pack_claim",
            "served_replay_flags_a_planted_capture_overestimate",
            "seeded_regression_is_no_go_on_the_savings_dimensions",
            "golden_served_go_no_go_report_is_stable",
            "percentiles_are_ordered",
            "savings_percentiles_bracket_the_gated_median",
            "reconciliation_flags_overestimates_beyond_tolerance",
        ],
        "eval",
    );
    require_tests(
        root,
        v,
        "rift-memd",
        "--bins",
        &["eval_rows_land_bitemporally_and_idempotently"],
        "eval",
    );
}

/// Leg 3 — the `EvalDefaults` artifact: exists, parses, carries provenance
/// (values never judged — ADR-0043 #5).
fn leg_defaults(root: &Path, v: &mut Vec<String>) -> String {
    let path = root.join("crates/rift-recall-eval/fixtures/sweep-v1/eval-defaults.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        violation(
            v,
            false,
            &format!("defaults: missing artifact {}", path.display()),
        );
        return String::new();
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
        violation(v, false, "defaults: eval-defaults.json is not valid JSON");
        return String::new();
    };
    let provenance = doc
        .get("provenance")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    violation(
        v,
        matches!(
            provenance,
            "derived-from-sweep" | "pre-sweep-conservative-fallback"
        ),
        &format!("defaults: unknown provenance {provenance:?}"),
    );
    violation(
        v,
        doc.get("sweep_config_version")
            .and_then(serde_json::Value::as_u64)
            .is_some_and(|n| n >= 1),
        "defaults: missing sweep_config_version provenance",
    );
    violation(
        v,
        doc.get("code_version")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.is_empty()),
        "defaults: missing code_version provenance",
    );
    format!("  eval-defaults provenance: {provenance}")
}

/// Leg 4 — coverage bars (≥85% line, pack + eval + mcp) and the byte-stable
/// security-crates list.
fn leg_coverage(root: &Path, v: &mut Vec<String>) -> String {
    let security = std::fs::read_to_string(root.join("ci/security-crates.txt")).unwrap_or_default();
    let listed: Vec<&str> = security
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    violation(
        v,
        listed
            == [
                "rift-crypto",
                "rift-hashchain",
                "rift-store",
                "rift-recall-policy",
                "rift-shadow-redact",
                "rift-recall-validity",
                "rift-shadow-entire",
            ],
        "coverage: the >=95% security-crates list drifted",
    );
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
                "rift-recall-pack",
                "-p",
                "rift-recall-eval",
                "-p",
                "rift-recall-mcp",
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
                    &format!("coverage: pack/eval/mcp line {pct:.1}% < 85%"),
                );
                return format!("  measured line coverage (pack/eval/mcp): {pct:.1}%");
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

/// The last ~500 chars of a command's combined output (compact evidence).
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
    leg_pack(root, &mut v);
    leg_eval(root, &mut v);
    let defaults = leg_defaults(root, &mut v);
    let cov = leg_coverage(root, &mut v);
    println!("gate-m6 evidence:");
    if !defaults.is_empty() {
        println!("{defaults}");
    }
    if !cov.is_empty() {
        println!("{cov}");
    }
    Ok(v)
}

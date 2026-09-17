//! Experiment records + the deterministic corpus: the
//! content-addressed run identity, the flattened `eval_run` row the verb
//! appends, the rendered go/no-go report (whose insta snapshot is the
//! committed, reviewable contract), and the seeded-regression condition that
//! proves the gate gates.
//!
//! Everything here is deterministic: fixed corpus, fixed tokenizer, no clock
//! — the bitemporal columns (`valid_from`/`recorded_at`) are stamped by the
//! *caller* at append time (the bin edge), never in here.

use crate::{
    evaluate_go_no_go, reconcile, replay, savings_pct, savings_percentiles, Dimension,
    GaThresholds, PairedRun, ReplayMode, Scenario, ScenarioFile, Verdict,
};

/// The deterministic served-condition corpus: the CI floor.
/// Four scenarios spanning corpus size, changed-set size, and fact position;
/// files are ~100 lines so the anchor's compact manifest probe is
/// reconciliation noise. Growth of the corpus is a tracked representativeness gap.
#[must_use]
pub fn deterministic_corpus() -> Vec<Scenario> {
    [(10, 1, 0), (20, 2, 1), (40, 2, 0), (15, 3, 2)]
        .into_iter()
        .map(|(n, changed, fact_pos)| corpus_scenario(n, changed, fact_pos))
        .collect()
}

/// One corpus scenario: `n` files of ~100 filler lines, `changed` of them
/// changed, the decisive fact in changed file `fact_pos`.
fn corpus_scenario(n: usize, changed: usize, fact_pos: usize) -> Scenario {
    let files: Vec<ScenarioFile> = (0..n)
        .map(|i| {
            let mut content = format!(
                "// file {i}\n{}",
                "fn filler() { let x = 42; }\n".repeat(100)
            );
            if i == fact_pos {
                content.push_str("THE_DECISIVE_FACT: retry policy moved to backoff.rs\n");
            }
            ScenarioFile {
                path: format!("src/f{i}.rs"),
                content,
                changed: i < changed,
            }
        })
        .collect();
    Scenario {
        name: format!("served_n{n}_changed{changed}_pos{fact_pos}"),
        files,
        decisive_fact: "THE_DECISIVE_FACT".to_string(),
        token_budget: 400,
    }
}

/// The seeded-regression condition (the gate-proves-it-gates evidence): the
/// delta side deliberately regresses to re-reading the world — both legs are
/// the full-read replay, so savings are exactly 0% and the verdict must be
/// `NoGo` naming the savings dimensions (and, on large scenarios, the latency
/// envelope — a full re-read is also slow in steps).
#[must_use]
pub fn seeded_regression_runs() -> Vec<PairedRun> {
    deterministic_corpus()
        .iter()
        .map(|s| {
            let full = replay(s, ReplayMode::FullRead);
            PairedRun {
                name: format!("regression_{}", s.name),
                baseline: full,
                delta: full,
                emitted_tokens_avoided: 0,
            }
        })
        .collect()
}

/// The content-addressed run identity: `sha256(task_set ‖
/// condition ‖ code_version)`, so re-running an identical experiment appends
/// nothing (insert-or-ignore lands on the same primary key).
#[must_use]
pub fn run_id(task_set: &str, condition: &str, code_version: &str) -> String {
    rift_recall_substrate::worktree::sha256_hex(
        format!("{task_set}\u{1f}{condition}\u{1f}{code_version}").as_bytes(),
    )
}

/// A stable, neutral tag per failing dimension (the `failing_dimensions`
/// column vocabulary).
fn dimension_tag(d: Dimension) -> &'static str {
    match d {
        Dimension::TokenSavings => "token_savings",
        Dimension::CostSavings => "cost_savings",
        Dimension::SuccessDegraded => "success_degraded",
        Dimension::TestsDegraded => "tests_degraded",
        Dimension::HumanModDegraded => "human_mod_degraded",
        Dimension::LatencyEnvelope => "latency_envelope",
    }
}

/// Percent → basis points (×100, rounded) — the integer form the `eval_run`
/// row stores so the record is byte-deterministic across platforms.
#[allow(clippy::cast_possible_truncation)] // savings percents are tiny
fn to_bp(pct: f64) -> i64 {
    (pct * 100.0).round() as i64
}

/// One flattened, deterministic `eval_run` row — everything except
/// the bitemporal stamps, which the appending edge supplies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalRunRecord {
    /// Content-addressed identity ([`run_id`]).
    pub run_id: String,
    /// The task-set name (the corpus identity).
    pub task_set: String,
    /// The condition (`served` / `synthetic` / `seeded-regression` / …).
    pub condition: String,
    /// The code version the verdict was computed by.
    pub code_version: String,
    /// The [`GaThresholds`] version used.
    pub thresholds_version: u32,
    /// Paired runs scored.
    pub runs: u32,
    /// P10 token savings, basis points.
    pub p10_token_savings_bp: i64,
    /// P50 token savings, basis points (the gated median).
    pub p50_token_savings_bp: i64,
    /// P90 token savings, basis points.
    pub p90_token_savings_bp: i64,
    /// P50 cost savings, basis points.
    pub p50_cost_savings_bp: i64,
    /// Σ full-read baseline tokens.
    pub baseline_tokens: u64,
    /// Σ delta-served tokens.
    pub delta_tokens: u64,
    /// Σ serving-side claims.
    pub emitted_avoided_tokens: u64,
    /// Claims flagged over the ±5% band.
    pub reconcile_over_estimates: u32,
    /// The slowest delta run, deterministic steps.
    pub max_latency_steps: u32,
    /// `go` or `no-go`.
    pub verdict: String,
    /// Comma-joined failing-dimension tags (`None` on go).
    pub failing_dimensions: Option<String>,
}

impl EvalRunRecord {
    /// Flatten a scored run set into the row the verb appends.
    #[must_use]
    pub fn from_runs(
        task_set: &str,
        condition: &str,
        code_version: &str,
        thresholds: &GaThresholds,
        runs: &[PairedRun],
    ) -> Self {
        let verdict = evaluate_go_no_go(thresholds, runs);
        let p = savings_percentiles(runs);
        let mut cost: Vec<f64> = runs
            .iter()
            .map(|r| savings_pct(r.baseline.cost, r.delta.cost))
            .collect();
        cost.sort_by(f64::total_cmp);
        let p50_cost = crate::p50(&cost);
        let (verdict_tag, failing) = match &verdict {
            Verdict::Go => ("go".to_string(), None),
            Verdict::NoGo(dims) => (
                "no-go".to_string(),
                Some(
                    dims.iter()
                        .map(|d| dimension_tag(*d))
                        .collect::<Vec<_>>()
                        .join(","),
                ),
            ),
        };
        Self {
            run_id: run_id(task_set, condition, code_version),
            task_set: task_set.to_string(),
            condition: condition.to_string(),
            code_version: code_version.to_string(),
            thresholds_version: thresholds.version,
            runs: u32::try_from(runs.len()).unwrap_or(u32::MAX),
            p10_token_savings_bp: to_bp(p.p10),
            p50_token_savings_bp: to_bp(p.p50),
            p90_token_savings_bp: to_bp(p.p90),
            p50_cost_savings_bp: to_bp(p50_cost),
            baseline_tokens: runs
                .iter()
                .fold(0u64, |a, r| a.saturating_add(r.baseline.tokens)),
            delta_tokens: runs
                .iter()
                .fold(0u64, |a, r| a.saturating_add(r.delta.tokens)),
            emitted_avoided_tokens: runs
                .iter()
                .fold(0u64, |a, r| a.saturating_add(r.emitted_tokens_avoided)),
            reconcile_over_estimates: u32::try_from(
                runs.iter().filter(|r| reconcile(r).over_estimate).count(),
            )
            .unwrap_or(u32::MAX),
            max_latency_steps: runs
                .iter()
                .map(|r| r.delta.latency_steps)
                .max()
                .unwrap_or(0),
            verdict: verdict_tag,
            failing_dimensions: failing,
        }
    }
}

/// Render the deterministic go/no-go report: per-task metrics, percentiles,
/// reconciliation, verdict + failing dimensions. The insta snapshot of this
/// rendering over the deterministic corpus is the committed, reviewable
/// contract — report drift is a reviewed diff, never silent.
#[must_use]
pub fn render_report(condition: &str, thresholds: &GaThresholds, runs: &[PairedRun]) -> String {
    use core::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "=== Lost-in-Delta go/no-go report ===");
    let _ = writeln!(out, "condition: {condition}");
    let _ = writeln!(
        out,
        "thresholds: v{} (P50 token >= {:.1}%, P50 cost >= {:.1}%, latency <= {} steps)",
        thresholds.version,
        thresholds.min_p50_token_savings_pct,
        thresholds.min_p50_cost_savings_pct,
        thresholds.max_latency_steps
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{:<34} {:>9} {:>8} {:>7} {:>5}  {:<4} {:>9}  reconciled",
        "task", "baseline", "delta", "saved%", "steps", "ok", "claim"
    );
    for r in runs {
        let rec = reconcile(r);
        let _ = writeln!(
            out,
            "{:<34} {:>9} {:>8} {:>6.2}% {:>5}  {:<4} {:>9}  {}",
            r.name,
            r.baseline.tokens,
            r.delta.tokens,
            savings_pct(r.baseline.tokens, r.delta.tokens),
            r.delta.latency_steps,
            if r.delta.success { "yes" } else { "NO" },
            r.emitted_tokens_avoided,
            if rec.within_tolerance {
                "within"
            } else if rec.over_estimate {
                "OVER-ESTIMATE"
            } else {
                "under"
            },
        );
    }
    let p = savings_percentiles(runs);
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "token savings percentiles: P10 {:.2}% | P50 {:.2}% | P90 {:.2}%",
        p.p10, p.p50, p.p90
    );
    let within = runs
        .iter()
        .filter(|r| reconcile(r).within_tolerance)
        .count();
    let over = runs.iter().filter(|r| reconcile(r).over_estimate).count();
    let _ = writeln!(
        out,
        "reconciliation: {within}/{} within \u{b1}5% | {over} over-estimate(s)",
        runs.len()
    );
    match evaluate_go_no_go(thresholds, runs) {
        Verdict::Go => {
            let _ = writeln!(out, "verdict: GO");
        }
        Verdict::NoGo(dims) => {
            let _ = writeln!(
                out,
                "verdict: NO-GO [{}]",
                dims.iter()
                    .map(|d| dimension_tag(*d))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{replay_intercepted, replay_served};

    /// The committed go/no-go contract: the deterministic
    /// served condition, rendered. Drift is a reviewed insta diff.
    #[test]
    fn golden_served_go_no_go_report_is_stable() {
        let runs: Vec<PairedRun> = deterministic_corpus()
            .iter()
            .map(|s| replay_served(s).unwrap())
            .collect();
        insta::assert_snapshot!(
            "served_go_no_go_report",
            render_report("served", &GaThresholds::V1, &runs)
        );
    }

    /// The committed contract for the intercepted condition: the corpus
    /// replayed against a window that already holds it, rendered. Drift is a
    /// reviewed insta diff, and the verdict line is the one this program
    /// publishes for Lost-in-Delta C4.
    #[test]
    fn golden_intercepted_go_no_go_report_is_stable() {
        let runs: Vec<PairedRun> = deterministic_corpus()
            .iter()
            .map(|s| replay_intercepted(s).unwrap())
            .collect();
        insta::assert_snapshot!(
            "intercepted_go_no_go_report",
            render_report(crate::INTERCEPTED, &GaThresholds::V1, &runs)
        );
    }

    /// The gate proves it gates: the seeded regression must be NO-GO on the
    /// savings dimensions, and its rendered report says so.
    #[test]
    fn seeded_regression_is_no_go_on_the_savings_dimensions() {
        let runs = seeded_regression_runs();
        let Verdict::NoGo(dims) = evaluate_go_no_go(&GaThresholds::V1, &runs) else {
            panic!("the seeded regression must not pass the gate");
        };
        assert!(dims.contains(&Dimension::TokenSavings));
        assert!(dims.contains(&Dimension::CostSavings));
        let report = render_report("seeded-regression", &GaThresholds::V1, &runs);
        assert!(report.contains("verdict: NO-GO"));
        assert!(report.contains("token_savings"));
        insta::assert_snapshot!("seeded_regression_report", report);
    }

    #[test]
    fn run_ids_are_content_addressed_and_distinct_per_input() {
        let a = run_id("corpus-v1", "served", "0.0.1");
        assert_eq!(a, run_id("corpus-v1", "served", "0.0.1"), "deterministic");
        assert_eq!(a.len(), 64);
        assert_ne!(a, run_id("corpus-v1", "seeded-regression", "0.0.1"));
        assert_ne!(a, run_id("corpus-v1", "served", "0.0.2"));
        assert_ne!(a, run_id("corpus-v2", "served", "0.0.1"));
    }

    #[test]
    fn record_flattens_the_verdict_and_figures() {
        let runs = seeded_regression_runs();
        let rec = EvalRunRecord::from_runs(
            "corpus-v1",
            "seeded-regression",
            "0.0.1",
            &GaThresholds::V1,
            &runs,
        );
        assert_eq!(rec.verdict, "no-go");
        let dims = rec.failing_dimensions.as_deref().unwrap_or_default();
        assert!(dims.contains("token_savings") && dims.contains("cost_savings"));
        assert_eq!(rec.p50_token_savings_bp, 0, "0% savings in basis points");
        assert_eq!(rec.baseline_tokens, rec.delta_tokens);
        assert_eq!(rec.emitted_avoided_tokens, 0);
        assert_eq!(rec.thresholds_version, 1);
        assert_eq!(rec.runs, 4);
        assert_eq!(
            rec.run_id,
            run_id("corpus-v1", "seeded-regression", "0.0.1")
        );
    }
}

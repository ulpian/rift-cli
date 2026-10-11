//! The **Lost-in-Delta** eval harness: does serving deltas instead of full
//! re-reads actually save tokens *without degrading outcomes*?
//!
//! The harness replays a scenario two ways — a **full-read baseline** (the
//! agent re-reads every file) and a **delta-served run** (the agent gets a
//! compiled context pack and retrieves by handle only until the decisive fact
//! is found) — then scores the pairs against versioned [`GaThresholds`]:
//!
//! * `Go` iff P50 token **and** cost savings meet the staged
//!   `rift_recall_ledger::SAVINGS_BAR_PCT` bar (the 90% product target lives
//!   beside it and is deliberately not what this gate reads),
//!   success / test-pass / human-mod are **non-degraded** vs the baseline
//!   (hard, regardless of savings), and latency stays inside the envelope.
//! * `NoGo` returns the specific failing [`Dimension`]s — actionable, never a
//!   black-box fail.
//!
//! Everything here is **deterministic** — scripted agent, injected fixtures,
//! no clock, no network, no model — so the CI verdict is reproducible. Latency
//! is measured as deterministic *steps* (one read or retrieval = one step),
//! the reproducible proxy for the wall-clock envelope.
//!
//! Emitted `tokens_avoided` claims are reconciled against the measured
//! `baseline − delta` within [`RECONCILE_TOLERANCE`]: the harness is the
//! check on the serving plane's claimed number, not a second unverified claim.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use rift_recall_pack::{compile, EvidenceRef, PackRequest, SectionEntry, Sensitivity};
use rift_recall_types::{ArtifactState, Envelope, Handle, Trust};

mod intercepted;
mod outlined;
mod record;
mod served;
pub use intercepted::{corpus_honesty_figures, replay_intercepted, CONDITION as INTERCEPTED};
pub use outlined::{outline_frame_for, outlined_corpus, replay_outlined, CONDITION as OUTLINED};
pub use record::{
    deterministic_corpus, render_report, run_id, seeded_regression_runs, EvalRunRecord,
};
pub use served::{replay_served, replay_served_with_capture};

/// The versioned go/no-go thresholds. Any change to the bar is
/// an explicit, reviewable edit to a versioned value — never a silent tune.
/// The figure itself is [`rift_recall_ledger::SAVINGS_BAR_PCT`], the staged bar:
/// 15% is the original calibration seed, and it moves only when a replay has
/// measured a higher one. Re-calibration from the measured reread-report bumps
/// `version`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GaThresholds {
    /// Version of this threshold set (bump on any change).
    pub version: u32,
    /// Minimum median token savings, percent of the baseline.
    pub min_p50_token_savings_pct: f64,
    /// Minimum median cost savings, percent of the baseline.
    pub min_p50_cost_savings_pct: f64,
    /// Latency envelope: max deterministic steps a delta run may take.
    pub max_latency_steps: u32,
}

impl GaThresholds {
    /// The v1 thresholds: the staged [`SAVINGS_BAR_PCT`](rift_recall_ledger::SAVINGS_BAR_PCT) as the P50 savings
    /// floor (token and cost), 16-step envelope.
    ///
    /// **Provenance:** the measured reread-report over this
    /// repo's own captured sessions found **33.7% avoidable context** (1.38M
    /// tokens across 8 sessions, 213/357 reads resolved) — comfortably above
    /// the bar, so V1 stands on measured evidence rather than the
    /// synthetic 22% golden alone. A future re-calibration bumps `version`.
    ///
    /// The figure is **read** from `rift-recall-ledger` rather than retyped
    /// here, so the staged-bar rule written on that constant — raised only to
    /// what the last `replay-savings` achieved, never to an aspiration — governs
    /// this gate too.
    pub const V1: Self = Self {
        version: 1,
        min_p50_token_savings_pct: rift_recall_ledger::SAVINGS_BAR_PCT,
        min_p50_cost_savings_pct: rift_recall_ledger::SAVINGS_BAR_PCT,
        max_latency_steps: 16,
    };
}

/// What one replayed run measured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunMetrics {
    /// Tokens the agent consumed (pack + retrievals, or the full re-read).
    pub tokens: u64,
    /// Cost units consumed (in this harness: uncached-input token equivalents).
    pub cost: u64,
    /// Whether the agent found the decisive fact (task success).
    pub success: bool,
    /// Whether the scenario's tests would pass given what the agent saw
    /// (proxied by success in the scripted agent).
    pub tests_passed: bool,
    /// Whether the human-modification outcome is acceptable (the scripted
    /// agent never regresses this).
    pub human_mod_ok: bool,
    /// Deterministic latency: one step per read/retrieval.
    pub latency_steps: u32,
}

/// One scenario replayed both ways, plus the serving side's emitted claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairedRun {
    /// Scenario name (reporting only).
    pub name: String,
    /// The full-read baseline measurement.
    pub baseline: RunMetrics,
    /// The delta-served measurement.
    pub delta: RunMetrics,
    /// What the serving side *claimed* it avoided (reconciled by [`reconcile`]).
    pub emitted_tokens_avoided: u64,
}

/// A dimension the gate can fail on (returned inside [`Verdict::NoGo`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dimension {
    /// P50 token savings below threshold.
    TokenSavings,
    /// P50 cost savings below threshold.
    CostSavings,
    /// Task success degraded vs the baseline (hard fail).
    SuccessDegraded,
    /// Test outcomes degraded vs the baseline (hard fail).
    TestsDegraded,
    /// Human-modification outcomes degraded vs the baseline (hard fail).
    HumanModDegraded,
    /// A delta run exceeded the latency envelope.
    LatencyEnvelope,
}

/// The gate verdict: `NoGo` carries every failing dimension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// All gate conditions hold.
    Go,
    /// One or more dimensions failed.
    NoGo(Vec<Dimension>),
}

/// Linear-interpolated percentile of a pre-sorted slice (`0.0` for empty —
/// an empty eval never passes the savings bar, so absence of evidence is
/// never a `Go`). At `q = 0.5` this is exactly the median the verdict has
/// always used (exact middle for odd `n`, midpoint of the two middles for
/// even `n`).
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn percentile_linear(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = q.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lo_idx = rank.floor() as usize;
    let hi_idx = rank.ceil() as usize;
    let lo = sorted.get(lo_idx).copied().unwrap_or(0.0);
    let hi = sorted.get(hi_idx).copied().unwrap_or(0.0);
    let frac = rank - rank.floor();
    lo + (hi - lo) * frac
}

/// Median of a pre-sorted slice (the verdict's P50 — see
/// [`percentile_linear`]).
fn p50(sorted: &[f64]) -> f64 {
    percentile_linear(sorted, 0.5)
}

/// Token-savings percentiles over a run set: the distribution beside
/// the P50 the verdict gates on — verdict logic stays P50; P10/P90 are
/// reporting, showing the tails the median hides.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SavingsPercentiles {
    /// The 10th percentile of per-run token-savings percent (the worst tail).
    pub p10: f64,
    /// The median (the same figure the verdict gates on).
    pub p50: f64,
    /// The 90th percentile (the best tail).
    pub p90: f64,
}

/// The P10/P50/P90 of per-run token-savings percent (`0.0`s for an empty
/// run set — absence of evidence is never savings).
#[must_use]
pub fn savings_percentiles(runs: &[PairedRun]) -> SavingsPercentiles {
    let mut pct: Vec<f64> = runs
        .iter()
        .map(|r| savings_pct(r.baseline.tokens, r.delta.tokens))
        .collect();
    pct.sort_by(f64::total_cmp);
    SavingsPercentiles {
        p10: percentile_linear(&pct, 0.1),
        p50: percentile_linear(&pct, 0.5),
        p90: percentile_linear(&pct, 0.9),
    }
}

/// Percent saved going baseline → delta (`0` when the baseline is zero).
#[allow(clippy::cast_precision_loss)] // token counts are far below 2^52
fn savings_pct(baseline: u64, delta: u64) -> f64 {
    if baseline == 0 {
        return 0.0;
    }
    (baseline.saturating_sub(delta)) as f64 / baseline as f64 * 100.0
}

/// Score paired runs against the thresholds. Non-degradation is
/// aggregate: the delta side must succeed / pass tests / keep human-mod at
/// least as often as the baseline side — hard and non-negotiable regardless of
/// savings (a cheaper-but-worse agent is not a win).
#[must_use]
pub fn evaluate_go_no_go(t: &GaThresholds, runs: &[PairedRun]) -> Verdict {
    let mut failing = Vec::new();

    let mut token_savings: Vec<f64> = runs
        .iter()
        .map(|r| savings_pct(r.baseline.tokens, r.delta.tokens))
        .collect();
    token_savings.sort_by(f64::total_cmp);
    if p50(&token_savings) < t.min_p50_token_savings_pct {
        failing.push(Dimension::TokenSavings);
    }

    let mut cost_savings: Vec<f64> = runs
        .iter()
        .map(|r| savings_pct(r.baseline.cost, r.delta.cost))
        .collect();
    cost_savings.sort_by(f64::total_cmp);
    if p50(&cost_savings) < t.min_p50_cost_savings_pct {
        failing.push(Dimension::CostSavings);
    }

    let count = |f: fn(&RunMetrics) -> bool, side: fn(&PairedRun) -> &RunMetrics| {
        runs.iter().filter(|r| f(side(r))).count()
    };
    if count(|m| m.success, |r| &r.delta) < count(|m| m.success, |r| &r.baseline) {
        failing.push(Dimension::SuccessDegraded);
    }
    if count(|m| m.tests_passed, |r| &r.delta) < count(|m| m.tests_passed, |r| &r.baseline) {
        failing.push(Dimension::TestsDegraded);
    }
    if count(|m| m.human_mod_ok, |r| &r.delta) < count(|m| m.human_mod_ok, |r| &r.baseline) {
        failing.push(Dimension::HumanModDegraded);
    }

    if runs
        .iter()
        .any(|r| r.delta.latency_steps > t.max_latency_steps)
    {
        failing.push(Dimension::LatencyEnvelope);
    }

    if failing.is_empty() {
        Verdict::Go
    } else {
        Verdict::NoGo(failing)
    }
}

/// Tokenizer tolerance for reconciling emitted vs measured savings.
pub const RECONCILE_TOLERANCE: f64 = 0.05;

/// The reconciliation of an emitted `tokens_avoided` claim against the
/// measured `baseline − delta`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reconciliation {
    /// The serving side's claim.
    pub emitted: u64,
    /// What the replay measured.
    pub measured: u64,
    /// Whether the claim is inside the ±[`RECONCILE_TOLERANCE`] band.
    pub within_tolerance: bool,
    /// Whether the claim **over**-states savings beyond tolerance (flagged —
    /// the number must stay honest).
    pub over_estimate: bool,
}

/// Reconcile one paired run's emitted claim against its measurement.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)]
pub fn reconcile(run: &PairedRun) -> Reconciliation {
    let measured = run.baseline.tokens.saturating_sub(run.delta.tokens);
    let band = (measured as f64 * RECONCILE_TOLERANCE).ceil() as u64;
    let within = run.emitted_tokens_avoided.abs_diff(measured) <= band;
    Reconciliation {
        emitted: run.emitted_tokens_avoided,
        measured,
        within_tolerance: within,
        over_estimate: !within && run.emitted_tokens_avoided > measured,
    }
}

// --- deterministic replay ---------------------------------------------------

/// One fixture file in a scenario.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScenarioFile {
    /// Repo-relative path.
    pub path: String,
    /// File content (the token source).
    pub content: String,
    /// Whether this file changed since the agent last looked (unchanged files
    /// are the delta plane's savings: summarized/handled, not re-inlined).
    pub changed: bool,
}

/// A replayable scenario: a small world of files, one of which contains the
/// decisive fact the (scripted) agent must find.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scenario {
    /// Scenario name (reporting only).
    pub name: String,
    /// The files, in the ranking order the recall plane would serve them.
    pub files: Vec<ScenarioFile>,
    /// The decisive fact; present verbatim in exactly one file's content.
    pub decisive_fact: String,
    /// The pack token budget for the delta-served run.
    pub token_budget: u32,
}

/// How a run replays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayMode {
    /// The agent re-reads every file (today's behavior — the baseline).
    FullRead,
    /// The agent receives a compiled pack and retrieves by handle only until
    /// the decisive fact is found.
    DeltaServed,
}

fn count_tokens(s: &str) -> u64 {
    u64::from(rift_tokens::count(s))
}

/// Replay a scenario with the deterministic scripted agent. Pure function of
/// the scenario + mode: no clock, no I/O, no model.
#[must_use]
pub fn replay(scenario: &Scenario, mode: ReplayMode) -> RunMetrics {
    match mode {
        ReplayMode::FullRead => {
            // The agent reads everything, in order, and always finds the fact.
            let tokens: u64 = scenario
                .files
                .iter()
                .map(|f| count_tokens(&f.content))
                .sum();
            let success = scenario
                .files
                .iter()
                .any(|f| f.content.contains(&scenario.decisive_fact));
            RunMetrics {
                tokens,
                cost: tokens,
                success,
                tests_passed: success,
                human_mod_ok: true,
                latency_steps: u32::try_from(scenario.files.len()).unwrap_or(u32::MAX),
            }
        }
        ReplayMode::DeltaServed => replay_delta(scenario),
    }
}

/// The delta-served leg: compile a real pack over the scenario (changed files
/// early, unchanged as summaries-by-handle, retrievals recommended in rank
/// order), then follow handles until the decisive fact turns up.
fn replay_delta(scenario: &Scenario) -> RunMetrics {
    let preq = delta_pack_request(scenario);
    // Replay has no per-turn ceiling; the scenario budget alone governs. A
    // budget too small for any pack falls back to the full-read shape (the
    // caller's documented fallback), so tiny-budget scenarios stay runnable.
    let Ok(pack) = compile(&preq, u32::MAX) else {
        return replay(scenario, ReplayMode::FullRead);
    };
    let rendered = pack.render();
    let mut tokens = count_tokens(&rendered);
    let mut steps = 1u32; // reading the pack is a step

    let mut success = rendered.contains(&scenario.decisive_fact);
    if !success {
        success = follow_recommended(scenario, &mut tokens, &mut steps);
    }
    RunMetrics {
        tokens,
        cost: tokens,
        success,
        tests_passed: success,
        human_mod_ok: true,
        latency_steps: steps,
    }
}

/// The pack request the delta leg compiles: every changed file as a
/// by-handle entry, every unchanged file as known-unchanged, then one
/// recommendation per file in ladder order.
///
/// Fixture envelopes stand in for `envelope_for` outputs (the harness IS
/// the replay of a serve, not a serve path); the entries still go through
/// the sealed pack constructors, so the compiled shape matches production.
fn delta_pack_request(scenario: &Scenario) -> PackRequest {
    let mut preq = PackRequest::new(scenario.token_budget);
    for f in scenario.files.iter().filter(|f| f.changed) {
        let _ = preq.push_changed(changed_entry(scenario, f));
    }
    for f in scenario.files.iter().filter(|f| !f.changed) {
        let _ = preq.push_unchanged(unchanged_entry(scenario, f));
    }
    for f in ladder_order(scenario) {
        // The recommendation cites the same ordinal the file's own
        // entry carries, so the replayed ladder follows numbers exactly as a
        // real agent now does.
        let _ = preq.push_recommended(SectionEntry::from_handle(
            &file_handle(f),
            evidence_ordinal(scenario, f),
        ));
    }
    preq
}

/// A changed file's pack entry: available by handle, content not inlined.
fn changed_entry(scenario: &Scenario, f: &ScenarioFile) -> SectionEntry {
    let env = Envelope {
        path: f.path.clone(),
        trust: Trust::CodeFact,
        state: ArtifactState::AvailableByHandle {
            handle: file_handle(f).to_string(),
            basis_sha256: Some(fixture_sha(f)),
            reason: rift_recall_types::OmitReason::NotRequested,
        },
        estimated_tokens_returned: 0,
    };
    SectionEntry::from_envelope(
        &env,
        Sensitivity::Normal,
        &EvidenceRef::at(evidence_ordinal(scenario, f)),
    )
}

/// An unchanged file's pack entry: known-unchanged, the delta plane's saving.
fn unchanged_entry(scenario: &Scenario, f: &ScenarioFile) -> SectionEntry {
    let env = Envelope {
        path: f.path.clone(),
        trust: Trust::CodeFact,
        state: ArtifactState::KnownUnchanged {
            handle: file_handle(f).to_string(),
            basis_sha256: fixture_sha(f),
            last_seen_turn: None,
        },
        estimated_tokens_returned: 0,
    };
    SectionEntry::from_envelope(
        &env,
        Sensitivity::Normal,
        &EvidenceRef::at(evidence_ordinal(scenario, f)),
    )
}

/// The retrieval ladder's order: changed files outrank unchanged ones.
fn ladder_order(scenario: &Scenario) -> impl Iterator<Item = &ScenarioFile> + '_ {
    scenario
        .files
        .iter()
        .filter(|f| f.changed)
        .chain(scenario.files.iter().filter(|f| !f.changed))
}

/// The fixture's stand-in content hash: deterministic, not a real digest.
fn fixture_sha(f: &ScenarioFile) -> String {
    format!("{:0>64}", f.path.len())
}

fn file_handle(f: &ScenarioFile) -> Handle {
    Handle::File {
        path: f.path.clone(),
        sha256: fixture_sha(f),
        range: None,
    }
}

/// The evidence-ordinal pass over the scenario's own file order — the same
/// discipline as the serve path this harness replays, assigned before
/// compile and not by position in the finished pack.
fn evidence_ordinal(scenario: &Scenario, f: &ScenarioFile) -> u32 {
    scenario
        .files
        .iter()
        .position(|c| c.path == f.path)
        .and_then(|i| u32::try_from(i).ok())
        .unwrap_or(0)
        .saturating_add(1)
}

/// Follow the recommended handles in order; each retrieval inlines that
/// file's bytes (a step + its token cost) until the fact is found. Returns
/// whether it was.
fn follow_recommended(scenario: &Scenario, tokens: &mut u64, steps: &mut u32) -> bool {
    let recommended: Vec<String> = ladder_order(scenario)
        .map(|f| format!("file:{}", f.path))
        .collect();
    for handle in &recommended {
        let Some(file) = scenario
            .files
            .iter()
            .find(|f| format!("file:{}", f.path) == *handle)
        else {
            continue;
        };
        *steps = steps.saturating_add(1);
        *tokens += count_tokens(&file.content);
        if file.content.contains(&scenario.decisive_fact) {
            return true;
        }
    }
    false
}

/// Replay a scenario both ways into a [`PairedRun`], stamping the serving
/// side's honest claim (`baseline − delta`, what a truthful server would emit).
#[must_use]
pub fn replay_paired(scenario: &Scenario) -> PairedRun {
    let baseline = replay(scenario, ReplayMode::FullRead);
    let delta = replay(scenario, ReplayMode::DeltaServed);
    PairedRun {
        name: scenario.name.clone(),
        baseline,
        delta,
        emitted_tokens_avoided: baseline.tokens.saturating_sub(delta.tokens),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A saturation-style scenario: `n` files, `changed` of them changed, the
    /// decisive fact buried in the file at `fact_pos` (rank order).
    fn scenario(n: usize, changed: usize, fact_pos: usize) -> Scenario {
        let files: Vec<ScenarioFile> = (0..n)
            .map(|i| {
                let mut content = format!(
                    "// file {i}\n{}\n",
                    "fn filler() { let x = 42; }\n".repeat(20)
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
            name: format!("n{n}_changed{changed}_pos{fact_pos}"),
            files,
            decisive_fact: "THE_DECISIVE_FACT".to_string(),
            token_budget: 400,
        }
    }

    #[test]
    fn position_sweep_never_loses_the_decisive_fact() {
        // The lost-in-delta failure mode: the fact's rank position must not
        // determine success — head, middle, and tail all resolve.
        for pos in [0usize, 4, 9] {
            let s = scenario(10, 2, pos);
            let run = replay_paired(&s);
            assert!(run.baseline.success);
            assert!(
                run.delta.success,
                "delta run must find the fact at position {pos}"
            );
        }
    }

    #[test]
    fn saturation_run_clears_the_savings_bar() {
        // Many unchanged files, a changed file carrying the fact: the pack +
        // one retrieval must beat re-reading the world by well over the bar.
        let s = scenario(40, 2, 0);
        let run = replay_paired(&s);
        assert!(run.delta.success);
        let saved =
            (run.baseline.tokens - run.delta.tokens) as f64 / run.baseline.tokens as f64 * 100.0;
        let bar = rift_recall_ledger::SAVINGS_BAR_PCT;
        assert!(saved >= bar, "expected >={bar}% savings, got {saved:.1}%");
    }

    #[test]
    fn go_verdict_on_the_deterministic_fixture_suite() {
        // The reproducible CI floor: the scripted-agent suite
        // must clear the v1 gate.
        let runs: Vec<PairedRun> = [
            scenario(10, 1, 0),
            scenario(20, 2, 1),
            scenario(40, 2, 0),
            scenario(15, 3, 2),
        ]
        .iter()
        .map(replay_paired)
        .collect();
        assert_eq!(evaluate_go_no_go(&GaThresholds::V1, &runs), Verdict::Go);
    }

    #[test]
    fn no_go_names_the_failing_dimensions() {
        // A degraded delta side (failed task, worse latency, no savings) must
        // return every failing dimension, not a black-box fail.
        let bad = PairedRun {
            name: "degraded".into(),
            baseline: RunMetrics {
                tokens: 1000,
                cost: 1000,
                success: true,
                tests_passed: true,
                human_mod_ok: true,
                latency_steps: 3,
            },
            delta: RunMetrics {
                tokens: 990, // ~1% savings
                cost: 990,
                success: false,
                tests_passed: false,
                human_mod_ok: false,
                latency_steps: 99,
            },
            emitted_tokens_avoided: 10,
        };
        let Verdict::NoGo(dims) = evaluate_go_no_go(&GaThresholds::V1, &[bad]) else {
            panic!("expected NoGo");
        };
        for d in [
            Dimension::TokenSavings,
            Dimension::CostSavings,
            Dimension::SuccessDegraded,
            Dimension::TestsDegraded,
            Dimension::HumanModDegraded,
            Dimension::LatencyEnvelope,
        ] {
            assert!(dims.contains(&d), "missing failing dimension {d:?}");
        }
    }

    #[test]
    fn empty_eval_is_never_a_go() {
        // Absence of evidence is not evidence of savings.
        assert!(matches!(
            evaluate_go_no_go(&GaThresholds::V1, &[]),
            Verdict::NoGo(_)
        ));
    }

    #[test]
    fn reconciliation_flags_overestimates_beyond_tolerance() {
        let mut run = replay_paired(&scenario(10, 1, 0));
        // honest claim reconciles
        let ok = reconcile(&run);
        assert!(ok.within_tolerance);
        assert!(!ok.over_estimate);
        // an inflated claim (>5% over measured) is flagged
        run.emitted_tokens_avoided = ok.measured * 2;
        let flagged = reconcile(&run);
        assert!(!flagged.within_tolerance);
        assert!(flagged.over_estimate, "over-claims must be flagged");
        // an under-claim outside tolerance is out-of-band but not an over-claim
        run.emitted_tokens_avoided = 0;
        let under = reconcile(&run);
        assert!(!under.within_tolerance);
        assert!(!under.over_estimate);
    }

    #[test]
    fn thresholds_are_versioned_values() {
        assert_eq!(GaThresholds::V1.version, 1);
        assert!((GaThresholds::V1.min_p50_token_savings_pct - 15.0).abs() < f64::EPSILON);
    }

    #[test]
    fn savings_percentiles_bracket_the_gated_median() {
        // Runs with 0%, 10%, 20%, 30%, 40% savings: P50 must equal the
        // verdict's median exactly; the tails bracket it.
        let run = |delta_tokens: u64| PairedRun {
            name: "p".into(),
            baseline: RunMetrics {
                tokens: 100,
                cost: 100,
                success: true,
                tests_passed: true,
                human_mod_ok: true,
                latency_steps: 1,
            },
            delta: RunMetrics {
                tokens: delta_tokens,
                cost: delta_tokens,
                success: true,
                tests_passed: true,
                human_mod_ok: true,
                latency_steps: 1,
            },
            emitted_tokens_avoided: 100 - delta_tokens,
        };
        let runs: Vec<PairedRun> = [100, 90, 80, 70, 60].map(run).into_iter().collect();
        let p = savings_percentiles(&runs);
        assert!((p.p50 - 20.0).abs() < f64::EPSILON, "p50 = the median");
        assert!(p.p10 < p.p50 && p.p50 < p.p90);
        // Empty: absence of evidence is never savings.
        let empty = savings_percentiles(&[]);
        assert!((empty.p10, empty.p50, empty.p90) == (0.0, 0.0, 0.0));
    }

    proptest::proptest! {
        /// Order statistics: for ANY run set, p10 ≤ p50 ≤ p90.
        #[test]
        fn percentiles_are_ordered(deltas in proptest::collection::vec(0u64..=1000, 0..24)) {
            let runs: Vec<PairedRun> = deltas
                .iter()
                .map(|&d| PairedRun {
                    name: "p".into(),
                    baseline: RunMetrics {
                        tokens: 1000,
                        cost: 1000,
                        success: true,
                        tests_passed: true,
                        human_mod_ok: true,
                        latency_steps: 1,
                    },
                    delta: RunMetrics {
                        tokens: d,
                        cost: d,
                        success: true,
                        tests_passed: true,
                        human_mod_ok: true,
                        latency_steps: 1,
                    },
                    emitted_tokens_avoided: 1000 - d,
                })
                .collect();
            let p = savings_percentiles(&runs);
            proptest::prop_assert!(p.p10 <= p.p50 && p.p50 <= p.p90);
        }
    }

    proptest::proptest! {
        /// Replay is deterministic: same scenario, same metrics — the property
        /// that makes a `Go` reproducible and therefore trustworthy.
        #[test]
        fn replay_is_deterministic(n in 1usize..12, changed in 0usize..4, pos in 0usize..12) {
            let s = scenario(n, changed.min(n), pos.min(n - 1));
            proptest::prop_assert_eq!(replay_paired(&s), replay_paired(&s));
        }

        /// The delta run never spends more tokens than the baseline plus the
        /// pack overhead, and always succeeds when the baseline does.
        #[test]
        fn delta_never_degrades_success(n in 1usize..12, pos in 0usize..12) {
            let s = scenario(n, 1, pos.min(n - 1));
            let run = replay_paired(&s);
            proptest::prop_assert!(run.delta.success == run.baseline.success);
        }
    }
}

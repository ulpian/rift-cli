//! The artifact a sweep produces, and the anatomy an owner rules on.
//!
//! Everything here is control-relative: a latency is this arm's median minus
//! the bare commit's in the same run, and a sweep that lost its control has
//! nothing to decompose rather than absolutes to quote as if they were
//! latencies.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{
    arm::{Arm, CORPUS_DIR},
    oracle::Oracles,
    phase::{exclusive_phases, TOTAL_PHASE},
};
use crate::{
    freshness::SourceClock,
    leg::LegStatus,
    measure::{Protocol, Stats},
    report::Host,
    BenchError, Result,
};

/// Schema of the git-latency artifact.
///
/// `2` adds provenance to the subject — `git_describe` and `dirty` — so an
/// artifact says which *commit* it is about rather than only which minute. The
/// committed `1` artifacts stay exactly as they are: they are the evidence that
/// produced the shipped caps and the two-facts verdict, they load unchanged,
/// and they may not derive a floor, which is the whole point of the field.
pub const HOOK_LATENCY_SCHEMA: u32 = 2;

/// The owner's hook budget: 100 ms of git latency, hard.
pub const BUDGET_MS: u64 = 100;

/// One arm's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArmResult {
    /// The arm.
    pub arm: Arm,
    /// Timing of the measured commits, or `None` when the arm could not run.
    pub stats: Option<Stats>,
    /// Why it could not run.
    pub error: Option<String>,
    /// What it exercised.
    pub oracles: Oracles,
    /// What the machine was doing when this arm started.
    ///
    /// Per arm rather than per run, because a sweep's arms are minutes apart
    /// and this machine's load moves inside that. A void reading does not stop
    /// the arm being published; it stops it deriving a floor.
    #[serde(default)]
    pub attestation: Option<crate::attest::Attestation>,
    /// Phase breakdown from one extra, **untimed** commit with
    /// `RIFT_HOOK_TIMING=1`: hook name -> phase -> microseconds.
    ///
    /// Untimed and separate because the timer takes clock readings the shipped
    /// default never takes; folding it into the published samples would measure
    /// the instrument.
    pub phases: BTreeMap<String, BTreeMap<String, u64>>,
}

impl ArmResult {
    /// Git latency: this arm's median minus the control's, in nanoseconds.
    /// `None` when either side is missing — never zero, which would read as
    /// "free".
    #[must_use]
    pub fn latency_ns(&self, control: &Stats) -> Option<u64> {
        Some(self.stats?.p50_ns.saturating_sub(control.p50_ns))
    }

    /// This arm's median as a multiple of the control's, the portable number.
    #[must_use]
    pub fn ratio(&self, control: &Stats) -> Option<f64> {
        let stats = self.stats?;
        (control.p50_ns > 0).then(|| stats.p50_ns as f64 / control.p50_ns as f64)
    }
}

/// The three facts an owner needs **together** to rule on a breach.
///
/// "It breaches" tells an owner nothing actionable. "It breaches, here is the
/// curve across three sizes, here is which side of the read/handed-on 2× each
/// number sits on, and here is the phase that owns the cost" tells them
/// everything — and it is the difference between a decision and another round.
///
/// **One caveat travels with every row and is not a disclaimer.** The magnitude
/// (`git_latency_us`) comes from the *median of the timed commits*; the
/// attribution comes from the arm's **one extra untimed commit** with the phase
/// timer on. They are two populations, so the residue below is an anatomy of a
/// representative commit set beside the magnitude of the median — not an exact
/// decomposition of the same commit. Subtracting them as if they were one
/// number is the mistake this doc comment exists to stop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BreachAnatomy {
    /// Which arm.
    pub arm: String,
    /// Bytes the opted-in corpus presents to be **read** — the unit
    /// `MAX_TOTAL_BYTES` is charged in. `None` for an arm that opts nothing in.
    pub bytes_read: Option<usize>,
    /// Bytes of the document handed to the redactor.
    pub document_bytes: Option<usize>,
    /// `document / read`. The ceiling's worst case is 2N + 2; a corpus of long
    /// lines sits near 1.0. Which end an arm is on changes what its number
    /// means.
    pub document_factor: Option<f64>,
    /// Median git latency: this arm's p50 minus the control's, microseconds.
    pub git_latency_us: u64,
    /// Sum of the three commit hooks' own `total_us`, from the phase commit.
    pub hook_bodies_us: u64,
    /// Git latency not inside any hook body: process creation, hook dispatch,
    /// and git's own cost of running three hooks.
    pub outside_hook_bodies_us: u64,
    /// `redact`, exclusive, summed across hooks.
    pub redact_us: u64,
    /// `predict_id` **with `redact` subtracted** — the id derivation and
    /// hashing alone.
    pub predict_id_excl_us: u64,
    /// The hook bodies measured larger than the median git latency. Possible
    /// across two populations (a slow phase commit against a fast median), and
    /// reported rather than clamped silently.
    pub bodies_exceed_latency: bool,
    /// Whether this arm is over the budget.
    pub breaches: bool,
}

/// A whole sweep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookLatencyReport {
    /// Artifact schema ([`HOOK_LATENCY_SCHEMA`]).
    pub schema_version: u32,
    /// Harness package version.
    pub harness_version: String,
    /// Unix seconds; provenance only.
    pub generated_at_unix: u64,
    /// Where it ran.
    pub host: Host,
    /// The rift binary measured, with its freshness provenance.
    pub subject: LegStatus,
    /// The newest source in the checkout the subject was resolved from.
    pub source_clock: Option<SourceClock>,
    /// What `git --version` reported: the control's identity, since a bare
    /// commit's cost is git's, not rift's.
    pub git_version: String,
    /// Iteration counts every arm used.
    pub protocol: Protocol,
    /// The budget, in milliseconds of git latency (owner-set).
    pub budget_ms: u64,
    /// Results in sweep order, control first.
    pub arms: Vec<ArmResult>,
}

impl HookLatencyReport {
    /// The control arm's timing, if it produced one.
    #[must_use]
    pub fn control(&self) -> Option<&Stats> {
        self.arms
            .iter()
            .find(|a| a.arm.is_control())
            .and_then(|a| a.stats.as_ref())
    }

    /// The per-arm anatomy the owner's decision needs.
    ///
    /// Empty without a control: every figure here is control-relative, and a
    /// sweep that lost its control has no latency to decompose.
    #[must_use]
    pub fn anatomy(&self) -> Vec<BreachAnatomy> {
        let Some(control) = self.control().copied() else {
            return Vec::new();
        };
        self.arms
            .iter()
            .filter(|a| !a.arm.is_control())
            .filter_map(|a| {
                let git_latency_us = a.latency_ns(&control)? / 1_000;
                let hook_bodies_us: u64 = a
                    .phases
                    .values()
                    .map(|p| p.get(TOTAL_PHASE).copied().unwrap_or_default())
                    .sum();
                let sum_exclusive = |name: &str| -> u64 {
                    a.phases
                        .values()
                        .map(|p| exclusive_phases(p).get(name).copied().unwrap_or_default())
                        .sum()
                };
                Some(BreachAnatomy {
                    arm: a.arm.id.clone(),
                    bytes_read: a.arm.include.map(|c| c.bytes()),
                    document_bytes: a.arm.include.map(|c| c.document_bytes(CORPUS_DIR)),
                    document_factor: a.arm.include.map(|c| c.document_factor(CORPUS_DIR)),
                    git_latency_us,
                    hook_bodies_us,
                    outside_hook_bodies_us: git_latency_us.saturating_sub(hook_bodies_us),
                    redact_us: sum_exclusive("redact"),
                    predict_id_excl_us: sum_exclusive("predict_id"),
                    bodies_exceed_latency: hook_bodies_us > git_latency_us,
                    breaches: git_latency_us > self.budget_ms.saturating_mul(1_000),
                })
            })
            .collect()
    }

    /// The file stem a sweep is written under.
    #[must_use]
    pub fn stem(&self) -> String {
        format!("hook-latency-{}", self.generated_at_unix)
    }

    /// Write `<dir>/<stem>.json`.
    ///
    /// # Errors
    /// [`BenchError::Io`] / [`BenchError::Json`] on failure.
    pub fn write_json(&self, dir: &Path) -> Result<PathBuf> {
        fs::create_dir_all(dir).map_err(|e| BenchError::io("create report dir", e))?;
        let path = dir.join(format!("{}.json", self.stem()));
        let body = serde_json::to_vec_pretty(self).map_err(|e| BenchError::json("report", e))?;
        fs::write(&path, body).map_err(|e| BenchError::io("write report", e))?;
        Ok(path)
    }

    /// Write `<dir>/<stem>.md`.
    ///
    /// # Errors
    /// [`BenchError::Io`] on failure.
    pub fn write_markdown(&self, dir: &Path) -> Result<PathBuf> {
        fs::create_dir_all(dir).map_err(|e| BenchError::io("create report dir", e))?;
        let path = dir.join(format!("{}.md", self.stem()));
        fs::write(&path, self.to_markdown()).map_err(|e| BenchError::io("write report", e))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooklat::{arms, parse_timings, tests_support, MAX_TOTAL_BYTES};

    /// The decision table: an owner must be able to read the curve, the units
    /// and the owning phase in one pass. Each of the three is asserted, because
    /// a table missing one of them sends the decision back for another round.
    #[test]
    fn the_anatomy_answers_the_curve_the_units_and_the_owning_phase_at_once() {
        let mut report = tests_support::report_with(&[
            ("A_bare", 37),
            ("G1_line_2k", 60),
            ("H3_newline_8k", 260),
        ]);
        // The phase commit for the ceiling arm: 210 ms of hook bodies, of which
        // 150 ms is the redaction and 20 ms the id derivation around it.
        let ceiling = report
            .arms
            .iter_mut()
            .find(|a| a.arm.id == "H3_newline_8k")
            .unwrap();
        ceiling.phases = parse_timings(
            "rift-timing hook=prepare-commit-msg total_us=207000 predict_id_us=170000 \
             redact_us=150000 open_projection_us=2030\n\
             rift-timing hook=commit-msg total_us=140\n\
             rift-timing hook=post-commit total_us=2860\n",
        );

        let rows = report.anatomy();
        assert_eq!(
            rows.len(),
            2,
            "the control is decomposed against, not itself"
        );
        let top = rows.iter().find(|r| r.arm == "H3_newline_8k").unwrap();

        // (1) The curve: the arms are ordered by the bytes they read.
        let small = rows.iter().find(|r| r.arm == "G1_line_2k").unwrap();
        assert_eq!(small.bytes_read, Some(2_048));
        assert_eq!(top.bytes_read, Some(MAX_TOTAL_BYTES));
        assert!(top.git_latency_us > small.git_latency_us);

        // (2) The units: both, and the factor, so which side of the 2× is not
        // left to the reader.
        assert!(top.document_bytes.unwrap() > top.bytes_read.unwrap());
        let factor = top.document_factor.unwrap();
        assert!(
            factor > 1.9,
            "the worst-case twin is the 2N + 2 end: {factor}"
        );

        // (3) The owning phase, exclusive, so redact is not counted twice.
        assert_eq!(top.git_latency_us, 223_000, "260 ms − the 37 ms control");
        assert_eq!(top.hook_bodies_us, 210_000, "the three hooks' own totals");
        assert_eq!(
            top.outside_hook_bodies_us, 13_000,
            "the measured outside-body term"
        );
        assert_eq!(top.redact_us, 150_000);
        assert_eq!(
            top.predict_id_excl_us, 20_000,
            "predict_id less redact, so the two columns may be added"
        );
        assert!(top.breaches, "223 ms against a 100 ms hard budget");
        assert!(!small.breaches);
        assert!(!top.bodies_exceed_latency);

        let md = report.to_markdown();
        assert!(md.contains("**BREACH: `H3_newline_8k`.**"), "{md}");
        assert!(
            md.contains("The corpus was not adjusted in response"),
            "{md}"
        );
        // The three answers are in one table, in one row.
        assert!(
            md.contains(
                "| `H3_newline_8k` | 8192 | 16576 | 2.02× | 223.00 ms | 210.00 ms | 13.00 ms \
 | 150.00 ms | 20.00 ms | **BREACH** |"
            ),
            "{md}"
        );
    }

    /// Every figure in the anatomy is control-relative, so a sweep that lost
    /// its control has nothing to decompose — and says so by being empty rather
    /// than by quoting absolutes as if they were latencies.
    #[test]
    fn an_anatomy_without_a_control_is_empty_rather_than_absolute() {
        let report = tests_support::report_with(&[("H3_newline_8k", 260)]);
        assert!(report.anatomy().is_empty());
        assert!(!report.to_markdown().contains("## Anatomy"));
    }

    /// Two populations can disagree; the report says so rather than clamping a
    /// zero into the column that would then read "process creation is free".
    #[test]
    fn hook_bodies_larger_than_the_median_latency_are_reported_not_clamped() {
        let mut report = tests_support::report_with(&[("A_bare", 37), ("B1_live", 50)]);
        let arm = report
            .arms
            .iter_mut()
            .find(|a| a.arm.id == "B1_live")
            .unwrap();
        arm.phases = parse_timings("rift-timing hook=prepare-commit-msg total_us=90000\n");
        let row = &report.anatomy()[0];
        assert_eq!(row.git_latency_us, 13_000);
        assert!(row.bodies_exceed_latency, "90 ms of body inside 13 ms");
        assert_eq!(row.outside_hook_bodies_us, 0, "saturating, never negative");
        assert!(report
            .to_markdown()
            .contains("reported rather than clamped"));
    }

    #[test]
    fn latency_is_measured_against_the_control_and_absent_without_one() {
        let control = Stats::of(&[crate::measure::Sample {
            wall_ns: 37_000_000,
            exit_code: Some(0),
        }])
        .unwrap();
        let arm = ArmResult {
            attestation: Some(crate::attest::Attestation {
                load1: Some(1.65),
                void_above: crate::attest::VOID_ABOVE_LOAD1,
            }),
            arm: arms()[2].clone(),
            stats: Stats::of(&[crate::measure::Sample {
                wall_ns: 94_000_000,
                exit_code: Some(0),
            }]),
            error: None,
            oracles: Oracles::default(),
            phases: BTreeMap::new(),
        };
        assert_eq!(arm.latency_ns(&control), Some(57_000_000));
        assert!((arm.ratio(&control).unwrap() - 2.540_54).abs() < 1e-4);

        let unmeasured = ArmResult { stats: None, ..arm };
        assert_eq!(unmeasured.latency_ns(&control), None, "absent, never zero");
    }
}

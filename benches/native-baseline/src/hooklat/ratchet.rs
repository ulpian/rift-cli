//! The perf-delta ratchet: **ratios, never absolutes**.
//!
//! The plan asks for a gate that fails on a >10% regression. Gating on one
//! machine's absolute p50 would false-red constantly: absolutes here move about
//! 2× with machine load, and this project has already watched a busy box
//! measure 108.65 s for a test that runs in 8.27 s isolated and nearly file a
//! 13× regression against its own change.
//!
//! So the compared quantity is each arm's ratio to **the bare-commit control
//! measured in the same run**. Machine weather multiplies both terms and
//! cancels; a real regression does not. The absolutes travel with the verdict
//! anyway, because a reader who wants them should not have to reopen the JSON.

use serde::{Deserialize, Serialize};

use super::HookLatencyReport;

/// The default tolerance the plan asks for.
pub const DEFAULT_TOLERANCE_PCT: f64 = 10.0;

/// **By owner decision this ratchet is REPORT-ONLY.**
///
/// It computes and publishes a verdict; it does not fail a build. Measured
/// on the development machine, the 10% gate fails **5 arms of 5 between two
/// identical runs of the same binary** (+19.0%, +98.2%, +37.8%, +56.6%,
/// +39.7%). The ratio half is demonstrably working — the bare control moved
/// 40.06 → 61.63 ms across four runs, a 54% spread, and the ratio absorbs
/// it — but it cannot absorb per-arm run-to-run variance reaching +98%.
///
/// A gate that reds on noise teaches everyone to ignore it, which is worse
/// than having no gate. So it reports until enough runs exist to set a
/// threshold that reflects real variance.
///
/// **A gate that cannot fail must say so in the same breath it prints a
/// verdict**, or it becomes the decorative guard this project has spent
/// two days removing — which is why the banner is emitted by
/// [`to_markdown`] itself and not by its caller.
pub const REPORT_ONLY: bool = true;

/// What characterising the variance would take, so the next person setting
/// a threshold does not start from zero.
///
/// Four runs are **not** enough: `C1_include_50k` and `C2_include_250k`
/// came out with the *same* median ratio (3.25×) over four runs, so a
/// fivefold difference in corpus size was unresolvable. Named here rather
/// than in a ledger nobody re-reads.
pub const VARIANCE_NOTE: &str = "four runs cannot resolve the middle of the curve — the \
     50 KB and 250 KB arms shared a median ratio of 3.25×. A usable threshold needs \
     several runs a side reduced by median-of-medians, more iterations per arm, or arms \
     interleaved rather than run in blocks (block order confounds a slow arm with a \
     drifting machine, which is what produced run 1's inversion that run 2 reversed).";

/// One arm's verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum ArmVerdict {
    /// Both runs measured the arm and the ratio did not grow past tolerance.
    Held {
        /// Ratio in the baseline run.
        baseline: f64,
        /// Ratio in the candidate run.
        candidate: f64,
        /// Percentage change of the ratio; negative is an improvement.
        growth_pct: f64,
    },
    /// The ratio grew past tolerance.
    Regressed {
        /// Ratio in the baseline run.
        baseline: f64,
        /// Ratio in the candidate run.
        candidate: f64,
        /// Percentage change of the ratio.
        growth_pct: f64,
    },
    /// One side has no ratio for this arm.
    ///
    /// **Not a pass.** An arm that stopped being measured is the cheapest
    /// possible way to make a ratchet green, so it is its own verdict and
    /// it fails the gate.
    Uncomparable {
        /// Why.
        reason: String,
    },
}

impl ArmVerdict {
    /// Whether this verdict lets a gate stay green.
    #[must_use]
    pub fn ok(&self) -> bool {
        matches!(self, Self::Held { .. })
    }
}

/// One arm, judged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArmDelta {
    /// The arm identifier.
    pub arm: String,
    /// Its verdict.
    pub verdict: ArmVerdict,
}

/// Compare `candidate` against `baseline`, arm by arm.
///
/// Arms are matched by id and the **union** is judged: an arm the candidate
/// dropped and an arm the baseline never had are both `Uncomparable`, and
/// both fail. Silence is the failure mode a ratchet is most vulnerable to.
#[must_use]
pub fn compare(
    baseline: &HookLatencyReport,
    candidate: &HookLatencyReport,
    tolerance_pct: f64,
) -> Vec<ArmDelta> {
    let mut ids: Vec<String> = baseline
        .arms
        .iter()
        .chain(&candidate.arms)
        .filter(|a| !a.arm.is_control())
        .map(|a| a.arm.id.clone())
        .collect();
    ids.sort_unstable();
    ids.dedup();

    ids.into_iter()
        .map(|arm| ArmDelta {
            verdict: judge(baseline, candidate, &arm, tolerance_pct),
            arm,
        })
        .collect()
}

fn judge(
    baseline: &HookLatencyReport,
    candidate: &HookLatencyReport,
    arm: &str,
    tolerance_pct: f64,
) -> ArmVerdict {
    let (Some(base), Some(cand)) = (ratio(baseline, arm), ratio(candidate, arm)) else {
        return ArmVerdict::Uncomparable {
            reason: format!(
                "`{arm}` has no control-relative ratio in both runs (an arm that stopped \
                 being measured is the cheapest way to make a ratchet green, so this is a \
                 failure, not a skip)"
            ),
        };
    };
    if base <= 0.0 {
        return ArmVerdict::Uncomparable {
            reason: format!("`{arm}`'s baseline ratio is {base}, which nothing can grow from"),
        };
    }
    let growth_pct = (cand - base) / base * 100.0;
    if growth_pct > tolerance_pct {
        ArmVerdict::Regressed {
            baseline: base,
            candidate: cand,
            growth_pct,
        }
    } else {
        ArmVerdict::Held {
            baseline: base,
            candidate: cand,
            growth_pct,
        }
    }
}

fn ratio(report: &HookLatencyReport, arm: &str) -> Option<f64> {
    let control = *report.control()?;
    report
        .arms
        .iter()
        .find(|a| a.arm.id == arm)?
        .ratio(&control)
}

/// Whether every arm held.
#[must_use]
pub fn passed(deltas: &[ArmDelta]) -> bool {
    !deltas.is_empty() && deltas.iter().all(|d| d.verdict.ok())
}

/// What the caller should do with a comparison.
///
/// Exists so the report-only decision is a **value the tests can assert**
/// rather than a branch buried in `main`. It was buried in `main`, and it
/// was not there at all: `--enforce` was declared and never read, so the
/// process printed "this does not fail a build" and then failed the build,
/// identically with and without the flag. A flag nobody reads is worse than
/// a missing flag, because the banner beside it becomes a lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Every arm held; nothing to report.
    Held,
    /// Arms regressed, and the run is report-only ([`REPORT_ONLY`]).
    Reported {
        /// Arms that did not hold.
        failed: usize,
        /// Arms compared.
        total: usize,
    },
    /// Arms regressed and the caller asked to enforce.
    Enforced {
        /// Arms that did not hold.
        failed: usize,
        /// Arms compared.
        total: usize,
    },
}

/// Decide what a comparison means, given whether the caller enforces.
#[must_use]
pub fn outcome(deltas: &[ArmDelta], enforce: bool) -> Outcome {
    if passed(deltas) {
        return Outcome::Held;
    }
    let failed = deltas.iter().filter(|d| !d.verdict.ok()).count();
    let total = deltas.len();
    if enforce {
        Outcome::Enforced { failed, total }
    } else {
        Outcome::Reported { failed, total }
    }
}

/// Render the comparison.
///
/// The report-only banner is emitted **here**, beside the verdict, rather
/// than left to a caller: a gate that cannot fail and does not say so in
/// the same breath as its verdict is the decorative guard this project
/// exists to remove.
#[must_use]
pub fn to_markdown(deltas: &[ArmDelta], tolerance_pct: f64) -> String {
    let mut out = format!(
        "# Hook-latency ratchet\n\n**REPORT-ONLY (the owner's call). This does not fail a build, and it must not be \
         read as a passing gate.** Measured: at {tolerance_pct:.0}% it fails 5 arms of 5 \
         between two identical runs of the same binary (+19.0%, +98.2%, +37.8%, +56.6%, \
         +39.7%), so as specified it would red on noise and teach everyone to ignore it. \
         It publishes a verdict until enough runs exist to set a threshold reflecting real \
         variance: {VARIANCE_NOTE}\n\nRatios to each run's **own** bare-commit control, tolerance {tolerance_pct:.0}%. \
         Absolutes are not compared: they move ~2× with machine load — this harness's own \
         control moved 54% across four runs — so a gate on them false-reds.\n\n\
 | arm | baseline × bare | candidate × bare | change | verdict |\n\
|---|---|---|---|---|\n"
    );
    for delta in deltas {
        match &delta.verdict {
            ArmVerdict::Held {
                baseline,
                candidate,
                growth_pct,
            } => out.push_str(&format!(
                "| `{}` | {baseline:.2}× | {candidate:.2}× | {growth_pct:+.1}% | held |\n",
                delta.arm
            )),
            ArmVerdict::Regressed {
                baseline,
                candidate,
                growth_pct,
            } => out.push_str(&format!(
                "| `{}` | {baseline:.2}× | {candidate:.2}× | {growth_pct:+.1}% | \
                 **REGRESSED** |\n",
                delta.arm
            )),
            ArmVerdict::Uncomparable { reason } => out.push_str(&format!(
                "| `{}` | — | — | — | **UNCOMPARABLE** — {reason} |\n",
                delta.arm
            )),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooklat::tests_support::report_with;

    #[test]
    fn machine_load_that_multiplies_every_arm_is_not_a_regression() {
        // Every absolute doubles — the exact shape a busy machine produces —
        // while the control doubles with them. An absolute gate reds here;
        // a ratio gate must not.
        let base = report_with(&[("A_bare", 37), ("B1_live", 94)]);
        let slow = report_with(&[("A_bare", 74), ("B1_live", 188)]);
        let deltas = compare(&base, &slow, DEFAULT_TOLERANCE_PCT);
        assert_eq!(deltas.len(), 1);
        assert!(passed(&deltas), "{deltas:?}");
        let ArmVerdict::Held { growth_pct, .. } = deltas[0].verdict else {
            panic!("expected held: {:?}", deltas[0].verdict)
        };
        assert!(growth_pct.abs() < 1e-9, "{growth_pct}");
    }

    #[test]
    fn an_arm_that_really_got_slower_relative_to_its_control_regresses() {
        let base = report_with(&[("A_bare", 37), ("B1_live", 94)]);
        // The control is unchanged and the arm grew 20%.
        let worse = report_with(&[("A_bare", 37), ("B1_live", 113)]);
        let deltas = compare(&base, &worse, DEFAULT_TOLERANCE_PCT);
        assert!(!passed(&deltas));
        let ArmVerdict::Regressed { growth_pct, .. } = deltas[0].verdict else {
            panic!("expected regressed: {:?}", deltas[0].verdict)
        };
        assert!(growth_pct > 10.0, "{growth_pct}");
        assert!(to_markdown(&deltas, 10.0).contains("**REGRESSED**"));
    }

    #[test]
    fn a_dropped_arm_fails_rather_than_disappearing() {
        let base = report_with(&[("A_bare", 37), ("B1_live", 94)]);
        let thin = report_with(&[("A_bare", 37)]);
        let deltas = compare(&base, &thin, DEFAULT_TOLERANCE_PCT);
        assert_eq!(deltas.len(), 1, "the union is judged, not the intersection");
        assert!(!passed(&deltas), "not measuring an arm is not passing it");
        assert!(matches!(deltas[0].verdict, ArmVerdict::Uncomparable { .. }));
    }

    #[test]
    fn a_run_with_no_control_cannot_ratchet_anything() {
        let base = report_with(&[("A_bare", 37), ("B1_live", 94)]);
        let no_control = report_with(&[("B1_live", 94)]);
        let deltas = compare(&base, &no_control, DEFAULT_TOLERANCE_PCT);
        assert!(!passed(&deltas));
        assert!(!passed(&compare(&base, &report_with(&[]), 10.0)));
    }

    /// Report-only as a value rather than a promise. The flag was
    /// declared and never read: the verifier ran the built binary with and
    /// without `--enforce` against two committed artifacts and got exit 1
    /// both times, byte-identical output, while the banner said the
    /// opposite.
    #[test]
    fn report_only_and_enforce_reach_different_outcomes_on_the_same_regression() {
        let base = report_with(&[("A_bare", 37), ("B1_live", 94)]);
        let worse = report_with(&[("A_bare", 37), ("B1_live", 113)]);
        let deltas = compare(&base, &worse, DEFAULT_TOLERANCE_PCT);
        assert!(!passed(&deltas));

        assert_eq!(
            outcome(&deltas, false),
            Outcome::Reported {
                failed: 1,
                total: 1
            },
            "the default must not fail a build — the banner says it does not"
        );
        assert_eq!(
            outcome(&deltas, true),
            Outcome::Enforced {
                failed: 1,
                total: 1
            },
            "--enforce must reach a different outcome, or it is decoration"
        );
        assert_ne!(outcome(&deltas, false), outcome(&deltas, true));

        // And a clean comparison is `Held` under both, so the flag changes
        // only what a regression means.
        let same = compare(&base, &base, DEFAULT_TOLERANCE_PCT);
        assert_eq!(outcome(&same, false), Outcome::Held);
        assert_eq!(outcome(&same, true), Outcome::Held);
        // The banner describes the default, so the default is what the
        // CLI must pass: `--enforce` off means report-only.
        assert_eq!(outcome(&deltas, !REPORT_ONLY), outcome(&deltas, false));
    }

    #[test]
    fn an_improvement_holds_however_large() {
        let base = report_with(&[("A_bare", 37), ("B1_live", 200)]);
        let fast = report_with(&[("A_bare", 37), ("B1_live", 50)]);
        assert!(passed(&compare(&base, &fast, DEFAULT_TOLERANCE_PCT)));
    }
}

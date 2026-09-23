//! One scenario's row, and the verdict published beside its number.
//!
//! A verdict is never one word for two facts: the measured hook budget can be
//! met at the median and missed at the tail, and a row that deliberately
//! delivers nothing is held to its ceiling without being judged against it —
//! passing a budget by not doing the work is not passing it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::outcome::{Comparison, LegOutcome};
use crate::{
    leg::LegId,
    measure::{Budget, BudgetVerdict, Protocol},
    scenario::{Delivery, ScenarioId},
};

/// One scenario's results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioReport {
    /// Which scenario.
    pub id: ScenarioId,
    /// Its one-line description.
    pub title: String,
    /// The fixture preset it ran against.
    pub fixture: String,
    /// Part of the headline set.
    pub headline: bool,
    /// Whether it is wired today.
    pub wired: bool,
    /// Iteration counts used.
    pub protocol: Protocol,
    /// The declared bar this measure is judged against, when it states one.
    pub budget: Option<Budget>,
    /// What one work unit is for this row (`checkpoints`, `hooks`, `shards`),
    /// when anything counts them. The rate column is meaningless without it,
    /// and a rate with no named unit is a number wearing a slash.
    #[serde(default)]
    pub work_unit: Option<String>,
    /// A sentence that must be read with this row's number, verbatim.
    ///
    /// A field of the record rather than prose in the markdown, because prose
    /// beside a table is a qualification a reader can quote the number without.
    /// `import-throughput` carries the reconstruction caveat: the figure is
    /// true about our own reconstruction of the pre-Rift format and silent
    /// about the format itself.
    #[serde(default)]
    pub caveat: Option<String>,
    /// Per-leg outcomes, keyed by leg identifier.
    pub legs: BTreeMap<String, LegOutcome>,
    /// The head-to-head, when there was one.
    pub comparison: Option<Comparison>,
}

impl ScenarioReport {
    /// How the rift leg stands against this measure's bar.
    ///
    /// `None` only when there is no bar or nothing clean to judge. It returns
    /// the **structured** verdict rather than a boolean, because the owner
    /// requires p50 and p95 to travel as separate facts: the measured hook
    /// budget can be met at the median and missed at the tail.
    #[must_use]
    pub fn budget_verdict_of(&self) -> Option<BudgetVerdict> {
        let budget = self.budget?;
        let stats = self.legs.get(LegId::Rift.as_str())?.stats()?;
        Some(budget.verdict(stats))
    }

    /// The verdict the report publishes beside the number.
    ///
    /// A replay row is *held to* the hook budget — it is a real hook path and a
    /// regression there is worth seeing — but it is not *judged* against it: it
    /// re-delivers an event whose ingest is a designed no-op, so scoring it
    /// "met" would credit rift for work it deliberately did not do, exactly the
    /// self-flattery the pair of rows exists to prevent.
    #[must_use]
    pub fn budget_verdict(&self) -> &'static str {
        let Some(budget) = self.budget else {
            return "not measured";
        };
        let Some(stats) = self
            .legs
            .get(LegId::Rift.as_str())
            .and_then(LegOutcome::stats)
        else {
            return "not measured";
        };
        if self.is_no_op_path() {
            return "n/a — no-op path";
        }
        match budget.verdict(stats) {
            // **Never one word for two facts, by owner decision.** The measured
            // budget is met at the median and missed at the tail; a single
            // verdict would either credit it with a p95 it does not have or
            // fail a p50 that genuinely meets it.
            BudgetVerdict::Latency { p50_met, p95_met } => match (p50_met, p95_met) {
                (true, true) => "p50 **met** · p95 **met**",
                (true, false) => "p50 **met** · p95 **OVER**",
                (false, true) => "p50 **OVER** · p95 **met**",
                (false, false) => "p50 **OVER** · p95 **OVER**",
            },
            // A throughput row whose corpus handed it nothing is unmeasurable:
            // `OVER` would blame the code for an empty fixture and `met` would
            // credit it for doing nothing.
            BudgetVerdict::NoWork => "unmeasurable — the corpus handed this row no work",
            // A bar nobody set cannot be cleared, and printing
            // "met" here would manufacture a pass out of the absence of a
            // target.
            BudgetVerdict::Unratified => "OPEN — awaiting owner ratification",
            BudgetVerdict::Throughput(true) => "**met**",
            BudgetVerdict::Throughput(false) => "**OVER**",
        }
    }

    /// Whether this row's iterations deliberately write nothing.
    #[must_use]
    pub fn is_no_op_path(&self) -> bool {
        crate::scenario::spec(self.id).delivery == Delivery::Replay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::tests_support::{measured, report, stats};

    /// A throughput row whose corpus handed it nothing must read neither `OVER`
    /// (which blames the code for an empty fixture) nor `met` (which credits it
    /// for doing nothing). This is `verify-throughput` today: wired since the
    /// harness was built, over a fixture that generates no checkpoints.
    #[test]
    fn a_throughput_row_with_an_empty_corpus_is_unmeasurable_not_over() {
        let mut legs = BTreeMap::new();
        legs.insert(
            "rift".to_owned(),
            LegOutcome::Measured {
                command: "rift verify".into(),
                stats: stats(30_000_000, 0).with_units(0),
                peak_rss_bytes: None,
                ingest: None,
                child_spawns: None,
            },
        );
        let mut r = report(legs, None);
        r.scenarios[0].work_unit = Some("checkpoints".to_owned());

        r.scenarios[0].budget = Some(Budget::unratified_throughput());
        assert_eq!(
            r.scenarios[0].budget_verdict(),
            "unmeasurable — the corpus handed this row no work"
        );
        // Even against a floor an owner HAS ratified: an empty corpus is still
        // unmeasurable, not a failure to reach it.
        r.scenarios[0].budget = Some(Budget::Throughput {
            min_units_per_s: Some(500),
        });
        assert_eq!(
            r.scenarios[0].budget_verdict(),
            "unmeasurable — the corpus handed this row no work"
        );
        assert_eq!(
            r.scenarios[0].budget_verdict_of(),
            Some(BudgetVerdict::NoWork)
        );
    }

    #[test]
    fn a_no_op_row_is_held_to_the_budget_but_never_judged_against_it() {
        let mut legs = BTreeMap::new();
        legs.insert("rift".to_owned(), measured(31_000_000));
        let mut r = report(legs, None);
        r.scenarios[0].id = ScenarioId::HookReplay;
        r.scenarios[0].budget = Some(Budget::hook());

        // The comparison still happens — the ceiling is published beside the
        // number, and a regression in the dedup path is still visible.
        assert_eq!(
            r.scenarios[0].budget_verdict_of(),
            Some(BudgetVerdict::Latency {
                p50_met: true,
                p95_met: true
            })
        );
        assert!(r.scenarios[0].is_no_op_path());
        assert_eq!(r.scenarios[0].budget_verdict(), "n/a — no-op path");
        let md = r.to_markdown();
        assert!(md.contains("n/a — no-op path"), "{md}");
        assert!(md.contains("not judged"), "{md}");
        assert!(!md.contains("**met**"), "{md}");

        // Nor is it scored as a miss when it is slow: the verdict does not
        // depend on the number at all, only on what the row delivers.
        r.scenarios[0]
            .legs
            .insert("rift".to_owned(), measured(300_000_000));
        // The structured verdict still says both facts; the *rendered* one is
        // the no-op exemption, which is the row's whole point.
        assert_eq!(
            r.scenarios[0].budget_verdict_of(),
            Some(BudgetVerdict::Latency {
                p50_met: false,
                p95_met: false
            })
        );
        assert_eq!(r.scenarios[0].budget_verdict(), "n/a — no-op path");

        // The very same numbers on the row that *does* deliver a new event are
        // judged, so the exemption is the delivery, not the leniency.
        r.scenarios[0].id = ScenarioId::HookRoundTrip;
        assert!(!r.scenarios[0].is_no_op_path());
        assert_eq!(
            r.scenarios[0].budget_verdict(),
            "p50 **OVER** · p95 **OVER**"
        );
    }
}

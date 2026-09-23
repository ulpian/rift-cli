//! What happened to one leg, and the proof beside it.
//!
//! A leg that was skipped, one that failed and one that measured are three
//! different facts and never collapse into a missing number: a comparison is
//! produced only when both sides measured cleanly, so a head-to-head is never
//! fabricated out of an absent leg.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    leg::{LegId, LegStatus},
    measure::Stats,
};

/// Child processes one invocation of a measured verb launched.
///
/// Counted in a **separate untimed pass**, for the same reason peak RSS is: the
/// count is taken by putting counting shims for the observed programs first on
/// the child's `PATH`, and a shim costs an extra exec per spawn — inside the
/// timed region it would measure the instrument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnCount {
    /// Child processes observed across the shimmed programs.
    pub total: u64,
    /// Per-program breakdown, e.g. `{"git": 2, "ioreg": 1}`. Only the programs
    /// the probe shimmed are counted; a program it could not resolve is absent
    /// rather than recorded as zero.
    pub by_program: BTreeMap<String, u64>,
}

impl SpawnCount {
    /// `"3 (git 2, ioreg 1)"` — the breakdown is what makes the total legible.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.by_program.is_empty() {
            return self.total.to_string();
        }
        let parts: Vec<_> = self
            .by_program
            .iter()
            .map(|(program, n)| format!("{program} {n}"))
            .collect();
        format!("{} ({})", self.total, parts.join(", "))
    }
}

/// Proof that a measured verb reached durable storage, taken from the leg's own
/// counter outside the timed region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestCheck {
    /// Durably captured events after warmup, before the first measured
    /// iteration.
    pub events_before: u64,
    /// The same counter after the last measured iteration.
    pub events_after: u64,
    /// The floor: everything one measured iteration owes — the timed verb's own
    /// event, plus one for each ingesting invocation in its untimed prelude,
    /// times the number of iterations. Held as `>=` because a *lower* count is
    /// the failure being caught; nothing legitimately pushes it higher.
    pub required_growth: u64,
}

impl IngestCheck {
    /// Whether the projection grew by at least one event per measured iteration.
    #[must_use]
    pub fn verified(&self) -> bool {
        self.events_after.saturating_sub(self.events_before) >= self.required_growth
    }
}

/// One leg as resolved for this run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegSummary {
    /// Which leg.
    pub id: LegId,
    /// Present (with provenance) or absent (with a reason).
    pub status: LegStatus,
    /// Binary size — one of the plan's measures, free to collect.
    pub binary_bytes: Option<u64>,
}

/// What happened to one leg in one scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum LegOutcome {
    /// Measured cleanly.
    Measured {
        /// The command line that was timed.
        command: String,
        /// Timing statistics.
        stats: Stats,
        /// Peak RSS from a separate untimed pass, when measurable.
        peak_rss_bytes: Option<u64>,
        /// Proof the timed verb wrote something durable, where the measure needs
        /// one and the leg publishes a counter to prove it with.
        ingest: Option<IngestCheck>,
        /// Child processes one invocation launched, from a separate untimed
        /// pass. `None` when the row does not ask for the count, or the host
        /// cannot take it.
        #[serde(default)]
        child_spawns: Option<SpawnCount>,
    },
    /// Not measured; the reason is published (absent leg, no analogue, or the
    /// scenario is not wired yet).
    Skipped {
        /// Why.
        reason: String,
    },
    /// Ran, but did not succeed. Never reported as a fast number.
    Failed {
        /// What went wrong.
        reason: String,
        /// Statistics of the attempt, when there were samples at all.
        stats: Option<Stats>,
    },
}

impl LegOutcome {
    /// Timing statistics when this leg produced clean ones.
    #[must_use]
    pub fn stats(&self) -> Option<&Stats> {
        match self {
            Self::Measured { stats, .. } => Some(stats),
            Self::Skipped { .. } | Self::Failed { .. } => None,
        }
    }
}

/// The head-to-head, produced **only** when both legs measured cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Comparison {
    /// `reference_p50 / rift_p50` — above 1.0 means rift is faster.
    pub p50_speedup: f64,
    /// `reference_p95 / rift_p95`.
    pub p95_speedup: f64,
}

impl Comparison {
    /// Compute a comparison from two outcomes, or `None` if either is missing.
    #[must_use]
    pub fn of(rift: &LegOutcome, reference: &LegOutcome) -> Option<Self> {
        let (r, e) = (rift.stats()?, reference.stats()?);
        if !r.clean() || !e.clean() || r.p50_ns == 0 || r.p95_ns == 0 {
            return None;
        }
        Some(Self {
            p50_speedup: e.p50_ns as f64 / r.p50_ns as f64,
            p95_speedup: e.p95_ns as f64 / r.p95_ns as f64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{
        tests_support::{measured, report, stats},
        Report,
    };

    #[test]
    fn a_comparison_needs_two_clean_legs() {
        let rift = measured(1_000_000);
        let reference = measured(3_000_000);
        let c = Comparison::of(&rift, &reference).unwrap();
        assert!((c.p50_speedup - 3.0).abs() < 1e-9, "{c:?}");

        let skipped = LegOutcome::Skipped {
            reason: "absent".into(),
        };
        assert!(Comparison::of(&rift, &skipped).is_none());

        let dirty = LegOutcome::Measured {
            command: "x".into(),
            stats: stats(1_000_000, 2),
            peak_rss_bytes: None,
            ingest: None,
            child_spawns: None,
        };
        assert!(
            Comparison::of(&rift, &dirty).is_none(),
            "a failing leg never yields a speedup number"
        );
    }

    #[test]
    fn a_spawn_count_publishes_its_breakdown() {
        let mut by_program = BTreeMap::new();
        by_program.insert("git".to_owned(), 2);
        by_program.insert("ioreg".to_owned(), 1);
        let count = SpawnCount {
            total: 3,
            by_program,
        };
        assert_eq!(count.describe(), "3 (git 2, ioreg 1)");
        // A total with nothing behind it says only the total.
        assert_eq!(
            SpawnCount {
                total: 0,
                by_program: BTreeMap::new()
            }
            .describe(),
            "0"
        );

        let mut legs = BTreeMap::new();
        legs.insert(
            "rift".to_owned(),
            LegOutcome::Measured {
                command: "rift hooks claude-code SessionStart".into(),
                stats: stats(40_000_000, 0),
                peak_rss_bytes: None,
                ingest: None,
                child_spawns: Some(count),
            },
        );
        let r = report(legs, None);
        let md = r.to_markdown();
        assert!(md.contains("## Child processes"), "{md}");
        assert!(md.contains("3 (git 2, ioreg 1)"), "{md}");
        // And it survives the artifact round trip under the bumped schema.
        let back: Report = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);

        // A report with no counts grows no section at all.
        let mut plain = BTreeMap::new();
        plain.insert("rift".to_owned(), measured(1_000_000));
        assert!(!report(plain, None)
            .to_markdown()
            .contains("Child processes"));
    }

    /// A `2` report has no `child_spawns` at all; it must still load, because
    /// the perf gate will read yesterday's artifacts.
    #[test]
    fn an_earlier_schemas_measured_outcome_still_loads() {
        let json = r#"{"outcome":"measured","command":"rift status",
            "stats":{"samples":1,"failures":0,"min_ns":1,"p50_ns":1,"p95_ns":1,
                     "max_ns":1,"mean_ns":1},
            "peak_rss_bytes":null,"ingest":null}"#;
        let back: LegOutcome = serde_json::from_str(json).unwrap();
        let LegOutcome::Measured { child_spawns, .. } = back else {
            panic!("measured")
        };
        assert_eq!(child_spawns, None);
    }
}

//! The versioned record a run leaves behind, and how it is written out.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{host::Host, outcome::LegSummary, row::ScenarioReport};
use crate::{fixture::FixtureManifest, freshness::SourceClock, leg::LegStatus, BenchError, Result};

/// Schema version of the report artifact.
///
/// `9` reads the **machine** into the record: `attestation`, the one-minute
/// load average taken when the run started, with the ceiling above which a run
/// may not derive a floor. Without it a scenario artifact was the one family
/// the artifact-consistency guard could not judge on load at all — a hole with
/// a name, since three of the four owed floors are derived from this family.
/// `8` gives a row a `caveat`: a sentence that must be read with its number,
/// carried in the RECORD rather than in prose beside a table. `import-throughput`
/// is why — its rate is about our own reconstruction of the pre-Rift format and
/// says nothing about the format, and a caveat only a markdown reader meets is
/// one a JSON consumer quotes the number without.
/// `7` records **which commit** a number is about: `git_describe` and `dirty`
/// on every leg this checkout can honestly speak for. A `6` report
/// carries an mtime and nothing else, and an mtime cannot distinguish two
/// builds of two branches taken a minute apart — so a `6` artifact is loadable,
/// is evidence, and **may not derive a floor**. Absent is not fresh.
/// `6` records the **scripted-session driver** beside the subject. The condense
/// row's whole workload is produced by `vogon`, and a report that named only
/// the binary under test left the binary that decided what the test *was*
/// unnamed — the same provenance hole the freshness guard was built to close,
/// one rung out.
/// `5` gives a measure a **denominator**: `Stats::units_per_iteration` (probed,
/// never assumed), `ScenarioReport::work_unit`, and a [`Budget`](crate::measure::Budget) that is now a
/// two-variant enum — a latency ceiling *or* a throughput floor, because a rate
/// bar cannot be expressed as a wall time. A `4` report deserializes only at the
/// old shape: its `budget` objects carry no `kind` tag and its rows have no
/// rate, so a `4` and a `5` cannot be compared on throughput at all.
/// `4` adds the provenance that says a number is about the tree it is published
/// against: [`LegStatus::Present`]'s `binary_mtime_unix`, and the run's
/// [`SourceClock`] — the newest source modification time in the checkout, with
/// the file that carried it. Before `4` a resolved binary was measured with no
/// freshness check at all, and a side checkout's release build was thirteen
/// days behind its own sources. A `3` report cannot be compared with a `4` one
/// on that axis: it does not record whether the binary contained the code.
/// `3` adds [`SpawnCount`](super::SpawnCount) — the child processes one invocation of a measured
/// verb launches — and redefines `IngestCheck::required_growth` from "one event
/// per measured iteration" to every event the iteration owes, its untimed
/// prelude included (a reader comparing a `2` report with a `3` one is
/// comparing two different floors). `2` added the hook rows' [`IngestCheck`](super::IngestCheck)
/// and the declared [`Budget`](crate::measure::Budget) a measure is judged against; `1` had neither.
pub const REPORT_SCHEMA: u32 = 9;

/// A whole run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    /// Artifact schema version ([`REPORT_SCHEMA`]).
    pub schema_version: u32,
    /// Harness package version.
    pub harness_version: String,
    /// Unix seconds — provenance only; nothing in the harness derives behavior
    /// from the clock.
    pub generated_at_unix: u64,
    /// Where it ran.
    pub host: Host,
    /// The newest source modification time in the checkout the run resolved its
    /// binaries from, and the file that carried it. `None` when the harness
    /// found no checkout — which is reported as unknown, never as fresh.
    #[serde(default)]
    pub source_clock: Option<SourceClock>,
    /// What the machine was doing when the run started.
    ///
    /// One reading for the run rather than one per row: a scenario run's rows
    /// share a process and a sitting, unlike a sweep's arms, which are minutes
    /// apart and get one each. `None` is unknown, and unknown is VOID for floor
    /// derivation — an unread machine is not a quiet one.
    #[serde(default)]
    pub attestation: Option<crate::attest::Attestation>,
    /// Both legs as resolved.
    pub legs: Vec<LegSummary>,
    /// The scripted-session driver (`vogon`) as resolved, when any row needed
    /// one. Not a leg — never measured, never compared — but it *authors* the
    /// condense row's workload, so a number about that row is only traceable if
    /// the report says which driver produced it.
    #[serde(default)]
    pub driver: Option<LegStatus>,
    /// The fixtures used.
    pub fixtures: Vec<FixtureManifest>,
    /// Results, in catalogue order.
    pub scenarios: Vec<ScenarioReport>,
}

impl Report {
    /// The file stem a run is written under.
    #[must_use]
    pub fn stem(&self) -> String {
        format!("native-baseline-{}", self.generated_at_unix)
    }

    /// Write `<dir>/<stem>.json`, creating `dir`.
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

    /// Write `<dir>/<stem>.md`, creating `dir`.
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
    use std::collections::BTreeMap;

    use super::*;
    use crate::report::{
        tests_support::{measured, report},
        LegOutcome,
    };

    #[test]
    fn a_report_round_trips_through_json_with_its_schema_version() {
        let mut legs = BTreeMap::new();
        legs.insert("rift".to_owned(), measured(2_000_000));
        legs.insert(
            "reference".to_owned(),
            LegOutcome::Skipped {
                reason: "leg absent".into(),
            },
        );
        let report = report(legs, None);
        let json = serde_json::to_string(&report).unwrap();
        // The report's own version, not the fixture manifest's (which is also
        // called `schema_version` and is a different number).
        assert!(
            json.starts_with(&format!("{{\"schema_version\":{REPORT_SCHEMA}")),
            "{json}"
        );
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
        assert_eq!(back.schema_version, 9);
    }
}

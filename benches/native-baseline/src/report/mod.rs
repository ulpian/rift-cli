//! The versioned benchmark report — the artifact the report-format decision
//! asks for.
//!
//! Everything a number depends on travels with it: both legs' resolved binary,
//! version and how it was found; the fixture manifests (spec + head oid); the
//! host; the iteration protocol; and the sample count behind every statistic. A
//! reader can therefore tell *which build, over which repository* produced a
//! figure — the minimum bar for publishing a comparison.
//!
//! The artifact carries a `schema_version` because it is a serialized record that
//! outlives the run that produced it and will be read by later tooling (the perf
//! ratchet).

mod artifact;
mod host;
mod markdown;
mod outcome;
mod row;

pub use self::artifact::{Report, REPORT_SCHEMA};
pub use self::host::{now_unix, Host};
pub use self::markdown::ms;
pub use self::outcome::{Comparison, IngestCheck, LegOutcome, LegSummary, SpawnCount};
pub use self::row::ScenarioReport;

#[cfg(test)]
pub(crate) mod tests_support {
    use std::{collections::BTreeMap, path::PathBuf};

    use super::{Comparison, Host, LegOutcome, LegSummary, Report, ScenarioReport, REPORT_SCHEMA};
    use crate::{
        fixture::{FixtureManifest, FixtureSpec, FIXTURE_MANIFEST_SCHEMA},
        leg::{LegId, LegStatus},
        measure::{Protocol, Stats},
        scenario::ScenarioId,
    };

    /// Clean statistics at a chosen median, with `failures` attempts that did
    /// not succeed.
    pub(crate) fn stats(p50: u64, failures: usize) -> Stats {
        Stats {
            units_per_iteration: None,
            samples: 10,
            failures,
            min_ns: p50 / 2,
            p50_ns: p50,
            p95_ns: p50 * 2,
            max_ns: p50 * 3,
            mean_ns: p50,
        }
    }

    /// A leg that measured cleanly at `p50`.
    pub(crate) fn measured(p50: u64) -> LegOutcome {
        LegOutcome::Measured {
            command: "rift status".into(),
            stats: stats(p50, 0),
            peak_rss_bytes: Some(1 << 20),
            ingest: None,
            child_spawns: None,
        }
    }

    /// A one-scenario report carrying the legs and comparison given.
    pub(crate) fn report(
        legs: BTreeMap<String, LegOutcome>,
        comparison: Option<Comparison>,
    ) -> Report {
        Report {
            schema_version: REPORT_SCHEMA,
            harness_version: "0.0.1".into(),
            generated_at_unix: 1_800_000_000,
            host: Host::detect("unavailable".into()),
            driver: None,
            attestation: Some(crate::attest::Attestation {
                load1: Some(1.65),
                void_above: crate::attest::VOID_ABOVE_LOAD1,
            }),
            source_clock: Some(crate::freshness::SourceClock {
                newest_unix: 1_799_999_000,
                newest_path: PathBuf::from("bins/rift/src/hooks.rs"),
                files: 1234,
                newest_subsec_nanos: 0,
            }),
            legs: vec![
                LegSummary {
                    id: LegId::Rift,
                    status: LegStatus::Present {
                        binary: PathBuf::from("/x/rift"),
                        version: "rift 0.0.1".into(),
                        source: "repo target/release".into(),
                        binary_mtime_unix: Some(1_799_999_500),
                        git_describe: Some(format!("ls-597-g{}", "b".repeat(40))),
                        dirty: Some(false),
                    },
                    binary_bytes: Some(4096 * 1024),
                },
                LegSummary {
                    id: LegId::Reference,
                    status: LegStatus::Absent {
                        reason: "not provisioned; the harness never downloads it".into(),
                    },
                    binary_bytes: None,
                },
            ],
            fixtures: vec![FixtureManifest {
                schema_version: FIXTURE_MANIFEST_SCHEMA,
                spec: FixtureSpec::small(),
                spec_digest: "deadbeefdeadbeef".into(),
                head_oid: "0123456789abcdef0123456789abcdef01234567".into(),
                worktree_files: 12,
                worktree_bytes: 4096,
            }],
            scenarios: vec![ScenarioReport {
                work_unit: None,
                caveat: None,
                id: ScenarioId::ColdStatus,
                title: "cold status".into(),
                fixture: "small".into(),
                headline: true,
                wired: true,
                protocol: Protocol::latency(),
                budget: None,
                legs,
                comparison,
            }],
        }
    }
}

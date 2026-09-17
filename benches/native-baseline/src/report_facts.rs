//! The ```` ```bench-report ```` fence: the aggregate across every benchmark
//! family, generated from the tracked artifacts and recomputed by the
//! artifact-consistency guard (its fence arm).
//!
//! `benchmarks/hook-latency.md` carries one ```` ```bench-facts ```` fence for
//! the git-latency family. The other families — the scenario runs under
//! `benchmarks/native-baseline/`, the displacement sweeps — had no aggregate
//! at all: sixty-odd artifacts, a floors file, and no one number set that
//! said what they add up to. This fence is that set, and it lives in
//! `benchmarks/README.md` under its own tag so the two fences can share a
//! document without either reader seeing the other (see
//! [`crate::facts::extract_tagged`]).
//!
//! # One home per number
//!
//! The git-latency arms stay in `hook-latency.md`'s fence; this one carries
//! only their **counts** (artifacts, records, load-void runs). Every
//! throughput row carries its scenario's caveat verbatim from the catalogue
//! (`ScenarioSpec::caveat`), so the caveat rides the fence rather than
//! sitting in prose beside it. Integers only, as in `facts.rs`: rates are
//! milli-units per second, times are nanoseconds, medians are nearest-rank
//! (an observed run, never an interpolation).

use serde::{Deserialize, Serialize};

use crate::{
    audit::{Artifact, Inputs},
    displace::Act,
    facts,
    floors::{FloorKind, FloorStatus, Floors},
    measure::{nearest_rank, Budget},
    report::LegOutcome,
    scenario::{self, Delivery, ScenarioId},
};

/// The fence's language tag.
pub const REPORT_TAG: &str = "bench-report";

/// Schema of the fence body.
pub const REPORT_SCHEMA: u32 = 1;

/// The sentence a row prints while no clean run measures it.
pub const NOT_YET_MEASURED: &str = "not yet measured in a clean run";

/// One artifact family under `benchmarks/`: `<family>-<stamp>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Family {
    /// The stem prefix.
    pub family: String,
    /// The stamps found, sorted.
    pub artifacts: Vec<u64>,
    /// How many are records (no provenance, or a dirty tree).
    pub records: usize,
    /// How many ran on a machine over the load ceiling (or did not read it).
    pub load_void: usize,
}

/// One throughput floor scenario, reduced across the runs that measured it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThroughputFacts {
    /// The scenario id.
    pub scenario: String,
    /// What one unit of its work is.
    pub work_unit: String,
    /// Runs with a clean rift measurement.
    pub runs: usize,
    /// Nearest-rank median across runs of the median-iteration rate, in
    /// milli-units per second.
    pub rate_milli_per_s_median: Option<u64>,
    /// The declared floor, milli-units per second; `None` while underived.
    pub floor_milli_per_s: Option<u64>,
    /// The floor row's status token (`underived` | `derived` |
    /// `owner-ratified`).
    pub status: String,
    /// Runs whose rate fell under a floor that binds.
    pub runs_under_floor: usize,
    /// The catalogue's caveat for this row, verbatim.
    pub caveat: Option<String>,
}

/// One latency row of the catalogue, reduced across the runs that measured it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatencyFacts {
    /// The scenario id.
    pub scenario: String,
    /// Runs with a clean rift measurement.
    pub runs: usize,
    /// Nearest-rank median across runs of the per-run p50, nanoseconds.
    pub p50_ns_median: Option<u64>,
    /// Nearest-rank median across runs of the per-run p95, nanoseconds.
    pub p95_ns_median: Option<u64>,
    /// The p50 ceiling the catalogue states, milliseconds — `None` when no
    /// bar is ratified, and for the replay row, which is held to the hook
    /// budget but never judged against it.
    pub budget_p50_ms: Option<u64>,
    /// The p95 ceiling, milliseconds.
    pub budget_p95_ms: Option<u64>,
    /// Runs whose p50 was over the ceiling.
    pub runs_over_p50: usize,
    /// Runs whose p95 was over the ceiling.
    pub runs_over_p95: usize,
}

/// The enable-displacement family: the one arm the git-latency budget binds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplacementFacts {
    /// Runs with a first-commit-after-enable git latency.
    pub runs: usize,
    /// Nearest-rank median across runs of that arm's git latency against its
    /// own run's control, nanoseconds.
    pub first_commit_after_enable_ns_median: Option<u64>,
    /// The hard budget, milliseconds, from the floors file.
    pub budget_ms: Option<u64>,
    /// Runs over it.
    pub runs_over_budget: usize,
}

/// The git-latency family, counts only — its arms have their home in
/// `hook-latency.md`'s own fence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitLatencyFacts {
    /// Sweeps under `benchmarks/`.
    pub artifacts: usize,
    /// Of which records.
    pub records: usize,
    /// Of which load-void.
    pub load_void: usize,
}

/// Everything `benchmarks/README.md` may be gated on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportFacts {
    /// Fence schema.
    pub schema_version: u32,
    /// Every artifact family found, by name.
    pub families: Vec<Family>,
    /// One row per throughput floor in the floors file, in file order.
    pub throughput: Vec<ThroughputFacts>,
    /// One row per catalogue latency row (no work unit), in catalogue order.
    pub latency: Vec<LatencyFacts>,
    /// The displacement family.
    pub displacement: DisplacementFacts,
    /// The git-latency family, counts only.
    pub git_latency: GitLatencyFacts,
}

/// Nearest-rank median over an unsorted slice; `None` for nothing.
fn median(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    Some(nearest_rank(&sorted, 50))
}

/// A rate in milli-units per second, rounded half up, saturating.
fn milli(rate: f64) -> u64 {
    let scaled = (rate * 1000.0).round();
    if scaled <= 0.0 {
        0
    } else if scaled >= u64::MAX as f64 {
        u64::MAX
    } else {
        // Truncation is exact here: `scaled` is integral and in range.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let n = scaled as u64;
        n
    }
}

const fn status_token(status: FloorStatus) -> &'static str {
    match status {
        FloorStatus::Underived => "underived",
        FloorStatus::Derived => "derived",
        FloorStatus::OwnerRatified => "owner-ratified",
    }
}

impl ReportFacts {
    /// Reduce everything under `benchmarks/` (already loaded) into the fence.
    ///
    /// `floors` is the tracked declaration when it parsed; without it the
    /// throughput and displacement rows are empty — a fence generated over
    /// a broken floors file must not look like a fence over a clean one, and
    /// the guard's floors arm says why.
    #[must_use]
    pub fn of(inputs: &Inputs, floors: Option<&Floors>) -> Self {
        Self {
            schema_version: REPORT_SCHEMA,
            families: families(inputs),
            throughput: floors.map(|f| throughput(inputs, f)).unwrap_or_default(),
            latency: latency(inputs),
            displacement: displacement(inputs, floors),
            git_latency: git_latency(inputs),
        }
    }

    /// The fenced block, ready to splice into a report.
    #[must_use]
    pub fn render_fence(&self) -> String {
        facts::render_fence_tagged(REPORT_TAG, self)
    }

    /// The human tables the fence is printed beside — commentary, never the
    /// gate. Rendered from the same struct so they cannot drift from it.
    #[must_use]
    pub fn render_table(&self) -> String {
        let ms = |ns: Option<u64>| {
            ns.map_or_else(
                || NOT_YET_MEASURED.to_owned(),
                |ns| format!("{:.2} ms", ns as f64 / 1_000_000.0),
            )
        };
        let mut out = String::from(
            "| throughput row | runs | rate (median of run medians) | floor | status | under the floor |\n\
             |---|---|---|---|---|---|\n",
        );
        for t in &self.throughput {
            let rate = t.rate_milli_per_s_median.map_or_else(
                || NOT_YET_MEASURED.to_owned(),
                |m| format!("{:.2} {}/s", m as f64 / 1000.0, t.work_unit),
            );
            let floor = t.floor_milli_per_s.map_or_else(
                || "—".to_owned(),
                |m| format!("{:.2} {}/s", m as f64 / 1000.0, t.work_unit),
            );
            out.push_str(&format!(
                "| `{}` | {} | {rate} | {floor} | {} | {} / {} |\n",
                t.scenario, t.runs, t.status, t.runs_under_floor, t.runs
            ));
        }
        out.push_str(
            "\n| latency row | runs | p50 (median of runs) | p95 (median of runs) | budget | over p50 | over p95 |\n\
             |---|---|---|---|---|---|---|\n",
        );
        for l in &self.latency {
            let budget = match (l.budget_p50_ms, l.budget_p95_ms) {
                (Some(p50), Some(p95)) => format!("p50 ≤ {p50} ms, p95 ≤ {p95} ms"),
                _ => "none ratified".to_owned(),
            };
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {budget} | {} / {} | {} / {} |\n",
                l.scenario,
                l.runs,
                ms(l.p50_ns_median),
                ms(l.p95_ns_median),
                l.runs_over_p50,
                l.runs,
                l.runs_over_p95,
                l.runs
            ));
        }
        let d = &self.displacement;
        out.push_str(&format!(
            "\n| displacement | runs | first commit after enable (median of runs) | budget | over |\n\
             |---|---|---|---|---|\n| `enable-displacement` | {} | {} | {} | {} / {} |\n",
            d.runs,
            ms(d.first_commit_after_enable_ns_median),
            d.budget_ms
                .map_or_else(|| "—".to_owned(), |b| format!("{b} ms, hard")),
            d.runs_over_budget,
            d.runs
        ));
        let g = &self.git_latency;
        out.push_str(&format!(
            "\n| git latency | artifacts | records | load-void |\n|---|---|---|---|\n\
             | `hook-latency` (arms live in `hook-latency.md`'s fence) | {} | {} | {} |\n",
            g.artifacts, g.records, g.load_void
        ));
        out
    }
}

/// The families under `benchmarks/`, by stem prefix.
fn families(inputs: &Inputs) -> Vec<Family> {
    let mut by_name: std::collections::BTreeMap<&str, Family> = std::collections::BTreeMap::new();
    for (stem, artifact) in &inputs.artifacts {
        let Some((family, stamp)) = stem
            .rsplit_once('-')
            .and_then(|(head, tail)| tail.parse::<u64>().ok().map(|s| (head, s)))
        else {
            continue;
        };
        let entry = by_name.entry(family).or_insert_with(|| Family {
            family: family.to_owned(),
            artifacts: Vec::new(),
            records: 0,
            load_void: 0,
        });
        entry.artifacts.push(stamp);
        entry.records += usize::from(artifact.is_record());
        entry.load_void += usize::from(artifact.load_void());
    }
    by_name
        .into_values()
        .map(|mut f| {
            f.artifacts.sort_unstable();
            f
        })
        .collect()
}

/// The rift leg's clean stats for `scenario`, one per scenario-run artifact.
fn rift_stats(inputs: &Inputs, scenario: &str) -> Vec<crate::measure::Stats> {
    let mut out = Vec::new();
    for artifact in inputs.artifacts.values() {
        let Artifact::VsReference(r) = artifact else {
            continue;
        };
        for s in &r.scenarios {
            if s.id.as_str() != scenario {
                continue;
            }
            if let Some(stats) = s
                .legs
                .get(crate::leg::LegId::Rift.as_str())
                .and_then(LegOutcome::stats)
            {
                if stats.clean() {
                    out.push(*stats);
                }
            }
        }
    }
    out
}

fn throughput(inputs: &Inputs, floors: &Floors) -> Vec<ThroughputFacts> {
    floors
        .floors
        .iter()
        .filter(|f| f.kind == FloorKind::Throughput)
        .map(|floor| {
            let rates: Vec<f64> = rift_stats(inputs, &floor.scenario)
                .iter()
                .filter_map(crate::measure::Stats::rate_per_s)
                .collect();
            let millis: Vec<u64> = rates.iter().map(|r| milli(*r)).collect();
            let binding = floor.binds().then_some(floor.min_units_per_s).flatten();
            ThroughputFacts {
                scenario: floor.scenario.clone(),
                work_unit: floor.work_unit.clone(),
                runs: rates.len(),
                rate_milli_per_s_median: median(&millis),
                floor_milli_per_s: floor.min_units_per_s.map(milli),
                status: status_token(floor.status).to_owned(),
                runs_under_floor: binding
                    .map_or(0, |min| rates.iter().filter(|rate| **rate < min).count()),
                caveat: ScenarioId::parse(&floor.scenario)
                    .and_then(|id| scenario::spec(id).caveat)
                    .map(str::to_owned),
            }
        })
        .collect()
}

fn latency(inputs: &Inputs) -> Vec<LatencyFacts> {
    scenario::catalogue()
        .iter()
        // A row nothing has measured yet is ABSENT here, not
        // published as a zero: the fence holds every row it carries to three
        // clean runs, and a row declared ahead of its measurement would either red
        // that or force it to be loosened for every row. `measurement_owed`
        // names the work that owes it, and the guard that retires the state
        // lives beside the fence test below.
        .filter(|s| s.work_unit.is_none() && s.measurement_owed.is_none())
        .map(|spec| {
            let stats = rift_stats(inputs, spec.id.as_str());
            let p50: Vec<u64> = stats.iter().map(|s| s.p50_ns).collect();
            let p95: Vec<u64> = stats.iter().map(|s| s.p95_ns).collect();
            let bar = match spec.budget {
                Some(Budget::Latency { p50_ms, p95_ms }) if spec.delivery != Delivery::Replay => {
                    Some((p50_ms, p95_ms))
                }
                _ => None,
            };
            let over = |values: &[u64], ceiling_ms: Option<u64>| {
                ceiling_ms.map_or(0, |ms| {
                    let ns = ms.saturating_mul(1_000_000);
                    values.iter().filter(|v| **v > ns).count()
                })
            };
            LatencyFacts {
                scenario: spec.id.as_str().to_owned(),
                runs: stats.len(),
                p50_ns_median: median(&p50),
                p95_ns_median: median(&p95),
                budget_p50_ms: bar.map(|(p50, _)| p50),
                budget_p95_ms: bar.map(|(_, p95)| p95),
                runs_over_p50: over(&p50, bar.map(|(p50, _)| p50)),
                runs_over_p95: over(&p95, bar.map(|(_, p95)| p95)),
            }
        })
        .collect()
}

fn displacement(inputs: &Inputs, floors: Option<&Floors>) -> DisplacementFacts {
    let budget_ms = floors
        .and_then(|f| f.get("enable-displacement"))
        .and_then(|f| f.budget_ms)
        .map(|b| milli(b) / 1000);
    let mut latencies: Vec<u64> = Vec::new();
    for artifact in inputs.artifacts.values() {
        let Artifact::Displacement(r) = artifact else {
            continue;
        };
        latencies.extend(
            r.arms
                .iter()
                .filter(|a| a.arm.act == Act::FirstCommitAfterEnable)
                .filter_map(|a| r.latency_ns(a)),
        );
    }
    DisplacementFacts {
        runs: latencies.len(),
        first_commit_after_enable_ns_median: median(&latencies),
        budget_ms,
        runs_over_budget: budget_ms.map_or(0, |b| {
            let ns = b.saturating_mul(1_000_000);
            latencies.iter().filter(|l| **l > ns).count()
        }),
    }
}

fn git_latency(inputs: &Inputs) -> GitLatencyFacts {
    let sweeps: Vec<&Artifact> = inputs
        .artifacts
        .values()
        .filter(|a| matches!(a, Artifact::HookLatency(_)))
        .collect();
    GitLatencyFacts {
        artifacts: sweeps.len(),
        records: sweeps.iter().filter(|a| a.is_record()).count(),
        load_void: sweeps.iter().filter(|a| a.load_void()).count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leg::default_repo_root;

    /// The fence over the tracked tree: every floors scenario appears, every
    /// catalogue latency row appears, the families are the three that exist,
    /// and the block round-trips through a document under its own tag
    /// beside a `bench-facts` fence.
    #[test]
    fn the_tracked_tree_reduces_to_one_fence_that_round_trips() {
        let root = default_repo_root();
        let inputs = Inputs::load(&root);
        let facts = ReportFacts::of(&inputs, inputs.floors.as_ref());
        let names: Vec<&str> = facts.families.iter().map(|f| f.family.as_str()).collect();
        assert_eq!(
            names,
            ["enable-displacement", "hook-latency", "native-baseline"]
        );
        let throughput: Vec<&str> = facts
            .throughput
            .iter()
            .map(|t| t.scenario.as_str())
            .collect();
        assert_eq!(
            throughput,
            ["import-throughput", "verify-throughput", "condense"]
        );
        for t in &facts.throughput {
            assert!(t.runs >= 3, "{} has {} runs", t.scenario, t.runs);
            assert!(t.rate_milli_per_s_median.is_some());
            // `owner-ratified`, not `derived`: the three throughput bars were
            // derived by the halving convention, but the history holding their
            // subject commits was collapsed to a fresh root, so the medians and
            // their artifact stems could no longer be resolved by anyone. The
            // owner ratified the same numbers as set bars instead — a decision
            // a reader can check beats a derivation nobody can re-run — and the
            // runs behind them are now kept as records.
            assert_eq!(t.status, "owner-ratified");
        }
        let import = facts
            .throughput
            .iter()
            .find(|t| t.scenario == "import-throughput")
            .unwrap();
        assert!(
            import
                .caveat
                .as_deref()
                .is_some_and(|c| c.contains("Nothing here was fetched")),
            "C2a rides the fence"
        );
        let latency: Vec<&str> = facts.latency.iter().map(|l| l.scenario.as_str()).collect();
        assert!(latency.contains(&"hook-stop"));
        assert!(latency.contains(&"resume-restore"));
        assert!(latency.contains(&"blame-why"));
        let replay = facts
            .latency
            .iter()
            .find(|l| l.scenario == "hook-replay")
            .unwrap();
        assert_eq!(replay.budget_p50_ms, None, "held to, never judged against");
        assert!(facts.displacement.runs >= 3);
        assert_eq!(facts.displacement.budget_ms, Some(100));
        assert!(facts.git_latency.artifacts > 0);

        let doc = format!(
            "# Benchmarks\n\n{}\n{}",
            facts.render_fence(),
            crate::facts::BenchFacts::of(&[], String::new()).render_fence()
        );
        let parsed: ReportFacts = facts::parse_tagged(&doc, REPORT_TAG, REPORT_SCHEMA).unwrap();
        assert_eq!(parsed, facts);
        let table = facts.render_table();
        // The 2026-08-29 sitting measured every latency row over three clean
        // runs, so the published tree carries no "not yet measured" row; the
        // honest-absence rendering itself is pinned by
        // `an_empty_tree_yields_an_empty_fence_with_every_latency_row_unmeasured`.
        // A row added later reds this until a clean run measures it — a
        // published report must not carry an unmeasured row.
        assert!(
            facts.latency.iter().all(|l| l.runs >= 3),
            "every latency row is measured over >= 3 clean runs: {table}"
        );
        assert!(!table.contains(NOT_YET_MEASURED), "{table}");
        assert!(table.contains("| `condense` |"));
    }

    /// The owed state is not a hiding place, and it retires itself.
    ///
    /// A row kept out of the fence by `measurement_owed` must have nothing to
    /// publish: the moment a clean run for one exists in the tracked tree, this
    /// reds and whoever measured it has to take it off the list. So the
    /// field can defer a number and can never suppress one.
    #[test]
    fn an_owed_row_has_no_measurement_to_publish() {
        let inputs = Inputs::load(&default_repo_root());
        let facts = ReportFacts::of(&inputs, inputs.floors.as_ref());
        for spec in scenario::catalogue() {
            let Some(owed_by) = spec.measurement_owed else {
                continue;
            };
            assert!(
                !owed_by.is_empty(),
                "{} owes its measurement to nobody: name who will take it",
                spec.id.as_str()
            );
            assert!(
                rift_stats(&inputs, spec.id.as_str()).is_empty(),
                "{} is measured in the tracked tree and still declares itself owed to {owed_by}: \
                 publish it",
                spec.id.as_str()
            );
            assert!(
                !facts.latency.iter().any(|l| l.scenario == spec.id.as_str()),
                "{} is owed and must be absent from the fence, not published as a zero",
                spec.id.as_str()
            );
        }
    }

    /// No artifacts and no floors: an honest empty fence, every latency row
    /// present and unmeasured, nothing invented.
    #[test]
    fn an_empty_tree_yields_an_empty_fence_with_every_latency_row_unmeasured() {
        let facts = ReportFacts::of(&Inputs::default(), None);
        assert!(facts.families.is_empty());
        assert!(facts.throughput.is_empty());
        assert_eq!(facts.displacement.runs, 0);
        assert_eq!(facts.displacement.budget_ms, None);
        assert!(!facts.latency.is_empty());
        assert!(facts
            .latency
            .iter()
            .all(|l| l.runs == 0 && l.p50_ns_median.is_none()));
        assert_eq!(milli(59.0), 59_000);
        assert_eq!(milli(124.737_96), 124_738);
        assert_eq!(milli(-1.0), 0);
    }
}

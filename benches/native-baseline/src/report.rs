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

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    fixture::FixtureManifest,
    freshness::SourceClock,
    leg::{LegId, LegStatus},
    measure::{Budget, BudgetVerdict, Protocol, Stats},
    scenario::{Delivery, ScenarioId},
    BenchError, Result,
};

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
/// never assumed), `ScenarioReport::work_unit`, and a [`Budget`] that is now a
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
/// `3` adds [`SpawnCount`] — the child processes one invocation of a measured
/// verb launches — and redefines `IngestCheck::required_growth` from "one event
/// per measured iteration" to every event the iteration owes, its untimed
/// prelude included (a reader comparing a `2` report with a `3` one is
/// comparing two different floors). `2` added the hook rows' [`IngestCheck`]
/// and the declared [`Budget`] a measure is judged against; `1` had neither.
pub const REPORT_SCHEMA: u32 = 9;

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

/// Where the run happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    /// `std::env::consts::OS`.
    pub os: String,
    /// `std::env::consts::ARCH`.
    pub arch: String,
    /// Logical CPUs, when the platform reports them.
    pub logical_cpus: Option<usize>,
    /// How peak RSS was obtained (or `unavailable`).
    pub rss_probe: String,
}

impl Host {
    /// Describe the current host.
    #[must_use]
    pub fn detect(rss_probe: String) -> Self {
        Self {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            logical_cpus: std::thread::available_parallelism().ok().map(Into::into),
            rss_probe,
        }
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

    /// Render the human-readable report.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# rift vs the pinned capture backend\n\n");
        out.push_str(&format!(
            "- harness `{}`, report schema {}\n- host: {} / {} ({} logical CPUs)\n\
             - peak RSS via {}\n\n",
            self.harness_version,
            self.schema_version,
            self.host.os,
            self.host.arch,
            self.host
                .logical_cpus
                .map_or_else(|| "?".to_owned(), |n| n.to_string()),
            self.host.rss_probe,
        ));
        out.push_str(&format!(
            "- machine: {}\n\n",
            self.attestation
                .map_or_else(|| "load1 unknown".to_owned(), |a| a.describe())
        ));
        self.legs_section(&mut out);
        self.fixtures_section(&mut out);
        self.measures_sections(&mut out);
        self.caveats_section(&mut out);
        self.unmeasurable_section(&mut out);
        self.budgets_section(&mut out);
        self.spawns_section(&mut out);
        self.protocol_note(&mut out);
        self.not_measured_section(&mut out);
        out
    }

    /// The sentences that must be read with the numbers above.
    ///
    /// Its own section, immediately after the measures and before the budgets,
    /// because a caveat placed after the verdict is one a reader meets only if
    /// they were not looking for the verdict.
    fn caveats_section(&self, out: &mut String) {
        let rows: Vec<_> = self
            .scenarios
            .iter()
            .filter_map(|s| s.caveat.as_ref().map(|c| (s.id, c)))
            .collect();
        if rows.is_empty() {
            return;
        }
        out.push_str("\n## Read these with the numbers\n\n");
        for (id, caveat) in rows {
            out.push_str(&format!("- **`{}`** — {caveat}\n", id.as_str()));
        }
    }

    /// Rows that ran and moved nothing.
    ///
    /// Its own section rather than a footnote under Budgets, because a row can
    /// be empty without carrying a budget at all — and the first version of
    /// this note lived under Budgets, where exactly that row would have been
    /// silent.
    fn unmeasurable_section(&self, out: &mut String) {
        let empty: Vec<_> = self
            .scenarios
            .iter()
            .filter(|s| {
                s.legs
                    .get(LegId::Rift.as_str())
                    .and_then(LegOutcome::stats)
                    .is_some_and(Stats::moved_no_work)
            })
            .map(|s| format!("`{}`", s.id.as_str()))
            .collect();
        if empty.is_empty() {
            return;
        }
        out.push_str(&format!(
            "\n## Unmeasurable as a rate\n\n{} ran and moved **zero work**, so they have no \
             rate: `0/s` would read as \"it did work, slowly\" and blame the code for a corpus \
             that handed it nothing. The zero is published as the work count rather than \
             hidden — a measure whose denominator is empty has never been about any work, and \
             that is the finding, not a footnote.\n",
            empty.join(", ")
        ));
    }

    /// The declared ceilings next to the numbers that answer them, plus the proof
    /// each number did the work it claims.
    fn budgets_section(&self, out: &mut String) {
        let rows: Vec<_> = self
            .scenarios
            .iter()
            .filter(|s| s.budget.is_some())
            .collect();
        if rows.is_empty() {
            return;
        }
        out.push_str(
            "\n## Budgets\n\n | measure | rift p50 | rift p95 | rate | work/it | budget | verdict | ingest proof |\n\
 |---|---|---|---|---|---|---|---|\n",
        );
        for s in rows {
            let outcome = s.legs.get(LegId::Rift.as_str());
            let stats = outcome.and_then(LegOutcome::stats);
            let cell =
                |pick: fn(&Stats) -> u64| stats.map_or_else(|| "—".to_owned(), |st| ms(pick(st)));
            let budget = s
                .budget
                .map_or_else(String::new, |b| b.describe(s.work_unit.as_deref()));
            let verdict = s.budget_verdict();
            let proof = match outcome {
                Some(LegOutcome::Measured {
                    ingest: Some(check),
                    ..
                }) => format!(
                    "+{} events (≥ {} required)",
                    check.events_after.saturating_sub(check.events_before),
                    check.required_growth
                ),
                Some(LegOutcome::Measured { ingest: None, .. }) => "none available".to_owned(),
                _ => "—".to_owned(),
            };
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | {budget} | {verdict} | {proof} |\n",
                s.id.as_str(),
                cell(|st| st.p50_ns),
                cell(|st| st.p95_ns),
                rate_cell(stats, s.work_unit.as_deref()),
                work_cell(stats, s.work_unit.as_deref()),
            ));
        }
        if self.scenarios.iter().any(|s| {
            matches!(
                s.budget_verdict_of(),
                Some(BudgetVerdict::Latency {
                    p50_met: true,
                    p95_met: false
                })
            )
        }) {
            out.push_str(
                "\n**A row reading `p50 met · p95 OVER` is reported as two facts on purpose, \
                 by owner decision.** Collapsing the pair into one word would either credit the \
                 budget with a p95 it does not have or fail a median that genuinely meets it. \
                 Process creation and hook dispatch remain visible in the same-run control and \
                 the hook phase breakdown.\n",
            );
        }
        let no_ops: Vec<_> = self
            .scenarios
            .iter()
            .filter(|s| s.budget.is_some() && s.is_no_op_path())
            .map(|s| format!("`{}`", s.id.as_str()))
            .collect();
        if !no_ops.is_empty() {
            out.push_str(&format!(
                "\nThe `n/a` rows ({}) re-deliver an event whose ingest is a designed no-op. \
                 Such a row is kept because the dedup short-circuit is a real hook path worth \
                 a number, and it is held to the same ceiling — but it is **not judged** \
                 against it, because passing a budget by not doing the work is not passing \
                 it.\n",
                no_ops.join(", ")
            ));
        }
    }

    /// The child processes a measured verb launches — a cost that is invisible
    /// in a wall time until someone asks what the wall time is made of.
    fn spawns_section(&self, out: &mut String) {
        let rows: Vec<_> = self
            .scenarios
            .iter()
            .filter_map(|s| {
                let counts: Vec<_> = s
                    .legs
                    .iter()
                    .filter_map(|(leg, outcome)| match outcome {
                        LegOutcome::Measured {
                            child_spawns: Some(count),
                            ..
                        } => Some((leg.clone(), count)),
                        _ => None,
                    })
                    .collect();
                (!counts.is_empty()).then_some((s, counts))
            })
            .collect();
        if rows.is_empty() {
            return;
        }
        out.push_str(
            "\n## Child processes\n\n\
             One invocation of the measured verb, counted in a separate untimed pass \
             (the counting shims cost an exec each, so they must never sit inside a \
             published timing).\n\n\
 | measure | leg | child processes |\n|---|---|---|\n",
        );
        for (scenario, counts) in rows {
            for (leg, count) in counts {
                out.push_str(&format!(
                    "| `{}` | `{leg}` | {} |\n",
                    scenario.id.as_str(),
                    count.describe()
                ));
            }
        }
    }

    /// What the percentiles in this report do and do not mean.
    fn protocol_note(&self, out: &mut String) {
        out.push_str(
            "\nPercentiles are nearest-rank over the measured samples: every figure is an \
             observed run, never an interpolation.\n",
        );
        let thin: Vec<_> = self
            .scenarios
            .iter()
            .filter(|s| s.protocol.iterations < 20 && s.legs.values().any(|o| o.stats().is_some()))
            .map(|s| format!("`{}` (n={})", s.id.as_str(), s.protocol.iterations))
            .collect();
        if !thin.is_empty() {
            out.push_str(&format!(
                "With fewer than 20 iterations the nearest-rank p95 **is** the maximum, so it \
                 carries no tail information and no threshold may be set on it: {}.\n",
                thin.join(", ")
            ));
        }
    }

    /// The legs, and the freshness evidence that says a number is about this
    /// tree. `built` and the source clock are printed together on purpose: two
    /// timestamps in different sections are an exercise for the reader, and the
    /// question "is this binary the code?" has to be answerable at a glance.
    fn legs_section(&self, out: &mut String) {
        out.push_str(
            "## Legs\n\n| leg | state | binary | version | size | built | commit |\n |---|---|---|---|---|---|---|\n",
        );
        for leg in &self.legs {
            let size = leg
                .binary_bytes
                .map_or_else(|| "—".to_owned(), |b| format!("{} KiB", b / 1024));
            match &leg.status {
                LegStatus::Present {
                    binary,
                    version,
                    source,
                    binary_mtime_unix,
                    git_describe,
                    dirty,
                } => out.push_str(&format!(
                    "| `{}` | present ({source}) | `{}` | {version} | {size} | {} | {} |\n",
                    leg.id.as_str(),
                    binary.display(),
                    binary_mtime_unix.map_or_else(|| "—".to_owned(), |t| t.to_string()),
                    commit_cell(git_describe.as_deref(), *dirty),
                )),
                LegStatus::Absent { reason } => out.push_str(&format!(
                    "| `{}` | **absent** | — | — | — | — | — |\n| | {reason} | | | | | |\n",
                    leg.id.as_str()
                )),
            }
        }
        if let Some(driver) = &self.driver {
            match driver {
                LegStatus::Present {
                    binary,
                    version,
                    source,
                    binary_mtime_unix,
                    git_describe,
                    dirty,
                } => out.push_str(&format!(
                    "| `vogon` (driver) | present ({source}) | `{}` | {version} | — | {} | {} |\n",
                    binary.display(),
                    binary_mtime_unix.map_or_else(|| "—".to_owned(), |t| t.to_string()),
                    commit_cell(git_describe.as_deref(), *dirty),
                )),
                LegStatus::Absent { reason } => out.push_str(&format!(
                    "| `vogon` (driver) | **absent** | — | — | — | — | — |\n| | {reason} | | | | | |\n"
                )),
            }
        }
        match &self.source_clock {
            Some(clock) => out.push_str(&format!(
                "\nNewest source in the checkout: `{}` at **{}** ({} files scanned). A rift                  binary older than that is refused rather than measured — a timing taken from                  a binary that predates its own sources is real and is about the wrong code.\n",
                clock.newest_path.display(),
                clock.newest_unix,
                clock.files,
            )),
            None => out.push_str(
                "\nNo source clock: the harness found no checkout to compare the binaries                  against, so **freshness was not checked** — which is not the same as checked                  and clean.\n",
            ),
        }
    }

    fn fixtures_section(&self, out: &mut String) {
        out.push_str("\n## Fixtures\n\n| fixture | files | bytes | commits | turns | head |\n|---|---|---|---|---|---|\n");
        for f in &self.fixtures {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | `{}` |\n",
                f.spec.name,
                f.worktree_files,
                f.worktree_bytes,
                f.spec.commits,
                f.spec.session_turns,
                f.head_oid.get(..12).unwrap_or(&f.head_oid),
            ));
        }
    }

    fn measures_sections(&self, out: &mut String) {
        for (heading, headline) in [("Headline measures", true), ("Other measures", false)] {
            let rows: Vec<_> = self
                .scenarios
                .iter()
                .filter(|s| s.headline == headline)
                .collect();
            if rows.is_empty() {
                continue;
            }
            out.push_str(&format!(
                "\n## {heading}\n\n | measure | rift p50 | rift p95 | rate | work/it | backend p50 | backend p95 |                  speedup | n |\n|---|---|---|---|---|---|---|---|---|\n"
            ));
            for s in rows {
                let cell = |leg: LegId, pick: fn(&Stats) -> u64| {
                    s.legs
                        .get(leg.as_str())
                        .and_then(LegOutcome::stats)
                        .map_or_else(|| "—".to_owned(), |st| ms(pick(st)))
                };
                let n = s
                    .legs
                    .get(LegId::Rift.as_str())
                    .and_then(LegOutcome::stats)
                    .map_or_else(|| "0".to_owned(), |st| st.samples.to_string());
                let speedup = s
                    .comparison
                    .map_or_else(|| "—".to_owned(), |c| format!("{:.2}×", c.p50_speedup));
                let rift = s.legs.get(LegId::Rift.as_str()).and_then(LegOutcome::stats);
                out.push_str(&format!(
                    "| `{}` | {} | {} | {} | {} | {} | {} | {speedup} | {n} |\n",
                    s.id.as_str(),
                    cell(LegId::Rift, |st| st.p50_ns),
                    cell(LegId::Rift, |st| st.p95_ns),
                    rate_cell(rift, s.work_unit.as_deref()),
                    work_cell(rift, s.work_unit.as_deref()),
                    cell(LegId::Reference, |st| st.p50_ns),
                    cell(LegId::Reference, |st| st.p95_ns),
                ));
            }
        }
    }

    fn not_measured_section(&self, out: &mut String) {
        out.push_str("\n## Not measured\n\n");
        let mut any = false;
        // A leg the run did not ask for is absent from the results but present in
        // the legs table; saying so is the difference between "measured nothing"
        // and "was never asked". Said once per leg rather than once per scenario:
        // with the backend off the default set that was sixteen identical lines,
        // and a reader who skims past a repeated line skims past the ones that
        // are not repeated.
        for leg in &self.legs {
            let missing = self
                .scenarios
                .iter()
                .filter(|s| !s.legs.contains_key(leg.id.as_str()))
                .count();
            if missing > 0 {
                any = true;
                out.push_str(&format!(
                    "- `{}`: not requested by this run (`--legs`) — {missing} of {} scenarios\n",
                    leg.id.as_str(),
                    self.scenarios.len()
                ));
            }
        }
        for s in &self.scenarios {
            for (leg, outcome) in &s.legs {
                match outcome {
                    LegOutcome::Skipped { reason } => {
                        any = true;
                        out.push_str(&format!("- `{}` / `{leg}`: {reason}\n", s.id.as_str()));
                    }
                    LegOutcome::Failed { reason, .. } => {
                        any = true;
                        out.push_str(&format!(
                            "- `{}` / `{leg}`: FAILED — {reason}\n",
                            s.id.as_str()
                        ));
                    }
                    LegOutcome::Measured { .. } => {}
                }
            }
        }
        if !any {
            out.push_str("- nothing: every declared measure ran on every leg.\n");
        }
    }
}

/// Format nanoseconds as milliseconds.
#[must_use]
pub fn ms(ns: u64) -> String {
    format!("{:.2} ms", ns as f64 / 1_000_000.0)
}

/// The commit a leg's number is about.
///
/// Three renderings for three different states, and none of them is blank: a
/// clean commit, a **dirty** one (a tree that exists nowhere else, so the run
/// is a record and derives no floor), and *unknown* — which is what a leg whose
/// sources are not in this checkout honestly is, and is not the same claim as
/// clean.
fn commit_cell(describe: Option<&str>, dirty: Option<bool>) -> String {
    match (describe, dirty) {
        (Some(d), Some(true)) => format!("`{d}` **DIRTY**"),
        (Some(d), _) => format!("`{d}`"),
        (None, _) => "unknown".to_owned(),
    }
}

/// The rate cell: work per second at the median iteration, in the row's unit.
///
/// `—` where nothing counted the work. That is not a formatting nicety: a row
/// with no denominator has no rate, and printing one anyway would invent it.
fn rate_cell(stats: Option<&Stats>, unit: Option<&str>) -> String {
    if stats.is_some_and(Stats::moved_no_work) {
        return format!("**unmeasurable** — 0 {}", unit.unwrap_or("units"));
    }
    let Some(rate) = stats.and_then(Stats::rate_per_s) else {
        return "—".to_owned();
    };
    let unit = unit.unwrap_or("units");
    if rate >= 100.0 {
        format!("{rate:.0} {unit}/s")
    } else {
        format!("{rate:.2} {unit}/s")
    }
}

/// The corroboration probe, printed beside the timing it explains: how much work
/// one measured iteration actually performed.
///
/// **`0` and `—` say different things and must never be collapsed.** `0` is a
/// measure that ran and moved nothing — the case where a fast wall time is
/// meaningless and the reader has to be able to see it. `—` is a measure nobody
/// counted, where the timing may be perfectly good and simply has no
/// denominator.
fn work_cell(stats: Option<&Stats>, unit: Option<&str>) -> String {
    let Some(units) = stats.and_then(|st| st.units_per_iteration) else {
        return "—".to_owned();
    };
    format!("{units} {}", unit.unwrap_or("units"))
}

/// Wall-clock seconds, for report provenance only.
#[must_use]
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{FixtureSpec, FIXTURE_MANIFEST_SCHEMA};

    fn stats(p50: u64, failures: usize) -> Stats {
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

    fn measured(p50: u64) -> LegOutcome {
        LegOutcome::Measured {
            command: "rift status".into(),
            stats: stats(p50, 0),
            peak_rss_bytes: Some(1 << 20),
            ingest: None,
            child_spawns: None,
        }
    }

    fn report(legs: BTreeMap<String, LegOutcome>, comparison: Option<Comparison>) -> Report {
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

    /// The provenance, rendered where a reader meets the number.
    /// `unknown` and `clean` are different claims and the cell says which.
    #[test]
    fn the_commit_a_number_is_about_is_printed_beside_the_leg() {
        let mut legs = BTreeMap::new();
        legs.insert("rift".to_owned(), measured(2_000_000));
        let md = report(legs, None).to_markdown();
        assert!(
            md.contains(&format!("`ls-597-g{}`", "b".repeat(40))),
            "{md}"
        );
        assert!(md.contains("| commit |"), "the column exists: {md}");

        assert_eq!(commit_cell(None, None), "unknown");
        assert_eq!(commit_cell(None, Some(false)), "unknown");
        assert_eq!(commit_cell(Some("abc"), Some(false)), "`abc`");
        assert_eq!(commit_cell(Some("abc"), None), "`abc`");
        assert_eq!(commit_cell(Some("abc"), Some(true)), "`abc` **DIRTY**");
    }

    #[test]
    fn a_budget_is_published_beside_the_number_that_answers_it() {
        let mut legs = BTreeMap::new();
        legs.insert(
            "rift".to_owned(),
            LegOutcome::Measured {
                command: "rift hooks claude-code Stop".into(),
                stats: stats(40_000_000, 0),
                peak_rss_bytes: None,
                ingest: Some(IngestCheck {
                    events_before: 100,
                    events_after: 190,
                    required_growth: 30,
                }),
                child_spawns: None,
            },
        );
        let mut r = report(legs, None);
        r.scenarios[0].budget = Some(Budget::hook());
        assert_eq!(
            r.scenarios[0].budget_verdict_of(),
            Some(BudgetVerdict::Latency {
                p50_met: true,
                p95_met: true
            })
        );
        let md = r.to_markdown();
        assert!(md.contains("## Budgets"), "{md}");
        assert!(md.contains("p50 ≤ 100 ms, p95 ≤ 500 ms"), "{md}");
        assert!(md.contains("**met**"), "{md}");
        assert!(md.contains("+90 events (≥ 30 required)"), "{md}");

        // p95 is 2× p50 in this fixture, so 300 ms p50 blows both halves.
        let over = LegOutcome::Measured {
            command: "rift hooks claude-code SessionStart".into(),
            stats: stats(300_000_000, 0),
            peak_rss_bytes: None,
            ingest: None,
            child_spawns: None,
        };
        r.scenarios[0].legs.insert("rift".to_owned(), over);
        assert_eq!(
            r.scenarios[0].budget_verdict_of(),
            Some(BudgetVerdict::Latency {
                p50_met: false,
                p95_met: false
            })
        );
        let md = r.to_markdown();
        assert!(md.contains("p50 **OVER** · p95 **OVER**"), "{md}");
        // No counter, no claim.
        assert!(md.contains("none available"), "{md}");
    }

    /// The unratified-floor rule rendered: a throughput row whose floor nobody has set
    /// reports `OPEN`, never green — and the rate and its probed denominator
    /// are printed together, because a rate whose work count is invisible is
    /// the corroboration failure wearing a slash.
    #[test]
    fn an_unratified_throughput_row_reports_open_with_its_rate_and_its_work_count() {
        let mut legs = BTreeMap::new();
        legs.insert(
            "rift".to_owned(),
            LegOutcome::Measured {
                command: "rift verify".into(),
                // 2 s median, 40 checkpoints re-derived: 20 checkpoints/s.
                stats: stats(2_000_000_000, 0).with_units(40),
                peak_rss_bytes: None,
                ingest: None,
                child_spawns: None,
            },
        );
        let mut r = report(legs, None);
        r.scenarios[0].budget = Some(Budget::unratified_throughput());
        r.scenarios[0].work_unit = Some("checkpoints".to_owned());

        assert_eq!(
            r.scenarios[0].budget_verdict_of(),
            Some(BudgetVerdict::Unratified),
            "an unset floor is unjudgeable, and unjudgeable is not passed"
        );
        assert_eq!(
            r.scenarios[0].budget_verdict(),
            "OPEN — awaiting owner ratification"
        );
        let md = r.to_markdown();
        assert!(md.contains("OPEN — awaiting owner ratification"), "{md}");
        assert!(md.contains("checkpoints/s — no floor ratified"), "{md}");
        assert!(md.contains("20.00 checkpoints/s"), "{md}");
        assert!(md.contains("40 checkpoints"), "{md}");
        assert!(!md.contains("**met**"), "never green: {md}");
    }

    /// **The two-facts headline case, which had no test until a plant found
    /// that out.** Deleting the `p50 met · p95 OVER` arm and collapsing it to
    /// `**OVER**` left every test green — the one rendering the rule exists
    /// for was the one nothing covered. This is the measured shape: the
    /// median clears the hook budget while the tail does not.
    #[test]
    fn a_median_that_meets_and_a_tail_that_does_not_are_rendered_as_two_facts() {
        let mut legs = BTreeMap::new();
        legs.insert(
            "rift".to_owned(),
            LegOutcome::Measured {
                command: "git commit".into(),
                // The fixture's p95 is 2x its p50, so 80 ms clears the 100 ms
                // median ceiling while 160 ms misses... the 500 ms p95 ceiling
                // is generous, so the budget is stated explicitly instead.
                stats: stats(80_000_000, 0),
                peak_rss_bytes: None,
                ingest: None,
                child_spawns: None,
            },
        );
        let mut r = report(legs, None);
        r.scenarios[0].budget = Some(Budget::Latency {
            p50_ms: 100,
            p95_ms: 120,
        });

        assert_eq!(
            r.scenarios[0].budget_verdict_of(),
            Some(BudgetVerdict::Latency {
                p50_met: true,
                p95_met: false
            }),
            "the two facts must survive as two"
        );
        assert_eq!(
            r.scenarios[0].budget_verdict(),
            "p50 **met** · p95 **OVER**",
            "collapsing this into one word credits a p95 the budget does not have"
        );

        let md = r.to_markdown();
        assert!(md.contains("p50 **met** · p95 **OVER**"), "{md}");
        assert!(
            md.contains("reported as two facts on purpose, by owner decision"),
            "the reason travels with the verdict: {md}"
        );
        assert!(
            md.contains("Process creation and hook dispatch remain visible"),
            "and it keeps the measurable terms visible: {md}"
        );
    }

    /// The condense row's workload is authored by a binary that is not the
    /// subject, so the report has to name it. A number traceable to the tested
    /// binary but not to the binary that decided what the test *was* is only
    /// half traceable.
    #[test]
    fn the_scripted_session_driver_is_named_in_the_report_or_its_absence_is() {
        let mut r = report(BTreeMap::new(), None);
        assert!(
            !r.to_markdown().contains("vogon"),
            "a run with no driven row must not invent a driver"
        );

        r.driver = Some(LegStatus::Present {
            binary: PathBuf::from("/x/target/release/vogon"),
            version: "unknown".into(),
            source: "repo target/release".into(),
            binary_mtime_unix: Some(1_787_118_443),
            git_describe: Some(format!("ls-597-g{}", "c".repeat(40))),
            dirty: Some(false),
        });
        let md = r.to_markdown();
        assert!(md.contains("`vogon` (driver)"), "{md}");
        assert!(md.contains("/x/target/release/vogon"), "{md}");
        // The mtime is the provenance that matters here: `vogon` answers no
        // `--version`, so freshness is the only thing distinguishing two builds.
        assert!(md.contains("1787118443"), "{md}");

        r.driver = Some(LegStatus::Absent {
            reason: "no vogon binary: no $RIFT_BENCH_VOGON_BIN".into(),
        });
        let md = r.to_markdown();
        assert!(md.contains("**absent**"), "{md}");
        assert!(
            md.contains("RIFT_BENCH_VOGON_BIN"),
            "an absent driver says why: {md}"
        );
    }

    /// The distinction the probe exists for: a measure that ran and moved
    /// nothing prints `0`, a measure nobody counted prints `—`. Collapsing them
    /// is how a fast wall time over an empty repository reads as a good number.
    #[test]
    fn zero_work_and_uncounted_work_render_differently() {
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
        let md = r.to_markdown();
        assert!(md.contains("0 checkpoints"), "the zero is published: {md}");
        assert!(
            !md.contains("0.00 checkpoints/s"),
            "an empty corpus has no rate; 0/s blames the code: {md}"
        );
        assert!(md.contains("**unmeasurable** — 0 checkpoints"), "{md}");
        assert!(
            md.contains("## Unmeasurable as a rate") && md.contains("`cold-status` ran"),
            "the reason is stated, not left to the reader: {md}"
        );

        let uncounted = LegOutcome::Measured {
            command: "rift verify".into(),
            stats: stats(30_000_000, 0),
            peak_rss_bytes: None,
            ingest: None,
            child_spawns: None,
        };
        r.scenarios[0].legs.insert("rift".to_owned(), uncounted);
        let md = r.to_markdown();
        assert!(!md.contains("0 checkpoints"), "absent is not zero: {md}");
        assert!(
            !md.contains("unmeasurable"),
            "uncounted is not the same claim as empty: {md}"
        );
    }

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

    #[test]
    fn a_thin_protocol_says_its_p95_is_only_the_maximum() {
        let mut legs = BTreeMap::new();
        legs.insert("rift".to_owned(), measured(2_000_000));
        let mut r = report(legs, None);
        assert!(
            !r.to_markdown().contains("carries no tail information"),
            "30 iterations is a real percentile"
        );
        r.scenarios[0].protocol = Protocol::heavy();
        let md = r.to_markdown();
        assert!(md.contains("nearest-rank"), "{md}");
        assert!(md.contains("carries no tail information"), "{md}");
        assert!(md.contains("`cold-status` (n=3)"), "{md}");
    }

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
    fn markdown_publishes_absence_and_skip_reasons() {
        let mut legs = BTreeMap::new();
        legs.insert("rift".to_owned(), measured(2_500_000));
        legs.insert(
            "reference".to_owned(),
            LegOutcome::Skipped {
                reason: "backend not provisioned".into(),
            },
        );
        let md = report(legs, None).to_markdown();
        assert!(md.contains("**absent**"));
        assert!(md.contains("the harness never downloads it"));
        assert!(md.contains("backend not provisioned"));
        assert!(md.contains("2.50 ms"), "{md}");
        // No fabricated head-to-head when one leg never ran.
        assert!(md.contains("| — | — | — |"), "{md}");
    }

    #[test]
    fn a_leg_that_was_never_asked_is_not_reported_as_measured() {
        // Found by running the harness on this repository: `--legs rift` left
        // the backend out of the results,
        // and the summary read "every declared measure ran on every leg".
        let mut legs = BTreeMap::new();
        legs.insert("rift".to_owned(), measured(2_000_000));
        let md = report(legs, None).to_markdown();
        assert!(md.contains("not requested by this run"), "{md}");
        assert!(!md.contains("every declared measure ran on every leg"));
    }

    #[test]
    fn failures_are_rendered_as_failures() {
        let mut legs = BTreeMap::new();
        legs.insert(
            "rift".to_owned(),
            LegOutcome::Failed {
                reason: "exit 2: not a rift repository".into(),
                stats: Some(stats(1_000, 3)),
            },
        );
        let md = report(legs, None).to_markdown();
        assert!(md.contains("FAILED — exit 2"));
    }

    /// The twin of `hooklat`'s indentation guard, and it exists because that
    /// one did not cover this renderer: an indented budgets-table header was
    /// planted here and the other test passed. CommonMark renders 4+ leading
    /// spaces as a code block, so an indented table is not a table.
    #[test]
    fn no_generated_line_is_indented_into_a_code_block() {
        let mut legs = BTreeMap::new();
        legs.insert("rift".to_owned(), measured(40_000_000));
        let mut r = report(legs, None);
        r.scenarios[0].budget = Some(Budget::hook());
        r.scenarios[0].work_unit = Some("checkpoints".to_owned());
        for (n, line) in r.to_markdown().lines().enumerate() {
            assert!(
                !line.starts_with("    ") || line.trim().is_empty(),
                "line {n} is indented and will render as a code block: {line:?}"
            );
        }
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

    #[test]
    fn formatting_helpers_are_stable() {
        assert_eq!(ms(1_500_000), "1.50 ms");
        assert_eq!(ms(0), "0.00 ms");
        assert!(now_unix() > 1_700_000_000);
    }
}

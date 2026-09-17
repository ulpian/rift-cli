//! The artifact-consistency guard.
//!
//! # What it is, and the thing it is not
//!
//! This answers exactly one question: **do the published numbers, the published
//! floors and the published provenance agree with each other?** It never
//! answers "is this machine fast today". Measured variance on this hardware
//! reaches +98% between two identical runs of the same binary
//! (`benchmarks/hook-latency.md` §9.8), so a live performance gate here would
//! red on weather and teach everyone to ignore it — which is worse than no
//! gate, and is exactly why the owner kept the ratchet report-only. The
//! floors this guard reads are a **publication convention**, halved so that a
//! *tracked record* can be checked for internal consistency; the halving is not
//! a noise budget.
//!
//! # The five arms
//!
//! * **Enumeration.** Every scenario that is owed a floor has a row in
//!   `benchmarks/floors.toml`. This is the arm that is fully live before any
//!   measurement has been taken at all, and it is what stops a scenario being
//!   dropped from the set while the gate stays green.
//! * **(i) Violation.** A measured value in `benchmarks/**.json` that misses a
//!   floor which binds.
//! * **(ii) Missing artifact.** A binding floor with nothing measuring it. A row
//!   that is still `underived` reports **PENDING** instead — loudly, with the
//!   command that would fill it — because a placeholder is a statement about
//!   what has not been done, and reddening on it would only tempt someone to
//!   delete the row.
//! * **(iii) Staleness.** An artifact that claims to be current must carry
//!   provenance, its subject must be an ancestor of `HEAD`, and — for the
//!   scenarios it measures — it must contain `HEAD`'s last change to that
//!   scenario's declared measured paths. An artifact with no provenance, or
//!   taken over a dirty tree, is a **record**: kept, quotable as history, and
//!   barred from deriving a floor.
//! * **(iv) Fence disagreement.** A report's single ```bench-facts fence is
//!   recomputed from exactly the artifacts it names, and must match. The fence
//!   is the only gate-bearing number set in any `benchmarks/*.md`. The same
//!   arm covers the all-families ```bench-report fence (`benchmarks/README.md`):
//!   recomputed from the stamps its `families` name, a named artifact that is
//!   not under `benchmarks/` is a finding, and every scenario the floors file
//!   declares must appear in it — a floor dropped from the aggregate must not
//!   leave a green gate behind it.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use crate::{
    displace::{Act, DisplacementReport},
    facts::{self, BenchFacts},
    floors::{Floor, FloorKind, Floors},
    hooklat::HookLatencyReport,
    leg::LegStatus,
    provenance::{self, GitProvenance},
    report::{LegOutcome, Report},
};

/// The scenarios that are owed a floor row, verbatim.
pub const OWED_SCENARIOS: &[&str] = &[
    "import-throughput",
    "verify-throughput",
    "condense",
    "enable-displacement",
];

/// Which arm a finding came from — so a reader can tell a stale artifact from a
/// slow one without reading the sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm {
    /// A scenario that is owed a floor has no row at all.
    Enumeration,
    /// The floors file does not hold together on its own terms.
    FloorsFile,
    /// (i) a measured value violates a floor that binds.
    Violation,
    /// (ii) a binding floor has no artifact.
    MissingArtifact,
    /// (iii) an artifact claims to be current and is not.
    Staleness,
    /// (iv) a fence and its own artifacts disagree.
    FenceDisagreement,
}

impl Arm {
    /// The label a finding is printed under.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enumeration => "ENUMERATION",
            Self::FloorsFile => "FLOORS-FILE",
            Self::Violation => "VIOLATION (i)",
            Self::MissingArtifact => "MISSING-ARTIFACT (ii)",
            Self::Staleness => "STALENESS (iii)",
            Self::FenceDisagreement => "FENCE-DISAGREEMENT (iv)",
        }
    }
}

/// One thing that does not agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Which arm caught it.
    pub arm: Arm,
    /// What disagreed, in one sentence naming both sides.
    pub message: String,
}

impl Finding {
    fn new(arm: Arm, message: impl Into<String>) -> Self {
        Self {
            arm,
            message: message.into(),
        }
    }

    /// The rendered line.
    #[must_use]
    pub fn render(&self) -> String {
        format!("{}: {}", self.arm.as_str(), self.message)
    }
}

/// A row that has not been derived yet, with the command that would derive it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    /// The scenario.
    pub scenario: String,
    /// What the row says about filling itself.
    pub note: String,
}

/// The whole verdict.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Audit {
    /// Everything that disagreed. Empty is the pass.
    pub findings: Vec<Finding>,
    /// Rows still awaiting a quiet-machine measurement.
    pub pending: Vec<Pending>,
    /// Artifact stems read, so a reader can see the guard was not looking at an
    /// empty directory and calling it clean.
    pub artifacts_read: Vec<String>,
}

impl Audit {
    /// Whether everything agreed.
    #[must_use]
    pub fn clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// The report a CI leg prints.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!(
            "artifact-consistency: {} artifact(s) read, {} finding(s), {} pending\n",
            self.artifacts_read.len(),
            self.findings.len(),
            self.pending.len()
        );
        for f in &self.findings {
            out.push_str(&format!("  {}\n", f.render()));
        }
        for p in &self.pending {
            out.push_str(&format!("  PENDING {}: {}\n", p.scenario, p.note));
        }
        if self.findings.is_empty() {
            out.push_str(
                "  no disagreement. This is a consistency verdict, NOT a performance verdict: \
                 nothing here says the machine is fast.\n",
            );
        }
        out
    }
}

/// One artifact, whichever family it belongs to.
#[derive(Debug, Clone)]
pub enum Artifact {
    /// A git-latency sweep.
    HookLatency(Box<HookLatencyReport>),
    /// A scenario comparison run.
    VsReference(Box<Report>),
    /// An enable-time displacement sweep.
    Displacement(Box<DisplacementReport>),
}

impl Artifact {
    /// The subject whose provenance decides whether this is current.
    #[must_use]
    pub fn subject(&self) -> Option<&LegStatus> {
        match self {
            Self::HookLatency(r) => Some(&r.subject),
            Self::VsReference(r) => r
                .legs
                .iter()
                .find(|l| l.id == crate::leg::LegId::Rift)
                .map(|l| &l.status),
            Self::Displacement(r) => Some(&r.subject),
        }
    }

    /// Whether this artifact is a record rather than a current measurement.
    #[must_use]
    pub fn is_record(&self) -> bool {
        self.subject().is_none_or(facts::is_record)
    }

    /// Whether any arm ran on a machine too busy to derive a floor from.
    ///
    /// One void arm voids the artifact **as a floor source**, not as a record:
    /// arms are minutes apart and a floor is one number over a whole run, so a
    /// run that was quiet for two arms and loaded for a third cannot back a
    /// floor for any of them.
    #[must_use]
    pub fn load_void(&self) -> bool {
        match self {
            Self::HookLatency(r) => r
                .arms
                .iter()
                .any(|a| a.attestation.is_none_or(|x| x.void())),
            Self::Displacement(r) => r
                .arms
                .iter()
                .any(|a| a.attestation.is_none_or(|x| x.void())),
            // One reading for the whole run: a scenario run's rows share a
            // process and a load reading. Absent is void, like everywhere else.
            Self::VsReference(r) => r.attestation.is_none_or(|a| a.void()),
        }
    }

    /// The subject's git provenance, when it has any.
    #[must_use]
    pub fn provenance(&self) -> Option<GitProvenance> {
        match self.subject()? {
            LegStatus::Present {
                git_describe: Some(describe),
                dirty,
                ..
            } => Some(GitProvenance {
                describe: describe.clone(),
                dirty: dirty.unwrap_or(false),
            }),
            _ => None,
        }
    }

    /// Which floor scenarios this artifact carries a measured value for, and
    /// what that value is.
    #[must_use]
    pub fn measurements(&self) -> Vec<Measurement> {
        match self {
            // A git-latency sweep measures hook arms, none of which is one of
            // the four scenarios that are owed floors. It still participates in
            // the staleness arm (any artifact claiming to be current does), and
            // in the fence arm.
            Self::HookLatency(_) => Vec::new(),
            // The displacement artifact's gate-bearing number is ONE arm: the
            // first commit in a repository enabled beforehand, which is what a
            // user's next commit costs and what the git-latency budget binds.
            // `E0` is a wall time for a different act and `E2` is a
            // counterfactual, so neither is judged against the budget —
            // publishing them as measured values would fail the artifact for
            // arms the budget never
            // aimed at.
            Self::Displacement(r) => r
                .arms
                .iter()
                .filter(|a| a.arm.act == Act::FirstCommitAfterEnable)
                .filter_map(|a| {
                    let ns = r.latency_ns(a)?;
                    Some(Measurement {
                        scenario: r.scenario.clone(),
                        label: format!("`{}` git latency against its own run's control", a.arm.id),
                        units_per_s: None,
                        millis: Some(ns as f64 / 1_000_000.0),
                    })
                })
                .collect(),
            Self::VsReference(r) => r
                .scenarios
                .iter()
                .filter_map(|s| {
                    let stats = s
                        .legs
                        .get(crate::leg::LegId::Rift.as_str())
                        .and_then(LegOutcome::stats)?;
                    let rate = stats.rate_per_s()?;
                    Some(Measurement {
                        scenario: s.id.as_str().to_owned(),
                        label: "rift rate at the median iteration".to_owned(),
                        units_per_s: Some(rate),
                        millis: None,
                    })
                })
                .collect(),
        }
    }
}

/// One measured value an artifact publishes for a floor scenario.
#[derive(Debug, Clone, PartialEq)]
pub struct Measurement {
    /// The floor scenario it belongs to.
    pub scenario: String,
    /// What it is, for the finding's sentence.
    pub label: String,
    /// A rate, when the row is a throughput row.
    pub units_per_s: Option<f64>,
    /// A wall time, when the row is a latency row.
    pub millis: Option<f64>,
}

/// Everything the guard reads, loaded once.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    /// The tracked declaration.
    pub floors: Option<Floors>,
    /// Why it could not be read, when it could not.
    pub floors_error: Option<String>,
    /// Every artifact under `benchmarks/`, by stem.
    pub artifacts: BTreeMap<String, Artifact>,
    /// Every markdown report under `benchmarks/`, by repo-relative path.
    pub reports: BTreeMap<String, String>,
}

impl Inputs {
    /// Read `benchmarks/` under `repo_root`.
    #[must_use]
    pub fn load(repo_root: &Path) -> Self {
        let benchmarks = repo_root.join("benchmarks");
        let (floors, floors_error) = match Floors::load(repo_root) {
            Ok(f) => (Some(f), None),
            Err(e) => (None, Some(e)),
        };
        let mut inputs = Self {
            floors,
            floors_error,
            ..Self::default()
        };
        collect(&benchmarks, &benchmarks, &mut inputs);
        inputs
    }
}

/// Walk `dir`, loading every artifact and report under it.
fn collect(base: &Path, dir: &Path, out: &mut Inputs) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect(base, &path, out);
            continue;
        }
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        match path.extension().and_then(|e| e.to_str()) {
            Some("json") => {
                // Which family an artifact belongs to is decided by what it
                // parses as, not by the directory it sits in: a run written to
                // the wrong place is still that run.
                if let Ok(r) = serde_json::from_str::<HookLatencyReport>(&body) {
                    out.artifacts
                        .insert(stem, Artifact::HookLatency(Box::new(r)));
                } else if let Ok(r) = serde_json::from_str::<Report>(&body) {
                    out.artifacts
                        .insert(stem, Artifact::VsReference(Box::new(r)));
                } else if let Ok(r) = serde_json::from_str::<DisplacementReport>(&body) {
                    out.artifacts
                        .insert(stem, Artifact::Displacement(Box::new(r)));
                }
            }
            Some("md") => {
                let rel = path
                    .strip_prefix(base)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                out.reports.insert(rel, body);
            }
            _ => {}
        }
    }
}

/// Run every arm against `repo_root`.
#[must_use]
pub fn audit(repo_root: &Path) -> Audit {
    let inputs = Inputs::load(repo_root);
    audit_inputs(repo_root, &inputs)
}

/// Run every arm against already-loaded inputs.
///
/// Split from [`audit`] so the arms can be calibrated against planted inputs
/// without a repository having to be built for each one.
#[must_use]
pub fn audit_inputs(repo_root: &Path, inputs: &Inputs) -> Audit {
    let mut out = Audit {
        artifacts_read: inputs.artifacts.keys().cloned().collect(),
        ..Audit::default()
    };
    let Some(floors) = inputs.floors.as_ref() else {
        out.findings.push(Finding::new(
            Arm::FloorsFile,
            inputs
                .floors_error
                .clone()
                .unwrap_or_else(|| "benchmarks/floors.toml did not load".to_owned()),
        ));
        return out;
    };

    check_enumeration(floors, &mut out);
    for floor in &floors.floors {
        if let Some(why) = floor.refusal() {
            out.findings.push(Finding::new(Arm::FloorsFile, why));
        }
        if floor.binds() {
            check_floor(floor, inputs, &mut out);
        } else {
            out.pending.push(Pending {
                scenario: floor.scenario.clone(),
                note: floor.note.clone(),
            });
        }
    }
    check_staleness(repo_root, floors, inputs, &mut out);
    check_fences(inputs, &mut out);
    check_report_fences(inputs, floors, &mut out);
    out
}

/// Every owed scenario has a row.
fn check_enumeration(floors: &Floors, out: &mut Audit) {
    for scenario in OWED_SCENARIOS {
        if floors.get(scenario).is_none() {
            out.findings.push(Finding::new(
                Arm::Enumeration,
                format!(
                    "`{scenario}` is owed a floor row and benchmarks/floors.toml has \
                     none: a scenario dropped from the set must not leave a green gate behind it"
                ),
            ));
        }
    }
}

/// Arms (i) and (ii) for one binding floor.
fn check_floor(floor: &Floor, inputs: &Inputs, out: &mut Audit) {
    // "No artifact without provenance may derive a floor", made
    // mechanical: the row names the runs its medians came from, and each one
    // must exist here and must not be a record.
    for stem in &floor.derivation_artifacts {
        match inputs.artifacts.get(stem) {
            None => out.findings.push(Finding::new(
                Arm::MissingArtifact,
                format!(
                    "`{}` derives its floor from `{stem}`, which is not under benchmarks/: a \
                     floor whose evidence is absent cannot be re-derived by anyone",
                    floor.scenario
                ),
            )),
            Some(artifact) if artifact.is_record() => out.findings.push(Finding::new(
                Arm::Staleness,
                format!(
                    "`{}` derives its floor from `{stem}`, which is a RECORD (no commit \
                     provenance, or a dirty tree): no artifact without provenance may derive a \
                     floor",
                    floor.scenario
                ),
            )),
            Some(artifact) if artifact.load_void() => out.findings.push(Finding::new(
                Arm::Staleness,
                format!(
                    "`{}` derives its floor from `{stem}`, which ran on a machine over the \
                     stated load ceiling (or did not read it at all): a busy box is published \
                     as a record and may not set a bar other runs are judged against",
                    floor.scenario
                ),
            )),
            Some(_) => {}
        }
    }
    let mut measured = 0usize;
    for (stem, artifact) in &inputs.artifacts {
        for m in artifact.measurements() {
            if m.scenario != floor.scenario {
                continue;
            }
            measured += 1;
            match (
                floor.kind,
                m.units_per_s,
                m.millis,
                floor.min_units_per_s,
                floor.budget_ms,
            ) {
                (FloorKind::Throughput, Some(rate), _, Some(min), _) if rate < min => {
                    out.findings.push(Finding::new(
                        Arm::Violation,
                        format!(
                            "`{stem}` measured {rate:.2} {}/s for `{}` ({}), under the floor of \
                             {min} {}/s",
                            floor.work_unit, floor.scenario, m.label, floor.work_unit
                        ),
                    ));
                }
                (FloorKind::Latency, _, Some(ms), _, Some(budget)) if ms > budget => {
                    out.findings.push(Finding::new(
                        Arm::Violation,
                        format!(
                            "`{stem}` measured {ms:.2} ms for `{}` ({}), over the {budget} ms \
                             budget",
                            floor.scenario, m.label
                        ),
                    ));
                }
                _ => {}
            }
        }
    }
    if measured == 0 {
        out.findings.push(Finding::new(
            Arm::MissingArtifact,
            format!(
                "`{}` states a bar that binds and no artifact under benchmarks/ measures it: a \
                 floor with nothing to judge is a gate that cannot fail",
                floor.scenario
            ),
        ));
    }
}

/// Arm (iii): an artifact that claims to be current must be.
fn check_staleness(repo_root: &Path, floors: &Floors, inputs: &Inputs, out: &mut Audit) {
    let head = provenance::head_commit(repo_root);
    for (stem, artifact) in &inputs.artifacts {
        // A record is exempt by construction — that is what recording it as
        // superseded MEANS — and it is barred from deriving a floor elsewhere.
        if artifact.is_record() {
            continue;
        }
        let Some(prov) = artifact.provenance() else {
            continue;
        };
        if let Some(why) = prov.refusal() {
            out.findings
                .push(Finding::new(Arm::Staleness, format!("`{stem}`: {why}")));
            continue;
        }
        let Some(subject) = prov.commit() else {
            continue;
        };
        let Some(head) = head.as_deref() else {
            out.findings.push(Finding::new(
                Arm::Staleness,
                format!(
                    "`{stem}` claims to be current but HEAD could not be resolved, so no \
                     artifact's currency can be checked at all"
                ),
            ));
            continue;
        };
        match provenance::is_ancestor(repo_root, subject, head) {
            Some(true) => {}
            Some(false) => {
                out.findings.push(Finding::new(
                    Arm::Staleness,
                    format!(
                        "`{stem}` was measured at `{subject}`, which is NOT an ancestor of HEAD: \
                         a real measurement of code this history does not contain"
                    ),
                ));
                continue;
            }
            None => {
                out.findings.push(Finding::new(
                    Arm::Staleness,
                    format!(
                        "`{stem}` names commit `{subject}`, which this checkout does not hold, \
                         so its currency is unknowable — and unknown is not fresh"
                    ),
                ));
                continue;
            }
        }
        // And the other half: does the subject contain the newest change to
        // what this scenario measures?
        for floor in &floors.floors {
            if !artifact
                .measurements()
                .iter()
                .any(|m| m.scenario == floor.scenario)
            {
                continue;
            }
            let Some(last) = provenance::last_commit_touching(repo_root, &floor.measured_paths)
            else {
                continue;
            };
            if provenance::is_ancestor(repo_root, &last, subject) == Some(false) {
                out.findings.push(Finding::new(
                    Arm::Staleness,
                    format!(
                        "`{stem}` measures `{}` at `{subject}`, which predates `{last}` — HEAD's \
                         last change to that scenario's declared measured paths",
                        floor.scenario
                    ),
                ));
            }
        }
    }
}

/// Arm (iv): a fence and its own artifacts.
fn check_fences(inputs: &Inputs, out: &mut Audit) {
    for (path, body) in &inputs.reports {
        let parsed = match facts::extract(body) {
            Ok(None) => continue,
            Ok(Some(_)) => facts::parse(body),
            Err(e) => Err(e),
        };
        let fence = match parsed {
            Ok(f) => f,
            Err(e) => {
                out.findings.push(Finding::new(
                    Arm::FenceDisagreement,
                    format!("`{path}`: {e}"),
                ));
                continue;
            }
        };
        // Recomputed from **exactly** the artifacts the fence names, so the
        // fence is self-describing and a reader can repeat the derivation
        // without knowing which directory it came from.
        let mut sources: Vec<(String, HookLatencyReport)> = Vec::new();
        let mut missing: Vec<&String> = Vec::new();
        let stems: Vec<String> = fence
            .artifacts
            .iter()
            .map(|stamp| facts::stem_of(&fence.family, *stamp))
            .collect();
        for stem in &stems {
            match inputs.artifacts.get(stem) {
                Some(Artifact::HookLatency(r)) => sources.push((stem.clone(), (**r).clone())),
                _ => missing.push(stem),
            }
        }
        if !missing.is_empty() {
            out.findings.push(Finding::new(
                Arm::FenceDisagreement,
                format!(
                    "`{path}` names {} artifact(s) that are not under benchmarks/: {}",
                    missing.len(),
                    missing
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
            continue;
        }
        let recomputed = BenchFacts::of(&sources, fence.superseded_reason.clone());
        if recomputed != fence {
            out.findings.push(Finding::new(
                Arm::FenceDisagreement,
                format!(
                    "`{path}`'s fence does not match the artifacts it names — {}",
                    first_difference(&fence, &recomputed)
                ),
            ));
        }
    }
}

/// Arm (iv) for the ```bench-report fence: recomputed from exactly the
/// artifacts its families name, over the tracked floors file, and every
/// floors scenario must have a row.
fn check_report_fences(inputs: &Inputs, floors: &Floors, out: &mut Audit) {
    use crate::report_facts::{ReportFacts, REPORT_SCHEMA, REPORT_TAG};

    for (path, body) in &inputs.reports {
        let fence: ReportFacts = match facts::extract_tagged(body, REPORT_TAG) {
            Ok(None) => continue,
            Ok(Some(_)) => match facts::parse_tagged(body, REPORT_TAG, REPORT_SCHEMA) {
                Ok(f) => f,
                Err(e) => {
                    out.findings.push(Finding::new(
                        Arm::FenceDisagreement,
                        format!("`{path}`: {e}"),
                    ));
                    continue;
                }
            },
            Err(e) => {
                out.findings.push(Finding::new(
                    Arm::FenceDisagreement,
                    format!("`{path}`: {e}"),
                ));
                continue;
            }
        };
        // Exactly the artifacts the fence names, so it is self-describing.
        let mut named: Inputs = Inputs {
            floors: Some(floors.clone()),
            ..Inputs::default()
        };
        let mut missing: Vec<String> = Vec::new();
        for family in &fence.families {
            for stamp in &family.artifacts {
                let stem = facts::stem_of(&family.family, *stamp);
                match inputs.artifacts.get(&stem) {
                    Some(a) => {
                        named.artifacts.insert(stem, a.clone());
                    }
                    None => missing.push(stem),
                }
            }
        }
        if !missing.is_empty() {
            out.findings.push(Finding::new(
                Arm::FenceDisagreement,
                format!(
                    "`{path}` names {} artifact(s) that are not under benchmarks/: {}",
                    missing.len(),
                    missing.join(", ")
                ),
            ));
            continue;
        }
        for floor in &floors.floors {
            let present = match floor.kind {
                FloorKind::Throughput => fence
                    .throughput
                    .iter()
                    .any(|t| t.scenario == floor.scenario),
                FloorKind::Latency => floor.scenario == "enable-displacement",
            };
            if !present {
                out.findings.push(Finding::new(
                    Arm::FenceDisagreement,
                    format!(
                        "`{path}`'s fence carries no row for `{}`, which benchmarks/floors.toml \
                         declares: a floor dropped from the aggregate must not leave a green \
                         gate behind it",
                        floor.scenario
                    ),
                ));
            }
        }
        let recomputed = ReportFacts::of(&named, Some(floors));
        if recomputed != fence {
            out.findings.push(Finding::new(
                Arm::FenceDisagreement,
                format!(
                    "`{path}`'s bench-report fence does not match the artifacts it names — {}",
                    first_report_difference(&fence, &recomputed)
                ),
            ));
        }
    }
}

/// The first section of a report fence that differs, named.
fn first_report_difference(
    fence: &crate::report_facts::ReportFacts,
    truth: &crate::report_facts::ReportFacts,
) -> String {
    if fence.families != truth.families {
        return "the families section (stamps, records or load-void counts)".to_owned();
    }
    for (got, want) in fence.throughput.iter().zip(&truth.throughput) {
        if got != want {
            return format!(
                "throughput row `{}`: fence says {:?} milli-units/s over {} run(s); the \
                 artifacts say {:?} over {}",
                got.scenario,
                got.rate_milli_per_s_median,
                got.runs,
                want.rate_milli_per_s_median,
                want.runs
            );
        }
    }
    if fence.throughput.len() != truth.throughput.len() {
        return "the throughput section (a row the floors file does not declare, or one it does that is absent)".to_owned();
    }
    for (got, want) in fence.latency.iter().zip(&truth.latency) {
        if got != want {
            return format!(
                "latency row `{}`: fence says p50 {:?} / p95 {:?} ns over {} run(s); the \
                 artifacts say {:?} / {:?} over {}",
                got.scenario,
                got.p50_ns_median,
                got.p95_ns_median,
                got.runs,
                want.p50_ns_median,
                want.p95_ns_median,
                want.runs
            );
        }
    }
    if fence.latency.len() != truth.latency.len() {
        return "the latency section (a row the catalogue does not declare, or one it does that is absent)".to_owned();
    }
    if fence.displacement != truth.displacement {
        return format!(
            "displacement: fence says {:?} ns over {} run(s); the artifacts say {:?} over {}",
            fence.displacement.first_commit_after_enable_ns_median,
            fence.displacement.runs,
            truth.displacement.first_commit_after_enable_ns_median,
            truth.displacement.runs
        );
    }
    if fence.git_latency != truth.git_latency {
        return "the git-latency counts".to_owned();
    }
    "the two differ in a field this summary does not name".to_owned()
}

/// The first field that differs, so a finding names the disagreement instead of
/// printing two blobs.
fn first_difference(fence: &BenchFacts, truth: &BenchFacts) -> String {
    if fence.budget_ms != truth.budget_ms {
        return format!(
            "budget_ms {} in the fence, {} in the artifacts",
            fence.budget_ms, truth.budget_ms
        );
    }
    if fence.superseded_pending_rerun != truth.superseded_pending_rerun {
        return format!(
            "{} superseded artifact(s) in the fence, {} derived from the artifacts",
            fence.superseded_pending_rerun.len(),
            truth.superseded_pending_rerun.len()
        );
    }
    if fence.control_p50_ns_median != truth.control_p50_ns_median {
        return format!(
            "control p50 median {} ns in the fence, {} ns in the artifacts",
            fence.control_p50_ns_median, truth.control_p50_ns_median
        );
    }
    for got in &fence.arms {
        match truth.arms.iter().find(|a| a.arm == got.arm) {
            None => return format!("the fence carries an arm `{}` nothing measured", got.arm),
            Some(want) if want != got => {
                return format!(
                    "arm `{}`: fence says {} ns over {} run(s) with {} over budget; the \
                     artifacts say {} ns over {} run(s) with {} over budget",
                    got.arm,
                    got.git_latency_ns_median,
                    got.runs,
                    got.over_budget_runs,
                    want.git_latency_ns_median,
                    want.runs,
                    want.over_budget_runs
                );
            }
            Some(_) => {}
        }
    }
    for want in &truth.arms {
        if !fence.arms.iter().any(|a| a.arm == want.arm) {
            return format!(
                "the artifacts measure `{}` and the fence omits it",
                want.arm
            );
        }
    }
    "the two differ in a field this summary does not name".to_owned()
}

//! The artifact-consistency guard, and the calibration that makes it a guard.
//!
//! The publication rules ask for five things to be true of `benchmarks/` and for a check
//! that reds when any of them stops being true. A check that has never been
//! seen red is an assertion about a check, so **every arm here is calibrated by
//! planting the defect it exists for** — and each calibration also runs the
//! unplanted case, because a guard that reds on everything is not a guard
//! either.
//!
//! The plants are built in throwaway git repositories, never against the real
//! `benchmarks/` tree: the committed artifacts are records and this project's
//! standing rule is that a record is never edited to make a check do anything.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use rift_bench_native_baseline as bench;
use rift_bench_native_baseline::{
    audit::{self, Arm, Audit},
    displace::{self, DisplacementReport, DISPLACEMENT_SCHEMA},
    facts::BenchFacts,
    fixture::FixtureSpec,
    hooklat::{
        Arm as SweepArm, ArmResult, Capture, HookLatencyReport, Oracles, HOOK_LATENCY_SCHEMA,
    },
    leg::{LegId, LegStatus},
    measure::{Budget, Protocol, Sample, Stats},
    report::{Comparison, Host, LegOutcome, LegSummary, Report, ScenarioReport, REPORT_SCHEMA},
    scenario::ScenarioId,
};

/// A throwaway checkout with a `benchmarks/` tree the guard can read.
struct Plant {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Plant {
    /// A repository with one commit touching the path the planted floors row
    /// declares as measured, so the staleness arm has a history to reason over.
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        git(&root, &["init", "--quiet"]);
        git(&root, &["config", "user.email", "bench@example.invalid"]);
        git(&root, &["config", "user.name", "bench"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        let plant = Self { _tmp: tmp, root };
        plant.commit("crates/measured/src/lib.rs", "// the measured path\n");
        plant
    }

    fn commit(&self, rel: &str, body: &str) -> String {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, body).expect("write");
        git(&self.root, &["add", "--", rel]);
        git(&self.root, &["commit", "--quiet", "-m", rel]);
        git(&self.root, &["rev-parse", "HEAD"])
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, body).expect("write");
    }

    fn floors(&self, status: &str, medians: &str, min: Option<&str>, artifacts: &str) {
        let mut rows = String::from("schema_version = 1\n");
        for scenario in audit::OWED_SCENARIOS {
            let is_target = *scenario == "condense";
            rows.push_str("\n[[floor]]\n");
            rows.push_str(&format!("scenario = \"{scenario}\"\n"));
            rows.push_str("kind = \"throughput\"\n");
            rows.push_str(&format!(
                "status = \"{}\"\n",
                if is_target { status } else { "underived" }
            ));
            rows.push_str("work_unit = \"events\"\n");
            if is_target {
                if let Some(min) = min {
                    rows.push_str(&format!("min_units_per_s = {min}\n"));
                }
                rows.push_str(&format!("derivation_medians = {medians}\n"));
                rows.push_str(&format!("derivation_artifacts = {artifacts}\n"));
            } else {
                rows.push_str("derivation_medians = []\n");
                rows.push_str("derivation_artifacts = []\n");
            }
            rows.push_str("machine_class = \"plant\"\n");
            rows.push_str("protocol = \"plant\"\n");
            rows.push_str("caps = \"plant\"\n");
            rows.push_str("measured_paths = [\"crates/measured/src/lib.rs\"]\n");
            rows.push_str(
                "derivation = \"min x 0.5, rounded down to <= 2 significant figures. NOT a noise budget.\"\n",
            );
            rows.push_str("note = \"planted\"\n");
        }
        self.write("benchmarks/floors.toml", &rows);
    }

    /// A floors file whose displacement row is an owner-ratified 100 ms
    /// latency ceiling, and whose throughput rows stay placeholders.
    fn latency_floor(&self) {
        let mut rows = String::from("schema_version = 1\n");
        for scenario in audit::OWED_SCENARIOS {
            let latency = *scenario == "enable-displacement";
            rows.push_str("\n[[floor]]\n");
            rows.push_str(&format!("scenario = \"{scenario}\"\n"));
            rows.push_str(&format!(
                "kind = \"{}\"\n",
                if latency { "latency" } else { "throughput" }
            ));
            rows.push_str(&format!(
                "status = \"{}\"\n",
                if latency {
                    "owner-ratified"
                } else {
                    "underived"
                }
            ));
            rows.push_str("work_unit = \"\"\n");
            if latency {
                rows.push_str("budget_ms = 100.0\n");
            }
            rows.push_str("derivation_medians = []\n");
            rows.push_str("derivation_artifacts = []\n");
            rows.push_str("machine_class = \"plant\"\n");
            rows.push_str("protocol = \"plant\"\n");
            rows.push_str("caps = \"plant\"\n");
            rows.push_str("measured_paths = [\"crates/measured/src/lib.rs\"]\n");
            rows.push_str("derivation = \"NOT a derived floor; NEVER a throughput floor.\"\n");
            rows.push_str("note = \"planted\"\n");
        }
        self.write("benchmarks/floors.toml", &rows);
    }

    fn audit(&self) -> Audit {
        audit::audit(&self.root)
    }
}

/// A displacement artifact: a control, an enable wall time, and the two orders.
fn displacement(control_ns: u64, after_ns: u64, describe: &str) -> String {
    let stats = |ns: u64| {
        Stats::of(&[Sample {
            wall_ns: ns,
            exit_code: Some(0),
        }])
    };
    let report = DisplacementReport {
        schema_version: DISPLACEMENT_SCHEMA,
        harness_version: "0.0.1".to_owned(),
        generated_at_unix: 1_800_000_000,
        scenario: displace::SCENARIO.to_owned(),
        host: Host::detect("n/a".to_owned()),
        subject: LegStatus::Present {
            binary: PathBuf::from("/x/rift"),
            version: "rift 0.0.1".to_owned(),
            source: "plant".to_owned(),
            binary_mtime_unix: Some(1),
            git_describe: Some(describe.to_owned()),
            dirty: Some(false),
        },
        source_clock: None,
        protocol: Protocol::new(3, 11),
        budget_ms: 100,
        arms: displace::arms()
            .into_iter()
            .map(|arm| displace::ArmResult {
                attestation: Some(bench::attest::Attestation {
                    load1: Some(1.65),
                    void_above: bench::attest::VOID_ABOVE_LOAD1,
                }),
                stats: stats(match arm.act {
                    displace::Act::BareCommit => control_ns,
                    displace::Act::Enable => 300_000_000,
                    displace::Act::FirstCommitAfterEnable => after_ns,
                    displace::Act::FirstCommitWithoutPriorEnable => after_ns + 70_000_000,
                }),
                error: None,
                trailer_present: (!arm.act.is_control()).then_some(true),
                arm,
            })
            .collect(),
    };
    serde_json::to_string_pretty(&report).expect("json")
}

/// The three runs a planted derived floor cites. Named rather than repeated so
/// a reader can see the floor is backed by three DIFFERENT artifacts, which is
/// the whole point of requiring three.
const THREE_RUNS: &str = "[\"scenario-1\", \"scenario-2\", \"scenario-3\"]";

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn findings(audit: &Audit, arm: Arm) -> Vec<String> {
    audit
        .findings
        .iter()
        .filter(|f| f.arm == arm)
        .map(|f| f.message.clone())
        .collect()
}

/// A scenario-comparison artifact publishing one row's rate.
fn scenario_run(scenario: ScenarioId, units: u64, p50_ns: u64, describe: Option<&str>) -> String {
    let stats = Stats::of(&[Sample {
        wall_ns: p50_ns,
        exit_code: Some(0),
    }])
    .expect("stats")
    .with_units(units);
    let mut legs = std::collections::BTreeMap::new();
    legs.insert(
        LegId::Rift.as_str().to_owned(),
        LegOutcome::Measured {
            command: "rift x".to_owned(),
            stats,
            peak_rss_bytes: None,
            ingest: None,
            child_spawns: None,
        },
    );
    let report = Report {
        schema_version: REPORT_SCHEMA,
        harness_version: "0.0.1".to_owned(),
        generated_at_unix: 1_800_000_000,
        host: Host::detect("n/a".to_owned()),
        source_clock: None,
        attestation: Some(bench::attest::Attestation {
            load1: Some(1.65),
            void_above: bench::attest::VOID_ABOVE_LOAD1,
        }),
        legs: vec![LegSummary {
            id: LegId::Rift,
            status: LegStatus::Present {
                binary: PathBuf::from("/x/rift"),
                version: "rift 0.0.1".to_owned(),
                source: "plant".to_owned(),
                binary_mtime_unix: Some(1),
                git_describe: describe.map(str::to_owned),
                dirty: describe.map(|_| false),
            },
            binary_bytes: None,
        }],
        driver: None,
        fixtures: Vec::new(),
        scenarios: vec![ScenarioReport {
            id: scenario,
            title: "planted".to_owned(),
            fixture: FixtureSpec::small().name,
            headline: false,
            wired: true,
            protocol: Protocol::heavy(),
            budget: Some(Budget::unratified_throughput()),
            work_unit: Some("events".to_owned()),
            caveat: None,
            legs,
            comparison: None::<Comparison>,
        }],
    };
    serde_json::to_string_pretty(&report).expect("json")
}

/// A `hook-latency` artifact with a control and one arm.
fn sweep(control_ns: u64, live_ns: u64) -> HookLatencyReport {
    let arm = |id: &str, ns: u64, control: bool| ArmResult {
        arm: SweepArm {
            id: id.to_owned(),
            title: id.to_owned(),
            capture: if control { Capture::Off } else { Capture::Live },
            include: None,
        },
        stats: Stats::of(&[Sample {
            wall_ns: ns,
            exit_code: Some(0),
        }]),
        error: None,
        attestation: Some(bench::attest::Attestation {
            load1: Some(1.65),
            void_above: bench::attest::VOID_ABOVE_LOAD1,
        }),
        oracles: Oracles::default(),
        phases: std::collections::BTreeMap::new(),
    };
    HookLatencyReport {
        schema_version: HOOK_LATENCY_SCHEMA,
        harness_version: "0.0.1".to_owned(),
        generated_at_unix: 1_787_120_100,
        host: Host::detect("n/a".to_owned()),
        subject: LegStatus::Absent {
            reason: "plant".to_owned(),
        },
        source_clock: None,
        git_version: "git version 2.50.1".to_owned(),
        protocol: Protocol::new(3, 11),
        budget_ms: 100,
        arms: vec![
            arm("A_bare", control_ns, true),
            arm("B1_live", live_ns, false),
        ],
    }
}

// ---------------------------------------------------------------------------
// The tree this repository actually ships.
// ---------------------------------------------------------------------------

/// The load-bearing assertion: the committed tree agrees with itself.
///
/// Read the pending list as part of the verdict, not as a footnote. The
/// quiet-machine measurement has now run, so every owed row binds and none
/// may fall back to a placeholder silently.
#[test]
fn the_committed_benchmarks_tree_is_internally_consistent() {
    let root = bench::leg::default_repo_root();
    let verdict = audit::audit(&root);
    assert!(
        verdict.clean(),
        "the committed tree disagrees with itself:\n{}",
        verdict.render()
    );
    assert!(
        // 25 = 19 git-latency sweeps + 3 catalogue runs + 3 displacement sweeps.
        // All of them are records today. The floor tracks the smallest honest
        // tree, never a count that once happened to be true.
        verdict.artifacts_read.len() >= 25,
        "the guard read {} artifact(s); an empty directory called clean is the failure this \
         count exists to prevent",
        verdict.artifacts_read.len()
    );
    assert!(
        verdict.pending.is_empty(),
        "the measurement closed every owed floor; pending rows: {:?}",
        verdict
            .pending
            .iter()
            .map(|p| p.scenario.as_str())
            .collect::<Vec<_>>()
    );
    assert!(verdict.render().contains("NOT a performance verdict"));
}

/// Every committed git-latency sweep is a record, and the fence says so by
/// name — that is what gives the staleness arm semantics.
///
/// Eighteen of the nineteen are the pre-cap sweeps, which never carried commit
/// provenance at all. The nineteenth did, and lost it deliberately: the history
/// holding its subject was collapsed to a fresh root, so a provenance string
/// naming a commit nobody can resolve would have been a claim of currency the
/// tree cannot support. It is kept exactly as measured and reclassified, which
/// is what recording a run as superseded MEANS. So the two sets are now equal:
/// every artifact behind this fence is a record, and none of them may derive a
/// bar. A future provenanced sweep makes `artifacts` the larger set again.
#[test]
fn every_committed_artifact_is_marked_a_record_by_the_fence() {
    let root = bench::leg::default_repo_root();
    let body = fs::read_to_string(root.join("benchmarks/hook-latency.md")).expect("the report");
    let fence = bench::facts::parse(&body).expect("one fence");
    assert_eq!(
        fence.superseded_pending_rerun.len(),
        19,
        "every committed sweep stays superseded; a record never becomes current"
    );
    assert!(
        fence
            .superseded_pending_rerun
            .iter()
            .all(|a| fence.artifacts.contains(a)),
        "every superseded stem is a committed artifact"
    );
    assert_eq!(
        fence.artifacts.len(),
        fence.superseded_pending_rerun.len(),
        "no committed sweep carries resolvable provenance today, so every artifact \
         behind the fence is a record"
    );
    assert!(fence.superseded_reason.contains("may not derive a floor"));
    assert!(
        fence.arms.iter().any(|a| a.arm == "F1_newline_8k"
            && a.git_latency_ns_median == 114_097_693
            && a.over_budget_runs == 2),
        "the breach the assessment named is reproduced FROM the artifacts, not retyped: {:?}",
        fence.arms
    );
}

// ---------------------------------------------------------------------------
// Calibration: one plant per arm, each with its unplanted control.
// ---------------------------------------------------------------------------

/// Arm (i): a measured value under a floor that binds.
#[test]
fn calibration_i_a_violating_value_reds() {
    let plant = Plant::new();
    // Medians 100/110/120 -> floor = min x 0.5 = 50 events/s.
    plant.floors("derived", "[100.0, 110.0, 120.0]", Some("50.0"), THREE_RUNS);
    let head = git(&plant.root, &["rev-parse", "HEAD"]);
    let describe = format!("plant-1-g{head}");

    // Three runs, 90 events in a 1 s median each = 90/s: above the floor.
    for n in 1..=3 {
        plant.write(
            &format!("benchmarks/runs/scenario-{n}.json"),
            &scenario_run(ScenarioId::Condense, 90, 1_000_000_000, Some(&describe)),
        );
    }
    let clean = plant.audit();
    assert!(
        clean.clean(),
        "three current runs above the floor are a pass: {}",
        clean.render()
    );

    // One of the three drops to 10 events in the same median = 10/s. The
    // finding NAMES the run, so a reader does not have to open three files to
    // find which one moved.
    plant.write(
        "benchmarks/runs/scenario-2.json",
        &scenario_run(ScenarioId::Condense, 10, 1_000_000_000, Some(&describe)),
    );
    let red = plant.audit();
    let hits = findings(&red, Arm::Violation);
    assert_eq!(hits.len(), 1, "{}", red.render());
    assert!(hits[0].contains("scenario-2"), "{}", hits[0]);
    assert!(hits[0].contains("10.00 events/s"), "{}", hits[0]);
    assert!(
        hits[0].contains("under the floor of 50 events/s"),
        "{}",
        hits[0]
    );
    assert!(!red.clean());
}

/// Arm (ii): a floor that binds with nothing measuring it — and the half of
/// the rule that says **no artifact without provenance may derive a floor**.
#[test]
fn calibration_ii_a_binding_floor_with_no_artifact_reds() {
    let plant = Plant::new();
    plant.floors("derived", "[100.0, 110.0, 120.0]", Some("50.0"), THREE_RUNS);
    let red = plant.audit();
    let hits = findings(&red, Arm::MissingArtifact);
    // One for the scenario nothing measures, three for the cited runs that are
    // not there: a floor whose evidence is absent cannot be re-derived.
    assert_eq!(hits.len(), 4, "{}", red.render());
    assert!(
        hits.iter().any(|h| h.contains("gate that cannot fail")),
        "{hits:?}"
    );
    assert!(
        hits.iter()
            .filter(|h| h.contains("not under benchmarks/"))
            .count()
            == 3,
        "{hits:?}"
    );

    // The runs exist, but carry no commit provenance: they are RECORDS, and a
    // record may not derive a floor however good its numbers are.
    for n in 1..=3 {
        plant.write(
            &format!("benchmarks/runs/scenario-{n}.json"),
            &scenario_run(ScenarioId::Condense, 90, 1_000_000_000, None),
        );
    }
    let records = plant.audit();
    let barred = findings(&records, Arm::Staleness);
    assert_eq!(barred.len(), 3, "{}", records.render());
    assert!(
        barred[0].contains("is a RECORD")
            && barred[0].contains("no artifact without provenance may derive a floor"),
        "{}",
        barred[0]
    );

    // The same absence under an UNDERIVED row is PENDING, not a finding: a
    // placeholder is a statement about what has not been done, and reddening on
    // it would only tempt someone to delete the row.
    plant.floors("underived", "[]", None, "[]");
    let pending = plant.audit();
    assert!(findings(&pending, Arm::MissingArtifact).is_empty());
    assert_eq!(pending.pending.len(), audit::OWED_SCENARIOS.len());
    assert!(pending.clean());
}

/// Arm (iii), first half: a subject that is not in this history.
#[test]
fn calibration_iii_a_subject_off_this_history_reds() {
    let plant = Plant::new();
    plant.floors("underived", "[]", None, "[]");
    let head = git(&plant.root, &["rev-parse", "HEAD"]);

    // Current: the subject IS HEAD.
    plant.write(
        "benchmarks/runs/scenario-1.json",
        &scenario_run(
            ScenarioId::Condense,
            90,
            1_000_000_000,
            Some(&format!("plant-1-g{head}")),
        ),
    );
    let clean = plant.audit();
    assert!(
        findings(&clean, Arm::Staleness).is_empty(),
        "a subject that IS HEAD is current: {}",
        clean.render()
    );

    // Planted: a real commit on a branch this history abandoned.
    git(&plant.root, &["checkout", "--quiet", "-b", "sidetrack"]);
    let orphan = plant.commit("crates/measured/src/lib.rs", "// sidetrack\n");
    git(&plant.root, &["checkout", "--quiet", "-"]);
    plant.write(
        "benchmarks/runs/scenario-1.json",
        &scenario_run(
            ScenarioId::Condense,
            90,
            1_000_000_000,
            Some(&format!("plant-1-g{orphan}")),
        ),
    );
    let red = plant.audit();
    let hits = findings(&red, Arm::Staleness);
    assert_eq!(hits.len(), 1, "{}", red.render());
    assert!(hits[0].contains("NOT an ancestor of HEAD"), "{}", hits[0]);
    assert!(!red.clean());
}

/// Arm (iii), second half — **the failure that actually happened here**: a
/// subject that is in this history but predates the last change to what it
/// claims to measure. An mtime cannot see this; that is why the field exists.
#[test]
fn calibration_iii_a_subject_predating_the_measured_paths_reds() {
    let plant = Plant::new();
    plant.floors("underived", "[]", None, "[]");
    let before = git(&plant.root, &["rev-parse", "HEAD"]);

    // The artifact is current as of `before` — and then the measured path
    // changes underneath it, exactly as the caps landed five hours after the
    // last committed sweep.
    plant.write(
        "benchmarks/runs/scenario-1.json",
        &scenario_run(
            ScenarioId::Condense,
            90,
            1_000_000_000,
            Some(&format!("plant-1-g{before}")),
        ),
    );
    assert!(
        findings(&plant.audit(), Arm::Staleness).is_empty(),
        "nothing has moved yet"
    );

    plant.commit("crates/measured/src/lib.rs", "// the cap changed\n");
    let red = plant.audit();
    let hits = findings(&red, Arm::Staleness);
    assert_eq!(hits.len(), 1, "{}", red.render());
    assert!(hits[0].contains("predates"), "{}", hits[0]);
    assert!(hits[0].contains("declared measured paths"), "{}", hits[0]);

    // And a DIRTY subject is refused outright, whatever its ancestry: the tree
    // it names exists on no other machine.
    let mut body: serde_json::Value = serde_json::from_str(&scenario_run(
        ScenarioId::Condense,
        90,
        1_000_000_000,
        Some(&format!("plant-1-g{before}")),
    ))
    .expect("json");
    body["legs"][0]["status"]["dirty"] = serde_json::Value::Bool(true);
    plant.write(
        "benchmarks/runs/scenario-1.json",
        &serde_json::to_string_pretty(&body).expect("json"),
    );
    let dirty = plant.audit();
    assert!(
        findings(&dirty, Arm::Staleness).is_empty(),
        "a dirty run is a RECORD, exempt from currency and barred from a floor — not a stale \
         claim: {}",
        dirty.render()
    );
}

/// Arm (iv): a fence that has stopped agreeing with the artifacts it names.
#[test]
fn calibration_iv_a_fence_that_disagrees_with_its_own_json_reds() {
    let plant = Plant::new();
    plant.floors("underived", "[]", None, "[]");
    let report = sweep(40_000_000, 190_000_000);
    plant.write(
        "benchmarks/hook-latency/hook-latency-1.json",
        &serde_json::to_string_pretty(&report).expect("json"),
    );
    let truth = BenchFacts::of(
        &[("hook-latency-1".to_owned(), report)],
        "planted".to_owned(),
    );
    let doc = format!("# Planted\n\nprose\n\n{}", truth.render_fence());
    plant.write("benchmarks/planted.md", &doc);
    let clean = plant.audit();
    assert!(
        findings(&clean, Arm::FenceDisagreement).is_empty(),
        "a generated fence agrees with its own artifacts: {}",
        clean.render()
    );

    // One digit, in the one number the budget question turns on.
    let mut tampered = truth.clone();
    tampered.arms[0].git_latency_ns_median = 90_000_000;
    plant.write(
        "benchmarks/planted.md",
        &format!("# Planted\n\nprose\n\n{}", tampered.render_fence()),
    );
    let red = plant.audit();
    let hits = findings(&red, Arm::FenceDisagreement);
    assert_eq!(hits.len(), 1, "{}", red.render());
    assert!(hits[0].contains("B1_live"), "{}", hits[0]);
    assert!(hits[0].contains("90000000"), "{}", hits[0]);
    assert!(hits[0].contains("150000000"), "{}", hits[0]);

    // A fence naming an artifact nobody committed is the other half of the
    // same arm: the fence must be re-derivable from files that exist.
    let mut orphaned = truth.clone();
    orphaned.artifacts.push(999_999);
    plant.write(
        "benchmarks/planted.md",
        &format!("# Planted\n\n{}", orphaned.render_fence()),
    );
    let missing = findings(&plant.audit(), Arm::FenceDisagreement);
    assert_eq!(missing.len(), 1);
    assert!(missing[0].contains("hook-latency-999999"), "{}", missing[0]);

    // And two fences is an error rather than a choice.
    plant.write(
        "benchmarks/planted.md",
        &format!("{}\n{}", truth.render_fence(), truth.render_fence()),
    );
    let two = findings(&plant.audit(), Arm::FenceDisagreement);
    assert_eq!(two.len(), 1);
    assert!(two[0].contains("one home"), "{}", two[0]);
}

/// The enumeration arm: a scenario dropped from the set must not leave a green
/// gate behind it.
#[test]
fn calibration_enumeration_a_dropped_scenario_reds() {
    let plant = Plant::new();
    plant.floors("underived", "[]", None, "[]");
    assert!(plant.audit().clean());

    let body = fs::read_to_string(plant.root.join("benchmarks/floors.toml")).expect("floors");
    let cut = body
        .split("\n[[floor]]")
        .filter(|chunk| !chunk.contains("\"condense\""))
        .collect::<Vec<_>>()
        .join("\n[[floor]]");
    plant.write("benchmarks/floors.toml", &cut);
    let red = plant.audit();
    let hits = findings(&red, Arm::Enumeration);
    assert_eq!(hits.len(), 1, "{}", red.render());
    assert!(hits[0].contains("condense"), "{}", hits[0]);
}

/// A floors file that does not hold together on its own terms reds before any
/// artifact is read — otherwise the guard would enforce a bar nobody derived.
#[test]
fn a_floors_file_that_contradicts_itself_reds() {
    let plant = Plant::new();
    // Derived, but the stated floor no longer follows from the medians.
    plant.floors(
        "derived",
        "[100.0, 110.0, 120.0]",
        Some("500.0"),
        THREE_RUNS,
    );
    let red = plant.audit();
    let hits = findings(&red, Arm::FloorsFile);
    assert_eq!(hits.len(), 1, "{}", red.render());
    assert!(hits[0].contains("imply 50"), "{}", hits[0]);

    // And an unparseable file is a finding, not an empty pass.
    plant.write("benchmarks/floors.toml", "this is not toml\n");
    let broken = plant.audit();
    assert_eq!(findings(&broken, Arm::FloorsFile).len(), 1);
    assert!(!broken.clean());
}

/// Arm (i) again, in its **other** direction: a latency ceiling.
///
/// A throughput floor and a latency budget fail in opposite directions, and
/// until the displacement artifact existed the latency branch of the violation
/// arm had never been seen red — a guard nobody has watched fail on a bar of
/// that shape is an assertion about the bar.
#[test]
fn calibration_i_latency_a_first_commit_over_the_hard_budget_reds() {
    let plant = Plant::new();
    let head = git(&plant.root, &["rev-parse", "HEAD"]);
    let describe = format!("plant-1-g{head}");
    plant.latency_floor();

    // 90 ms first commit against a 40 ms control = 50 ms of git latency.
    plant.write(
        "benchmarks/enable-displacement/enable-displacement-1.json",
        &displacement(40_000_000, 90_000_000, &describe),
    );
    let clean = plant.audit();
    assert!(
        clean.clean(),
        "a first commit inside the budget is a pass: {}",
        clean.render()
    );

    // 190 ms against the same control = 150 ms, over the 100 ms HARD budget.
    plant.write(
        "benchmarks/enable-displacement/enable-displacement-1.json",
        &displacement(40_000_000, 190_000_000, &describe),
    );
    let red = plant.audit();
    let hits = findings(&red, Arm::Violation);
    assert_eq!(hits.len(), 1, "{}", red.render());
    assert!(hits[0].contains("150.00 ms"), "{}", hits[0]);
    assert!(hits[0].contains("over the 100 ms budget"), "{}", hits[0]);
    assert!(hits[0].contains("E1_commit_after_enable"), "{}", hits[0]);
}

/// The two arms the budget deliberately does NOT bind. `E0` is a wall time for
/// a different act; `E2` is the counterfactual, and it is *expected* to be
/// slower — that is the displacement being measured. Judging either against the
/// budget would fail the artifact for arms it never aimed at.
#[test]
fn the_budget_binds_the_first_commit_arm_and_not_the_other_two() {
    let plant = Plant::new();
    let head = git(&plant.root, &["rev-parse", "HEAD"]);
    plant.latency_floor();
    // E0 at 900 ms and E2 at 400 ms, both far over 100 ms; E1 inside.
    let mut body: serde_json::Value = serde_json::from_str(&displacement(
        40_000_000,
        90_000_000,
        &format!("plant-1-g{head}"),
    ))
    .expect("json");
    body["arms"][1]["stats"]["p50_ns"] = serde_json::json!(900_000_000u64);
    body["arms"][3]["stats"]["p50_ns"] = serde_json::json!(400_000_000u64);
    plant.write(
        "benchmarks/enable-displacement/enable-displacement-1.json",
        &serde_json::to_string_pretty(&body).expect("json"),
    );
    let verdict = plant.audit();
    assert!(
        verdict.clean(),
        "only the first-commit arm is judged: {}",
        verdict.render()
    );
}

/// The load ceiling, calibrated — **the arm a verifier found dormant**.
///
/// `Artifact::load_void` was written, wired into the derivation check, and
/// never once seen red: disabling it left the guard's output identical to
/// pristine, because every floor row in the tree is `underived` and the check
/// only runs for a floor that BINDS. It is dormant today and it is the safety
/// catch for the next quiet-machine measurement, on a box that has been
/// measured at load 35 to 101 against a stated ceiling of 8.0. So it
/// gets the calibration every other arm has.
///
/// Both void shapes, because they are different sentences: a reading over the
/// ceiling, and no reading at all. An unread machine is not a quiet one.
#[test]
fn calibration_load_a_floor_derived_from_a_busy_or_unread_run_reds() {
    let plant = Plant::new();
    plant.floors("derived", "[100.0, 110.0, 120.0]", Some("50.0"), THREE_RUNS);
    let head = git(&plant.root, &["rev-parse", "HEAD"]);
    let describe = format!("plant-1-g{head}");

    // The control: three current runs, above the floor, all taken quiet.
    let quiet = |n: u32| {
        plant.write(
            &format!("benchmarks/runs/scenario-{n}.json"),
            &scenario_run(ScenarioId::Condense, 90, 1_000_000_000, Some(&describe)),
        );
    };
    for n in 1..=3 {
        quiet(n);
    }
    let clean = plant.audit();
    assert!(
        clean.clean(),
        "three quiet runs above the floor are a pass: {}",
        clean.render()
    );

    // One of the three was taken on a box over the ceiling — a real reading
    // from a real session here, not an invented one.
    let with_load = |load: serde_json::Value| {
        let mut body: serde_json::Value = serde_json::from_str(&scenario_run(
            ScenarioId::Condense,
            90,
            1_000_000_000,
            Some(&describe),
        ))
        .expect("json");
        body["attestation"] = load;
        serde_json::to_string_pretty(&body).expect("json")
    };
    plant.write(
        "benchmarks/runs/scenario-2.json",
        &with_load(serde_json::json!({ "load1": 55.06, "void_above": 8.0 })),
    );
    let busy = plant.audit();
    let hits = findings(&busy, Arm::Staleness);
    assert_eq!(hits.len(), 1, "{}", busy.render());
    assert!(hits[0].contains("scenario-2"), "{}", hits[0]);
    assert!(
        hits[0].contains("over the stated load ceiling"),
        "{}",
        hits[0]
    );
    assert!(
        hits[0].contains("may not set a bar other runs are judged against"),
        "{}",
        hits[0]
    );
    assert!(!busy.clean());

    // And the other void shape: the reading was never taken. Same arm, same
    // refusal — silence about the machine is not evidence the machine was
    // quiet.
    plant.write(
        "benchmarks/runs/scenario-3.json",
        &with_load(serde_json::Value::Null),
    );
    let unread = plant.audit();
    assert_eq!(
        findings(&unread, Arm::Staleness).len(),
        2,
        "{}",
        unread.render()
    );

    // Restore both and the finding goes away, so the arm is keyed on the load
    // and not on anything else that moved.
    quiet(2);
    quiet(3);
    assert!(plant.audit().clean());
}

/// Arm (iv) for the all-families ```bench-report fence: a generated fence
/// over the planted tree agrees; one tampered digit reds naming the row; a
/// stamp nobody committed reds; a throughput row dropped from the aggregate
/// reds naming the floor it left behind.
#[test]
fn calibration_iv_a_report_fence_that_disagrees_with_its_artifacts_reds() {
    use bench::report_facts::{ReportFacts, REPORT_TAG};

    let plant = Plant::new();
    plant.floors("underived", "[]", None, "[]");
    plant.write(
        "benchmarks/native-baseline/native-baseline-1.json",
        &scenario_run(ScenarioId::Condense, 25, 2_000_000_000, None),
    );
    plant.write(
        "benchmarks/hook-latency/hook-latency-1.json",
        &serde_json::to_string_pretty(&sweep(40_000_000, 90_000_000)).expect("json"),
    );
    let inputs = bench::audit::Inputs::load(&plant.root);
    let truth = ReportFacts::of(&inputs, inputs.floors.as_ref());
    assert_eq!(truth.families.len(), 2, "{truth:?}");
    let condense = truth
        .throughput
        .iter()
        .find(|t| t.scenario == "condense")
        .expect("the planted row");
    assert_eq!(condense.runs, 1);
    assert_eq!(condense.rate_milli_per_s_median, Some(12_500));
    let doc = |fence: &ReportFacts| format!("# Planted\n\nprose\n\n{}", fence.render_fence());
    plant.write("benchmarks/README.md", &doc(&truth));
    let clean = plant.audit();
    assert!(
        findings(&clean, Arm::FenceDisagreement).is_empty(),
        "a generated report fence agrees with its own artifacts: {}",
        clean.render()
    );

    // One digit, in the one number the floor question turns on.
    let mut tampered = truth.clone();
    let row = tampered
        .throughput
        .iter_mut()
        .find(|t| t.scenario == "condense")
        .expect("row");
    row.rate_milli_per_s_median = Some(99_000);
    plant.write("benchmarks/README.md", &doc(&tampered));
    let red = findings(&plant.audit(), Arm::FenceDisagreement);
    assert_eq!(red.len(), 1, "{red:?}");
    assert!(red[0].contains(REPORT_TAG), "{}", red[0]);
    assert!(red[0].contains("`condense`"), "{}", red[0]);
    assert!(red[0].contains("99000"), "{}", red[0]);
    assert!(red[0].contains("12500"), "{}", red[0]);

    // A stamp nobody committed: the fence must be re-derivable from files
    // that exist.
    let mut orphaned = truth.clone();
    orphaned.families[1].artifacts.push(999_999);
    plant.write("benchmarks/README.md", &doc(&orphaned));
    let missing = findings(&plant.audit(), Arm::FenceDisagreement);
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert!(
        missing[0].contains("native-baseline-999999"),
        "{}",
        missing[0]
    );

    // A floor dropped from the aggregate must not leave a green gate.
    let mut dropped = truth.clone();
    dropped.throughput.retain(|t| t.scenario != "condense");
    plant.write("benchmarks/README.md", &doc(&dropped));
    let gone = findings(&plant.audit(), Arm::FenceDisagreement);
    assert!(
        gone.iter()
            .any(|f| f.contains("carries no row for `condense`")),
        "{gone:?}"
    );
}

/// The clean arm on the real tree: `benchmarks/README.md`'s report fence,
/// where present, agrees with the artifacts it names. Kept apart from the
/// whole-tree verdict so the fence's own agreement is visible even while a
/// staleness finding stands elsewhere.
#[test]
fn the_committed_report_fence_agrees_with_its_artifacts() {
    let root = bench::leg::default_repo_root();
    let verdict = audit::audit(&root);
    let fence_findings = findings(&verdict, Arm::FenceDisagreement);
    assert!(
        fence_findings.is_empty(),
        "the committed fences disagree with their artifacts:\n{}",
        fence_findings.join("\n")
    );
}

//! End-to-end: the harness runs today, with the backend leg absent.
//!
//! This is the mode the harness ships in — rift-only. A stand-in binary plays the
//! measured leg (the real one is built by the workspace, not by this package), so
//! the test proves the *harness*: fixtures get built, sandboxes get prepared,
//! capture gets enabled before the timed verb, samples get collected, and the
//! absent backend is recorded with its reason instead of being invented.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::{fs, path::Path, path::PathBuf};

use rift_bench_native_baseline::{
    driver::{Driver, RunOptions},
    fixture::FixtureSet,
    leg::{LegId, LegResolver},
    measure::{BudgetVerdict, Protocol},
    report::{LegOutcome, Report, REPORT_SCHEMA},
    scenario::ScenarioId,
};

/// A stand-in for a leg binary: appends its argv (and stdin, for hooks) to a log
/// and exits 0. Its `status` answers with the projection counter the harness
/// reads for the ingest proof — here, the number of hook calls it has logged —
/// so the whole proof path is exercised without the real binary. Its
/// `--json session …` and `--json why …` answer the answer probes the two
/// resolver rows ask (a restored transcript; a resolved checkpoint), so
/// those rows measure through the same stand-in.
#[cfg(unix)]
fn stand_in(dir: &Path, log: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("stand-in");
    let script = format!(
        "#!/bin/sh\nprintf 'argv: %s\\n' \"$*\" >> {log}\n\
         if [ \"$1\" = hooks ]; then cat >> {log}; printf '\\n' >> {log}; fi\n\
         if [ \"$1\" = status ]; then\n\
         \x20 n=$(grep -c '^argv: hooks' {log} 2>/dev/null)\n\
         \x20 printf 'stand-in status\\n  captured events: %s\\n' \"${{n:-0}}\"\n\
         fi\n\
         if [ \"$1\" = --json ] && [ \"$2\" = session ]; then\n\
         \x20 printf '{{\"sessions\":[{{\"restored\":\"restored\"}}]}}\\n'\n\
         fi\n\
         if [ \"$1\" = --json ] && [ \"$2\" = why ]; then\n\
         \x20 printf '{{\"selection\":{{\"checkpoint_id\":\"stand-in\",\"resolved\":true}}}}\\n'\n\
         fi\nexit 0\n",
        log = log.display()
    );
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// The measured leg's row for `id`: real samples, from the stand-in, and — for a
/// hook row — the proof it wrote something durable.
#[cfg(unix)]
fn assert_measured_leg(report: &Report, id: ScenarioId) {
    let scenario = report
        .scenarios
        .iter()
        .find(|s| s.id == id)
        .expect("scenario in report");
    let LegOutcome::Measured {
        stats,
        command,
        ingest,
        ..
    } = &scenario.legs["rift"]
    else {
        panic!("{} should have measured: {:?}", id.as_str(), scenario.legs);
    };
    assert_eq!(stats.samples, 2, "both measured iterations kept");
    assert!(stats.clean(), "the stand-in exits 0");
    assert!(stats.p50_ns > 0);
    assert!(command.contains("stand-in"));
    // No head-to-head can exist with one leg.
    assert!(scenario.comparison.is_none());

    // A hook row carries proof it wrote; a read-only verb has none to give.
    if id == ScenarioId::HookRoundTrip {
        let check = ingest.expect("a hook row proves its ingest");
        assert!(check.verified());
        assert_eq!(check.required_growth, 2);
        assert!(check.events_after >= check.events_before + 2);
        // Two facts, never one.
        assert_eq!(
            scenario.budget_verdict_of(),
            Some(BudgetVerdict::Latency {
                p50_met: true,
                p95_met: true
            })
        );
    } else {
        assert!(ingest.is_none(), "{} writes nothing", id.as_str());
    }
}

/// The four rows the rift-only run drives, in report order.
#[cfg(unix)]
const RIFT_ONLY_ROWS: [ScenarioId; 4] = [
    ScenarioId::ColdStatus,
    ScenarioId::HookRoundTrip,
    ScenarioId::ResumeRestore,
    ScenarioId::BlameWhy,
];

#[cfg(unix)]
#[test]
fn a_rift_only_run_measures_one_leg_and_records_the_other_as_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("calls.log");
    let binary = stand_in(tmp.path(), &log);
    let report = rift_only_report(tmp.path(), binary);

    // --- the measured leg -------------------------------------------------
    for id in RIFT_ONLY_ROWS {
        assert_measured_leg(&report, id);
    }
    assert_resolver_rows_were_asked_their_answer(&log);

    // --- the absent leg ---------------------------------------------------
    assert_backend_leg_absent(&report);

    // --- what the leg actually saw ---------------------------------------
    assert_capture_enabled_outside_the_timed_region(&log);

    // --- the artifact -----------------------------------------------------
    assert_artifact_round_trips(&report, &tmp.path().join("out"));
}

/// Both legs asked for over the four rows, two measured iterations each, no
/// untimed probes: only the stand-in plays a leg, so the backend is absent.
#[cfg(unix)]
fn rift_only_report(root: &Path, binary: PathBuf) -> Report {
    let resolver = LegResolver {
        rift_override: Some(binary),
        reference_override: None,
        vogon_override: None,
        repo_root: root.join("repo-root"),
        path_var: None,
        install_root: Some(root.join("install")),
        allow_debug: false,
        source_clock: None,
        allow_stale: false,
        git_provenance: None,
    };
    let driver = Driver::new(
        root.join("runs"),
        FixtureSet::new(root.join("fixtures")),
        resolver,
    )
    .unwrap();

    driver
        .run(&RunOptions {
            scenarios: RIFT_ONLY_ROWS.to_vec(),
            legs: LegId::all().to_vec(),
            protocol: Some(Protocol::new(0, 2)),
            probe_rss: false,
            probe_spawns: false,
        })
        .unwrap()
}

/// The two resolver rows ran their preludes — a session on a session id
/// of its own, a file, a commit — and were asked their answer once more.
#[cfg(unix)]
fn assert_resolver_rows_were_asked_their_answer(log: &Path) {
    let calls = fs::read_to_string(log).unwrap();
    assert!(
        calls.contains("argv: --json session resume main --force"),
        "the resume verb was timed: {calls}"
    );
    assert!(
        calls.contains("argv: --json why bench/why_1.txt:1"),
        "the why verb was timed on the last measured iteration's file: {calls}"
    );
    let why_probe = calls.matches("argv: --json why bench/why_1.txt:1").count();
    assert_eq!(
        why_probe, 2,
        "timed once, then asked its answer once: {calls}"
    );
}

/// Every row skips the backend with a reason, the leg list still names it,
/// and only the fixture this run asked for was built.
#[cfg(unix)]
fn assert_backend_leg_absent(report: &Report) {
    for scenario in &report.scenarios {
        let LegOutcome::Skipped { reason } = &scenario.legs["reference"] else {
            panic!("the backend is not installed; it cannot be measured");
        };
        assert!(!reason.is_empty(), "an absence always carries a reason");
    }
    let backend = report
        .legs
        .iter()
        .find(|l| l.id == LegId::Reference)
        .expect("both legs are always listed");
    assert!(!backend.status.is_present());
    assert!(backend.binary_bytes.is_none());

    // Only the `small` fixture was built: nothing in this run asked for more.
    assert_eq!(report.fixtures.len(), 1);
    assert_eq!(report.fixtures[0].spec.name, "small");
}

/// Capture was enabled before the timed verbs, and the hook payload reached
/// the leg on stdin.
#[cfg(unix)]
fn assert_capture_enabled_outside_the_timed_region(log: &Path) {
    let calls = fs::read_to_string(log).unwrap();
    assert!(
        calls.contains("argv: init --no-recall"),
        "capture is enabled outside the timed region: {calls}"
    );
    assert!(calls.contains("argv: status"));
    assert!(calls.contains("argv: hooks claude-code PreToolUse"));
    assert!(
        calls.contains("\"hook_event_name\":\"PreToolUse\""),
        "the hook payload reaches the leg on stdin: {calls}"
    );
}

/// The JSON artifact reads back as the report it was written from, and the
/// Markdown one says the backend is absent.
#[cfg(unix)]
fn assert_artifact_round_trips(report: &Report, out: &Path) {
    let json = report.write_json(out).unwrap();
    let md = report.write_markdown(out).unwrap();
    let round_tripped: Report = serde_json::from_slice(&fs::read(&json).unwrap()).unwrap();
    assert_eq!(&round_tripped, report);
    assert_eq!(round_tripped.schema_version, REPORT_SCHEMA);
    let rendered = fs::read_to_string(&md).unwrap();
    assert!(rendered.contains("**absent**"));
    assert!(rendered.contains("Not measured"));
}

#[cfg(unix)]
#[test]
fn a_leg_that_fails_is_reported_failed_not_fast() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let binary = tmp.path().join("angry");
    fs::write(
        &binary,
        "#!/bin/sh\necho 'not a rift repository' >&2\nexit 2\n",
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();

    let resolver = LegResolver {
        rift_override: Some(binary),
        reference_override: None,
        vogon_override: None,
        repo_root: tmp.path().join("repo-root"),
        path_var: None,
        install_root: None,
        allow_debug: false,
        source_clock: None,
        allow_stale: false,
        git_provenance: None,
    };
    let driver = Driver::new(
        tmp.path().join("runs"),
        FixtureSet::new(tmp.path().join("fixtures")),
        resolver,
    )
    .unwrap();

    let report = driver
        .run(&RunOptions {
            scenarios: vec![ScenarioId::ColdStatus],
            legs: vec![LegId::Rift],
            protocol: Some(Protocol::new(0, 2)),
            probe_rss: false,
            probe_spawns: false,
        })
        .unwrap();

    let LegOutcome::Failed { reason, .. } = &report.scenarios[0].legs["rift"] else {
        panic!("a leg whose setup exits 2 has not been measured");
    };
    assert!(reason.contains("not a rift repository"), "{reason}");
    assert!(report.to_markdown().contains("FAILED"));
}

/// A stand-in that also *spawns*: two `git` children per hook invocation, which
/// is what the identity row exists to count.
#[cfg(unix)]
fn spawning_stand_in(dir: &Path, log: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("spawning-stand-in");
    let script = format!(
        "#!/bin/sh\nprintf 'argv: %s\\n' \"$*\" >> {log}\n\
         if [ \"$1\" = hooks ]; then cat >> {log}; printf '\\n' >> {log}\n\
         \x20 git --version > /dev/null; git --version > /dev/null; fi\n\
         if [ \"$1\" = status ]; then\n\
         \x20 n=$(grep -c '^argv: hooks' {log} 2>/dev/null)\n\
         \x20 printf 'stand-in status\\n  captured events: %s\\n' \"${{n:-0}}\"\n\
         fi\nexit 0\n",
        log = log.display()
    );
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// The identity row publishes a child-process count, and the count is of a hook
/// that really ran: the same row still owes — and delivers — its ingest proof.
#[cfg(unix)]
#[test]
fn the_identity_row_counts_the_child_processes_its_hook_launches() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("stand-in.log");
    let resolver = LegResolver {
        rift_override: Some(spawning_stand_in(tmp.path(), &log)),
        reference_override: None,
        vogon_override: None,
        repo_root: tmp.path().join("repo-root"),
        path_var: None,
        install_root: None,
        allow_debug: false,
        source_clock: None,
        allow_stale: false,
        git_provenance: None,
    };
    let driver = Driver::new(
        tmp.path().join("runs"),
        FixtureSet::new(tmp.path().join("fixtures")),
        resolver,
    )
    .unwrap();
    // Without a shimmable `git` on this host there is nothing to count, and the
    // harness says so rather than reporting zero.
    let countable = driver.spawn_probe().is_some();

    let report = driver
        .run(&RunOptions {
            scenarios: vec![ScenarioId::IdentityAttribution],
            legs: vec![LegId::Rift],
            protocol: Some(Protocol::new(0, 2)),
            probe_rss: false,
            probe_spawns: true,
        })
        .unwrap();

    let LegOutcome::Measured {
        child_spawns,
        ingest,
        ..
    } = &report.scenarios[0].legs["rift"]
    else {
        panic!("the identity row measured: {:?}", report.scenarios[0].legs);
    };
    // Two events owed over two iterations, and the stand-in logged them.
    assert_eq!(ingest.map(|c| c.required_growth), Some(2));
    assert!(ingest.is_some_and(|c| c.verified()));

    match (countable, child_spawns) {
        (true, Some(count)) => {
            assert_eq!(count.by_program.get("git"), Some(&2), "{count:?}");
            assert_eq!(count.total, 2);
            assert!(report.to_markdown().contains("## Child processes"));
        }
        (true, other) => panic!("a shimmable host must produce a count: {other:?}"),
        (false, other) => assert_eq!(*other, None, "no probe, no number"),
    }

    // And the count came from a hook the probe ran itself, one iteration past
    // the measured window — not from re-timing the last measured invocation.
    let logged = fs::read_to_string(&log).unwrap();
    assert_eq!(
        logged.matches("argv: hooks").count(),
        if countable { 3 } else { 2 },
        "{logged}"
    );
}

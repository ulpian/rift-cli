//! The ratchet's **exit code**, through the shipped binary.
//!
//! `ratchet::outcome` is unit-tested, but the defect that shipped was not in
//! the decision — it was in the wiring: `main.rs` declared `--enforce` and
//! never read it, so the process printed "this does not fail a build" and then
//! failed the build, identically with and without the flag. **No unit test
//! could have caught that**, because the bug lived between the flag and the
//! decision. Only running the binary does, which is why this suite exists and
//! why it drives `CARGO_BIN_EXE_vs-reference` (cargo rebuilds it for this test, so
//! it cannot go stale — a hazard that cost a day to close elsewhere).
//!
//! It uses two **committed** artifacts rather than fixtures it writes, so it
//! also fails if the report schema drifts away from what is on disk.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::PathBuf;
use std::process::Command;

/// A pair of committed artifacts that actually **disagree**.
///
/// Chosen by asking the binary rather than by index: a pair that happens to
/// hold would make every assertion below vacuous, which is the failure this
/// whole suite exists to catch one level up.
fn regressing_pair() -> Option<(PathBuf, PathBuf)> {
    let all = artifacts()?;
    for (i, a) in all.iter().enumerate() {
        for b in all.iter().skip(i + 1) {
            if run(a, b, false).1.contains("REGRESSED") {
                return Some((a.clone(), b.clone()));
            }
            if run(b, a, false).1.contains("REGRESSED") {
                return Some((b.clone(), a.clone()));
            }
        }
    }
    None
}

/// Every committed run artifact, sorted.
fn artifacts() -> Option<Vec<PathBuf>> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .join("benchmarks/hook-latency");
    let mut json: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    json.sort();
    (!json.is_empty()).then_some(json)
}

fn run(baseline: &PathBuf, candidate: &PathBuf, enforce: bool) -> (i32, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vs-reference"));
    cmd.arg("ratchet")
        .arg("--baseline")
        .arg(baseline)
        .arg("--candidate")
        .arg(candidate);
    if enforce {
        cmd.arg("--enforce");
    }
    let out = cmd.output().expect("run vs-reference ratchet");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

/// Report-only, asserted where it actually lives: the exit code.
#[test]
fn the_flag_changes_the_exit_code_and_the_default_cannot_fail_a_build() {
    let Some((baseline, candidate)) = regressing_pair() else {
        panic!(
            "no committed pair of hook-latency artifacts disagrees, so this test would \
             assert nothing"
        );
    };

    let (code, text) = run(&baseline, &candidate, false);
    assert_eq!(
        code, 0,
        "REPORT-ONLY must exit 0. It exited {code}, which is the shipped defect:\n{text}"
    );
    assert!(
        text.contains("REPORT-ONLY (the owner's call)"),
        "the banner must be re-emitted on the way out, for a reader who scrolled \
         past the header to the exit code:\n{text}"
    );
    assert!(
        text.contains("NOT a passing gate"),
        "exit 0 must not be readable as a pass:\n{text}"
    );

    let (enforced_code, enforced_text) = run(&baseline, &candidate, true);
    assert_eq!(
        enforced_code, 1,
        "--enforce must fail the build:\n{enforced_text}"
    );
    assert!(enforced_text.contains("ratchet FAILED"), "{enforced_text}");

    // The defect was that these two were byte-identical. If they ever are
    // again, the flag has stopped being read.
    assert_ne!(
        code, enforced_code,
        "the flag reached no decision — identical exit codes are the original defect"
    );
    assert_ne!(
        text, enforced_text,
        "identical output means the flag is decoration"
    );
}

/// **The help text is a surface, so a test reads it.**
///
/// `ratchet --help` printed "Exits non-zero on a regression, so it can be a
/// gate leg" three options above the flag that made it report-only — added by
/// the very commit that made it report-only. `ratchet_exit` asserted exit
/// codes and banner text and never the help, so nothing reddened it. A
/// docstring is a claim on the CLI surface; this is the assertion behind it.
#[test]
fn the_help_text_does_not_promise_an_exit_code_the_binary_does_not_give() {
    let out = Command::new(env!("CARGO_BIN_EXE_vs-reference"))
        .args(["ratchet", "--help"])
        .output()
        .expect("run vs-reference ratchet --help");
    assert!(out.status.success(), "--help must succeed");
    let help = String::from_utf8_lossy(&out.stdout).into_owned();

    // Case-INSENSITIVE. The first version compared against the capital-E
    // spelling while the doc comment carried the same phrase in lowercase as
    // quoted history, so the guard passed on a technicality and read stronger
    // than it was — the failure mode this suite watches for, in miniature.
    assert!(
        !help
            .to_lowercase()
            .contains("exits non-zero on a regression"),
        "the help promises an exit code the default does not give:\n{help}"
    );
    assert!(
        help.contains("Report-only by default"),
        "the help must state the default it actually has:\n{help}"
    );
    assert!(
        help.contains("--enforce"),
        "and name the flag that changes it:\n{help}"
    );

    // The help and the behaviour are asserted against each other rather than
    // separately: whatever the help says about exit 0 has to be what happens.
    let Some((baseline, candidate)) = regressing_pair() else {
        panic!("no regressing pair to check the help against");
    };
    let (code, _) = run(&baseline, &candidate, false);
    assert_eq!(
        code, 0,
        "the help says report-only by default; the binary exited {code}"
    );
}

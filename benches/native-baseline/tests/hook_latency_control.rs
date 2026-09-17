//! The git-latency instrument, driven end to end on the one arm that needs no
//! rift binary.
//!
//! An instrument whose only tests parse strings has never been shown to
//! measure anything. This drives `A_bare` — `git init`, a seed commit, staged
//! files, timed commits, the oracle pass — through the real hermetic process
//! plane, and asserts the two things that make the control a control: it
//! produces one sample per measured iteration, and it exercises **no** capture
//! (no projection, no trailer). The second half is what makes every other arm's
//! oracles mean something; a control that quietly captured would make every
//! quoted `hook latency` an understatement.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use rift_bench_native_baseline::{
    hermetic::Hermetic,
    hooklat::{arms, run_arm, Arm, Capture},
    measure::Protocol,
};

#[test]
fn the_bare_control_commits_for_real_and_captures_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("sweep");
    let hermetic = Hermetic::prepare(root.join("hermetic")).expect("hermetic");
    let control = arms()
        .into_iter()
        .find(Arm::is_control)
        .expect("the sweep has a control");
    assert_eq!(control.capture, Capture::Off);

    // No rift binary is needed or used by this arm; the path is deliberately
    // one that does not exist, so a regression that started invoking it here
    // would fail loudly rather than measure a different arm.
    let missing_rift = tmp.path().join("no-such-rift");
    let result = run_arm(
        &hermetic,
        &missing_rift,
        &control,
        &root,
        Protocol::new(1, 3),
    );

    let stats = result
        .stats
        .unwrap_or_else(|| panic!("control produced no samples: {:?}", result.error));
    assert_eq!(result.error, None);
    assert_eq!(stats.samples, 3, "one sample per measured iteration");
    assert_eq!(stats.failures, 0, "every commit exited zero");
    assert!(stats.p50_ns > 0, "a real commit takes measurable time");

    assert!(
        result.oracles.projections.is_empty(),
        "the control must create no projection: {:?}",
        result.oracles.projections
    );
    assert!(
        !result.oracles.trailer_present,
        "the control must not write a checkpoint trailer"
    );
    assert_eq!(result.oracles.captured_events, None);
    assert!(
        result.phases.is_empty(),
        "no hooks are installed, so nothing emits a timing line"
    );

    // The commits are real: seed + warmup + measured + the untimed phase pass.
    let log = std::process::Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(root.join(&control.id).join("repo"))
        .output()
        .expect("git log");
    let lines = String::from_utf8_lossy(&log.stdout).lines().count();
    assert_eq!(lines, 6, "seed + 1 warmup + 3 measured + 1 phase pass");
}

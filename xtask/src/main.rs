//! `cargo run -p xtask -- <command>` — workspace tooling entrypoint.
//!
//! * `check-coverage <cov.json>` — the per-crate ≥95% line gate for the security
//!   set (`ci/security-crates.txt`) **and** the ≥75% workspace branch floor,
//!   over a `cargo llvm-cov --json` report (run with `--branch`); fails closed
//!   on a missing crate or absent branch data.
//! * `ci-meta` — the CI/lint meta-checks (required jobs, read-only workflow
//!   permissions, lint-table baseline, no unsafe, no crate-root blanket allows,
//!   no schema fast-path in bins, the dialect-conformance job's suite set,
//!   deleted paths staying deleted, no retired program vocabulary in any
//!   tracked path or byte, no network crate in the binaries' default-resolved
//!   graph, no `.rs` file over 1,500 lines or function over 80, no bare
//!   `#[ignore]`).
//! * `gate-intercept` — the read-only guard on the read-interception surface;
//!   one leg per property, each named on its own output line.
//! * `dogfood` — the scripted-session savings reading, played through the
//!   clients installed on this machine. It spends real API quota, refuses to
//!   run under CI, and is a reading rather than a gate. `--compare` plays
//!   every session twice — once with every read lever off — and prints what
//!   the booked saving is worth against the removal that measures, at twice
//!   the spend.
//! * `replay-savings` — an offline, model-free replay of recorded Claude Code
//!   transcripts through this build's own read recogniser and window-journal
//!   decision. It reads private transcripts and prints aggregates only; it is
//!   a measurement, never a gate.
//! * `rustdoc-baseline` — the shrink-only baseline of rustdoc warning sites
//!   (`ci/rustdoc-baseline.txt`, both arms; runs `cargo doc` itself).
//!
//! The topology/toolchain guards run separately as `cargo test -p xtask`.
#![forbid(unsafe_code)]

use xtask::coverage::{
    check_branch_floor, check_crate_coverage, read_security_crates, BRANCH_FLOOR,
    SECURITY_THRESHOLD,
};
use xtask::workspace_root;

fn fail_with(violations: &[String], what: &str) -> Result<(), String> {
    if violations.is_empty() {
        return Ok(());
    }
    for v in violations {
        eprintln!("{what}: {v}");
    }
    Err(format!("{} {what} violation(s)", violations.len()))
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check-coverage") => {
            let path = args
                .get(1)
                .ok_or_else(|| "usage: xtask check-coverage <cov.json>".to_string())?;
            let json = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
            let crates = read_security_crates(&workspace_root())?;
            let mut violations = check_crate_coverage(&json, &crates, SECURITY_THRESHOLD)?;
            violations.extend(check_branch_floor(&json, BRANCH_FLOOR)?);
            fail_with(&violations, "coverage gate")?;
            println!(
                "coverage: all {} security crates >= {SECURITY_THRESHOLD:.0}% line, \
                 workspace branches >= {BRANCH_FLOOR:.0}%",
                crates.len()
            );
            Ok(())
        }
        Some("ci-meta") => {
            let report = xtask::ci_meta::run_all(&workspace_root())?;
            fail_with(&report.violations, "ci-meta")?;
            println!(
                "ci-meta: all checks pass ({} tracked files screened for retired vocabulary; \
                 {} .rs files and {} functions under the ceilings; {} #[ignore]s with a reason)",
                report.vocabulary_files,
                report.ceilings.files,
                report.ceilings.functions,
                report.ignores
            );
            Ok(())
        }
        Some("gate-intercept") => {
            let legs = xtask::gate_intercept::run_all(&workspace_root())?;
            let mut failed = 0_usize;
            for leg in &legs {
                let mark = if leg.violations.is_empty() {
                    "ok"
                } else {
                    failed += 1;
                    "FAIL"
                };
                println!("gate-intercept: {} {mark} — {}", leg.name, leg.detail);
                for violation in &leg.violations {
                    eprintln!("gate-intercept {}: {violation}", leg.name);
                }
            }
            if failed > 0 {
                return Err(format!("{failed} gate-intercept leg(s) failed"));
            }
            Ok(())
        }
        Some("dogfood") => {
            let opts = xtask::dogfood::parse_args(args.get(1..).unwrap_or_default())?;
            xtask::dogfood::run(&workspace_root(), &opts)
        }
        Some("replay-savings") => {
            let opts = xtask::replay_savings::parse_args(
                args.get(1..).unwrap_or_default(),
                &workspace_root(),
            )?;
            xtask::replay_savings::run(&opts)
        }
        Some("rustdoc-baseline") => {
            let violations = xtask::rustdoc_baseline::run(&workspace_root())?;
            fail_with(&violations, "rustdoc-baseline")?;
            println!("rustdoc-baseline: every rustdoc warning site is listed and every listed site still warns");
            Ok(())
        }
        _ => Err(
            "usage: xtask <check-coverage <cov.json> | ci-meta | dogfood | \
                 gate-intercept | replay-savings | rustdoc-baseline>"
                .to_string(),
        ),
    }
}

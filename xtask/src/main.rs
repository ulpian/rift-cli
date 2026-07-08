//! `cargo run -p xtask -- <command>` — workspace tooling entrypoint.
//!
//! * `check-coverage <cov.json>` — the per-crate ≥95% line gate for the security
//!   set (`ci/security-crates.txt`) **and** the ≥75% workspace branch floor,
//!   over a `cargo llvm-cov --json` report (run with `--branch`); fails closed
//!   on a missing crate or absent branch data.
//! * `ci-meta` — the CI/lint meta-checks (required jobs, read-only workflow
//!   permissions, lint-table baseline, no unsafe, no crate-root blanket allows,
//!   no schema fast-path in bins).
//! * `pin-cut <version>` — the scripted capture-backend pin-cut (ADR-0007):
//!   cross-checks the upstream archives against the release's `checksums.txt`
//!   and rewrites `pinned-substrate.toml` with extracted-binary hashes.
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
            let violations = xtask::ci_meta::run_all(&workspace_root())?;
            fail_with(&violations, "ci-meta")?;
            println!("ci-meta: all checks pass");
            Ok(())
        }
        Some("pin-cut") => {
            let version = args
                .get(1)
                .ok_or_else(|| "usage: xtask pin-cut <version>  (e.g. 0.7.8)".to_string())?;
            let summary = xtask::pin_cut::run(&workspace_root(), version)?;
            print!("{summary}");
            Ok(())
        }
        Some("gate-m1") => {
            let violations = xtask::gate_m1::run(&workspace_root())?;
            fail_with(&violations, "gate-m1")?;
            println!("gate-m1: all checks pass");
            Ok(())
        }
        Some("gate-m55") => {
            let violations = xtask::gate_m55::run(&workspace_root())?;
            fail_with(&violations, "gate-m55")?;
            println!("gate-m55: all checks pass");
            Ok(())
        }
        Some("gate-m56") => {
            let violations = xtask::gate_m56::run(&workspace_root())?;
            fail_with(&violations, "gate-m56")?;
            println!("gate-m56: all checks pass");
            Ok(())
        }
        Some("gate-m5") => {
            let violations = xtask::gate_m5::run(&workspace_root())?;
            fail_with(&violations, "gate-m5")?;
            println!("gate-m5: all checks pass");
            Ok(())
        }
        Some("gate-m4") => {
            let violations = xtask::gate_m4::run(&workspace_root())?;
            fail_with(&violations, "gate-m4")?;
            println!("gate-m4: all checks pass");
            print!("{}", xtask::gate_m4::dogfood(&workspace_root()));
            Ok(())
        }
        _ => Err(
            "usage: xtask <check-coverage <cov.json> | ci-meta | pin-cut <version> | gate-m1 | gate-m4 | gate-m5 | gate-m55 | gate-m56>"
                .to_string(),
        ),
    }
}

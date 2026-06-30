//! `cargo run -p xtask -- <command>` — workspace tooling entrypoint.
//!
//! One command today: `check-coverage <cov.json>` enforces the per-crate ≥95%
//! line-coverage gate for the security set (`ci/security-crates.txt`) over a
//! `cargo llvm-cov --json` report, failing closed on a missing crate. The
//! topology/toolchain guards run separately as `cargo test -p xtask`.
#![forbid(unsafe_code)]

use xtask::coverage::{check_crate_coverage, read_security_crates, SECURITY_THRESHOLD};
use xtask::workspace_root;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check-coverage") => {
            let path = args
                .get(1)
                .ok_or_else(|| "usage: xtask check-coverage <cov.json>".to_string())?;
            let json = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
            let crates = read_security_crates(&workspace_root())?;
            let violations = check_crate_coverage(&json, &crates, SECURITY_THRESHOLD)?;
            if violations.is_empty() {
                println!(
                    "coverage: all {} security crates >= {SECURITY_THRESHOLD:.0}% line coverage",
                    crates.len()
                );
                Ok(())
            } else {
                for v in &violations {
                    eprintln!("coverage gate: {v}");
                }
                Err(format!(
                    "{} security crate(s) below {SECURITY_THRESHOLD:.0}%",
                    violations.len()
                ))
            }
        }
        _ => Err("usage: xtask check-coverage <cov.json>".to_string()),
    }
}

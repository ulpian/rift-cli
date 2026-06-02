//! `rift scan` — detect (and optionally enable) an `entire` setup.

use std::ffi::OsString;

use anyhow::Result;
use rift_core::ScanReport;
use rift_entire::EntireRunner;

use crate::cli::{Cli, ScanArgs};

/// Run the scan command.
///
/// # Errors
/// Propagates JSON serialization and `entire enable` failures.
pub fn run(args: &ScanArgs, cli: &Cli) -> Result<i32> {
    let report = rift_core::scan(&args.path);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }

    if args.init && !report.is_entire_enabled() {
        if !report.is_git_repo {
            eprintln!(
                "rift: {} is not a git repo — `entire enable` needs one",
                args.path.display()
            );
            return Ok(1);
        }
        println!("\nrunning `entire enable` ...");
        let runner = crate::commands::runner(cli)?;
        let code = runner.run_inherited(&[OsString::from("enable")])?;
        return Ok(code);
    }

    Ok(0)
}

/// Print a human-readable scan report (also reused by `rift status`).
pub(crate) fn print_report(report: &ScanReport) {
    let mark = |b: bool| if b { "✓" } else { "·" };
    println!("scan: {}", report.root.display());
    println!("  {} git repository", mark(report.is_git_repo));
    println!("  {} .entire/settings.json", mark(report.settings_present));
    println!(
        "  {} .git/entire-sessions/",
        mark(report.sessions_dir_present)
    );
    println!(
        "  {} entire/checkpoints/v1 branch",
        mark(report.checkpoint_branch_present)
    );
    if !report.enabled_agents.is_empty() {
        println!("  agents: {}", report.enabled_agents.join(", "));
    }
    println!(
        "  => entire is {}",
        if report.is_entire_enabled() {
            "ENABLED"
        } else {
            "not enabled"
        }
    );
}

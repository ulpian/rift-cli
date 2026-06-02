//! `rift status` — `entire status` augmented with a Rift scan summary.

use std::ffi::OsString;

use anyhow::Result;
use rift_entire::EntireRunner;

use crate::cli::{Cli, StatusArgs};
use crate::commands::scan;

/// Run the augmented status command.
///
/// # Errors
/// Never fails on a missing `entire`; surfaces only unexpected IO errors.
pub fn run(args: &StatusArgs, cli: &Cli) -> Result<i32> {
    let entire_code = match crate::commands::runner(cli) {
        Ok(runner) => match runner.run_inherited(&[OsString::from("status")]) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("rift: could not run `entire status`: {e}");
                0
            }
        },
        Err(e) => {
            eprintln!("rift: {e}");
            0
        }
    };

    println!("\n--- rift scan summary ---");
    let report = rift_core::scan(&args.path);
    scan::print_report(&report);

    Ok(entire_code)
}

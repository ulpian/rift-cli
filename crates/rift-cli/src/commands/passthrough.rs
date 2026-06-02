//! Pass-through: forward an unrecognized command verbatim to `entire`.

use std::ffi::OsString;

use anyhow::Result;

use crate::cli::Cli;

/// Forward `args` to `entire` with inherited stdio and return its exit code.
///
/// # Errors
/// Returns an error if `entire` cannot be located or spawned.
pub fn run(args: &[OsString], cli: &Cli) -> Result<i32> {
    let runner = crate::commands::runner(cli)?;
    let code = rift_entire::EntireRunner::run_inherited(&runner, args)?;
    Ok(code)
}

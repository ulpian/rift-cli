//! Command dispatch and shared helpers.

pub mod passthrough;
pub mod redact;
pub mod scan;
pub mod status;
pub mod tui;

use anyhow::{Context, Result};
use rift_core::config::Config;
use rift_entire::{locate, RealRunner};

use crate::cli::{Cli, Commands};

/// Build a runner for the `entire` binary using flag/env → config → PATH order.
///
/// # Errors
/// Returns an error if no `entire` binary can be located.
pub fn runner(cli: &Cli) -> Result<RealRunner> {
    let config = Config::load(".").unwrap_or_default();
    let bin = locate(cli.entire_bin.clone(), config.entire.bin)
        .context("locating the `entire` binary")?;
    Ok(RealRunner::new(bin))
}

/// Route a parsed [`Cli`] to its command, returning the process exit code.
///
/// # Errors
/// Propagates any command error (surfaced by `main` as a non-zero exit).
pub fn dispatch(cli: Cli) -> Result<i32> {
    match &cli.command {
        Commands::Scan(args) => scan::run(args, &cli),
        Commands::Redact(args) => redact::run(args),
        Commands::Tui(args) => tui::run(args, &cli),
        Commands::Status(args) => status::run(args, &cli),
        Commands::Entire(args) => passthrough::run(args, &cli),
    }
}

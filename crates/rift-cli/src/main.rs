//! The `rift` binary entrypoint.

mod cli;
mod commands;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    match commands::dispatch(cli) {
        Ok(code) => ExitCode::from((code & 0xff) as u8),
        Err(err) => {
            eprintln!("rift: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Initialize tracing. Verbosity from `-v` flags; overridable via `RIFT_LOG`.
fn init_tracing(verbose: u8) {
    use tracing_subscriber::{fmt, EnvFilter};

    let default = match verbose {
        0 => "warn",
        1 => "info,rift=debug",
        _ => "debug,rift=trace",
    };
    let filter = EnvFilter::try_from_env("RIFT_LOG").unwrap_or_else(|_| EnvFilter::new(default));

    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

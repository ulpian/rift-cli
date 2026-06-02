//! `rift tui` — launch the dashboard.

use anyhow::Result;
use rift_entire::EntireRunner;
use rift_tui::TuiConfig;

use crate::cli::{Cli, TuiArgs};

/// Run the TUI. The dashboard still opens if `entire` is unavailable; the
/// command bar simply reports that when a command is dispatched.
///
/// # Errors
/// Propagates terminal errors from the TUI run loop.
pub fn run(args: &TuiArgs, cli: &Cli) -> Result<i32> {
    let runner: Option<Box<dyn EntireRunner>> = match crate::commands::runner(cli) {
        Ok(r) => Some(Box::new(r)),
        Err(_) => None,
    };
    rift_tui::run(
        TuiConfig {
            repo: args.repo.clone(),
        },
        runner,
    )?;
    Ok(0)
}

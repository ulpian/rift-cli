//! The ratatui dashboard for Rift.
//!
//! [`run`] owns terminal setup/teardown, the draw loop, and dispatching
//! `entire` commands typed into the bottom bar (it suspends the TUI, runs the
//! command with inherited stdio, then re-enters). All state lives in
//! [`app::AppState`]; rendering is in [`ui`].

pub mod app;
pub mod command_bar;
pub mod event;
pub mod metrics;
pub mod refresh;
pub mod ui;
pub mod widgets;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use ratatui::DefaultTerminal;
use rift_core::scan;
use rift_entire::EntireRunner;

use crate::app::{Action, AppState, ProjectItem};
use crate::event::AppEvent;
use crate::metrics::MetricsSnapshot;
use crate::refresh::MetricsRefresher;

/// How the dashboard is launched.
pub struct TuiConfig {
    /// The repo (or folder of repos) to inspect.
    pub repo: PathBuf,
}

const TICK: Duration = Duration::from_millis(250);
const METRICS_REFRESH: Duration = Duration::from_secs(5);

/// Run the dashboard against `config`. `runner`, when present, lets the command
/// bar dispatch `entire` commands.
///
/// # Errors
/// Returns an error if the terminal cannot be driven.
pub fn run(config: TuiConfig, runner: Option<Box<dyn EntireRunner>>) -> Result<()> {
    install_panic_hook();

    let mut app = build_state(&config.repo);
    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, &mut app, runner.as_deref());
    ratatui::restore();
    result
}

/// Seed [`AppState`] by discovering entire-enabled repos under `repo` (falling
/// back to `repo` itself) and loading a metrics snapshot.
fn build_state(repo: &Path) -> AppState {
    let mut options: Vec<ProjectItem> = scan::find_entire_repos(repo, 4)
        .into_iter()
        .map(|path| ProjectItem {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            path,
        })
        .collect();

    if options.is_empty() {
        options.push(ProjectItem {
            name: repo
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| repo.display().to_string()),
            path: repo.to_path_buf(),
        });
    }

    // Start with a "loading" snapshot; the background refresher fills in the
    // real data shortly so startup never blocks on git.
    AppState::new(repo, options, MetricsSnapshot::empty("loading metrics…"))
}

fn run_loop(
    terminal: &mut DefaultTerminal,
    app: &mut AppState,
    runner: Option<&dyn EntireRunner>,
) -> Result<()> {
    // Refreshes metrics off the checkpoint branch on a background thread;
    // stopped and joined when dropped at the end of this function.
    let refresher = MetricsRefresher::spawn(app.repo.clone(), METRICS_REFRESH);

    while !app.should_quit {
        if let Some(snapshot) = refresher.latest() {
            app.metrics = snapshot;
        }

        terminal.draw(|frame| ui::draw(frame, app))?;

        let AppEvent::Key(key) = event::poll(TICK)? else {
            continue;
        };

        if let Some(action) = app.on_key(key) {
            match action {
                Action::Quit => break,
                Action::RunExternal(args) => {
                    run_external(terminal, app, runner, &args)?;
                }
            }
        }
    }
    Ok(())
}

/// Suspend the TUI, run `entire <args>` with inherited stdio, then re-enter.
fn run_external(
    terminal: &mut DefaultTerminal,
    app: &mut AppState,
    runner: Option<&dyn EntireRunner>,
    args: &[std::ffi::OsString],
) -> Result<()> {
    let Some(runner) = runner else {
        app.status = "`entire` is not available (set RIFT_ENTIRE_BIN or install it)".to_string();
        return Ok(());
    };

    ratatui::restore();
    let outcome = runner.run_inherited(args);
    *terminal = ratatui::init();

    app.status = match outcome {
        Ok(code) => format!("entire exited with code {code} — press a key to continue"),
        Err(e) => format!("failed to run entire: {e}"),
    };
    Ok(())
}

fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        original(info);
    }));
}

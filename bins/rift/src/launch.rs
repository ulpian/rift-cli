//! Launching the dashboard and the Recall Viewer: the interactive loop when
//! stdout is a terminal, else one ASCII frame drawn by the same renderer.
//!
//! The launch directory is resolved once here — a threaded TUI must never
//! consult process cwd afterwards — and so is the operator's home, so the
//! render crate stays env-free.

use std::path::{Path, PathBuf};

use rift_canopy::render::Size;
#[cfg(feature = "tui")]
use rift_canopy::PersistOp;
use rift_canopy::{AppState, Frame, Project};
use rift_registry::{default_registry_path, ProjectRegistry};

use crate::cli::{self, Cli};
#[cfg(feature = "tui")]
use crate::dispatch::RiftDispatcher;
use crate::hooks;
#[cfg(feature = "tui")]
use crate::memory;
#[cfg(feature = "tui")]
use crate::project_settings;
use crate::projection::project_name;

/// Launch the dashboard: interactive xre TUI when possible, else one ASCII frame.
///
/// The dashboard's registry is the append-only canopy UI-state store; the legacy
/// TOML registry is imported once (idempotent) so existing users keep their list.
pub(crate) fn launch_canopy(cli: &Cli) -> rift_core::Result<()> {
    let cache_root = crate::family::cache_root();
    let cstore = rift_canopy_data::CanopyStore::open(&cache_root)?;
    #[cfg(feature = "tui")]
    if interactive_terminal() && !crate::onboarding::prepare(&cstore)? {
        return Ok(());
    }
    let projects = launch_projects(&cstore)?;
    #[cfg(feature = "tui")]
    if interactive_terminal() {
        match run_dashboard(cli, &cstore, projects.clone()) {
            Ok(()) => return Ok(()),
            Err(e) => eprintln!("rift: could not start the interactive dashboard: {e}"),
        }
    }
    let _ = cli; // only consulted on the interactive path
    let mut profile_loader = rift_canopy_data::StoreProfileLoader::new(&cstore);
    print_ascii_frame(projects, &mut profile_loader)?;
    Ok(())
}

/// The rows the dashboard launches with. The legacy mutable TOML registry is
/// imported once (a no-op afterwards) and the default profile is seeded on
/// first launch (idempotent like the import: one `created` + one `selected`
/// event when the log is empty); then the resolved LAUNCH profile's view —
/// the pin when live, else the restored last-selected chain — through the
/// same helper the loop's launch load resolves through, a pure read.
fn launch_projects(cstore: &rift_canopy_data::CanopyStore) -> rift_core::Result<Vec<Project>> {
    let reg = ProjectRegistry::load(&default_registry_path()).unwrap_or_default();
    let legacy: Vec<(String, PathBuf)> = reg
        .projects()
        .iter()
        .map(|p| (p.name.clone(), p.path.clone()))
        .collect();
    cstore.import_legacy(&legacy, hooks::now_ms())?;
    cstore.seed_default_profile(hooks::now_ms())?;
    cstore.launch_projects()
}

/// Whether the dashboard can take the terminal: stdout is a TTY and nobody
/// asked for the headless frame.
#[cfg(feature = "tui")]
fn interactive_terminal() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdout().is_terminal() && std::env::var_os("RIFT_HEADLESS").is_none()
}

/// The live loop, with every seam injected: the data bus seeded to populate
/// every registered project's status, the append-only persist (every registry
/// mutation returns the refreshed list), the settings manager, the palette's
/// in-process dispatcher over an explicit repository root, the profile loader
/// (the active profile, live names, membership and arrangements all read
/// through the store), and the destination explorer's directory lister from
/// the data plane — the reducer only ever requests a directory.
#[cfg(feature = "tui")]
fn run_dashboard(
    cli: &Cli,
    cstore: &rift_canopy_data::CanopyStore,
    projects: Vec<Project>,
) -> rift_core::Result<()> {
    let (launch_cwd, launch_cwd_is_repo) = launch_cwd_context();
    // One-shot background ingest for the launch-selected project when
    // rift-total-recall is enabled there. Watermark-idempotent; a failure
    // logs and never blocks the dashboard.
    spawn_launch_ingest(&projects, launch_cwd.as_deref());
    let opts = rift_canopy::CanopyOpts {
        ascii: cli.ascii,
        skip_splash: cli.skip_splash || std::env::var("RIFT_SPLASH").is_ok_and(|v| v == "never"),
        viewer_launch: None,
        launch_cwd,
        launch_cwd_is_repo,
        version: env!("CARGO_PKG_VERSION").to_string(),
        home_dir: home_dir_from(|k| std::env::var(k).ok()),
    };
    let paths: Vec<String> = projects.iter().map(|p| p.path.clone()).collect();
    let bus = rift_canopy_data::spawn_with_root(crate::family::cache_root(), paths);
    let on_persist = |op: &PersistOp| cstore.apply(op, hooks::now_ms());
    let mut settings_manager = project_settings::RiftProjectSettingsManager;
    let mut dispatcher = RiftDispatcher;
    let reload = || cstore.active_projects();
    let mut profile_loader = rift_canopy_data::StoreProfileLoader::new(cstore);
    rift_canopy::app::live::run(rift_canopy::app::live::LoopDeps {
        opts,
        initial: projects,
        on_persist,
        data_rx: bus.evt_rx,
        req_tx: bus.req_tx,
        settings_manager: &mut settings_manager,
        dispatcher: &mut dispatcher,
        reload,
        profile_loader: &mut profile_loader,
        list_dir: rift_canopy_data::list_dir,
    })
}

/// Resolve and launch `rift viewer` without touching any startup write path.
/// The Canopy registry is optional and read-only; an unregistered repository
/// becomes one session-local rail row.
pub(crate) fn launch_viewer(cli: &Cli, args: &cli::ViewerArgs) -> rift_core::Result<()> {
    let cache_root = crate::family::cache_root();
    let cstore = rift_canopy_data::CanopyStore::open_read_only(&cache_root)?;
    let mut projects = cstore.as_ref().map_or(
        Ok(Vec::new()),
        rift_canopy_data::CanopyStore::launch_projects,
    )?;
    let selected_repo = viewer_repo(args, &projects)?;
    let selected_repo = selected_repo.canonicalize().unwrap_or(selected_repo);
    let (repo, transient) = viewer_project(&mut projects, selected_repo);
    let launch = rift_canopy::ViewerLaunch {
        repo,
        target: args.target.clone(),
        transient,
    };
    run_viewer(cli, projects, launch, cache_root, cstore.as_ref())
}

/// Which repository the viewer opens: `--repo`, else the repository enclosing
/// the working directory, else the first registered project.
fn viewer_repo(args: &cli::ViewerArgs, projects: &[Project]) -> rift_core::Result<PathBuf> {
    if let Some(path) = &args.repo {
        return viewer_repo_root(path);
    }
    let first_registered = || projects.first().map(|project| PathBuf::from(&project.path));
    match std::env::current_dir() {
        Ok(cwd) => rift_git::discover(&cwd)
            .map(|paths| paths.work_tree)
            .ok()
            .or_else(first_registered)
            .ok_or_else(|| {
                rift_core::RiftError::Other(
                    "no enclosing Git repository or read-only registered project; pass --repo PATH"
                        .to_owned(),
                )
            }),
        Err(_) => first_registered().ok_or_else(|| {
            rift_core::RiftError::Other(
                "working directory unavailable and no registered project; pass --repo PATH"
                    .to_owned(),
            )
        }),
    }
}

/// The rail row the viewer selects: the registered project at that path, or
/// one session-local row appended for an unregistered repository. Returns the
/// row's path and whether it is transient.
fn viewer_project(projects: &mut Vec<Project>, selected_repo: PathBuf) -> (String, bool) {
    let registered = projects.iter().find(|project| {
        Path::new(&project.path)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(&project.path))
            == selected_repo
    });
    if let Some(project) = registered {
        return (project.path.clone(), false);
    }
    let path = selected_repo.to_string_lossy().into_owned();
    projects.push(Project {
        name: project_name(&selected_repo),
        path: path.clone(),
        status: rift_canopy::ProjectStatus::Idle,
        kind: if selected_repo.join(".git").is_file() {
            rift_canopy::EntryKind::Worktree
        } else {
            rift_canopy::EntryKind::Repo
        },
    });
    (path, true)
}

/// Run the viewer: the interactive loop on a terminal, else one ASCII frame.
/// The ASCII frame takes nothing from the top-level command line: the one
/// option it would read is `--ascii`, and this path is already that.
fn run_viewer(
    cli: &Cli,
    projects: Vec<Project>,
    launch: rift_canopy::ViewerLaunch,
    cache_root: PathBuf,
    cstore: Option<&rift_canopy_data::CanopyStore>,
) -> rift_core::Result<()> {
    let mut loader = profile_loader(cstore);
    #[cfg(not(feature = "tui"))]
    let _ = cli;
    #[cfg(feature = "tui")]
    if interactive_terminal() {
        return run_viewer_tui(cli, projects, launch, cache_root, loader.as_mut());
    }
    print_ascii_viewer_frame(projects, launch, cache_root, loader.as_mut())
}

/// The profile plane's loader: the store's when the registry opened, else
/// the null one, so an unregistered machine still gets a frame.
fn profile_loader(
    cstore: Option<&rift_canopy_data::CanopyStore>,
) -> Box<dyn rift_canopy::ProfileLoader + '_> {
    match cstore {
        Some(cstore) => Box::new(rift_canopy_data::StoreProfileLoader::new(cstore)),
        None => Box::new(rift_canopy::NullProfileLoader),
    }
}

fn viewer_repo_root(path: &Path) -> rift_core::Result<PathBuf> {
    let canonical = path.canonicalize().map_err(|error| {
        rift_core::RiftError::Other(format!(
            "cannot open viewer repository {}: {error}",
            path.display()
        ))
    })?;
    rift_git::discover(&canonical)
        .map(|paths| paths.work_tree)
        .map_err(|_| {
            rift_core::RiftError::Other(format!(
                "{} is not inside a readable local Git repository",
                canonical.display()
            ))
        })
}

#[cfg(feature = "tui")]
fn run_viewer_tui(
    cli: &Cli,
    projects: Vec<Project>,
    launch: rift_canopy::ViewerLaunch,
    cache_root: PathBuf,
    loader: &mut dyn rift_canopy::ProfileLoader,
) -> rift_core::Result<()> {
    let bus = rift_canopy_data::spawn_read_only_viewer(cache_root);
    let opts = rift_canopy::CanopyOpts {
        ascii: cli.ascii,
        skip_splash: true,
        viewer_launch: Some(launch.clone()),
        launch_cwd: Some(launch.repo.clone()),
        launch_cwd_is_repo: true,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        home_dir: home_dir_from(|key| std::env::var(key).ok()),
    };
    let reload_rows = projects.clone();
    let on_persist = |_op: &PersistOp| -> rift_core::Result<Vec<Project>> {
        Err(rift_core::RiftError::policy_denied(
            "Recall Viewer is read-only; persistence is disabled",
        ))
    };
    let reload = move || Ok(reload_rows.clone());
    let mut settings = rift_canopy::NullProjectSettingsManager;
    let mut dispatcher = RiftDispatcher;
    rift_canopy::app::live::run(rift_canopy::app::live::LoopDeps {
        opts,
        initial: projects,
        on_persist,
        data_rx: bus.evt_rx,
        req_tx: bus.req_tx,
        settings_manager: &mut settings,
        dispatcher: &mut dispatcher,
        reload,
        profile_loader: loader,
        list_dir: rift_canopy_data::list_dir,
    })
}

/// Spawn the one-shot launch auto-ingest thread: the launch-selected
/// project (per the same [`rift_canopy::launch_selection`] rule), ingested in
/// the background when rift-total-recall is enabled for it. The ingest
/// watermark makes a relaunch a no-op; failures log and never block or crash
/// the dashboard (the thread is detached — the loop never joins it).
#[cfg(feature = "tui")]
fn spawn_launch_ingest(projects: &[Project], launch_cwd: Option<&str>) {
    let selected = launch_cwd
        .and_then(|cwd| rift_canopy::launch_selection(projects, cwd))
        .or_else(|| projects.first().map(|p| p.path.as_str()))
        .map(str::to_owned);
    let Some(path) = selected else { return };
    std::thread::spawn(move || {
        match memory::launch_ingest(Path::new(&path)) {
            Ok(true) => tracing::debug!(repo = %path, "launch recall ingest complete"),
            Ok(false) => {} // recall disabled here — nothing touched
            Err(e) => tracing::warn!(repo = %path, error = %e, "launch recall ingest failed"),
        }
    });
}

/// The canonicalized launch directory + whether it sits inside a git repo —
/// the two facts `apply_launch_cwd` selects/hints from. Resolved once
/// at launch; a threaded TUI must never consult process cwd afterwards.
fn launch_cwd_context() -> (Option<String>, bool) {
    let Ok(cwd) = std::env::current_dir() else {
        return (None, false);
    };
    let canonical = cwd.canonicalize().unwrap_or(cwd);
    let is_repo = rift_git::discover(&canonical).is_ok();
    (Some(canonical.to_string_lossy().into_owned()), is_repo)
}

/// The operator's home directory — the destination explorer's start. The same
/// env ladder `rift_registry::resolve_from` walks (`HOME` on
/// unix, `USERPROFILE` on Windows; no `dirs` crate), resolved once here so the
/// render crate stays env-free. `None` lets the reducer fall back to the launch
/// cwd, then `/`.
pub(crate) fn home_dir_from(env: impl Fn(&str) -> Option<String>) -> Option<String> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    env(key).filter(|h| !h.is_empty())
}

/// Render one ASCII dashboard frame to stdout (the headless / fallback path).
/// The same `view::draw` the live loop uses, over a fixed 100×40 frame — one
/// renderer, so the headless output can never drift from the TUI. The frame is
/// **live**: the real data bus runs briefly (bounded by `RIFT_HEADLESS_WAIT_MS`,
/// default 1200) with the first project active, so statuses and the column-3
/// sections render from the developer's own projections — placeholders only
/// where data is honestly absent, exactly like the TUI.
fn print_ascii_frame(
    projects: Vec<Project>,
    loader: &mut dyn rift_canopy::ProfileLoader,
) -> rift_core::Result<()> {
    let mut state = AppState::new(projects, Size::new(100, 40));
    state.app.splash.skip();
    state.version = env!("CARGO_PKG_VERSION").to_string();
    // The same launch-cwd selection the interactive run applies — the
    // headless frame follows the developer's launch directory too.
    let (launch_cwd, launch_cwd_is_repo) = launch_cwd_context();
    rift_canopy::apply_launch_cwd(&mut state.app, launch_cwd.as_deref(), launch_cwd_is_repo);
    // The SAME profile-plane load the interactive loop runs at its launch seam
    // seam: identity + theme render on the headless frame
    // too, so a one-frame check can no longer be blind to a launch-wiring gap.
    rift_canopy::load_profile_plane(&mut state, loader)?;

    // Drain the live bus for a bounded window, applying updates through the
    // same reducer the TUI uses.
    let wait_ms: u64 = std::env::var("RIFT_HEADLESS_WAIT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1200);
    if wait_ms > 0 && !state.app.projects.is_empty() {
        let paths: Vec<String> = state.app.projects.iter().map(|p| p.path.clone()).collect();
        let bus = rift_canopy_data::spawn_with_root(crate::family::cache_root(), paths);
        // The SAME prime the interactive loop runs at its seam — one startup
        // path, so the headless frame can never again mask a dead launch
        // subscribe — a blind spot a one-frame check once had.
        rift_canopy::prime_startup(&state, &mut |req| {
            let _ = bus.req_tx.send(req);
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
        while std::time::Instant::now() < deadline {
            match bus
                .evt_rx
                .recv_timeout(std::time::Duration::from_millis(100))
            {
                Ok(ev) => {
                    let _ = rift_canopy::update(&mut state, ev);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    let vm = state.snapshot();
    let mut frame = Frame::new(vm.size);
    let mut backend = rift_canopy::AsciiBackend::new(rift_canopy::Capabilities::empty());
    rift_canopy::view::draw(&vm, &mut backend, &mut frame);
    let mut lines: Vec<String> = (0..frame.height())
        .map(|y| {
            (0..frame.width())
                .map(|x| frame.get(x, y).map_or(' ', |c| c.ch))
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

fn print_ascii_viewer_frame(
    projects: Vec<Project>,
    launch: rift_canopy::ViewerLaunch,
    cache_root: PathBuf,
    loader: &mut dyn rift_canopy::ProfileLoader,
) -> rift_core::Result<()> {
    let mut state = AppState::new(projects, Size::new(100, 40));
    state.app.splash.skip();
    env!("CARGO_PKG_VERSION").clone_into(&mut state.version);
    state.app.launch_cwd = Some(launch.repo.clone());
    rift_canopy::load_profile_plane(&mut state, loader)?;
    state.app.select_path(&launch.repo);
    let effects = state.open_recall_viewer(launch.target);
    let bus = rift_canopy_data::spawn_read_only_viewer(cache_root);
    for request in effects.data_requests {
        let _ = bus.req_tx.send(request);
    }
    let wait_ms = std::env::var("RIFT_HEADLESS_WAIT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3_000);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
    while std::time::Instant::now() < deadline {
        match bus
            .evt_rx
            .recv_timeout(std::time::Duration::from_millis(100))
        {
            Ok(event) => {
                if let rift_canopy::Flow::Continue(effects) = rift_canopy::update(&mut state, event)
                {
                    debug_assert!(effects.persist.is_empty());
                    for request in effects.data_requests {
                        let _ = bus.req_tx.send(request);
                    }
                }
                if matches!(
                    state.app.viewer.detail_state,
                    rift_canopy::ViewerLoadState::Ready
                ) && matches!(
                    state.app.viewer.catalogue_state,
                    rift_canopy::ViewerLoadState::Ready
                ) {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let vm = state.snapshot();
    let mut frame = Frame::new(vm.size);
    let mut backend = rift_canopy::AsciiBackend::new(rift_canopy::Capabilities::empty());
    rift_canopy::view::draw(&vm, &mut backend, &mut frame);
    let mut lines = (0..frame.height())
        .map(|y| {
            (0..frame.width())
                .map(|x| frame.get(x, y).map_or(' ', |cell| cell.ch))
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

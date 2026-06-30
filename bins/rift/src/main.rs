//! Rift entrypoint.
//!
//! Bare `rift` launches the **Canopy** dashboard. In an interactive terminal
//! (and with the default `tui` feature) that is the live xre-backed event loop
//! from [`rift_canopy::xre`]; when stdout is not a TTY, `RIFT_HEADLESS` is set,
//! or the `tui` feature is disabled, it degrades to printing a single ASCII
//! frame so scripts and tests stay deterministic.
//!
//! Subcommands form a `clap`-derived tree (P8 S1): `init`/`setup`, `status`,
//! `doctor`, `enable`/`disable`, `verify`, `checkpoint`, `session`, `explain`,
//! `gc`, `configure`, and the hidden `hooks` wire entry. Every subcommand is
//! short-lived (no daemon); query commands are read-only over the projection. The
//! exit-code contract (`0/1/2/3/4/70`, with `verify` carrying its own) is mapped
//! here from each command's typed [`cli::Outcome`].
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::process::ExitCode;

use clap::Parser;
use rift_canopy::{App, AsciiBackend, ProjStatus, Project, Rect, RenderBackend, SplashState};
use rift_registry::{default_registry_path, ProjectRegistry};

use cli::{exit, Cli, Command, ConfigureArgs, InitArgs, Outcome};

mod agents;
mod cli;
mod hooks;
mod query;
mod verify;

/// A default `.rift/config.toml` (secure defaults apply for absent keys).
const DEFAULT_CONFIG: &str = "# Rift project configuration (.rift/config.toml).\n\
                              # Secure defaults apply when keys are absent.\n";

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Bare `rift`: launch Canopy (TTY) or print a status-style frame (non-TTY).
    let Some(cmd) = cli.cmd.as_ref() else {
        return match launch_canopy() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("rift: {e}");
                ExitCode::from(1)
            }
        };
    };

    // The hidden `hooks` wire entry manages its own output and exit (it must never
    // fail the agent or block a git op).
    if let Command::Hooks(a) = cmd {
        return match hooks::run(&a.rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("rift: {e}");
                ExitCode::from(1)
            }
        };
    }

    match route(&cli, cmd) {
        Ok(outcome) => emit(&cli, &outcome),
        Err(e) => {
            // A clean non-zero exit with a message — never a panic/backtrace.
            eprintln!("rift: {e}");
            ExitCode::from(u8::try_from(exit::FAILURE).unwrap_or(1))
        }
    }
}

/// Route a parsed subcommand to its handler, producing a typed [`Outcome`].
fn route(_cli: &Cli, cmd: &Command) -> rift_core::Result<Outcome> {
    use cli::{CheckpointCmd, SessionCmd};
    match cmd {
        Command::Init(a) => cmd_init(a),
        Command::Doctor => Ok(cmd_doctor()),
        Command::Verify => Ok(verify_outcome()),
        Command::Configure(a) => cmd_configure(a),
        // enable/disable mutate; they may run before `init` and create `.rift/`.
        Command::Enable(a) => {
            let store = open_projection()?;
            runtime()?.block_on(agents::enable(&store, &a.agent, hooks::now_ms()))
        }
        Command::Disable(a) => {
            let store = open_projection()?;
            runtime()?.block_on(agents::disable(&store, &a.agent, hooks::now_ms()))
        }
        // Read-only projection queries — guard on init state first.
        Command::Status => projection_read(|s, rt| rt.block_on(query::status(s))),
        Command::Checkpoint(CheckpointCmd::List) => {
            projection_read(|s, rt| rt.block_on(query::checkpoint_list(s)))
        }
        Command::Checkpoint(CheckpointCmd::Show(i)) => {
            projection_read(|s, rt| rt.block_on(query::checkpoint_show(s, &i.id)))
        }
        Command::Session(SessionCmd::List) => {
            projection_read(|s, rt| rt.block_on(query::session_list(s)))
        }
        Command::Session(SessionCmd::Show(i)) => {
            projection_read(|s, rt| rt.block_on(query::session_show(s, &i.id)))
        }
        Command::Explain(a) => {
            projection_read(|s, rt| rt.block_on(query::explain(s, &a.reference)))
        }
        Command::Gc(a) => projection_read(|s, rt| rt.block_on(query::gc(s, a.dry_run))),
        // Hooks is handled in `main` before routing.
        Command::Hooks(_) => Ok(Outcome::ok(String::new(), serde_json::json!({}))),
    }
}

/// Run a read-only projection query: guard on init state, open the store + a
/// runtime, and hand both to `f` (which drives the async query to completion).
fn projection_read<F>(f: F) -> rift_core::Result<Outcome>
where
    F: FnOnce(&rift_store::SqliteStore, &tokio::runtime::Runtime) -> rift_core::Result<Outcome>,
{
    if let Some(o) = not_init_guard() {
        return Ok(o);
    }
    let store = open_projection()?;
    let rt = runtime()?;
    f(&store, &rt)
}

/// `Some(NOT_INIT outcome)` when the cwd is not a Rift repo (`.rift/` absent).
fn not_init_guard() -> Option<Outcome> {
    if is_initialized() {
        return None;
    }
    Some(Outcome::coded(
        exit::NOT_INIT,
        "not a rift repo (run `rift init`)",
        serde_json::json!({ "error": "not_initialized" }),
    ))
}

/// Whether the current directory has been initialized (`.rift/` present).
fn is_initialized() -> bool {
    std::env::current_dir().is_ok_and(|c| c.join(".rift").is_dir())
}

/// Open (creating if needed) the per-repo projection store with the canonical schema.
fn open_projection() -> rift_core::Result<rift_store::SqliteStore> {
    let db = hooks::projection_db()?;
    if let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = rift_store::SqliteStore::open(&db)?;
    store.create_canonical_schema()?;
    Ok(store)
}

/// A short-lived current-thread async runtime for the store seam.
fn runtime() -> rift_core::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)
}

/// Emit a command outcome per the output conventions and map it to an exit code.
fn emit(cli: &Cli, outcome: &Outcome) -> ExitCode {
    if cli.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&outcome.json).unwrap_or_else(|_| "{}".into())
        );
    } else if !cli.quiet && !outcome.human.is_empty() {
        if outcome.code == exit::OK {
            println!("{}", outcome.human);
        } else {
            eprintln!("{}", outcome.human);
        }
    }
    ExitCode::from(u8::try_from(outcome.code).unwrap_or(70))
}

// --- commands ---------------------------------------------------------------

/// `rift init` / `rift setup`: register the cwd, scaffold `.rift/`, init the
/// projection, install git hooks (if a repo), and provision the backend.
fn cmd_init(args: &InitArgs) -> rift_core::Result<Outcome> {
    let canon = std::env::current_dir()?.canonicalize()?;
    let name = project_name(&canon);

    // Register into the global project registry (shared with the dashboard).
    let reg_path = default_registry_path();
    let mut reg = ProjectRegistry::load(&reg_path)?;
    let added = reg.add(name.clone(), canon.clone());
    reg.save(&reg_path)?;

    // Scaffold `.rift/` (marks the repo initialized).
    let rift_dir = canon.join(".rift");
    std::fs::create_dir_all(&rift_dir)?;
    let cfg = rift_dir.join("config.toml");
    if !cfg.exists() {
        std::fs::write(&cfg, DEFAULT_CONFIG)?;
    }

    // Best-effort projection init.
    let _ = open_projection();

    let mut human = if added {
        format!("registered {name} at {}\n", canon.display())
    } else {
        format!("{name} is already registered ({})\n", canon.display())
    };
    human.push_str(&format!("registry: {}\n", reg_path.display()));
    human.push_str(&format!("config: {}\n", cfg.display()));

    // Install the git hooks when this is a git repo (best-effort, never fatal).
    let git_dir = canon.join(".git");
    if git_dir.is_dir() {
        let rift_bin = std::env::current_exe()?;
        match rift_shadow::hooks::install_hooks(&rift_shadow::hooks::HookInstall {
            git_dir,
            rift_bin,
            hooks_dir: None,
        }) {
            Ok(written) => human.push_str(&format!("installed {} git hooks\n", written.len())),
            Err(e) => human.push_str(&format!("git hooks: skipped ({e})\n")),
        }
    } else {
        human.push_str("git hooks: skipped (not a git repo)\n");
    }

    provision_if_requested(args.no_provision)?;

    let json = serde_json::json!({
        "registered": name,
        "newly_registered": added,
        "config": cfg.display().to_string(),
    });
    Ok(Outcome::ok(human.trim_end().to_owned(), json))
}

/// `rift doctor`: report capture-backend readiness without any network I/O.
fn cmd_doctor() -> Outcome {
    let status = backend_status();
    Outcome::ok(
        format!("rift doctor\n  capture backend: {status}"),
        serde_json::json!({ "capture_backend": status }),
    )
}

/// `rift configure`: manage `.rift/config.toml` (the checkpoint remote / policy).
fn cmd_configure(args: &ConfigureArgs) -> rift_core::Result<Outcome> {
    let rift_dir = std::env::current_dir()?.join(".rift");
    std::fs::create_dir_all(&rift_dir)?;
    let cfg = rift_dir.join("config.toml");
    if let Some(remote) = &args.checkpoint_remote {
        let doc = format!("# Rift project configuration (.rift/config.toml).\n[checkpoint]\nremote = \"{remote}\"\n");
        std::fs::write(&cfg, doc)?;
        Ok(Outcome::ok(
            format!("configured checkpoint remote: {remote}"),
            serde_json::json!({ "checkpoint_remote": remote, "config": cfg.display().to_string() }),
        ))
    } else {
        let doc = std::fs::read_to_string(&cfg).unwrap_or_default();
        Ok(Outcome::ok(
            format!("config: {}\n{doc}", cfg.display()),
            serde_json::json!({ "config": cfg.display().to_string() }),
        ))
    }
}

/// `rift verify`: carries its own exit contract (P5 S4 subset): 0 ok, 1 tamper, 70 internal.
fn verify_outcome() -> Outcome {
    match verify::run() {
        Ok(()) => Outcome::coded(
            exit::OK,
            "verify: ok",
            serde_json::json!({ "verify": "ok" }),
        ),
        Err(rift_core::RiftError::Integrity(m)) => Outcome::coded(
            1,
            format!("verify: tamper detected ({m})"),
            serde_json::json!({ "verify": "tamper", "detail": m }),
        ),
        Err(e) => Outcome::coded(
            exit::INTERNAL,
            format!("verify: internal error ({e})"),
            serde_json::json!({ "verify": "internal", "detail": e.to_string() }),
        ),
    }
}

// --- Canopy + registry helpers ----------------------------------------------

/// Project the registry onto Canopy's view model (all projects start `Idle`).
fn registry_to_projects(reg: &ProjectRegistry) -> Vec<Project> {
    reg.projects()
        .iter()
        .map(|p| Project {
            name: p.name.clone(),
            path: p.path.display().to_string(),
            status: ProjStatus::Idle,
        })
        .collect()
}

/// Render one ASCII dashboard frame to stdout (the headless / fallback path).
fn print_ascii_frame(projects: Vec<Project>) {
    let mut app = App::new(projects);
    app.splash = SplashState::new();
    app.splash.skip();
    let area = Rect {
        x: 0,
        y: 0,
        w: 100,
        h: 40,
    };
    for line in AsciiBackend.render(&app, area) {
        println!("{line}");
    }
}

/// Derive a project's display name from its path (directory basename).
fn project_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Register the directory at `path` into the global registry, returning the
/// refreshed project list. Used by the dashboard's `a` key.
#[cfg(feature = "tui")]
fn register_project(path: &str) -> rift_core::Result<Vec<Project>> {
    let canon = std::fs::canonicalize(path)?;
    if !canon.is_dir() {
        return Err(rift_core::RiftError::validity(format!(
            "{} is not a directory",
            canon.display()
        )));
    }
    let reg_path = default_registry_path();
    let mut reg = ProjectRegistry::load(&reg_path)?;
    reg.add(project_name(&canon), canon);
    reg.save(&reg_path)?;
    Ok(registry_to_projects(&reg))
}

/// Launch the dashboard: interactive xre TUI when possible, else one ASCII frame.
fn launch_canopy() -> rift_core::Result<()> {
    let reg_path = default_registry_path();
    let reg = ProjectRegistry::load(&reg_path)?;
    let projects = registry_to_projects(&reg);

    #[cfg(feature = "tui")]
    {
        use std::io::IsTerminal;
        let interactive =
            std::io::stdout().is_terminal() && std::env::var_os("RIFT_HEADLESS").is_none();
        if interactive {
            match rift_canopy::xre::run_tui(projects.clone(), register_project) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    eprintln!("rift: could not start the interactive dashboard: {e}");
                }
            }
        }
    }

    print_ascii_frame(projects);
    Ok(())
}

/// Report backend readiness by checking the pinned, opaque install path. Never
/// downloads; never names the upstream tool.
fn backend_status() -> &'static str {
    match rift_shadow::entire::PinnedSubstrate::from_embedded() {
        Ok(pin) => {
            let bp = pin.binary_path();
            if bp.exists() && pin.verify_on_disk(&bp).is_ok() {
                "ready"
            } else {
                "needs-setup"
            }
        }
        Err(_) => "unsupported-platform",
    }
}

/// Provision the capture backend from the upstream release — the **default** for
/// `rift init`. Idempotent and fail-closed (a bad download is never executed).
/// Skipped by `--no-provision` or `RIFT_NO_PROVISION`.
#[cfg(feature = "provision-http")]
fn provision_if_requested(no_provision: bool) -> rift_core::Result<()> {
    let opted_out = no_provision || std::env::var_os("RIFT_NO_PROVISION").is_some();
    if opted_out {
        println!(
            "  capture backend: {} (provisioning skipped)",
            backend_status()
        );
        return Ok(());
    }
    let pin = match rift_shadow::entire::PinnedSubstrate::from_embedded() {
        Ok(p) => p,
        Err(e) => {
            println!("  capture backend: unavailable ({e})");
            return Ok(());
        }
    };
    let db = hooks::projection_db()?;
    if let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = rift_store::SqliteStore::open(&db)?;
    store.create_canonical_schema()?;
    let rt = runtime()?;
    let fetcher = rift_shadow::entire::HttpFetcher::new();
    if backend_status() != "ready" {
        println!("  capture backend: downloading…");
    }
    match rt.block_on(rift_shadow::provision_and_audit(
        &pin,
        &fetcher,
        &store,
        hooks::now_ms(),
    )) {
        Ok(_) => println!("  capture backend: ready"),
        Err(e) => println!("  capture backend: needs-setup ({e})"),
    }
    Ok(())
}

/// Without the `provision-http` feature, provisioning support is not built in.
#[cfg(not(feature = "provision-http"))]
fn provision_if_requested(_no_provision: bool) -> rift_core::Result<()> {
    println!(
        "  capture backend: {} (built without provisioning support)",
        backend_status()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn crates_link() {
        assert!(rift_canopy::status().contains("canopy"));
        assert!(rift_shadow::status().contains("shadow"));
    }
}

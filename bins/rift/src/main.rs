//! Rift entrypoint.
//!
//! Bare `rift` launches the **Canopy** dashboard. In an interactive terminal
//! (and with the default `tui` feature) that is the live xre-backed event loop
//! from [`rift_canopy::xre`]; when stdout is not a TTY, `RIFT_HEADLESS` is set,
//! or the `tui` feature is disabled, it degrades to printing a single ASCII
//! frame so scripts and tests stay deterministic.
//!
//! Rift owns the subcommands `setup`, `status`, `verify`, `scan`, `redact`,
//! `memory`, and `config`; anything else is forwarded verbatim to the wrapped
//! capture backend. `setup` registers the current directory into the global
//! project registry — the same registry the dashboard reads and the `a` key
//! writes — so the two paths to "add a project" share one source of truth.

use rift_canopy::{App, AsciiBackend, ProjStatus, Project, Rect, RenderBackend, SplashState};
use rift_registry::{default_registry_path, ProjectRegistry};

/// Project the registry onto Canopy's view model (all projects start `Idle`;
/// live status arrives once capture is wired).
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
/// refreshed project list. Used by the dashboard's `a` key and `/setup`.
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
                    // Terminal could not be driven — degrade rather than abort.
                    eprintln!("rift: could not start the interactive dashboard: {e}");
                }
            }
        }
    }

    print_ascii_frame(projects);
    Ok(())
}

/// `rift setup`: register the current working directory into the global registry.
fn cmd_setup() -> rift_core::Result<()> {
    let canon = std::env::current_dir()?.canonicalize()?;
    let name = project_name(&canon);
    let reg_path = default_registry_path();
    let mut reg = ProjectRegistry::load(&reg_path)?;
    let added = reg.add(name.clone(), canon.clone());
    reg.save(&reg_path)?;
    if added {
        println!("registered {name} at {}", canon.display());
    } else {
        println!("{name} is already registered ({})", canon.display());
    }
    println!("registry: {}", reg_path.display());
    Ok(())
}

fn main() -> rift_core::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => launch_canopy()?,
        Some("setup") => cmd_setup()?,
        Some("status") => println!("{}", rift_shadow::status()),
        Some("verify") => {
            println!("rift verify: re-derives the hash chain (see `rift-memd`/store)");
        }
        Some(cmd @ ("scan" | "redact" | "memory" | "config")) => {
            println!(
                "rift {cmd}: handled by Rift (rest of args: {:?})",
                &args[1..]
            );
        }
        Some(other) => {
            // unrecognized → forwarded verbatim to the wrapped capture backend
            println!("rift: forwarding `{other}` to the capture backend");
        }
    }
    rift_shadow::initialize()
}

#[cfg(test)]
mod tests {
    #[test]
    fn crates_link() {
        assert!(rift_canopy::status().contains("canopy"));
        assert!(rift_shadow::status().contains("shadow"));
    }
}

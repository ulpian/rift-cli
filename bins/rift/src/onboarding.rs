//! Interactive launch preparation. Project setup itself stays in the shared manager.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};

/// Finish setup before the dashboard's registry, workers, or view are created.
/// Return the family root to register, or `None` when the operator cancels.
pub(crate) fn prepare(ascii: bool) -> Result<Option<PathBuf>> {
    use rift_canopy::{ProjectInitialization, ProjectSettingsManager};
    let cwd = std::env::current_dir().map_err(RiftError::from)?;
    let Some(paths) = rift_git::discover(&cwd)
        .map(Some)
        .or_else(|_| initialize_git(&cwd))?
    else {
        return Ok(None);
    };
    let mut manager = crate::project_settings::RiftProjectSettingsManager;
    let mut view = manager
        .load(&paths.work_tree.to_string_lossy())
        .map_err(RiftError::Other)?;
    crate::installation::defaults::seed(&mut view)?;
    if view.initialization != ProjectInitialization::Initialized
        && !rift_canopy::app::live::run_onboarding(view, &mut manager, ascii)?
    {
        return Ok(None);
    }
    // The catalogue owns one row per family and derives linked worktrees as
    // children. Register the main checkout so launch can select the member
    // without violating that invariant, including the first launch in a member.
    let registered = if paths.is_linked() {
        rift_git::family_members(&paths)
            .into_iter()
            .find(|member| member.is_main && member.work_tree.is_dir())
            .map_or(paths.work_tree, |member| member.work_tree)
    } else {
        paths.work_tree
    };
    Ok(Some(registered))
}

fn initialize_git(path: &Path) -> Result<Option<rift_git::RepoPaths>> {
    if !confirm_git_init(path)? {
        return Ok(None);
    }
    let status = std::process::Command::new("git")
        .args(["init", "--"])
        .arg(path)
        .status()
        .map_err(RiftError::from)?;
    if !status.success() {
        return Err(RiftError::Other(
            "Git initialization failed; Rift setup was not started".into(),
        ));
    }
    rift_git::discover(path).map(Some)
}

fn confirm_git_init(path: &Path) -> Result<bool> {
    if !io::stdin().is_terminal() {
        return Err(RiftError::Other(
            "project setup needs an interactive terminal; run `git init` and then `rift`".into(),
        ));
    }
    println!("Rift needs a Git repository to capture this project.");
    println!(
        "Initialize Git in {}? No files will be committed.",
        path.display()
    );
    print!("[y/N] ");
    io::stdout().flush().map_err(RiftError::from)?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(RiftError::from)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

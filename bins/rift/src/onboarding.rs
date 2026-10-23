//! Interactive launch preparation. Project setup itself stays in the shared manager.

use std::io::{self, IsTerminal, Write};
use std::path::Path;

use rift_core::{Result, RiftError};

/// Ensure the launch repository is in the dashboard without provisioning it.
/// A fresh project opens staged settings after its first read-only status delivery.
pub(crate) fn prepare(store: &rift_canopy_data::CanopyStore) -> Result<bool> {
    let cwd = std::env::current_dir().map_err(RiftError::from)?;
    let Some(paths) = rift_git::discover(&cwd)
        .map(Some)
        .or_else(|_| initialize_git(&cwd))?
    else {
        return Ok(false);
    };
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
    let path = registered.to_string_lossy().into_owned();
    store.apply(
        &rift_canopy::PersistOp::AddProject { path },
        crate::hooks::now_ms(),
    )?;
    Ok(true)
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

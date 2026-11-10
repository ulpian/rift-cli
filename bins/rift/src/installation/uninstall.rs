//! Uninstall through the same ownership-aware integration services used by setup.

use super::lock::InstallLock;
use super::{path_entry, refusal, Installation, UninstallArgs};
use crate::cli::{exit, Outcome};
use rift_core::Result;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub(crate) fn run(args: &UninstallArgs) -> Result<Outcome> {
    let install = Installation::current()?;
    let projects = project_paths()?;
    let purge = args.purge.then(|| purge_paths(&projects)).transpose()?;
    // The installer's PATH line in each startup file it wrote; the user's own
    // lines are never touched.
    let path_plan = path_entry::planned_removals();
    let path_files: Vec<&Path> = path_plan.iter().map(|(file, _)| file.as_path()).collect();
    let summary = serde_json::json!({
        "binary": install.binary, "projects": projects,
        "purge": args.purge, "dry_run": args.dry_run,
        "git_history": "preserved", "repository_policy": "preserved",
        "purge_paths": purge, "path_entries": path_files,
    });
    if args.dry_run {
        return Ok(Outcome::ok(format!("Would remove {} and its owned integrations in {} known project/worktree paths, and the installer's PATH line from {} startup files. Local data: {}.", install.binary.display(), projects.len(), path_files.len(), if args.purge { "purge requested" } else { "preserved" }), summary));
    }
    if args.purge && !args.yes {
        return Ok(Outcome::coded(exit::PRECONDITION,
            "Purging deletes Rift's machine-local capture/Recall caches, settings, and project runtime state. Stop agents first. Review `rift uninstall --purge --dry-run`, then confirm with `rift uninstall --purge --yes`.", summary));
    }
    let install_lock = InstallLock::acquire(
        install
            .binary
            .parent()
            .ok_or_else(|| refusal("installation directory unavailable"))?,
    )?;
    install.recheck()?;
    for project in &projects {
        remove_integrations(project, &install.binary)?;
    }
    if let Some(paths) = &purge {
        for path in paths {
            remove_directory(path)?;
        }
    }
    install.recheck()?;
    let status = remove_executable(&install, install_lock.path())?;
    path_entry::remove(&path_plan)?;
    #[cfg(windows)]
    std::mem::forget(install_lock); // The deferred child now owns the installation lock.
    Ok(Outcome::ok(format!("Rift uninstall {status}. Captured Git history and repository policy were preserved. Local data: {}.", if args.purge { "purged" } else { "preserved" }), serde_json::json!({ "status": status, "operation": "uninstall", "purged": args.purge, "binary": install.binary })))
}

fn project_paths() -> Result<Vec<PathBuf>> {
    let mut paths = BTreeSet::new();
    let legacy = rift_registry::default_registry_path();
    if legacy.exists() {
        let registry = rift_registry::ProjectRegistry::load(&legacy)
            .map_err(|error| refusal(&format!("cannot read project registry: {error}")))?;
        paths.extend(
            registry
                .projects()
                .iter()
                .map(|project| project.path.clone()),
        );
    }
    if let Some(store) =
        rift_canopy_data::CanopyStore::open_read_only(&crate::family::cache_root())?
    {
        paths.extend(
            store
                .installed_project_paths()?
                .into_iter()
                .map(PathBuf::from),
        );
    }
    for path in paths.clone() {
        if let Ok(repo) = rift_git::discover(&path) {
            paths.extend(
                rift_git::list_worktrees(&repo)
                    .into_iter()
                    .map(|member| member.work_tree),
            );
        }
    }
    Ok(paths.into_iter().collect())
}

fn remove_integrations(project: &Path, binary: &Path) -> Result<()> {
    if !project.exists() {
        return Ok(());
    }
    for row in rift_agent_install::native::diagnose_all(project) {
        if !rift_agent_install::native::installed_hook_binary(project, &row.agent)
            .is_some_and(|path| same_file(&path, binary))
        {
            continue;
        }
        rift_agent_install::native::remove_recall_skill(project, &row.agent)
            .map_err(|error| refusal(&error.to_string()))?;
        rift_agent_install::native::disable(project, &row.agent)
            .map_err(|error| refusal(&error.to_string()))?;
    }
    if let Ok(paths) = rift_git::discover(project) {
        let hooks_dir = crate::hooks::effective_hooks_dir(project).ok_or_else(|| {
            refusal("cannot resolve Git's effective hooks directory; executable was preserved")
        })?;
        rift_shadow::hooks::uninstall_hooks(&rift_shadow::hooks::HookInstall {
            git_dir: paths.git_dir,
            rift_bin: binary.to_path_buf(),
            hooks_dir: Some(hooks_dir),
            family: crate::hooks::family_sites(&paths.common_dir),
            recorded_by: rift_shadow::hooks::RecordedBy::Init,
        })?;
    }
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    a.canonicalize().unwrap_or_else(|_| a.to_path_buf())
        == b.canonicalize().unwrap_or_else(|_| b.to_path_buf())
}

fn purge_paths(projects: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let cache = crate::family::cache_root().join("rift");
    let config = rift_registry::default_registry_path()
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| refusal("configuration directory unavailable"))?;
    // Refuse broad environment overrides such as a home directory or drive root.
    for root in [&cache, &config] {
        if !root
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("rift"))
        {
            return Err(refusal("purge refuses a nonstandard Rift data root; inspect and remove that directory explicitly"));
        }
        refuse_link(root)?;
    }
    let mut paths = vec![cache, config];
    for project in projects {
        let rift = project.join(".rift");
        refuse_link(&rift)?;
        let state = rift.join("state");
        paths.push(state);
    }
    // Validate every target before integrations or data are changed.
    for path in &paths {
        refuse_link(path)?;
        if path.exists() && !path.is_dir() {
            return Err(refusal("purge target must be a directory"));
        }
    }
    Ok(paths)
}

fn refuse_link(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(refusal("purge refuses symbolic-link data roots"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remove_directory(path: &Path) -> Result<()> {
    refuse_link(path)?;
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn remove_executable(install: &Installation, _lock: &Path) -> Result<&'static str> {
    std::fs::remove_file(&install.binary)?;
    std::fs::remove_file(&install.receipt)?;
    Ok("completed")
}

#[cfg(windows)]
fn remove_executable(install: &Installation, lock: &Path) -> Result<&'static str> {
    // Windows cannot remove the running image. One bounded child waits for this
    // exact process to exit, verifies ownership again, then removes only two files.
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
try {
# This child now holds the lock: record it, or a run that sees the parent gone would reclaim it.
[IO.File]::WriteAllText((Join-Path $env:RIFT_UNINSTALL_LOCK 'owner'), "$PID $([DateTimeOffset]::UtcNow.ToUnixTimeSeconds())`n")
try { $parent = [Diagnostics.Process]::GetProcessById([int]$env:RIFT_UNINSTALL_PID) }
catch [ArgumentException] { $parent = $null }
if ($parent -and -not $parent.WaitForExit(30000)) { exit 1 }
$file = Get-Item -LiteralPath $env:RIFT_UNINSTALL_BINARY -Force
if ($file.Attributes -band [IO.FileAttributes]::ReparsePoint) { exit 1 }
if ((Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant() -ne $env:RIFT_UNINSTALL_SHA) { exit 1 }
$receipt = Get-Item -LiteralPath $env:RIFT_UNINSTALL_RECEIPT -Force
if ($receipt.Attributes -band [IO.FileAttributes]::ReparsePoint) { exit 1 }
if ([IO.File]::ReadAllText($receipt.FullName) -ne $env:RIFT_UNINSTALL_RECORD) { exit 1 }
Remove-Item -LiteralPath $file.FullName
Remove-Item -LiteralPath $env:RIFT_UNINSTALL_RECEIPT
} finally { [IO.Directory]::Delete($env:RIFT_UNINSTALL_LOCK, $true) }
"#;
    let parent = install.binary.parent().unwrap_or(install.binary.as_path());
    // After the checks and the removal, never before: a deferred run that
    // refuses the binary must leave the user's Path as it was.
    let script = format!("{SCRIPT}{}", path_entry::windows_path_script(parent));
    std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .env("RIFT_UNINSTALL_BIN", parent)
        .env("RIFT_UNINSTALL_PID", std::process::id().to_string())
        .env("RIFT_UNINSTALL_BINARY", &install.binary)
        .env("RIFT_UNINSTALL_RECEIPT", &install.receipt)
        .env("RIFT_UNINSTALL_SHA", &install.checksum)
        .env(
            "RIFT_UNINSTALL_RECORD",
            std::fs::read_to_string(&install.receipt)?,
        )
        .env("RIFT_UNINSTALL_LOCK", lock)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok("scheduled after this process exits")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn purge_refuses_linked_directories_without_deleting_their_contents() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("unrelated");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), b"keep").unwrap();
        let link = dir.path().join("rift");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(remove_directory(&link).is_err());
        assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"keep");
    }
}

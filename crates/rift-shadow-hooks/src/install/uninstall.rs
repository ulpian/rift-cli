//! Remove exactly our hooks and restore any chained user hooks.

use super::entry::{exists_without_following, hook_entry, link_marker, HookEntry};
use super::{shim_text, HookInstall, CHAINED_SUFFIX, HOOK_NAMES};
use rift_core::{Result, RiftError};
use std::path::{Path, PathBuf};

/// Remove hooks naming this installation, preserving foreign and edited hooks.
/// Every candidate is validated before the first removal. Chained hooks are
/// renamed back without interpreting or following their contents.
///
/// # Errors
/// Refuses an edited shim belonging to this installation, or a filesystem error.
pub fn uninstall_hooks(cfg: &HookInstall) -> Result<Vec<PathBuf>> {
    let dir = cfg.resolved_hooks_dir();
    let mut plan = Vec::new();
    for name in HOOK_NAMES {
        let path = dir.join(name);
        if owned_by(&path, &cfg.rift_bin, name)? {
            plan.push((path, *name));
        }
    }
    let mut changed = Vec::new();
    for (path, name) in plan {
        if !owned_by(&path, &cfg.rift_bin, name)? {
            return Err(RiftError::Other(
                "Git hook changed during uninstall; executable was preserved".into(),
            ));
        }
        std::fs::remove_file(&path)?;
        let marker = link_marker(&path);
        if exists_without_following(&marker)? {
            std::fs::remove_file(marker)?;
        }
        let chained = dir.join(format!("{name}{CHAINED_SUFFIX}"));
        if exists_without_following(&chained)? {
            std::fs::rename(chained, &path)?;
        }
        changed.push(path);
    }
    Ok(changed)
}

fn owned_by(path: &Path, binary: &Path, name: &str) -> Result<bool> {
    match hook_entry(path) {
        HookEntry::ManagedLink(target) => Ok(target == binary),
        HookEntry::ManagedShim => {
            let text = std::fs::read_to_string(path)?;
            if text == shim_text(binary, name) {
                return Ok(true);
            }
            if text.contains(&binary.to_string_lossy().to_string()) {
                return Err(RiftError::Other(format!(
                    "edited Rift hook {}; restore or remove it explicitly before uninstalling",
                    path.display()
                )));
            }
            Ok(false)
        }
        HookEntry::Missing | HookEntry::Foreign => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninstall_restores_user_hooks_and_preserves_other_installations() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = super::super::tests::cfg(tmp.path(), None);
        let dir = cfg.resolved_hooks_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("post-commit"),
            shim_text(&cfg.rift_bin, "post-commit"),
        )
        .unwrap();
        std::fs::write(
            dir.join("post-commit.rift-chained"),
            "#!/bin/sh\necho user\n",
        )
        .unwrap();
        let other = shim_text(Path::new("/another/rift"), "post-checkout");
        std::fs::write(dir.join("post-checkout"), &other).unwrap();
        uninstall_hooks(&cfg).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("post-commit")).unwrap(),
            "#!/bin/sh\necho user\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("post-checkout")).unwrap(),
            other
        );
    }

    #[test]
    fn edited_owned_hook_refuses_before_removing_any_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = super::super::tests::cfg(tmp.path(), None);
        let dir = cfg.resolved_hooks_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let text = format!("{}\necho custom\n", shim_text(&cfg.rift_bin, "post-commit"));
        std::fs::write(dir.join("post-commit"), &text).unwrap();
        assert!(uninstall_hooks(&cfg).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join("post-commit")).unwrap(),
            text
        );
    }
}

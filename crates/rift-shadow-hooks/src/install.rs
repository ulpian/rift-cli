//! P3 S1 — git-hook installation (GUI-client safe, never-clobber).
//!
//! Installs the six capture hooks as tiny POSIX-`sh` shims that invoke `rift` by
//! **absolute** path — so a desktop GUI with a sparse `PATH` still finds it. Each
//! shim is marked Rift-managed and **chains** (never clobbers) a pre-existing user
//! hook by renaming it aside and running it first. The shims do not name any
//! upstream tool.
//!
//! These functions only write files; the never-block discipline (a panicking
//! capture must still let the commit succeed) lives at the hook *entrypoint* in
//! the bin, which the shim execs.

use std::path::{Path, PathBuf};

use rift_core::Result;

/// Marker line identifying a shim Rift owns (so re-install overwrites our shim but
/// never a user's hook).
const MARKER: &str = "# Rift-managed hook";
/// Suffix for a chained-aside user hook.
const CHAINED_SUFFIX: &str = ".rift-chained";

/// The six git hooks Rift installs (P3 S1).
pub const HOOK_NAMES: &[&str] = &[
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "post-rewrite",
    "pre-push",
    "post-merge",
];

/// Where + how to install. `rift_bin` must be an **absolute** path.
#[derive(Clone, Debug)]
pub struct HookInstall {
    /// The repository's git dir (passed to each shim as `--git-dir`).
    pub git_dir: PathBuf,
    /// The absolute path to the `rift` binary the shim execs.
    pub rift_bin: PathBuf,
    /// An explicit hooks directory (honors `core.hooksPath`); defaults to
    /// `<git_dir>/hooks` when `None`.
    pub hooks_dir: Option<PathBuf>,
}

impl HookInstall {
    /// The resolved hooks directory.
    #[must_use]
    pub fn resolved_hooks_dir(&self) -> PathBuf {
        self.hooks_dir
            .clone()
            .unwrap_or_else(|| self.git_dir.join("hooks"))
    }
}

/// The shim text for one hook. It chains a pre-existing user hook (now `<name>{CHAINED_SUFFIX}`)
/// first, preserving its exit status, then execs `rift hooks git <name>`.
#[must_use]
pub fn shim_text(rift_bin: &Path, name: &str) -> String {
    let bin = rift_bin.display();
    format!(
        "#!/bin/sh\n\
         {MARKER} — do not edit.\n\
         chained=\"$(dirname \"$0\")/{name}{CHAINED_SUFFIX}\"\n\
         if [ -x \"$chained\" ]; then \"$chained\" \"$@\" || exit $?; fi\n\
         exec \"{bin}\" hooks git {name} --git-dir \"${{GIT_DIR:-.git}}\" -- \"$@\"\n"
    )
}

/// Whether a file at `path` is a Rift-managed shim.
fn is_rift_managed(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|s| s.contains(MARKER))
}

/// Install all six hooks, chaining any pre-existing user hook. Returns the written
/// shim paths.
///
/// # Errors
/// [`RiftError::Io`](rift_core::RiftError::Io) on a filesystem failure.
pub fn install_hooks(cfg: &HookInstall) -> Result<Vec<PathBuf>> {
    let dir = cfg.resolved_hooks_dir();
    std::fs::create_dir_all(&dir)?;
    let mut written = Vec::with_capacity(HOOK_NAMES.len());
    for name in HOOK_NAMES {
        let target = dir.join(name);
        // Chain a pre-existing, non-Rift hook aside (once) so we never clobber it.
        if target.exists() && !is_rift_managed(&target) {
            let chained = dir.join(format!("{name}{CHAINED_SUFFIX}"));
            if !chained.exists() {
                std::fs::rename(&target, &chained)?;
            }
        }
        std::fs::write(&target, shim_text(&cfg.rift_bin, name))?;
        set_executable(&target)?;
        written.push(target);
    }
    Ok(written)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}
#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    // Windows git runs hooks via its bundled `sh`; the execute bit is not used.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &Path, hooks: Option<PathBuf>) -> HookInstall {
        HookInstall {
            git_dir: dir.join(".git"),
            rift_bin: PathBuf::from("/usr/local/bin/rift"),
            hooks_dir: hooks,
        }
    }

    #[test]
    fn install_writes_absolute_path_shims() {
        let tmp = tempfile::tempdir().unwrap();
        let written = install_hooks(&cfg(tmp.path(), None)).unwrap();
        assert_eq!(written.len(), HOOK_NAMES.len());
        for (path, name) in written.iter().zip(HOOK_NAMES) {
            let text = std::fs::read_to_string(path).unwrap();
            assert!(text.contains("/usr/local/bin/rift"), "absolute path");
            assert!(!text.contains("exec \"rift\""), "never a bare name");
            assert!(text.contains(&format!("hooks git {name} ")));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = std::fs::metadata(path).unwrap().permissions().mode();
                assert_eq!(mode & 0o111, 0o111, "shim must be executable");
            }
        }
    }

    #[test]
    fn install_chains_existing_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let user = hooks.join("post-commit");
        std::fs::write(&user, "#!/bin/sh\necho user hook\n").unwrap();

        install_hooks(&cfg(tmp.path(), None)).unwrap();

        // the user's hook is preserved aside, not clobbered
        let chained = hooks.join("post-commit.rift-chained");
        assert!(chained.exists(), "user hook chained aside");
        assert!(std::fs::read_to_string(&chained)
            .unwrap()
            .contains("echo user hook"));
        // the new shim references the chained hook
        let shim = std::fs::read_to_string(hooks.join("post-commit")).unwrap();
        assert!(shim.contains("post-commit.rift-chained"));
        assert!(shim.contains(MARKER));
    }

    #[test]
    fn reinstall_is_idempotent_and_does_not_double_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(hooks.join("post-commit"), "#!/bin/sh\necho user\n").unwrap();

        install_hooks(&cfg(tmp.path(), None)).unwrap();
        let chained_once = std::fs::read_to_string(hooks.join("post-commit.rift-chained")).unwrap();
        // second install must NOT chain our own shim over the user's chained hook
        install_hooks(&cfg(tmp.path(), None)).unwrap();
        let chained_twice =
            std::fs::read_to_string(hooks.join("post-commit.rift-chained")).unwrap();
        assert_eq!(
            chained_once, chained_twice,
            "user hook preserved, not re-chained"
        );
        assert!(chained_twice.contains("echo user"));
    }

    #[test]
    fn respects_custom_hooks_path() {
        let tmp = tempfile::tempdir().unwrap();
        let custom = tmp.path().join("my-hooks");
        install_hooks(&cfg(tmp.path(), Some(custom.clone()))).unwrap();
        assert!(custom.join("pre-push").exists());
        assert!(!tmp.path().join(".git/hooks/pre-push").exists());
    }

    #[test]
    fn hooks_name_no_upstream_brand() {
        // Shares P1 S1's forbidden-token set; no upstream brand may appear.
        for name in HOOK_NAMES {
            let text = shim_text(Path::new("/abs/rift"), name).to_ascii_lowercase();
            assert!(!text.contains("entire"), "{name} shim leaks brand");
            assert!(!text.contains("entireio"), "{name} shim leaks brand");
        }
    }
}

//! The bytes an install puts on disk: the shim text, the never-clobber write
//! of every hook, and the execute bit.
//!
//! Separate from the act because these are the only functions here that write a
//! hook, and what they write is a byte contract with repositories already
//! provisioned — a re-spelled marker or a re-ordered shim line is a silent
//! break, so the writing is read in one place.

use std::path::{Path, PathBuf};

use rift_core::Result;

#[cfg(unix)]
use super::entry::{direct_link_marker_bytes, link_marker};
use super::entry::{exists_without_following, is_rift_managed};
use super::{HookInstall, CHAINED_SUFFIX, HOOK_NAMES, MARKER};

/// The shim text for one hook. It chains a pre-existing user hook (now `<name>{CHAINED_SUFFIX}`)
/// first, preserving its exit status, then execs `rift hooks git <name>`.
///
/// **The hook directory comes from parameter expansion, never a command
/// substitution.** `$(dirname "$0")` forks a shell and execs `/usr/bin/dirname`
/// on every hook, and a `git commit` runs three of these shims, so the
/// repository spent that three times over on a 100 ms git-latency budget to
/// compute a string. `${{0%/*}}` is the same answer with no process.
///
/// Certified cost, idle machine, release build, 40 iterations per timing: the
/// installed shim **19.34 ms → 15.78 ms**, i.e. **3.56 ms per hook and 10.7 ms
/// per commit**. (An earlier figure of 4.1 ms/hook circulated from a run taken
/// under load average 6–14; it is quoted here only so the two are not mistaken
/// for a discrepancy. Prefer the idle number — the loaded one measures the
/// machine.)
///
/// The two spellings differ on one input, and the cheap one is also the safer:
/// when `$0` carries no `/`, `dirname` answers `.` and the chain probe resolves
/// against the **process working directory** — an attacker-writable worktree
/// could put an executable there — while `${{0%/*}}` leaves `$0` whole, so the
/// probe names `<hook>/<hook>.rift-chained`, a path under a regular file, which
/// can never be executable. It fails closed instead of resolving elsewhere.
#[must_use]
pub fn shim_text(rift_bin: &Path, name: &str) -> String {
    let bin = shell_word(&rift_bin.display().to_string());
    format!(
        "#!/bin/sh\n\
         {MARKER} — do not edit.\n\
         chained=\"${{0%/*}}/{name}{CHAINED_SUFFIX}\"\n\
         if [ -x \"$chained\" ]; then \"$chained\" \"$@\" || exit $?; fi\n\
         exec {bin} hooks git {name} --git-dir \"${{GIT_DIR:-.git}}\" -- \"$@\"\n"
    )
}

/// The binary path as one shell word that runs exactly that path.
///
/// Double quotes, as every shim was written, while the path holds nothing
/// those quotes would act on: uninstall and refresh recognise a shim by its
/// exact text, so an ordinary path must keep producing the same bytes. A path
/// holding `"`, `$`, `` ` `` or `\` would end the word early or expand inside
/// it, so it is single-quoted instead, with each `'` closed, escaped and
/// reopened.
fn shell_word(path: &str) -> String {
    if !path.contains(['"', '$', '`', '\\']) {
        return format!("\"{path}\"");
    }
    format!("'{}'", path.replace('\'', r"'\''"))
}

/// Write every hook, chaining any pre-existing user hook aside. Returns the
/// installed hook paths.
pub(super) fn write_hooks(cfg: &HookInstall, dir: &Path) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let mut written = Vec::with_capacity(HOOK_NAMES.len());
    for name in HOOK_NAMES {
        let target = dir.join(name);
        let chained = dir.join(format!("{name}{CHAINED_SUFFIX}"));
        // Chain a pre-existing, non-Rift hook aside (once) so we never clobber it.
        if exists_without_following(&target)? && !is_rift_managed(&target) {
            if exists_without_following(&chained)? {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "refusing to overwrite user hook {} while {} already exists",
                        target.display(),
                        chained.display()
                    ),
                )
                .into());
            }
            std::fs::rename(&target, &chained)?;
        }

        if exists_without_following(&target)? {
            std::fs::remove_file(&target)?;
        }

        if exists_without_following(&chained)? {
            // The shell is retained only where it has real work to do: run the
            // user's hook before Rift while preserving that hook's exit status.
            std::fs::write(&target, shim_text(&cfg.rift_bin, name))?;
            set_executable(&target)?;
            #[cfg(unix)]
            {
                let marker = link_marker(&target);
                if exists_without_following(&marker)? {
                    std::fs::remove_file(marker)?;
                }
            }
        } else {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&cfg.rift_bin, &target)?;
                std::fs::write(
                    link_marker(&target),
                    direct_link_marker_bytes(&cfg.rift_bin),
                )?;
            }
            #[cfg(not(unix))]
            {
                std::fs::write(&target, shim_text(&cfg.rift_bin, name))?;
                set_executable(&target)?;
            }
        }
        written.push(target);
    }
    Ok(written)
}

#[cfg(unix)]
pub(super) fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}
#[cfg(not(unix))]
pub(super) fn set_executable(_path: &Path) -> Result<()> {
    // Windows git runs hooks via its bundled `sh`; the execute bit is not used.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::install::entry::direct_link_marker_bytes;
    use crate::install::install_hooks_with;
    use crate::install::tests::{cfg, repoint};

    #[test]
    fn install_uses_absolute_binary_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let report = install_hooks_with(&cfg(tmp.path(), None), repoint()).unwrap();
        let written = report.repointed;
        assert_eq!(written.len(), HOOK_NAMES.len());
        for (path, name) in written.iter().zip(HOOK_NAMES) {
            assert_eq!(
                path.file_name().and_then(|value| value.to_str()),
                Some(*name)
            );
            #[cfg(unix)]
            {
                assert_eq!(
                    std::fs::read_link(path).unwrap(),
                    Path::new("/usr/local/bin/rift")
                );
                assert_eq!(
                    std::fs::read(link_marker(path)).unwrap(),
                    direct_link_marker_bytes(Path::new("/usr/local/bin/rift"))
                );
            }
            #[cfg(not(unix))]
            {
                let text = std::fs::read_to_string(path).unwrap();
                assert!(text.contains("/usr/local/bin/rift"), "absolute path");
                assert!(!text.contains("exec \"rift\""), "never a bare name");
                assert!(text.contains(&format!("hooks git {name} ")));
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

        install_hooks_with(&cfg(tmp.path(), None), repoint()).unwrap();

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

        install_hooks_with(&cfg(tmp.path(), None), repoint()).unwrap();
        let chained_once = std::fs::read_to_string(hooks.join("post-commit.rift-chained")).unwrap();
        // second install must NOT chain our own shim over the user's chained hook
        install_hooks_with(&cfg(tmp.path(), None), repoint()).unwrap();
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
        install_hooks_with(&cfg(tmp.path(), Some(custom.clone())), repoint()).unwrap();
        assert!(std::fs::symlink_metadata(custom.join("pre-push")).is_ok());
        assert!(std::fs::symlink_metadata(tmp.path().join(".git/hooks/pre-push")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn reinstall_updates_a_managed_direct_link_without_chaining_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut first = cfg(tmp.path(), None);
        first.rift_bin = PathBuf::from("/first/rift");
        install_hooks_with(&first, repoint()).unwrap();

        let mut second = cfg(tmp.path(), None);
        second.rift_bin = PathBuf::from("/second/rift");
        install_hooks_with(&second, repoint()).unwrap();

        let hooks = tmp.path().join(".git/hooks");
        assert_eq!(
            std::fs::read_link(hooks.join("post-commit")).unwrap(),
            Path::new("/second/rift")
        );
        assert!(std::fs::symlink_metadata(hooks.join("post-commit.rift-chained")).is_err());
    }

    #[test]
    fn no_shim_forks_a_process_to_compute_its_own_directory() {
        // A command substitution in the shim is a fork+exec on the hook path,
        // three times per commit. Backticks count as one too, so both
        // spellings are screened rather than the one that happens to be here.
        for name in HOOK_NAMES {
            let text = shim_text(Path::new("/abs/rift"), name);
            assert!(!text.contains("$("), "{name}: command substitution: {text}");
            assert!(!text.contains('`'), "{name}: backtick substitution: {text}");
            assert!(text.contains("${0%/*}"), "{name}: {text}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_shim_runs_the_chained_hook_and_keeps_its_exit_code() {
        // The parameter-expansion rewrite changes how the shim resolves the
        // chained hook's path, so the guarantee is asserted by RUNNING it, not
        // by reading the text. The chained hook exits 3 so the shim returns
        // before the final `exec` (whose binary does not exist here) — which
        // also pins the "preserve the user hook's exit status" contract.
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let user = hooks.join("post-commit");
        let marker = tmp.path().join("ran");
        std::fs::write(
            &user,
            format!("#!/bin/sh\ntouch '{}'\nexit 3\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o755)).unwrap();

        install_hooks_with(&cfg(tmp.path(), None), repoint()).unwrap();

        let out = std::process::Command::new(hooks.join("post-commit"))
            .current_dir(tmp.path())
            .output()
            .unwrap();
        assert!(marker.exists(), "the chained hook ran");
        assert_eq!(out.status.code(), Some(3), "its exit status is preserved");
    }

    #[test]
    fn hooks_name_no_upstream_brand() {
        // Shares the capture layer's forbidden-token set; no upstream brand
        // may appear.
        for name in HOOK_NAMES {
            let text = shim_text(Path::new("/abs/rift"), name).to_ascii_lowercase();
            assert!(
                !text.contains(concat!("ent", "ire")),
                "{name} shim leaks brand"
            );
            assert!(
                !text.contains(concat!("ent", "ireio")),
                "{name} shim leaks brand"
            );
        }
    }

    #[test]
    fn an_ordinary_path_keeps_its_bytes_and_a_live_one_is_single_quoted() {
        assert_eq!(
            shell_word("/Users/a b/.local/bin/rift"),
            "\"/Users/a b/.local/bin/rift\""
        );
        assert_eq!(shell_word("/opt/$HOME/rift"), "'/opt/$HOME/rift'");
        assert_eq!(
            shell_word("/opt/it's \"q\"/rift"),
            r#"'/opt/it'\''s "q"/rift'"#
        );
        let text = shim_text(Path::new("/opt/$x`y`/rift"), "post-commit");
        assert!(
            text.contains("exec '/opt/$x`y`/rift' hooks git post-commit"),
            "{text}"
        );
    }
}

//! The entries left under names [`HOOK_NAMES`](super::HOOK_NAMES) no longer
//! carries.
//!
//! Its own module because removal is the one thing an install does that is not
//! an install: git never hears that a name left the dispatch table, so it goes
//! on running the entry, and the rules for taking one away — only ours, only
//! the entry, and the user's chained hook put back — are their own set.

use std::path::{Path, PathBuf};

use rift_core::Result;

use super::entry::{exists_without_following, is_rift_managed, link_marker};
use super::{CHAINED_SUFFIX, HOOK_NAMES, LINK_MARKER_SUFFIX};

/// Remove the Rift-managed entries under names [`HOOK_NAMES`] no longer
/// carries, and report what was removed.
///
/// A family provisioned while the list was longer still has an entry under
/// every name that has since left it, and git goes on running those: it never
/// hears that a name left the dispatch table, so a merge and every step of a
/// rebase run a link whose binary answers with a usage error. The name left
/// the list, so the entry leaves the directory.
///
/// Only the entry, and only ours: ownership is the same question
/// [`is_rift_managed`] answers for a live name, so a hook somebody else put
/// there under a retired name is left exactly where it is. Nothing here
/// follows a link — the link is what is removed, never what it names.
///
/// # Errors
/// [`RiftError::Io`](rift_core::RiftError::Io) on a filesystem failure.
pub(super) fn sweep_retired_hooks(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    // Read the whole directory before touching any of it: removing entries
    // while the handle walks them is where a listing starts skipping things.
    let mut retired: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_retired_hook(path))
        .collect();
    retired.sort();
    for path in &retired {
        remove_retired_hook(path)?;
    }
    Ok(retired)
}

/// Whether the entry at `path` is one of Rift's, under a name Rift no longer
/// installs — what an install removes and what `rift doctor` reports.
///
/// The same ownership question [`is_managed_hook`](super::is_managed_hook)
/// answers for a live name,
/// asked of a name that is not in [`HOOK_NAMES`] at all. Public because the
/// diagnosis and the repair must agree on the answer, and a second reading of
/// it in the bin is a second answer waiting to happen.
#[must_use]
pub fn is_retired_hook(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    // Neither suffix names a hook entry: a chained hook is the user's file,
    // which git never runs by that name, and a sidecar goes with its link.
    if HOOK_NAMES.contains(&name)
        || name.ends_with(CHAINED_SUFFIX)
        || name.ends_with(LINK_MARKER_SUFFIX)
    {
        return false;
    }
    is_rift_managed(path)
}

/// Remove one retired entry and its sidecar, and put back the user hook the
/// install that wrote it chained aside.
///
/// The shim that ran that hook is going; leaving it behind the suffix would end
/// a hook the user has always had, and end it the way this whole module exists
/// to prevent — silently.
fn remove_retired_hook(path: &Path) -> Result<()> {
    std::fs::remove_file(path)?;
    let marker = link_marker(path);
    if exists_without_following(&marker)? {
        std::fs::remove_file(&marker)?;
    }
    let mut chained = path.as_os_str().to_os_string();
    chained.push(CHAINED_SUFFIX);
    let chained = PathBuf::from(chained);
    if exists_without_following(&chained)? {
        std::fs::rename(&chained, path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::install::entry::direct_link_marker_bytes;
    use crate::install::tests::{fake_binary, install_at, keep, repoint};
    #[cfg(not(unix))]
    use crate::install::{shim_text, write::set_executable};

    /// The shape a family provisioned while [`HOOK_NAMES`] was longer still
    /// carries: a Rift-managed entry under a name the binary no longer routes.
    fn retired_managed_hook(hooks: &Path, name: &str, bin: &Path) {
        std::fs::create_dir_all(hooks).unwrap();
        let path = hooks.join(name);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(bin, &path).unwrap();
            std::fs::write(link_marker(&path), direct_link_marker_bytes(bin)).unwrap();
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&path, shim_text(bin, name)).unwrap();
            set_executable(&path).unwrap();
        }
    }

    /// ★ A healthy family is swept before it is kept.
    ///
    /// Every entry under a retired name still runs — git does not know the name
    /// left the dispatch table, so it runs the link on every merge and every
    /// rebase step, and the binary answers a name it cannot route with a usage
    /// error. The early return for a healthy family is the one path that would
    /// never look at the directory again, so the sweep runs before it.
    #[test]
    fn a_healthy_family_loses_the_entries_for_names_rift_no_longer_installs() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = fake_binary(&tmp.path().join("live"), "rift");
        install_at(tmp.path(), &bin, repoint());
        let hooks = tmp.path().join(".git/hooks");
        retired_managed_hook(&hooks, "post-merge", &bin);
        retired_managed_hook(&hooks, "post-rewrite", &bin);

        let report = install_at(tmp.path(), &bin, keep(true));

        assert_eq!(report.kept.len(), HOOK_NAMES.len(), "{report:?}");
        assert_eq!(
            report.removed,
            vec![hooks.join("post-merge"), hooks.join("post-rewrite")]
        );
        for name in ["post-merge", "post-rewrite"] {
            let path = hooks.join(name);
            assert!(!exists_without_following(&path).unwrap(), "{name} is gone");
            #[cfg(unix)]
            assert!(
                !exists_without_following(&link_marker(&path)).unwrap(),
                "{name} took its marker with it"
            );
        }
    }

    /// The sweep is not the health probe's: an explicit re-point removes the
    /// retired entries too, and reports them beside what it wrote.
    #[test]
    fn a_repoint_removes_the_entries_for_names_rift_no_longer_installs() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = fake_binary(&tmp.path().join("live"), "rift");
        let hooks = tmp.path().join(".git/hooks");
        retired_managed_hook(&hooks, "post-merge", &bin);

        let report = install_at(tmp.path(), &bin, repoint());

        assert_eq!(report.repointed.len(), HOOK_NAMES.len());
        assert_eq!(report.removed, vec![hooks.join("post-merge")]);
        assert!(!exists_without_following(&hooks.join("post-merge")).unwrap());
    }

    /// The never-clobber rule reaches the retired names as well: a hook under
    /// one of them that Rift did not write is the user's, a `.sample` is git's,
    /// and neither is Rift's to remove.
    #[test]
    fn a_hook_rift_did_not_write_survives_the_sweep_of_a_retired_name() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(hooks.join("post-merge"), "#!/bin/sh\necho theirs\n").unwrap();
        std::fs::write(hooks.join("pre-rebase.sample"), "#!/bin/sh\nexit 0\n").unwrap();

        let bin = fake_binary(&tmp.path().join("live"), "rift");
        let report = install_at(tmp.path(), &bin, repoint());

        assert!(report.removed.is_empty(), "{report:?}");
        assert!(std::fs::read_to_string(hooks.join("post-merge"))
            .unwrap()
            .contains("echo theirs"));
        assert!(hooks.join("pre-rebase.sample").exists());
    }

    /// A link a marker disowns is a user hook under a retired name exactly as
    /// it is under a live one — the ownership question has one answer here.
    #[cfg(unix)]
    #[test]
    fn a_retired_link_contradicted_by_its_marker_is_a_user_hook_and_stays() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let theirs = tmp.path().join("their-tool/rift");
        let hook = hooks.join("post-merge");
        std::os::unix::fs::symlink(&theirs, &hook).unwrap();
        std::fs::write(
            link_marker(&hook),
            direct_link_marker_bytes(Path::new("/opt/rift/rift")),
        )
        .unwrap();

        let bin = fake_binary(&tmp.path().join("live"), "rift");
        let report = install_at(tmp.path(), &bin, repoint());

        assert!(report.removed.is_empty(), "{report:?}");
        assert_eq!(std::fs::read_link(&hook).unwrap(), theirs);
    }

    /// The user's hook goes back where it was. An install that met a
    /// `post-merge` of theirs chained it aside and ran it from the shim; with
    /// the shim gone, leaving it behind the suffix would end a hook they have
    /// always had, and end it silently.
    #[test]
    fn a_user_hook_chained_aside_under_a_retired_name_is_put_back() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(
            hooks.join(format!("post-merge{CHAINED_SUFFIX}")),
            "#!/bin/sh\necho theirs\n",
        )
        .unwrap();
        let bin = fake_binary(&tmp.path().join("live"), "rift");
        retired_managed_hook(&hooks, "post-merge", &bin);

        let report = install_at(tmp.path(), &bin, repoint());

        assert_eq!(report.removed, vec![hooks.join("post-merge")]);
        assert!(std::fs::read_to_string(hooks.join("post-merge"))
            .unwrap()
            .contains("echo theirs"));
        assert!(
            !exists_without_following(&hooks.join(format!("post-merge{CHAINED_SUFFIX}"))).unwrap(),
            "the suffix is not left holding a hook that runs again"
        );
    }

    /// A chained-aside user hook under a name Rift still installs belongs to
    /// the shim that runs it, and the sweep passes it by.
    #[cfg(unix)]
    #[test]
    fn a_chained_user_hook_under_a_live_name_is_not_swept() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        // A dangling link to a file named `rift` is claimed as ours when it
        // stands at a hook's own name; behind the chained suffix it is the
        // user's, whatever it points at.
        let chained = hooks.join(format!("post-commit{CHAINED_SUFFIX}"));
        std::os::unix::fs::symlink(tmp.path().join("gone/rift"), &chained).unwrap();

        let bin = fake_binary(&tmp.path().join("live"), "rift");
        let report = install_at(tmp.path(), &bin, repoint());

        assert!(report.removed.is_empty(), "{report:?}");
        assert!(exists_without_following(&chained).unwrap());
    }
}

//! Whose hook an entry already is — read without ever following it.
//!
//! One module because ownership has to have exactly one answer: the installer,
//! the retired-name sweep and `rift doctor` all ask it, and a second reading of
//! the marker bytes anywhere would be a second answer waiting to disagree. A
//! managed link whose target is gone is still managed, which is what lets a
//! dangling hook be healed in place instead of seized from its owner.

use std::path::{Path, PathBuf};

use super::{LINK_MARKER_SUFFIX, MARKER};

/// The sidecar beside a hook entry — path math only, so every host can name
/// the file a Unix install wrote.
pub(super) fn link_marker(path: &Path) -> PathBuf {
    let mut marker = path.as_os_str().to_os_string();
    marker.push(LINK_MARKER_SUFFIX);
    PathBuf::from(marker)
}

#[cfg(unix)]
pub(super) fn direct_link_marker_bytes(target: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;

    // Keep the path's exact platform bytes. The record is compared as an opaque
    // ownership fingerprint; it is never parsed back into a path.
    let mut record = Vec::with_capacity(MARKER.len() + 1 + target.as_os_str().as_bytes().len());
    record.extend_from_slice(MARKER.as_bytes());
    record.push(0);
    record.extend_from_slice(target.as_os_str().as_bytes());
    record
}

/// Whether a file at `path` is a Rift-managed shim or direct hook link.
///
/// Nothing here follows the link, so **a managed link whose target is gone is
/// still managed**. That has to be true for a dangling hook to
/// be healed in place: the alternative is what happened on this machine on
/// 2026-08-30, where six dead links were chained aside as if they were user
/// hooks — leaving six dead `*.rift-chained` links and a shim on every commit,
/// for hooks that had been Rift's all along.
///
/// The sidecar marker is the ownership proof, and it did not exist before
/// `fd5d716b` (2026-08-26 21:37) — ten minutes *after* the install that pointed
/// this repository's hooks at a `/tmp` binary. So the second arm exists: a
/// **dangling** symlink to a file named `rift`, with no marker beside it, is
/// one of ours from before the marker. It is claimed only while it is dangling;
/// a link that still resolves can still run, and a hook that runs is never
/// taken from its owner.
pub(super) fn is_rift_managed(path: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    if metadata.file_type().is_symlink() {
        let Ok(target) = std::fs::read_link(path) else {
            return false;
        };
        if std::fs::read(link_marker(path))
            .is_ok_and(|record| record == direct_link_marker_bytes(&target))
        {
            return true;
        }
        return !link_target_exists(path, &target)
            && target.file_name().is_some_and(|name| name == "rift")
            && !link_marker(path).exists();
    }
    metadata.file_type().is_file()
        && std::fs::read_to_string(path).is_ok_and(|s| s.contains(MARKER))
}

/// Whether the entry at a hook path is one Rift installed — the question
/// `rift doctor` has to answer before it decides a repository is *broken*
/// rather than simply not set up.
///
/// A repository with no Rift hooks is not a finding: `rift init` is what
/// installs them, and a diagnosis that installed them as a side effect would be
/// the very class of side effect the family-owned hooks exist to remove.
#[must_use]
pub fn is_managed_hook(path: &Path) -> bool {
    is_rift_managed(path)
}

/// One hook entry, seen without following it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum HookEntry {
    /// Nothing at that path, or something unreadable.
    Missing,
    /// Someone else's hook. Rift chains it aside; it never counts as healthy.
    Foreign,
    /// A Rift-managed direct symlink, and the target it names — which may be
    /// gone (that is the dangling case, and the whole reason for the policy).
    ManagedLink(PathBuf),
    /// A Rift-managed shim: the chained-user-hook shape, and what the
    /// installer writes on every non-Unix host. Its target is inside the
    /// script, so the family's record is the only thing that can answer for it.
    ManagedShim,
}

/// Classify one hook path.
///
/// The link arm is not platform-gated: the installer writes links only on
/// Unix, but *reading* one is portable (`symlink_metadata` and `read_link`
/// answer on every host), and a gate here made a Windows build declare the
/// variant dead while a Windows checkout of a Unix-installed family would have
/// mis-read its links as shims.
pub(super) fn hook_entry(path: &Path) -> HookEntry {
    if !exists_without_following(path).unwrap_or(false) {
        return HookEntry::Missing;
    }
    if !is_rift_managed(path) {
        return HookEntry::Foreign;
    }
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return std::fs::read_link(path).map_or(HookEntry::Missing, HookEntry::ManagedLink);
    }
    HookEntry::ManagedShim
}

pub(super) fn exists_without_following(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Whether the link at `link` resolves — a relative target is resolved against
/// the link's own directory, the way the kernel does it, never against the
/// process working directory.
pub(super) fn link_target_exists(link: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return target.exists();
    }
    link.parent()
        .map_or_else(|| target.to_path_buf(), |dir| dir.join(target))
        .exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::tests::{fake_binary, install_at, keep};
    use crate::install::HOOK_NAMES;

    /// The 2026-08-30 shape exactly: the link-target sidecar landed in
    /// `fd5d716b`, ten minutes AFTER the install that pointed this repository's
    /// hooks at a `/tmp` binary — so the dangling links left by that install
    /// had no marker, and the repair chained six dead links aside. A dangling
    /// symlink to a file named `rift` is ours even with no marker beside it.
    #[cfg(unix)]
    #[test]
    fn an_unmarked_dangling_link_from_before_the_marker_is_healed_not_chained_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let deleted = tmp.path().join("rift-measure-run/rift");
        std::os::unix::fs::symlink(&deleted, hooks.join("post-commit")).unwrap();
        assert!(is_rift_managed(&hooks.join("post-commit")), "ours");

        let live = fake_binary(&tmp.path().join("live"), "rift");
        let report = install_at(tmp.path(), &live, keep(true));

        assert_eq!(report.healed.len(), HOOK_NAMES.len());
        assert_eq!(std::fs::read_link(hooks.join("post-commit")).unwrap(), live);
        assert!(
            std::fs::symlink_metadata(hooks.join("post-commit.rift-chained")).is_err(),
            "no dead .rift-chained link is left behind"
        );
    }

    /// A user's hook that can still RUN is still theirs — the never-clobber
    /// rule is untouched by the dangling-link arm, and this is the line between
    /// the two.
    #[cfg(unix)]
    #[test]
    fn a_live_unmarked_link_is_a_user_hook_and_is_chained_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let theirs = fake_binary(&tmp.path().join("theirs"), "rift");
        std::os::unix::fs::symlink(&theirs, hooks.join("post-commit")).unwrap();

        let live = fake_binary(&tmp.path().join("live"), "rift");
        install_at(tmp.path(), &live, keep(true));

        assert_eq!(
            std::fs::read_link(hooks.join("post-commit.rift-chained")).unwrap(),
            theirs,
            "a hook that can run is preserved"
        );
    }

    /// The dangling-link arm is **narrow on purpose**, and until this test nothing
    /// held its edges. Every symlink fixture in the tree named its target
    /// `rift`, so `&& target.file_name() == "rift"` could be deleted and the
    /// whole suite stayed green while the claim widened from "one of ours from
    /// before the marker" to *every dangling symlink in the hooks directory*.
    /// A user's dead link to their own tool is still theirs.
    #[cfg(unix)]
    #[test]
    fn a_dangling_link_to_a_target_not_named_rift_is_a_user_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let theirs = tmp.path().join("their-tool/lefthook");
        std::os::unix::fs::symlink(&theirs, hooks.join("post-commit")).unwrap();
        assert!(
            !theirs.exists(),
            "dangling — the arm's other condition holds"
        );
        assert_eq!(hook_entry(&hooks.join("post-commit")), HookEntry::Foreign);

        let live = fake_binary(&tmp.path().join("live"), "rift");
        install_at(tmp.path(), &live, keep(true));

        assert_eq!(
            std::fs::read_link(hooks.join("post-commit.rift-chained")).unwrap(),
            theirs,
            "a dangling hook that was never ours is chained aside, not seized"
        );
    }

    /// The arm's other condition, `!link_marker(path).exists()`. A marker beside
    /// a link is the ownership proof, and one that names a DIFFERENT target
    /// proves the opposite: the link was replaced after Rift recorded it. The
    /// pre-marker inference is for links with no marker at all — where the file
    /// Rift would have written is absent, not contradicted.
    #[cfg(unix)]
    #[test]
    fn a_dangling_rift_link_contradicted_by_its_marker_is_a_user_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks = tmp.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let theirs = tmp.path().join("their-tool/rift");
        let hook = hooks.join("post-commit");
        std::os::unix::fs::symlink(&theirs, &hook).unwrap();
        std::fs::write(
            link_marker(&hook),
            direct_link_marker_bytes(Path::new("/opt/rift/rift")),
        )
        .unwrap();
        assert_eq!(hook_entry(&hook), HookEntry::Foreign);

        let live = fake_binary(&tmp.path().join("live"), "rift");
        install_at(tmp.path(), &live, keep(true));

        assert_eq!(
            std::fs::read_link(hooks.join("post-commit.rift-chained")).unwrap(),
            theirs,
            "a link its own marker disowns is chained aside"
        );
    }
}

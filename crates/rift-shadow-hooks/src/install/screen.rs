//! The transient screen: whether a path is durable enough for a family's hooks
//! to be pointed at it.
//!
//! Its own module because the three refusals are one idea — a path that
//! outlives the decision to install by less than the family does — and because
//! the executable-file probe beside them is the health question every other
//! part of the install asks about a binary it did not choose.

use std::path::{Path, PathBuf};

use super::{FamilySites, TransientReason};

/// The temporary roots a family's binary may not live under.
///
/// `/var/folders` **and** `/private/var/folders` are both listed on purpose:
/// macOS's `$TMPDIR` is reached through a `/var` symlink, so the same file has
/// two spellings and only one of them survives canonicalization. The screen
/// runs against both the path as offered and its canonical form, so a binary
/// that exists is caught by its real location and one that does not is still
/// caught by its name.
fn temp_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/var/folders"),
        PathBuf::from("/private/var/folders"),
    ];
    let tmp = std::env::temp_dir();
    if let Ok(canonical) = tmp.canonicalize() {
        roots.push(canonical);
    }
    roots.push(tmp);
    roots
}

/// Whether `path` sits under a `<profile>/deps/` or `<profile>/build/`
/// directory.
///
/// The `target` component is deliberately not required: `CARGO_TARGET_DIR`
/// renames it, and what makes the path transient is the directory itself,
/// whose contents carry a content hash that changes on every rebuild.
/// `deps/` is where cargo has always put test binaries; `build/` was always
/// build-script territory and is ALSO where the 1.100-era build-dir layout
/// runs test binaries from (`<profile>/build/<name>/<hash>/out/<name>-<hash>`)
/// — found live on 2026-09-01, when a doctor --fix under the coverage harness
/// pointed a sandbox family at exactly that shape and
/// `a_doctor_fix_refuses_to_point_the_family_at_a_transient_binary` went red.
fn is_build_artifact(path: &Path) -> bool {
    let names: Vec<&std::ffi::OsStr> = path.iter().collect();
    names.windows(2).any(|pair| {
        pair.first()
            .is_some_and(|p| *p == "debug" || *p == "release")
            && pair.get(1).is_some_and(|p| *p == "deps" || *p == "build")
    })
}

/// Whether a family may be pointed at `path` — and if not, why.
///
/// All three refusals are the same shape: a path that outlives the *decision*
/// to install by less than the family does. The screen runs over the
/// **canonicalized** path as well as the offered one, because a binary reached
/// through `/tmp` (a symlink to `/private/tmp` on macOS) is the same file
/// either way, and the session cache already records that equivalence.
///
/// `None` means "not transient", which is the only answer that lets an install
/// through without `--allow-transient`.
#[must_use]
pub fn is_transient(path: &Path, family: &FamilySites) -> Option<TransientReason> {
    let canonical = path.canonicalize().ok();
    let mut spellings: Vec<&Path> = vec![path];
    if let Some(canonical) = canonical.as_deref() {
        spellings.push(canonical);
    }
    for spelling in spellings {
        if temp_roots().iter().any(|root| spelling.starts_with(root)) {
            return Some(TransientReason::TempRoot);
        }
        if is_build_artifact(spelling) {
            return Some(TransientReason::BuildArtifact);
        }
        for member in &family.linked_work_trees {
            if spelling.starts_with(member) {
                return Some(TransientReason::LinkedMember);
            }
            if member
                .canonicalize()
                .is_ok_and(|member| spelling.starts_with(member))
            {
                return Some(TransientReason::LinkedMember);
            }
        }
    }
    None
}

/// Whether `path` is a file this machine can execute — what a hook target has
/// to be for git to run it, and the health question `binary.json` answers.
#[must_use]
pub fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    // Windows has no execute bit; git runs a hook through its bundled `sh`.
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

/// Whether `path` names an entry of `hooks_dir` itself.
///
/// Only the directories are canonicalized, never the entry: a hook entry is a
/// link to the binary, so resolving it would answer with the binary's own
/// directory and let exactly this path through.
pub(super) fn is_in_hooks_dir(path: &Path, hooks_dir: &Path) -> bool {
    let canonical = |dir: &Path| dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    path.parent()
        .is_some_and(|parent| canonical(parent) == canonical(hooks_dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::tests::fake_binary;

    #[test]
    fn is_transient_names_the_three_shapes_and_allows_the_main_worktree() {
        let member = PathBuf::from("/Users/dev/rift-worktrees/swt-b");
        let family = FamilySites {
            dir: PathBuf::from("/Users/dev/repo/.git/rift-family"),
            linked_work_trees: vec![member.clone()],
        };
        let cases = [
            ("/tmp/rift-measure-run.42/rift", TransientReason::TempRoot),
            ("/private/tmp/rift", TransientReason::TempRoot),
            ("/var/folders/x9/T/rift", TransientReason::TempRoot),
            ("/private/var/folders/x9/T/rift", TransientReason::TempRoot),
            (
                "/Users/dev/repo/target/debug/deps/rift-3f2a",
                TransientReason::BuildArtifact,
            ),
            (
                "/Users/dev/repo/target/release/deps/rift-3f2a",
                TransientReason::BuildArtifact,
            ),
            // The 1.100-era build-dir layout: test binaries run from
            // `<profile>/build/<name>/<hash>/out/<name>-<hash>` — the shape a
            // doctor --fix under the coverage harness pointed a sandbox
            // family at before `build` joined the screen.
            (
                "/Users/dev/repo/target/llvm-cov-target/debug/build/rift/dd1d/out/rift-dd1d",
                TransientReason::BuildArtifact,
            ),
            (
                "/Users/dev/repo/target/debug/build/rift-abc123/out/rift",
                TransientReason::BuildArtifact,
            ),
            (
                "/Users/dev/rift-worktrees/swt-b/target/debug/rift",
                TransientReason::LinkedMember,
            ),
        ];
        for (path, reason) in cases {
            assert_eq!(
                is_transient(Path::new(path), &family),
                Some(reason),
                "{path}"
            );
        }
        // The developer's own build: the MAIN work tree's `target/`, which is
        // not a member and is deliberately not screened.
        assert_eq!(
            is_transient(Path::new("/Users/dev/repo/target/debug/rift"), &family),
            None
        );
        assert_eq!(
            is_transient(Path::new("/usr/local/bin/rift"), &family),
            None
        );
        // A member that is not this family's is not this family's problem.
        assert_eq!(
            is_transient(
                Path::new("/Users/dev/other/target/debug/rift"),
                &FamilySites::new(family.dir.clone())
            ),
            None
        );
    }

    #[test]
    fn the_screen_reads_a_temp_path_through_its_canonical_form() {
        // `/tmp` is a symlink to `/private/tmp` on macOS: the same file has two
        // spellings and the screen has to refuse both.
        let tmp = tempfile::tempdir().unwrap();
        let bin = fake_binary(tmp.path(), "rift");
        let family = FamilySites::new(tmp.path().join("rift-family"));
        assert_eq!(
            is_transient(&bin, &family),
            Some(TransientReason::TempRoot),
            "{}",
            bin.display()
        );
        assert_eq!(
            is_transient(&bin.canonicalize().unwrap(), &family),
            Some(TransientReason::TempRoot)
        );
    }
}

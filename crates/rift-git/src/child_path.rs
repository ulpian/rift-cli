//! The spelling of a path that a child process gets.
//!
//! Everything Rift mints, compares or stores is keyed by the output of
//! [`std::fs::canonicalize`], which on Windows carries the `\\?\` verbatim
//! prefix. Git for Windows refuses that spelling in `GIT_DIR` and `--git-dir`
//! (`fatal: not a git repository`, exit 128), so a canonical path must be
//! re-spelled at the one moment it leaves the process for a child — and only
//! there. Nothing here changes what a family id hashes or what a manifest
//! records; see the crate docs for why those keep the canonical bytes.

use std::path::{Path, PathBuf};

/// The spelling of `path` to hand to a child process such as `git`.
///
/// On Windows a verbatim disk path (`\\?\C:\…`) becomes `C:\…` and a verbatim
/// UNC path (`\\?\UNC\server\share\…`) becomes `\\server\share\…`; any other
/// spelling is returned unchanged. The verbatim form is kept when the plain
/// one would not round-trip: a path at or past the classic 260-character
/// limit, or one whose components a plain spelling would resolve differently
/// (`.` or `..`, which canonical output never contains). On every other
/// platform this is the identity.
#[must_use]
pub fn child_process_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        unverbatim(path).unwrap_or_else(|| path.to_path_buf())
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// The spelling of `path` for a report a person reads: the same plain form
/// a child process gets, under a name that says what it is for.
///
/// Display only. A report string (`rift doctor`'s `queue_dir`, `registered …
/// at`) is read by a person and pasted into a shell, where the verbatim
/// prefix is noise; the identity behind it — what is minted, compared or
/// stored — keeps the canonical bytes and never passes through here.
#[must_use]
pub fn display_path(path: &Path) -> PathBuf {
    child_process_path(path)
}

/// The longest plain spelling Win32 accepts without the verbatim prefix,
/// counted as `MAX_PATH` minus the terminating NUL. Measured in bytes, which
/// never under-counts characters, so the bound is conservative.
#[cfg(windows)]
const MAX_PLAIN_PATH_CHARS: usize = 259;

/// `Some(plain)` when `path` is verbatim and its plain spelling round-trips.
#[cfg(windows)]
fn unverbatim(path: &Path) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::path::{Component, Prefix};

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return None;
    };
    let mut plain = OsString::new();
    match prefix.kind() {
        Prefix::VerbatimDisk(letter) => {
            plain.push(char::from(letter).to_string());
            plain.push(":\\");
        }
        Prefix::VerbatimUNC(server, share) => {
            plain.push("\\\\");
            plain.push(server);
            plain.push("\\");
            plain.push(share);
            plain.push("\\");
        }
        _ => return None,
    }
    let mut out = PathBuf::from(plain);
    for component in components {
        match component {
            Component::RootDir => {}
            Component::Normal(part) => out.push(part),
            _ => return None,
        }
    }
    (out.as_os_str().len() <= MAX_PLAIN_PATH_CHARS).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain path is handed on as it is, on every platform.
    #[test]
    fn a_plain_path_is_unchanged() {
        let plain = Path::new("some").join("where");
        assert_eq!(child_process_path(&plain), plain);
    }

    #[cfg(windows)]
    #[test]
    fn a_verbatim_disk_path_loses_its_prefix() {
        assert_eq!(
            child_process_path(Path::new(r"\\?\C:\repo\.git")),
            PathBuf::from(r"C:\repo\.git")
        );
        assert_eq!(
            child_process_path(Path::new(r"\\?\C:\")),
            PathBuf::from(r"C:\")
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_verbatim_unc_path_becomes_a_plain_share() {
        assert_eq!(
            child_process_path(Path::new(r"\\?\UNC\host\share\repo")),
            PathBuf::from(r"\\host\share\repo")
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_plain_windows_path_and_a_device_path_are_unchanged() {
        for spelling in [
            r"C:\repo",
            r"\\host\share\repo",
            r"\\.\pipe\x",
            r"\\?\pipe\x",
        ] {
            assert_eq!(
                child_process_path(Path::new(spelling)),
                PathBuf::from(spelling)
            );
        }
    }

    /// The plain spelling is kept only where it round-trips: a path past the
    /// classic limit and one with a `..` stay verbatim.
    #[cfg(windows)]
    #[test]
    fn a_path_the_plain_form_cannot_carry_stays_verbatim() {
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(child_process_path(Path::new(&long)), PathBuf::from(&long));
        let dotted = r"\\?\C:\repo\..\other";
        assert_eq!(child_process_path(Path::new(dotted)), PathBuf::from(dotted));
    }

    /// The canonical spelling of a real repository is what the family plane
    /// stores; the spelling git gets must open it. This is the defect: the
    /// verbatim `GIT_DIR` was refused, so every best-effort git call from a
    /// family member silently failed.
    #[cfg(windows)]
    #[test]
    fn git_opens_the_canonical_repository_through_the_child_spelling() {
        let tmp = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        let canonical = std::fs::canonicalize(tmp.path()).unwrap();
        assert!(
            canonical.to_string_lossy().starts_with(r"\\?\"),
            "premise: canonical output is verbatim here: {}",
            canonical.display()
        );
        let repo = crate::discover(&canonical).unwrap();
        assert!(repo.git_dir.to_string_lossy().starts_with(r"\\?\"));
        let (ok, out) = crate::run_git_status(
            &repo,
            &["rev-parse".to_owned(), "--git-dir".to_owned()],
            &[],
            &[],
        )
        .unwrap();
        assert!(ok, "git refused the repository");
        let answered = String::from_utf8_lossy(&out).trim().to_owned();
        assert_eq!(
            std::fs::canonicalize(&answered).unwrap(),
            repo.git_dir,
            "git opened some other repository: {answered}"
        );
    }
}

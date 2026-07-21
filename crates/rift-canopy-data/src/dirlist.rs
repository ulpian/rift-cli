//! The destination explorer's directory walk (ADR-0052 §3, ADR-0009).
//!
//! `rift-canopy` performs no filesystem I/O on the interactive path, so the
//! actual `read_dir` lives here, in the data plane, and crosses back to the
//! reducer as an infallible [`DirListing`] through the lister seam. The crate
//! already depends on `rift-canopy`, so the types flow with **no new
//! crate-graph edge**.
//!
//! The walker's contract is four promises:
//!
//! - **Total** — it never panics and never throws; a `read_dir` failure folds
//!   into `error` and an unreadable single entry is skipped, never fabricated
//!   (the `rift_git::list_worktrees` degradation discipline).
//! - **Deterministic** — one comparator, [`rift_canopy::explorer::sort_entries`],
//!   imported from the state module so the two sides of the seam cannot drift.
//! - **Bounded** — immediate children only, capped at
//!   [`rift_canopy::explorer::MAX_ENTRIES`].
//! - **Honest** — truncation is counted in `omitted`, never silent.

use std::path::Path;

use rift_canopy::explorer::{sort_entries, DirListing, ExplorerEntry, MAX_ENTRIES};

/// List one directory's immediate children for the destination explorer.
///
/// Symlinks are classified by their **target** (one `metadata()` call), so a
/// link to a directory is a directory the operator can enter; that is
/// classification only — the store re-canonicalizes at registration, and we
/// never recurse, so a symlink cycle is harmless.
#[must_use]
pub fn list_dir(path: &Path) -> DirListing {
    let read = match std::fs::read_dir(path) {
        Ok(read) => read,
        Err(e) => {
            return DirListing {
                path: path.to_path_buf(),
                entries: Vec::new(),
                omitted: 0,
                error: Some(e.to_string()),
            }
        }
    };
    let mut entries = Vec::new();
    for entry in read.flatten() {
        // A symlink's own file_type says "symlink"; the TARGET decides whether
        // it is enterable. An unresolvable target is simply not a directory.
        let is_dir = match entry.file_type() {
            Ok(ft) if ft.is_symlink() => std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir()),
            Ok(ft) => ft.is_dir(),
            // An entry we cannot stat is listed as a plain (inert) row rather
            // than dropped — it exists, we just cannot enter it.
            Err(_) => false,
        };
        entries.push(ExplorerEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir,
        });
    }
    sort_entries(&mut entries);
    let omitted = entries.len().saturating_sub(MAX_ENTRIES);
    entries.truncate(MAX_ENTRIES);
    DirListing {
        path: path.to_path_buf(),
        entries,
        omitted,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mixed_directory_lists_dirs_first_and_keeps_dot_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("B")).unwrap();
        std::fs::create_dir(root.join("a")).unwrap();
        std::fs::create_dir(root.join(".hidden")).unwrap();
        std::fs::write(root.join("zz.txt"), b"x").unwrap();

        let listing = list_dir(root);
        assert!(listing.error.is_none());
        assert_eq!(listing.path, root);
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [".hidden", "a", "B", "zz.txt"],
            "dirs first, then case-insensitive by name"
        );
        assert!(
            listing.entries.iter().any(|e| e.name == ".hidden"),
            "dot-entries are listed — filtering is the state's read-time job"
        );
        assert_eq!(listing.omitted, 0, "a small directory is complete");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_directory_counts_as_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("real")).unwrap();
        std::fs::write(root.join("file.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        std::os::unix::fs::symlink(root.join("file.txt"), root.join("flink")).unwrap();

        let listing = list_dir(root);
        let by_name = |n: &str| {
            listing
                .entries
                .iter()
                .find(|e| e.name == n)
                .map(|e| e.is_dir)
        };
        assert_eq!(by_name("link"), Some(true), "the target decides");
        assert_eq!(by_name("flink"), Some(false));
        assert_eq!(by_name("real"), Some(true));
    }

    #[cfg(unix)]
    #[test]
    fn a_broken_symlink_is_listed_as_a_plain_row_never_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::os::unix::fs::symlink(root.join("gone"), root.join("dangling")).unwrap();
        let listing = list_dir(root);
        assert_eq!(
            listing.entries,
            vec![ExplorerEntry {
                name: "dangling".to_owned(),
                is_dir: false
            }],
            "an unresolvable target is inert, not invented and not hidden"
        );
        assert!(
            listing.error.is_none(),
            "one bad entry is not a bad listing"
        );
    }

    #[test]
    fn an_unreadable_path_folds_into_the_error_and_never_throws() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let listing = list_dir(&missing);
        assert!(listing.error.is_some(), "the failure is folded, not thrown");
        assert!(listing.entries.is_empty());
        assert_eq!(listing.omitted, 0);
        assert_eq!(listing.path, missing, "the reply names what was asked for");
    }

    #[test]
    fn a_huge_directory_is_capped_and_discloses_the_overflow() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let total = MAX_ENTRIES + 3;
        for i in 0..total {
            std::fs::write(root.join(format!("f{i:04}")), b"x").unwrap();
        }
        let listing = list_dir(root);
        assert_eq!(listing.entries.len(), MAX_ENTRIES, "the cap holds");
        assert_eq!(listing.omitted, 3, "truncation is counted, never silent");
    }

    #[test]
    fn an_empty_directory_lists_nothing_without_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let listing = list_dir(tmp.path());
        assert!(listing.entries.is_empty());
        assert!(listing.error.is_none(), "empty is not a failure");
    }
}

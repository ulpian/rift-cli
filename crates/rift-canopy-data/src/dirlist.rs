//! The destination explorer's directory walk (the destination explorer, the data-plane split).
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

use rift_canopy::explorer::{sort_entries, DirListing, EntryTag, ExplorerEntry, MAX_ENTRIES};

/// `FILE_ATTRIBUTE_DIRECTORY` — the entry is a directory.
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

/// `FILE_ATTRIBUTE_REPARSE_POINT` — a junction, symbolic link or mount point.
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// The shape of one entry as its Windows attributes describe it — pure bit
/// arithmetic, so the classification is tested on every platform even though
/// only Windows ever reads the attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryShape {
    /// A plain file (or a file symlink).
    File,
    /// A plain directory.
    Dir,
    /// A directory that is also a reparse point: a junction or a directory
    /// symlink, whose listing Windows may refuse regardless of the target.
    ReparseDir,
}

/// Read the shape off the attribute bits.
pub(crate) const fn shape_from_attributes(attrs: u32) -> EntryShape {
    if attrs & FILE_ATTRIBUTE_DIRECTORY == 0 {
        EntryShape::File
    } else if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        EntryShape::ReparseDir
    } else {
        EntryShape::Dir
    }
}

/// A reparse-point directory's row: enterable exactly when Windows let us list
/// it, and tagged either way so the operator knows the name is a junction.
///
/// Attributes alone cannot tell a listable junction from an ACL-denied one
/// (both carry the same bits), so the walker probes each with one `read_dir`
/// — a handle open per junction, of which a profile root holds a handful, and
/// [`MAX_ENTRIES`] bounds the rest.
pub(crate) const fn classify_reparse_dir(listable: bool) -> (bool, Option<EntryTag>) {
    (listable, Some(EntryTag::Junction))
}

/// The entry's own attributes (the directory listing's, never the target's).
#[cfg(windows)]
fn entry_attributes(entry: &std::fs::DirEntry) -> Option<u32> {
    use std::os::windows::fs::MetadataExt as _;
    entry.metadata().ok().map(|m| m.file_attributes())
}

/// Off Windows there are no attribute bits; the symlink rule below decides.
#[cfg(not(windows))]
fn entry_attributes(_entry: &std::fs::DirEntry) -> Option<u32> {
    None
}

/// List one directory's immediate children for the destination explorer.
///
/// Symlinks are classified by their **target** (one `metadata()` call), so a
/// link to a directory is a directory the operator can enter; that is
/// classification only — the store re-canonicalizes at registration, and we
/// never recurse, so a symlink cycle is harmless. On Windows a reparse-point
/// directory (a junction) is enterable only if its listing is not refused, and
/// carries [`EntryTag::Junction`] either way.
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
        let (is_dir, tag) = match entry_attributes(&entry).map(shape_from_attributes) {
            Some(EntryShape::ReparseDir) => {
                classify_reparse_dir(std::fs::read_dir(entry.path()).is_ok())
            }
            Some(EntryShape::Dir) => (true, None),
            Some(EntryShape::File) => (false, None),
            // A symlink's own file_type says "symlink"; the TARGET decides whether
            // it is enterable. An unresolvable target is simply not a directory.
            None => (
                match entry.file_type() {
                    Ok(ft) if ft.is_symlink() => {
                        std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir())
                    }
                    Ok(ft) => ft.is_dir(),
                    // An entry we cannot stat is listed as a plain (inert) row rather
                    // than dropped — it exists, we just cannot enter it.
                    Err(_) => false,
                },
                None,
            ),
        };
        entries.push(ExplorerEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir,
            tag,
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
                is_dir: false,
                tag: None,
            }],
            "an unresolvable target is inert, not invented and not hidden"
        );
        assert!(
            listing.error.is_none(),
            "one bad entry is not a bad listing"
        );
    }

    /// The attribute classification is pure bit arithmetic: the directory bit
    /// decides file vs directory, and the reparse bit only matters on a
    /// directory (a file symlink is a plain, inert row).
    #[test]
    fn shape_from_attributes_reads_directory_and_reparse_bits() {
        assert_eq!(shape_from_attributes(0x10), EntryShape::Dir);
        assert_eq!(
            shape_from_attributes(0x20),
            EntryShape::File,
            "archive bit only"
        );
        assert_eq!(shape_from_attributes(0x10 | 0x400), EntryShape::ReparseDir);
        assert_eq!(
            shape_from_attributes(0x400),
            EntryShape::File,
            "a file reparse point"
        );
        assert_eq!(
            shape_from_attributes(0x10 | 0x2 | 0x4),
            EntryShape::Dir,
            "hidden+system"
        );
    }

    /// A junction Windows refuses to list is inert; one it lists is enterable;
    /// both carry the tag so the row says what the name is.
    #[test]
    fn a_reparse_dir_is_enterable_exactly_when_listable_and_always_tagged() {
        assert_eq!(classify_reparse_dir(true), (true, Some(EntryTag::Junction)));
        assert_eq!(
            classify_reparse_dir(false),
            (false, Some(EntryTag::Junction))
        );
    }

    /// The real attribute read carries the directory bit for a directory and
    /// not for a file — pinning the constants against the OS on the Windows
    /// CI leg without needing the privilege a junction creation would.
    #[cfg(windows)]
    #[test]
    fn real_attributes_carry_the_directory_bit() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("d")).unwrap();
        std::fs::write(tmp.path().join("f"), b"x").unwrap();
        for entry in std::fs::read_dir(tmp.path()).unwrap().flatten() {
            let shape = entry_attributes(&entry).map(shape_from_attributes);
            let name = entry.file_name().to_string_lossy().into_owned();
            match name.as_str() {
                "d" => assert_eq!(shape, Some(EntryShape::Dir), "{name}"),
                "f" => assert_eq!(shape, Some(EntryShape::File), "{name}"),
                other => panic!("unexpected entry {other}"),
            }
        }
        // And the listing itself classifies them the same way, untagged.
        let listing = list_dir(tmp.path());
        let dir = listing.entries.iter().find(|e| e.name == "d").unwrap();
        assert!(dir.is_dir && dir.tag.is_none());
        let file = listing.entries.iter().find(|e| e.name == "f").unwrap();
        assert!(!file.is_dir && file.tag.is_none());
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

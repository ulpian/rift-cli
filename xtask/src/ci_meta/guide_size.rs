//! The written guide's size ceiling.
//!
//! The guide is read whole by people deciding whether to trust the toolchain,
//! so its length is a property worth holding. The measure is the byte length
//! of every tracked file under `docs/guide/`, read from the working tree at
//! the index's paths: an untracked draft does not count, and block sizes do
//! not either, so the number is the same on every filesystem.
use std::path::Path;

use super::tracked_paths;

/// The most bytes the tracked guide may hold: 400 KiB.
///
/// The guide describes the finished product within 400 KB. A chapter that
/// grows past this either replaces older text or makes the case for raising
/// the limit in the same diff, where a reviewer sees it.
pub const GUIDE_BYTE_CEILING: u64 = 409_600;

/// The directory the ceiling measures.
const GUIDE_DIR: &str = "docs/guide/";

/// The tracked guide holds at most [`GUIDE_BYTE_CEILING`] bytes.
///
/// Returns `(total bytes, violations)`.
///
/// # Errors
/// If `git ls-files` fails, a tracked guide file cannot be read, or the index
/// holds no guide file at all — a ceiling over nothing guards nothing.
pub fn guide_within_ceiling(root: &Path) -> Result<(u64, Vec<String>), String> {
    let guide: Vec<String> = tracked_paths(root)?
        .into_iter()
        .filter(|rel| rel.starts_with(GUIDE_DIR))
        .collect();
    measure(root, &guide, GUIDE_BYTE_CEILING)
}

/// The ceiling over an explicit path list — the seam the fixtures use.
fn measure(root: &Path, paths: &[String], ceiling: u64) -> Result<(u64, Vec<String>), String> {
    if paths.is_empty() {
        return Err(format!(
            "guide size: the index holds no file under {GUIDE_DIR}"
        ));
    }
    let mut total = 0_u64;
    for rel in paths {
        let metadata = std::fs::metadata(root.join(rel))
            .map_err(|e| format!("read tracked guide file {rel}: {e}"))?;
        total += metadata.len();
    }
    if total <= ceiling {
        return Ok((total, Vec::new()));
    }
    Ok((
        total,
        vec![format!(
            "{GUIDE_DIR}: the tracked guide is {total} bytes, over its {ceiling}-byte ceiling — \
             cut or replace text rather than raise the limit"
        )],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_meta::tests::scratch_dir;
    use crate::workspace_root;

    /// Two guide files of the given sizes in a fresh fixture root.
    fn planted(name: &str, sizes: [usize; 2]) -> (std::path::PathBuf, Vec<String>) {
        let root = scratch_dir(name);
        std::fs::create_dir_all(root.join(GUIDE_DIR)).unwrap();
        let mut paths = Vec::new();
        for (index, size) in sizes.into_iter().enumerate() {
            let rel = format!("{GUIDE_DIR}{index:02}-chapter.md");
            std::fs::write(root.join(&rel), vec![b'x'; size]).unwrap();
            paths.push(rel);
        }
        (root, paths)
    }

    #[test]
    fn a_guide_over_the_ceiling_is_one_violation_naming_both_numbers() {
        let (root, paths) = planted("guide-size-over", [60, 41]);
        let (total, violations) = measure(&root, &paths, 100).expect("the check runs");
        assert_eq!(total, 101);
        assert_eq!(violations.len(), 1, "{violations:?}");
        let only = violations.first().unwrap();
        assert!(
            only.contains("101 bytes") && only.contains("100-byte"),
            "{only}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_guide_exactly_at_the_ceiling_passes() {
        let (root, paths) = planted("guide-size-at", [60, 40]);
        let (total, violations) = measure(&root, &paths, 100).expect("the check runs");
        assert_eq!(total, 100);
        assert!(violations.is_empty(), "{violations:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_guide_under_the_ceiling_passes() {
        let (root, paths) = planted("guide-size-under", [10, 5]);
        let (total, violations) = measure(&root, &paths, 100).expect("the check runs");
        assert_eq!(total, 15);
        assert!(violations.is_empty(), "{violations:?}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_scan_that_found_no_guide_file_is_an_error() {
        assert!(measure(&workspace_root(), &[], 100).is_err());
    }

    #[test]
    fn the_live_guide_is_within_its_ceiling() {
        let (total, violations) = guide_within_ceiling(&workspace_root()).expect("the check runs");
        assert!(violations.is_empty(), "{violations:?}");
        assert!(total > 0);
    }
}

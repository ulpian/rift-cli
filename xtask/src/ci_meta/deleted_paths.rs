//! The screen that keeps owner-deleted paths deleted — ignored by
//! `.gitignore` and absent from both the index and the disk.
use std::path::Path;

use super::read;

/// The paths an owner decision deleted stay deleted.
///
/// This machine's iCloud sync RESURRECTS them on disk: the first four were
/// observed untracked-present in a sibling worktree on 2026-08-18, with the
/// wrapper crate carrying `Cargo.toml`, `pinned-substrate.toml` and
/// `src/{fetch,lib,pin}.rs` — the closure commit `94a1426` deleted.
/// `.gitignore` stops a bulk `git add -A` re-adding them, but ignoring ALONE
/// would only hide the resurrection, so this asserts BOTH halves: every path
/// is ignored, and no path is tracked. A `git add -f` that re-adds one reds
/// here.
///
/// `crates/rift-fff/` and `crates/rift-codegraph/` join the list under a
/// second owner decision (2026-08-20), and they are the reason the list is not
/// wrapper-specific: both had been ruled deleted once already, and both came
/// BACK as manifest-only scaffolds with an empty `src/` — staged, not merely
/// present. Nothing in `crates/`, `bins/`, `xtask/`, `ci/`, `.github/` or the
/// root manifest ever referenced them, so no compile would have complained.
/// The one thing that WOULD have complained is
/// [`crate::check_members_match_dirs`], which walks the FILESYSTEM — and it
/// was already red in the primary checkout while every linked worktree passed,
/// because an untracked directory does not exist in a linked worktree. That
/// asymmetry is exactly why the ignore half and the absence half are both
/// needed: the ignore stops the re-add, the absence check is what a gate run
/// from a linked worktree can still see.
///
/// A declared weakness: the ignore half looks for the literal line, so a later
/// negation (`!/crates/rift-fff/`) would re-enable the bulk add while this
/// check still saw its entry. The pair survives it — the ABSENCE half is what
/// actually reds on a re-add, and a negation cannot help there — but the
/// ignore half alone is weaker than it reads.
///
/// Returns `(paths examined, violations)`. The count is not decoration: a
/// caller can refuse a screen that examined nothing, so emptying `DELETED`
/// is visible instead of quietly turning this check into a no-op.
///
/// # Errors
/// If `.gitignore` cannot be read or `git ls-files` cannot be run.
pub fn deleted_paths_stay_deleted(root: &Path) -> Result<(usize, Vec<String>), String> {
    // The boolean says whether the path must also be ignored. The two retired
    // branded paths stay visible because joining their token in `.gitignore`
    // would violate the zero-token tree contract; this guard constructs them
    // from pieces and still rejects either tracked or untracked resurrection.
    const DELETED: &[(&str, bool)] = &[
        (concat!("crates/rift-shadow-", "ent", "ire", "/"), false),
        ("xtask/src/pin_cut.rs", true),
        (".rift/rewrite.toml", true),
        (concat!("tests/fixtures/fake-", "ent", "ire", "/"), false),
        ("crates/rift-fff/", true),
        ("crates/rift-codegraph/", true),
        (".mcp.json", true),
        ("bins/rift-memd/src/mcp_serve.rs", true),
        // The document helper binary: its parsers moved into `rift`, which
        // re-spawns itself on `hooks doc` instead of looking for a sibling.
        ("bins/rift-doc/", true),
    ];
    let ignore = read(root, ".gitignore")?;
    let mut v = Vec::new();
    for &(path, must_be_ignored) in DELETED {
        let ignored = ignore
            .lines()
            .any(|l| l.trim().strip_prefix('/') == Some(path));
        if must_be_ignored && !ignored {
            v.push(format!(
                ".gitignore: `/{path}` is missing — a bulk `git add -A` would re-add a deleted path"
            ));
        }
        if root.join(path.trim_end_matches('/')).exists() {
            v.push(format!(
                "present: `{path}` was deleted by owner decision and exists again"
            ));
        }
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("ls-files")
        .arg("--")
        .args(DELETED.iter().map(|(path, _)| path))
        .output()
        .map_err(|e| format!("git ls-files (deleted-path absence check): {e}"))?;
    let tracked = String::from_utf8_lossy(&out.stdout);
    for line in tracked.lines().map(str::trim).filter(|l| !l.is_empty()) {
        v.push(format!(
            "tracked: `{line}` was deleted by owner decision and is tracked again"
        ));
    }
    Ok((DELETED.len(), v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_meta::tests::scratch_dir;

    /// Both halves plant: a deleted path that exists on disk again, and a
    /// `.gitignore` that no longer stops a bulk add from re-adding it. The
    /// screen passes on the real tree only because the paths are gone, and
    /// this is what says so.
    #[test]
    fn a_resurrected_deleted_path_and_a_missing_ignore_are_both_violations() {
        let root = scratch_dir("deleted-paths");
        std::fs::create_dir_all(root.join(".rift")).unwrap();
        std::fs::write(root.join(".rift").join("rewrite.toml"), "x = 1\n").unwrap();
        std::fs::write(root.join(".gitignore"), "/xtask/src/pin_cut.rs\n").unwrap();

        let (examined, violations) = deleted_paths_stay_deleted(&root).expect("the screen runs");
        assert!(examined > 0, "a screen over no paths guards nothing");
        assert!(
            violations.iter().any(|v| v
                == ".gitignore: `/.rift/rewrite.toml` is missing — a bulk `git add -A` would re-add a deleted path"),
            "{violations:?}"
        );
        assert!(
            violations.iter().any(|v| v
                == "present: `.rift/rewrite.toml` was deleted by owner decision and exists again"),
            "{violations:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}

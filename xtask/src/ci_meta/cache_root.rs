//! The screen that keeps the `rift` binary's cache root in one function.
//!
//! The binary's unit tests run in one process with the developer's real
//! environment, and they once wrote thousands of stores into the real cache
//! root. What keeps them out now is `family::cache_root`: in a test build it
//! answers one temp directory per test process. That holds only while every
//! state path the binary derives starts there, so this screen refuses every
//! other spelling that resolves the root from the process environment in the
//! crate's production code.
use std::path::Path;

use super::ceilings::{line_of, line_starts, masked, tracked_rust_paths};
use super::production_half;

/// The crate whose production code the screen reads.
const CRATE: &str = "bins/rift/src/";

/// The file holding the one resolver, and the one call it makes.
const RESOLVER_FILE: &str = "bins/rift/src/family.rs";

/// The call the resolver itself makes, allowed once and only in
/// [`RESOLVER_FILE`].
const ROOT_FROM_ENV: &str = "resolve_cache_root(";

/// Calls that resolve the cache root from the process environment inside
/// another crate, so a test build of this crate cannot redirect them. Each
/// has a form that takes the root, which is what a call site here uses.
const RESOLVES_FROM_ENV: [&str; 10] = [
    "recall_db_for_family(",
    "projection_db_for_family(",
    "recall_db_for_repo(",
    "capture_projection_db_for_repo(",
    "connection_state_dir_for_repo(",
    "default_projection_db(",
    "default_projection_dir(",
    "default_recall_db(",
    "default_canopy_ui_db(",
    "rift_canopy_data::spawn(",
];

/// The `rift` binary resolves its cache root in `family::cache_root` alone.
///
/// Read over the production half of every tracked file under `bins/rift/src/`
/// with comments and strings masked. A trailing test module is not read: a
/// test may compare a derived path, and what it opens goes through production
/// code, which is what is read.
///
/// **What this misses.** It matches spellings, not types: a function imported
/// under another name (`use rift_canopy_data::spawn as start`), an
/// env-resolving function of another crate not listed above, or a direct read
/// of `RIFT_CACHE_DIR` is not seen. The binary's integration tests spawn a
/// process that is not a test build, so they are held by the environment each
/// spawn sets, not by this screen; `rift-canopy-data`'s read-only
/// `project_settings_view` also resolves from the environment and is not
/// listed, because it opens nothing for writing.
///
/// Returns `(files read, violations)`.
///
/// # Errors
/// If `git ls-files` fails or a tracked file cannot be read.
pub fn one_cache_root_in_the_rift_binary(root: &Path) -> Result<(usize, Vec<String>), String> {
    let paths: Vec<String> = tracked_rust_paths(root)?
        .into_iter()
        .filter(|rel| rel.starts_with(CRATE))
        .collect();
    screen_files(root, &paths)
}

/// The screen over an explicit path list — the seam the planting test uses.
fn screen_files(root: &Path, paths: &[String]) -> Result<(usize, Vec<String>), String> {
    let mut violations = Vec::new();
    for rel in paths {
        let text = std::fs::read_to_string(root.join(rel))
            .map_err(|e| format!("read tracked file {rel}: {e}"))?;
        // The cut leads with the newline it pads the file with; dropping it
        // keeps line numbers the file's own.
        let half = production_half(&text);
        let half = half.strip_prefix('\n').unwrap_or(&half);
        let code = masked(half.as_bytes());
        violations.extend(screen_file(rel, &code));
    }
    Ok((paths.len(), violations))
}

/// The violations in one file's masked production half.
fn screen_file(rel: &str, code: &[u8]) -> Vec<String> {
    let starts = line_starts(code);
    let mut out = Vec::new();
    for spelling in RESOLVES_FROM_ENV {
        for at in offsets(code, spelling) {
            out.push(format!(
                "{rel}:{}: `{spelling}` resolves the cache root from the environment \
                 — take the root from `crate::family::cache_root()`",
                line_of(&starts, at)
            ));
        }
    }
    let roots = offsets(code, ROOT_FROM_ENV);
    let allowed = usize::from(rel == RESOLVER_FILE);
    for at in roots.into_iter().skip(allowed) {
        out.push(format!(
            "{rel}:{}: `{ROOT_FROM_ENV}` outside the one resolver — \
             call `crate::family::cache_root()`",
            line_of(&starts, at)
        ));
    }
    out
}

/// Every offset at which `needle` starts in `code`.
fn offsets(code: &[u8], needle: &str) -> Vec<usize> {
    let needle = needle.as_bytes();
    code.windows(needle.len())
        .enumerate()
        .filter(|(_, window)| *window == needle)
        .map(|(at, _)| at)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_meta::tests::scratch_dir;

    /// A resolver call outside the one function is reported by path and line,
    /// as is a second root resolution in the resolver's own file; a spelling
    /// in a comment, a string or the trailing test module is not.
    #[test]
    fn an_environment_resolved_cache_root_outside_the_resolver_is_reported() {
        let root = scratch_dir("cache-root-planted");
        let dir = root.join("bins/rift/src");
        std::fs::create_dir_all(&dir).unwrap();
        let leaker = [
            "// rift_store::recall_db_for_family(&family)",
            "const SPELLED: &str = \"resolve_cache_root(\";",
            "fn open(family: &Family) -> PathBuf {",
            "    rift_store::recall_db_for_family(family)",
            "}",
            "fn root() -> PathBuf {",
            "    rift_store::resolve_cache_root(|k| std::env::var(k).ok())",
            "}",
            "#[cfg(test)]",
            "mod tests {",
            "    fn compare() { rift_store::recall_db_for_repo(p); }",
            "}",
        ]
        .join("\n");
        std::fs::write(dir.join("leaker.rs"), leaker).unwrap();
        let resolver = [
            "fn cache_root_from(env: Env) -> PathBuf {",
            "    rift_store::resolve_cache_root(env)",
            "}",
            "fn second() -> PathBuf {",
            "    rift_store::resolve_cache_root(env)",
            "}",
        ]
        .join("\n");
        std::fs::write(dir.join("family.rs"), resolver).unwrap();

        let paths = [
            "bins/rift/src/leaker.rs".to_owned(),
            "bins/rift/src/family.rs".to_owned(),
        ];
        let (files, violations) = screen_files(&root, &paths).expect("the screen runs");
        assert_eq!(files, 2);
        let lines: Vec<&str> = violations
            .iter()
            .map(|v| v.split_once(": `").map_or(v.as_str(), |(at, _)| at))
            .collect();
        assert_eq!(
            lines,
            [
                "bins/rift/src/leaker.rs:4",
                "bins/rift/src/leaker.rs:7",
                "bins/rift/src/family.rs:5",
            ]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}

//! The screen that keeps the no-key column cipher out of production code.
//!
//! `rift_store::PlainCipher` writes a sensitive column as plaintext. Before
//! the per-family column key shipped, every production path passed it, and
//! the recall projection held its summary bodies in the clear. Production
//! code under `bins/` now takes its cipher from `column_key::column_cipher`,
//! and the recall crates take theirs from the caller; a production spelling
//! of `PlainCipher` in either place is a path that writes plaintext again.
use std::path::Path;

use super::ceilings::{line_of, line_starts, masked, tracked_rust_paths};
use super::production_half;

/// The type no production path may name.
const PLAIN: &str = "PlainCipher";

/// The trees whose production code the screen reads.
const SCREENED: [&str; 2] = ["bins/", "crates/rift-recall-"];

/// No `PlainCipher` in the production code of `bins/` or the recall crates.
///
/// Read over the production half of every tracked `.rs` file under those
/// trees, with comments and strings masked; a file under a `tests/` or
/// `benches/` directory, or named `tests.rs` / `*_tests.rs`, is test code
/// and is not read. The type's own definition lives in `rift-store`, which
/// is not screened.
///
/// **What this misses.** It matches the spelling, not the type: an alias
/// (`use rift_store::PlainCipher as P`) is caught at its `use`, but a
/// re-export from a crate outside the screened trees under another name is
/// not.
///
/// Returns `(files read, violations)`.
///
/// # Errors
/// If `git ls-files` fails or a tracked file cannot be read.
pub fn no_plain_cipher_in_production(root: &Path) -> Result<(usize, Vec<String>), String> {
    let paths: Vec<String> = tracked_rust_paths(root)?
        .into_iter()
        .filter(|rel| SCREENED.iter().any(|tree| rel.starts_with(tree)) && !is_test_path(rel))
        .collect();
    screen_files(root, &paths)
}

/// A path whose whole file is test code.
fn is_test_path(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    rel.contains("/tests/")
        || rel.contains("/benches/")
        || name == "tests.rs"
        || name.ends_with("_tests.rs")
}

/// The screen over an explicit path list — the seam the planting test uses.
fn screen_files(root: &Path, paths: &[String]) -> Result<(usize, Vec<String>), String> {
    let mut violations = Vec::new();
    for rel in paths {
        let text = std::fs::read_to_string(root.join(rel))
            .map_err(|e| format!("read tracked file {rel}: {e}"))?;
        let half = production_half(&text);
        let half = half.strip_prefix('\n').unwrap_or(&half);
        let code = masked(half.as_bytes());
        let starts = line_starts(&code);
        violations.extend(word_offsets(&code, PLAIN).into_iter().map(|at| {
            format!(
                "{rel}:{}: `{PLAIN}` in production code writes sensitive columns in \
                 plaintext — take the caller's cipher, or `column_key::column_cipher` \
                 in the `rift` binary",
                line_of(&starts, at)
            )
        }));
    }
    Ok((paths.len(), violations))
}

/// Every offset at which `word` appears as a whole identifier in `code`.
fn word_offsets(code: &[u8], word: &str) -> Vec<usize> {
    let needle = word.as_bytes();
    let is_ident = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
    code.windows(needle.len())
        .enumerate()
        .filter(|(at, window)| {
            *window == needle
                && !is_ident(at.checked_sub(1).and_then(|i| code.get(i)))
                && !is_ident(code.get(at + needle.len()))
        })
        .map(|(at, _)| at)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_meta::tests::scratch_dir;

    /// A production use is reported by path and line; a mention in a
    /// comment or string, a longer identifier, the trailing test module and
    /// a whole test file are not.
    #[test]
    fn a_plain_cipher_in_production_code_is_reported() {
        let root = scratch_dir("plain-cipher-planted");
        let dir = root.join("crates/rift-recall-x/src");
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        let leaker = [
            "// PlainCipher in a comment",
            "const S: &str = \"PlainCipher\";",
            "struct PlainCipherish;",
            "fn put() { write(&rift_store::PlainCipher); }",
            "#[cfg(test)]",
            "mod tests {",
            "    fn t() { write(&PlainCipher); }",
            "}",
        ]
        .join("\n");
        std::fs::write(dir.join("lib.rs"), leaker).unwrap();
        std::fs::write(dir.join("tests/fixture.rs"), "fn f() { PlainCipher; }").unwrap();

        let paths = [
            "crates/rift-recall-x/src/lib.rs".to_owned(),
            "crates/rift-recall-x/src/tests/fixture.rs".to_owned(),
        ];
        let screened: Vec<String> = paths.iter().filter(|p| !is_test_path(p)).cloned().collect();
        let (files, violations) = screen_files(&root, &screened).expect("the screen runs");
        assert_eq!(files, 1);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].starts_with("crates/rift-recall-x/src/lib.rs:4:"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn whole_test_files_are_recognised_by_path() {
        assert!(is_test_path("bins/rift/tests/e2e.rs"));
        assert!(is_test_path(
            "crates/rift-recall-serve/src/tests/outline.rs"
        ));
        assert!(is_test_path("bins/rift/src/hooks/agent/injection_tests.rs"));
        assert!(is_test_path("crates/rift-recall-x/src/tests.rs"));
        assert!(!is_test_path("bins/rift/src/column_key.rs"));
    }
}

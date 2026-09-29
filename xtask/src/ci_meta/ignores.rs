//! The screen that keeps every `#[ignore]` carrying its reason.
//!
//! An ignored test is one the required suite never runs, so the reason is
//! what tells a reader whether that is still true: a generator that writes
//! committed files, a measurement instrument, or a test that needs something
//! a CI runner cannot provide. A bare `#[ignore]` says none of those.
use std::path::Path;

use super::ceilings::{
    ident_end, line_of, line_starts, masked, skip_whitespace, tracked_rust_paths,
};

/// Every `#[ignore]` in a tracked `.rs` file carries a reason string.
///
/// `#[ignore = "…"]` passes and a bare `#[ignore]` is a violation. Comments
/// and strings are masked before the attribute is looked for, so a doc
/// comment that spells `#[ignore]`, or a fixture string that plants one, is
/// not read as an attribute.
///
/// Returns `(ignore attributes examined, violations)`, the count printed on
/// the `ci-meta` ok line.
///
/// # Errors
/// If `git ls-files` fails or a tracked file cannot be read.
pub fn every_ignore_carries_a_reason(root: &Path) -> Result<(usize, Vec<String>), String> {
    screen_files(root, &tracked_rust_paths(root)?)
}

/// The screen over an explicit path list — the seam the planting test uses.
fn screen_files(root: &Path, paths: &[String]) -> Result<(usize, Vec<String>), String> {
    let mut examined = 0;
    let mut v = Vec::new();
    for rel in paths {
        let bytes =
            std::fs::read(root.join(rel)).map_err(|e| format!("read tracked file {rel}: {e}"))?;
        let code = masked(&bytes);
        let starts = line_starts(&code);
        for (at, reasoned) in ignore_attributes(&code) {
            examined += 1;
            if !reasoned {
                v.push(format!(
                    "{rel}:{}: a bare `#[ignore]` — an `#[ignore]` carries its reason, \
                     `#[ignore = \"…\"]`",
                    line_of(&starts, at)
                ));
            }
        }
    }
    Ok((examined, v))
}

/// The offset of every `#[ignore …]` attribute in masked code, each paired
/// with whether a `=` and a reason follow the word.
fn ignore_attributes(code: &[u8]) -> Vec<(usize, bool)> {
    let mut found = Vec::new();
    for (at, _) in code.iter().enumerate().filter(|(_, byte)| **byte == b'#') {
        let bracket = skip_whitespace(code, at + 1);
        if code.get(bracket) != Some(&b'[') {
            continue;
        }
        let word = skip_whitespace(code, bracket + 1);
        let word_end = ident_end(code, word);
        if code.get(word..word_end) != Some(b"ignore") {
            continue;
        }
        let next = skip_whitespace(code, word_end);
        found.push((at, code.get(next) == Some(&b'=')));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_meta::tests::scratch_dir;

    /// A bare ignore is reported by path and line; a reasoned one, and a
    /// bare one spelled only in a comment or a string, are not.
    #[test]
    fn a_bare_ignore_is_reported_and_a_reasoned_one_passes() {
        let root = scratch_dir("ignores-planted");
        let planted = [
            "/// Kept `#[ignore]`d on purpose.",
            "#[test]",
            "#[ignore = \"needs a real OS keychain\"]",
            "fn reasoned() {}",
            "// #[ignore]",
            "const SPELLED: &str = \"#[ignore]\";",
            "#[test]",
            "# [ ignore ]",
            "fn bare() {}",
        ]
        .join("\n");
        std::fs::write(root.join("planted.rs"), planted).unwrap();

        let (examined, violations) =
            screen_files(&root, &["planted.rs".to_owned()]).expect("the screen runs");
        assert_eq!(
            violations,
            [
                "planted.rs:8: a bare `#[ignore]` — an `#[ignore]` carries its reason, \
              `#[ignore = \"…\"]`"
            ]
        );
        assert_eq!(examined, 2);
        std::fs::remove_dir_all(&root).unwrap();
    }
}

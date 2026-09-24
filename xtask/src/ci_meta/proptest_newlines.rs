//! The screen that keeps proptest string generators able to produce a
//! newline — the character this codebase turns on, and the one a `.`-based
//! regex never emits.
use std::path::Path;

use super::{read, source_files};

/// `path::test_name` for every proptest string generator in `text` whose regex
/// uses `.` without the `(?s)` flag — i.e. one that can never generate a newline.
///
/// The needle is assembled with `concat!` so this scanner never matches itself.
fn newline_blind_generators(text: &str) -> Vec<String> {
    const GENERATOR: &str = concat!("in", " \"");
    const DOT_FORMS: [&str; 3] = [concat!(".", "{"), concat!(".", "*"), concat!(".", "+")];
    let mut out = Vec::new();
    let mut current_fn = String::new();
    for raw in text.lines() {
        let line = raw.split("//").next().unwrap_or("");
        if let Some(after_fn) = line.split("fn ").nth(1) {
            if let Some(name) = after_fn.split(['(', '<', ' ']).next() {
                if !name.is_empty() {
                    name.clone_into(&mut current_fn);
                }
            }
        }
        let Some(after) = line.split(GENERATOR).nth(1) else {
            continue;
        };
        let Some(pattern) = after.split('"').next() else {
            continue;
        };
        if pattern.contains("(?s)") || !DOT_FORMS.iter().any(|d| pattern.contains(d)) {
            continue;
        }
        if !current_fn.is_empty() && !out.contains(&current_fn) {
            out.push(current_fn.clone());
        }
    }
    out
}

/// No NEW proptest generator can be newline-blind; the known ones are listed in
/// `ci/proptest-newline-baseline.txt` and that list may only shrink.
///
/// `proptest::string::string_regex` parses with `ParserBuilder::new().build()`,
/// whose `dot_matches_new_line` defaults to false, so a `.`-based generator
/// never produces `\n` — measured here at 0 newline-bearing samples in 5000,
/// against 4237/5000 once `(?s)` is added. Every such property is therefore a
/// single-line property, while newline is the character this codebase turns on:
/// the capture document is line-oriented, pack lines are role-marker-anchored,
/// trailers are column-0-anchored, and TOML is line-oriented. A test that reads
/// as "arbitrary input" and has never seen a newline is the most expensive kind
/// of green.
///
/// A baseline entry that no longer matches is also a violation — that is what
/// makes the list shrink-only rather than a place to park debt.
///
/// # Errors
/// If the baseline or a source file cannot be read.
pub fn proptest_generators_reach_newlines(root: &Path) -> Result<Vec<String>, String> {
    let baseline_rel = "ci/proptest-newline-baseline.txt";
    let baseline_text = read(root, baseline_rel)?;
    let baseline: Vec<&str> = baseline_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let mut found: Vec<String> = Vec::new();
    for f in source_files(root) {
        let rel = f
            .strip_prefix(root)
            .unwrap_or(&f)
            .display()
            .to_string()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&f).map_err(|e| format!("read {rel}: {e}"))?;
        for name in newline_blind_generators(&text) {
            found.push(format!("{rel}::{name}"));
        }
    }
    found.sort();
    let mut v = Vec::new();
    for site in &found {
        if !baseline.contains(&site.as_str()) {
            v.push(format!(
                "{site}: proptest generator uses `.` without `(?s)`, so it never \
                 generates a newline — prefix the regex with `(?s)`"
            ));
        }
    }
    for site in &baseline {
        if !found.iter().any(|f| f == site) {
            v.push(format!(
                "{baseline_rel} names `{site}`, which is no longer newline-blind — \
                 delete the line (this baseline only shrinks)"
            ));
        }
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_meta::tests::scratch_dir;

    /// The scanner finds a `.`-based generator, ignores one already carrying
    /// `(?s)`, and attributes each to its enclosing test — including when the
    /// signature wraps onto a later line.
    #[test]
    fn newline_blind_generators_are_found_and_attributed() {
        let blind = "proptest! {\n    #[test]\n    fn takes_any(s in \".{0,40}\") {\n";
        assert_eq!(newline_blind_generators(blind), vec!["takes_any"]);

        let fixed = "fn takes_any(s in \"(?s).{0,40}\") {\n";
        assert!(newline_blind_generators(fixed).is_empty());

        let star = "fn splat(m in \".*\") {\n";
        assert_eq!(newline_blind_generators(star), vec!["splat"]);

        // A generator on a later line still belongs to the fn above it.
        let wrapped = "fn wrapped(\n    a in \".{0,8}\",\n) {\n";
        assert_eq!(newline_blind_generators(wrapped), vec!["wrapped"]);

        // No dot metacharacter: a literal alternation is not newline-blind.
        assert!(newline_blind_generators("fn lit(s in \"a|b\") {\n").is_empty());
    }

    /// The screen itself over a planted tree, in both directions: a
    /// newline-blind generator the baseline does not list is reported by path
    /// and test name, and a baseline entry that no longer matches anything is
    /// reported as one to delete.
    #[test]
    fn an_unlisted_blind_generator_and_a_stale_baseline_entry_are_reported() {
        let root = scratch_dir("proptest-newlines");
        let src = root.join("crates").join("planted").join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(root.join("ci")).unwrap();
        std::fs::write(src.join("lib.rs"), "fn takes_any(s in \".{0,40}\") {\n").unwrap();
        std::fs::write(
            root.join("ci").join("proptest-newline-baseline.txt"),
            "# known sites\ncrates/planted/src/lib.rs::fixed_long_ago\n",
        )
        .unwrap();

        let violations = proptest_generators_reach_newlines(&root).expect("the screen runs");
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert!(
            violations.iter().any(|v| v.starts_with(
                "crates/planted/src/lib.rs::takes_any: proptest generator uses `.` without `(?s)`"
            )),
            "{violations:?}"
        );
        assert!(
            violations.iter().any(|v| {
                v.starts_with(
                "ci/proptest-newline-baseline.txt names `crates/planted/src/lib.rs::fixed_long_ago`"
            )
            }),
            "{violations:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}

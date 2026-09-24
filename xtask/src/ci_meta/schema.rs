//! The screen that keeps `bins/` production code off the schema fast-path,
//! and the production/test horizon it cuts a file at.
//!
//! The horizon is a separate item because it is the part that can go blind:
//! the gate reads the filesystem, the horizon reads a string, and only the
//! second can be tested on the shapes that once disabled it.
use std::path::Path;

use super::source_files;

/// The production half of a source file: everything before its trailing test
/// module, located by the joined column-zero anchor described on
/// [`no_schema_fastpath_in_bins`].
///
/// Split out from the walk so the horizon itself is testable on strings — the
/// gate it feeds reads the filesystem, and an untested horizon is how the
/// previous spelling went blind over 64% of a real file unnoticed.
pub(crate) fn production_half(text: &str) -> String {
    // Assembled so this scanner never matches itself.
    let anchor = ["\n#[cfg(", "test)]\nmod "].concat();
    // Lead with a newline so an anchor on line 1 still reads as column-zero.
    let padded = format!("\n{text}");
    padded.split(&anchor).next().unwrap_or("").to_owned()
}

/// `bins/` production code never uses the `create_canonical_schema` fast-path
/// (it bypasses the checksum-locked migration ledger).
///
/// The exempt region is the file's trailing test module, located by the JOINED
/// anchor `\n#[cfg(test)]\nmod ` — a column-zero `#[cfg(test)]` whose very next
/// line opens a module. Anything the anchor does not match is SCREENED, not
/// exempted, so an ambiguous marker becomes a finding rather than a silent
/// hole.
///
/// That is deliberate, and it is not hypothetical. The previous spelling cut at
/// the bare token, which an item-level `#[cfg(test)]` truncates just as
/// effectively as a test module: `bins/rift/src/features.rs` gates a test-only
/// helper `fn` at line 92 of 260, so the gate exempted 64% of the file — and
/// two production functions with it. Four further shapes (an indented marker,
/// and the token inside a string, a `//` comment, or a `//!` doc comment) each
/// disabled the gate for a whole file with no test module involved at all.
///
/// Consequence worth knowing before you hit it: a `#[cfg(test)]`-gated helper
/// that legitimately calls the fast-path IS now reported. Move it inside the
/// test module. That direction is fail-closed, which is the correct posture for
/// a gate of last resort — and by this workspace's own precedent, a gate of
/// last resort must not depend on another gate to be correct.
///
/// Deliberately NOT a uniqueness rule (`count != 1` → report): a file may
/// legitimately carry several test modules (`crates/rift-canopy/src/suggest.rs`
/// carries three), and a gate that cries wolf on a large file gets silenced.
///
/// Residual, stated rather than papered over: a string or block comment holding
/// the exact sequence `\n#[cfg(test)]\nmod ` at column zero still truncates. No
/// `bins/` file holds one today. Narrower than what it replaces, not absent.
///
/// # Errors
/// If a source file cannot be read.
pub fn no_schema_fastpath_in_bins(root: &Path) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    for f in source_files(root) {
        let rel = f.strip_prefix(root).unwrap_or(&f).display().to_string();
        if !rel.starts_with("bins/") || rel.contains("/tests/") {
            continue;
        }
        let text = std::fs::read_to_string(&f).map_err(|e| format!("read {rel}: {e}"))?;
        if production_half(&text).contains("create_canonical_schema") {
            v.push(format!(
                "{rel}: `create_canonical_schema` in production bin code — open the \
                 projection via `rift_store::open_migrated`/`open_capture_projection`"
            ));
        }
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_meta::tests::scratch_dir;

    /// The shape that was LIVE on main: `bins/rift/src/features.rs` gates a
    /// test-only helper `fn` at line 92 of 260, and the old bare-token cut
    /// exempted everything below it — 64% of the file, two production
    /// functions included. An item-level attribute is not a module boundary.
    ///
    /// The call sits AFTER the marker on purpose. That is what `features.rs`
    /// looks like, and it is the only arrangement that exercises the
    /// truncation: with the call ABOVE the marker this test passes under the
    /// old spelling too, which is how the first draft of it was vacuous.
    #[test]
    fn a_test_only_helper_fn_does_not_truncate_the_screen() {
        let src = "pub fn a() {}\n#[cfg(test)]\nfn helper() {}\n\npub fn p() { create_canonical_schema(); }\n";
        assert!(production_half(src).contains("create_canonical_schema"));
    }

    /// The other direction, which is what stops this being a broken exemption
    /// rather than a tightened one.
    #[test]
    fn a_real_test_module_is_still_exempt() {
        let src = "pub fn p() {}\n#[cfg(test)]\nmod tests {\n    fn t() { create_canonical_schema(); }\n}\n";
        assert!(!production_half(src).contains("create_canonical_schema"));
    }

    /// Four shapes that each disabled the gate for a whole file under the old
    /// spelling, with no test module involved at all.
    #[test]
    fn a_marker_that_is_not_a_module_boundary_never_truncates() {
        for src in [
            // indented — a nested test helper
            "pub fn p() {\n    #[cfg(test)]\n    fn n() {}\n    create_canonical_schema();\n}\n",
            // inside a string literal
            "const S: &str = \"#[cfg(test)]\";\npub fn p() { create_canonical_schema(); }\n",
            // inside a line comment
            "// #[cfg(test)]\npub fn p() { create_canonical_schema(); }\n",
            // inside a doc comment
            "//! #[cfg(test)]\npub fn p() { create_canonical_schema(); }\n",
        ] {
            assert!(
                production_half(src).contains("create_canonical_schema"),
                "truncated at a non-boundary marker: {src:?}"
            );
        }
    }

    /// A file may carry several test modules, which is why this is not a
    /// uniqueness rule: the cut is at the FIRST module boundary, and everything
    /// after it is test code.
    #[test]
    fn several_test_modules_cut_at_the_first() {
        let src = "pub fn p() {}\n#[cfg(test)]\nmod a { fn t() { create_canonical_schema(); } }\n#[cfg(test)]\nmod b {}\n";
        assert!(!production_half(src).contains("create_canonical_schema"));
        assert!(production_half(src).contains("pub fn p"));
    }

    /// A file whose test module opens on line 1 still has it recognised — the
    /// anchor is column-zero, and column zero includes the first column of the
    /// first line.
    #[test]
    fn an_anchor_on_the_first_line_is_still_column_zero() {
        let src = "#[cfg(test)]\nmod tests { fn t() { create_canonical_schema(); } }\n";
        assert!(!production_half(src).contains("create_canonical_schema"));
    }

    /// The declared residual, asserted so it cannot silently widen: the joined
    /// sequence inside a string still truncates. No `bins/` file holds one.
    /// If this test ever fails, the gate got STRONGER — delete it and say so.
    #[test]
    fn the_declared_residual_is_exactly_as_wide_as_documented() {
        let src = "const S: &str = \"\n#[cfg(test)]\nmod x {\";\npub fn p() { create_canonical_schema(); }\n";
        assert!(!production_half(src).contains("create_canonical_schema"));
    }

    /// The screen itself over a planted tree, not only the horizon it cuts at:
    /// a `bins/` production call is reported by path, and the same call inside
    /// a real test module is not.
    #[test]
    fn a_schema_fastpath_in_production_bin_code_is_reported() {
        let root = scratch_dir("schema-fastpath");
        let src = root.join("bins").join("planted").join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("main.rs"),
            "fn main() { create_canonical_schema(); }\n",
        )
        .unwrap();
        std::fs::write(
            src.join("exempt.rs"),
            "pub fn p() {}\n#[cfg(test)]\nmod tests { fn t() { create_canonical_schema(); } }\n",
        )
        .unwrap();

        let violations = no_schema_fastpath_in_bins(&root).expect("the screen runs");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations
                .first()
                .unwrap()
                .starts_with("bins/planted/src/main.rs: `create_canonical_schema`"),
            "{violations:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}

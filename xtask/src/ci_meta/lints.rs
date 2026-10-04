//! Screens over source text that hold the lint policy: the workspace lint
//! table against its baseline, crate-root blanket allows, and `unsafe`.
//!
//! One module because all three ask the same question of a source tree —
//! whether a lint the workspace manages has been quietly turned off, at the
//! table, at a crate root, or by writing `unsafe` where the workspace forbids
//! it.
use std::path::Path;

use super::{read, source_files};

/// Normalize a lint-table body: keep `[section]` headers and `key = value`
/// lines, dropping comments and blanks.
fn normalized_lint_lines(text: &str, sections: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut keep = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            keep = sections.iter().any(|s| line == format!("[{s}]"));
            if keep {
                out.push(line.to_string());
            }
            continue;
        }
        if keep && !line.is_empty() && !line.starts_with('#') {
            out.push(line.to_string());
        }
    }
    out
}

/// `[workspace.lints.rust]` + `[workspace.lints.clippy]` in the root manifest
/// match `ci/lint-baseline.txt` exactly (order-sensitive: the table is policy).
///
/// # Errors
/// If either file cannot be read.
pub fn lint_table_matches_baseline(root: &Path) -> Result<Vec<String>, String> {
    let sections = ["workspace.lints.rust", "workspace.lints.clippy"];
    let manifest = normalized_lint_lines(&read(root, "Cargo.toml")?, &sections);
    let baseline = normalized_lint_lines(
        &read(root, "ci/lint-baseline.txt")?,
        &["workspace.lints.rust", "workspace.lints.clippy"],
    );
    if manifest == baseline {
        return Ok(Vec::new());
    }
    let mut v = vec![
        "workspace lint table diverges from ci/lint-baseline.txt (change both, with rationale):"
            .to_string(),
    ];
    for line in &manifest {
        if !baseline.contains(line) {
            v.push(format!("  manifest has: {line}"));
        }
    }
    for line in &baseline {
        if !manifest.contains(line) {
            v.push(format!("  baseline has: {line}"));
        }
    }
    Ok(v)
}

/// No `unsafe` code outside the (currently empty) sanctioned-module allowlist.
///
/// # Errors
/// If a source file cannot be read.
pub fn unsafe_only_in_sanctioned_modules(root: &Path) -> Result<Vec<String>, String> {
    const SANCTIONED: &[&str] = &[]; // extend with FFI seams as they land
    let mut v = Vec::new();
    for f in source_files(root) {
        let rel = slash_relative(root, &f);
        if SANCTIONED.iter().any(|s| rel.starts_with(s)) {
            continue;
        }
        let text = std::fs::read_to_string(&f).map_err(|e| format!("read {rel}: {e}"))?;
        // needles assembled via concat! so this scanner never matches itself
        const NEEDLES: [&str; 3] = [
            concat!("unsafe", " fn"),
            concat!("unsafe", " impl"),
            concat!("unsafe", " {"),
        ];
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if NEEDLES.iter().any(|n| code.contains(n)) {
                v.push(format!(
                    "{rel}:{}: `unsafe` outside a sanctioned module",
                    i + 1
                ));
            }
        }
    }
    Ok(v)
}

/// One-based line numbers of blanket clippy `#![allow(...)]` inner attributes
/// in `text`, reported at the attribute's opening line.
///
/// Catches both the single-line `#![allow(clippy::foo)]` form and the
/// multi-line form
///
/// ```text
/// #![allow(
///     clippy::foo,
///     clippy::bar
/// )]
/// ```
///
/// which the earlier line-prefix check missed, letting a crate-root blanket
/// evade the gate simply by wrapping. The test-only
/// `#![cfg_attr(test, allow(...))]` form is never matched — it opens with
/// `#![cfg_attr`, not `#![allow(` — and blocks that name no `clippy::` lint
/// (e.g. `#![allow(dead_code)]`) are left to the rust-lint policy.
fn blanket_clippy_allow_lines(text: &str) -> Vec<usize> {
    // Drop a trailing line comment before inspecting attribute syntax, so a
    // `clippy::` mentioned only in a comment can neither trigger nor mask a
    // match (mirrors the `unsafe` scanner above). `str::lines` already strips a
    // trailing `\r`, so CRLF sources are handled.
    fn code(line: &str) -> &str {
        line.split("//").next().unwrap_or("")
    }
    let mut hits = Vec::new();
    let mut it = text.lines().enumerate().peekable();
    while let Some((start, first)) = it.next() {
        // An `#![allow (...)]` inner attribute — whitespace between `allow` and
        // `(` is legal Rust. The test-only `#![cfg_attr(test, allow(...))]` form
        // and item attributes (`#[allow(...)]`, a single `#[`) are excluded:
        // neither starts with `#![allow`.
        let is_opener = code(first)
            .trim_start()
            .strip_prefix("#![allow")
            .is_some_and(|rest| rest.trim_start().starts_with('('));
        if !is_opener {
            continue;
        }
        // Accumulate comment-stripped code from the opener to the closing `)]`,
        // so a wrapped list reads as one attribute. Stop before a fresh
        // inner-attr opener so a malformed, never-closed block cannot swallow
        // the next one.
        let mut block = code(first).trim().to_string();
        while !block.contains(")]") {
            let Some((_, next)) = it.peek() else { break };
            if next.trim_start().starts_with("#![") {
                break;
            }
            if let Some((_, line)) = it.next() {
                block.push_str(code(line).trim());
            }
        }
        if block.contains("clippy::") {
            hits.push(start + 1);
        }
    }
    hits
}

/// Crate roots carry no blanket `#![allow(clippy::...)]` of workspace-managed
/// lints — the workspace table is the single policy point; site allows must be
/// narrow and justified. (`#![cfg_attr(test, allow(...))]` is fine: test-only.)
///
/// # Errors
/// If a crate-root file cannot be read.
pub fn no_blanket_allow_at_crate_root(root: &Path) -> Result<Vec<String>, String> {
    let mut v = Vec::new();
    for f in source_files(root) {
        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let is_crate_root = (name == "lib.rs" || name == "main.rs")
            && f.parent().is_some_and(|p| p.ends_with("src"));
        if !is_crate_root {
            continue;
        }
        let rel = slash_relative(root, &f);
        let text = std::fs::read_to_string(&f).map_err(|e| format!("read {rel}: {e}"))?;
        for line in blanket_clippy_allow_lines(&text) {
            v.push(format!(
                "{rel}:{line}: blanket crate-root `#![allow(clippy::…)]` — manage the lint \
                 in [workspace.lints] or allow narrowly at the site"
            ));
        }
    }
    Ok(v)
}

/// The path of `file` under `root`, its components joined with `/`.
///
/// Not the native display string: on Windows that spells the location with
/// backslashes, so the same finding would read differently per OS and a
/// `SANCTIONED` prefix written with slashes would never match.
fn slash_relative(root: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(root).unwrap_or(file);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci_meta::tests::scratch_dir;

    #[test]
    fn detects_single_line_blanket_clippy_allow() {
        let src = "#![allow(clippy::pedantic)]\nfn f() {}\n";
        assert_eq!(blanket_clippy_allow_lines(src), vec![1]);
    }

    #[test]
    fn detects_multiline_blanket_clippy_allow() {
        // The wrapped form that evaded the old line-prefix check.
        let src = "//! doc\n#![allow(\n    clippy::doc_markdown,\n    \
                   clippy::cast_precision_loss\n)]\n\npub fn g() {}\n";
        assert_eq!(blanket_clippy_allow_lines(src), vec![2]);
    }

    #[test]
    fn ignores_cfg_attr_test_allow() {
        // Test-only allows are sanctioned and must never be flagged.
        let src = "#![cfg_attr(\n    test,\n    allow(\n        \
                   clippy::unwrap_used,\n        clippy::cast_possible_wrap\n    )\n)]\n";
        assert!(blanket_clippy_allow_lines(src).is_empty());
    }

    #[test]
    fn ignores_non_clippy_blanket_allow() {
        // The gate governs clippy workspace lints; a rust-lint allow is out of scope.
        let src = "#![allow(dead_code)]\n#![allow(\n    unused_variables\n)]\n";
        assert!(blanket_clippy_allow_lines(src).is_empty());
    }

    #[test]
    fn ignores_clippy_named_only_in_a_comment() {
        // A clippy lint mentioned in a trailing comment must not flag a block
        // whose attribute names no clippy lint.
        let src = "#![allow(dead_code)] // TODO: consider clippy::pedantic here\nfn f() {}\n";
        assert!(blanket_clippy_allow_lines(src).is_empty());
    }

    #[test]
    fn catches_whitespace_before_the_paren() {
        // `#![allow (clippy::all)]` is legal Rust and must not evade the gate.
        let src = "#![allow (clippy::all)]\nfn main() {}\n";
        assert_eq!(blanket_clippy_allow_lines(src), vec![1]);
    }

    #[test]
    fn unclosed_block_does_not_swallow_the_next_opener() {
        // A malformed never-closed opener must not absorb a following real blanket.
        let src = "#![allow(dead_code\n#![allow(clippy::pedantic)]\nfn f() {}\n";
        assert_eq!(blanket_clippy_allow_lines(src), vec![2]);
    }

    #[test]
    fn lint_baseline_drift_is_detected() {
        // A manifest whose clippy table dropped indexing_slicing ≠ the baseline.
        let manifest = "[workspace.lints.clippy]\nunwrap_used = \"warn\"\n";
        let baseline =
            "[workspace.lints.clippy]\nunwrap_used = \"warn\"\nindexing_slicing = \"warn\"\n";
        let m = normalized_lint_lines(manifest, &["workspace.lints.clippy"]);
        let b = normalized_lint_lines(baseline, &["workspace.lints.clippy"]);
        assert_ne!(m, b, "drift must be visible after normalization");
    }

    /// A planted `unsafe` item is caught and located. The workspace is
    /// `#![forbid(unsafe_code)]`, so this screen never fires on the real tree
    /// — which is exactly the position from which a hollowed-out walk is
    /// indistinguishable from a clean one.
    ///
    /// The needle is assembled at run time so the planted source, read back by
    /// the screen over THIS tree, is not itself a finding.
    #[test]
    fn an_unsafe_item_in_first_party_source_is_located() {
        let root = scratch_dir("unsafe-scan");
        let src = root.join("crates").join("planted").join("src");
        std::fs::create_dir_all(&src).unwrap();
        let planted = format!(
            "pub fn clean() {{}}\n{} impl Send for Planted {{}}\n",
            "unsafe"
        );
        std::fs::write(src.join("lib.rs"), planted).unwrap();

        let violations = unsafe_only_in_sanctioned_modules(&root).expect("the screen runs");
        assert_eq!(
            violations,
            vec!["crates/planted/src/lib.rs:2: `unsafe` outside a sanctioned module".to_owned()]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A location is reported in the slash spelling whatever separator the
    /// platform splits on, so a diagnostic reads the same from every OS.
    #[test]
    fn a_located_path_is_spelled_with_slashes() {
        let root = Path::new("root");
        let file = root
            .join("crates")
            .join("planted")
            .join("src")
            .join("lib.rs");
        assert_eq!(slash_relative(root, &file), "crates/planted/src/lib.rs");
    }

    /// The lint-table screen itself, not just its normalizer: a manifest whose
    /// clippy table dropped a lint the baseline names is reported, and the
    /// dropped lint is named in the report.
    #[test]
    fn a_manifest_that_dropped_a_baselined_lint_is_reported() {
        let root = scratch_dir("lint-baseline");
        std::fs::create_dir_all(root.join("ci")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace.lints.clippy]\nunwrap_used = \"warn\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("ci").join("lint-baseline.txt"),
            "[workspace.lints.clippy]\nunwrap_used = \"warn\"\nindexing_slicing = \"warn\"\n",
        )
        .unwrap();

        let violations = lint_table_matches_baseline(&root).expect("the screen runs");
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert!(
            violations
                .iter()
                .any(|v| v == "  baseline has: indexing_slicing = \"warn\""),
            "{violations:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}

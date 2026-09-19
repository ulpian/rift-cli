//! The single-mint rule made executable: `RedactedBytes::seal` is called in
//! this crate and nowhere else, and no second function named `seal` exists to
//! make the needle ambiguous.

use std::collections::{BTreeMap, BTreeSet};

use super::fixtures::{visit_text_files, workspace_root};

/// The mint of `RedactedBytes::seal` is confined to this (the redaction) crate.
/// `rift-core` *defines* `seal` (as `pub fn seal(`, which this call-pattern
/// does not match) and is its type's home; no other crate may mint. This makes
/// the single-call-site rule executable — add a mint elsewhere and CI
/// goes red.
///
/// **The needle is `::seal(`, not `RedactedBytes::seal(`, and that is the
/// whole point.** `RedactedBytes` is `pub` in `rift-core`, so the earlier
/// type-spelled needle was defeated by every path that renames it —
/// `use rift_core::RedactedBytes as RB; RB::seal(..)`, `<RedactedBytes>::seal(..)`,
/// or a local `type` alias. A verifier demonstrated all three in a probe crate
/// outside this repository: it compiles, it mints, and the type-spelled needle
/// matched none of them. Anchoring on the `::` collapses every qualified path
/// into one case, which is the same repair `rift-shadow`'s inclusion screens made for `::refuse(`
/// after four rounds of widening one spelling at a time.
///
/// **What the wider needle would also catch, stated because it was a live
/// cost and not a hypothetical.** `rift-crypto` used to define an unrelated
/// `pub fn seal` (AEAD sealing); one `::`-qualified call to it would have
/// redded this guard with a false positive and invited a path exception.
/// An earlier review closed that by renaming the primitive to
/// `aead_seal`/`aead_open`,
/// and `no_other_fn_seal_is_defined_in_the_workspace` below keeps the name
/// unique, so this needle has exactly one definition to match. If a second
/// `seal` is ever defined, **resolve that by distinguishing the two `seal`s,
/// never by excepting `rift-crypto` by path**: it depends on `rift-core`, so it
/// can mint `RedactedBytes`, and a path exception there would be a real hole
/// rather than a false-positive fix.
///
/// Among the cases this does not cover — an open list, and these were planted
/// rather than argued for: a mint reached without the `::` at all (a bare
/// `seal(..)` after `use rift_core::RedactedBytes::seal`, were that legal for
/// an inherent associated fn — it is not today), and any file this walk does
/// not read. The walk reads whole files, so it does not inherit the
/// `#[cfg(test)]` horizon hazard that `rift-shadow`'s inclusion screens close for themselves.
///
/// The walk covers **every** workspace source root, not just `crates/`: a
/// binary, a build task, a fuzz target or a bench harness compiles against
/// `rift-core` exactly like a library does, so a mint in any of them would be
/// just as real and — until this test walked them — just as invisible. The
/// roots the walk must reach are asserted, so deleting or renaming one turns
/// the guard red rather than quietly hollow.
#[test]
fn seal_is_minted_only_in_the_redaction_crate() {
    const SOURCE_ROOTS: [&str; 5] = ["crates", "bins", "xtask", "fuzz", "benches"];
    let root = workspace_root();
    // Assembled, not written whole, so this literal does not match itself.
    let needle = ["::", "seal("].concat();
    let mut here = 0usize;
    let mut offenders = Vec::new();
    let mut rust_files_per_root: BTreeMap<String, usize> = BTreeMap::new();
    visit_text_files(&root, &mut |path, contents| {
        if path.extension().is_none_or(|e| e != "rs") {
            return;
        }
        if let Some(source_root) = path
            .strip_prefix(&root)
            .ok()
            .and_then(|rel| rel.components().next())
        {
            *rust_files_per_root
                .entry(source_root.as_os_str().to_string_lossy().into_owned())
                .or_default() += 1;
        }
        let count = contents.matches(needle.as_str()).count();
        if count == 0 {
            return;
        }
        let p = path.to_string_lossy().to_string();
        if p.contains("rift-shadow-redact") {
            here += count;
        } else if !p.contains("rift-core") {
            offenders.push(format!("{p} ({count})"));
        }
    });
    assert!(
        offenders.is_empty(),
        "RedactedBytes::seal minted outside the redaction crate: {offenders:?}"
    );
    assert!(
        here >= 1,
        "expected the single production mint in this crate"
    );
    for source_root in SOURCE_ROOTS {
        assert!(
            rust_files_per_root.get(source_root).is_some_and(|n| *n > 0),
            "the seal guard reached no Rust source under {source_root}/ — it walked \
             {rust_files_per_root:?}, so a mint there would go unseen"
        );
    }
}

/// The other half of the guard above. `::seal(` is an exact needle only
/// while `RedactedBytes::seal` is the sole function named `seal` in the
/// workspace: a second definition anywhere would make every qualified call
/// to it ambiguous with a mint, and the cheap repair — excepting that crate
/// by path — is the hole the guard above refuses. So the set of files that
/// define a `seal` function (comment-stripped line starting `fn seal(`,
/// `pub fn seal(` or `pub(crate) fn seal(`) is pinned to exactly
/// `{crates/rift-core/src/redacted.rs}`. A red here is resolved by renaming
/// the newcomer (as `rift-crypto`'s AEAD pair became `aead_seal`/`aead_open`),
/// never by widening this set. Same five roots as the guard above.
#[test]
fn no_other_fn_seal_is_defined_in_the_workspace() {
    let root = workspace_root();
    let mut defining = BTreeSet::new();
    visit_text_files(&root, &mut |path, contents| {
        if path.extension().is_none_or(|e| e != "rs") {
            return;
        }
        let defines = contents.lines().any(|line| {
            let code = line.split("//").next().unwrap_or_default().trim_start();
            ["fn seal(", "pub fn seal(", "pub(crate) fn seal("]
                .iter()
                .any(|prefix| code.starts_with(prefix))
        });
        if defines {
            let rel = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            defining.insert(rel);
        }
    });
    let expected: BTreeSet<String> = ["crates/rift-core/src/redacted.rs".to_owned()].into();
    assert_eq!(
        defining, expected,
        "a function named `seal` is defined outside rift-core — rename it (the \
         `::seal(` mint guard must stay exact; never path-except a crate)"
    );
}

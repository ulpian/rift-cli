//! The `family-keyed-caches` leg: a cache is keyed by the repository family,
//! never by which checkout happened to be open.

use std::path::Path;

use crate::ci_meta::production_half;

use super::text::{fn_bodies, mask_comments, token_offsets};
use super::{is_test_surface, rust_corpus, Leg};

/// The caches this program adds, as they appear in a path expression.
const MEMBER_CACHES: [&str; 2] = [".rift/state/doc", ".rift/state/search"];

/// The family's own directory in the git common dir, as a path literal.
const FAMILY_STORE: &str = "rift-family";

/// The names a body uses for that directory instead of its literal: the
/// constants that hold it. The literal itself sits in the constant, outside
/// every body, so a builder over a constant is seen only by naming it.
const FAMILY_STORE_NAMES: [&str; 2] = ["FAMILY_STATE_DIR", "FAMILY_DIR"];

/// The call that hands out the family's run-state directory, which every
/// hook-side store joins its own subdirectory onto.
const FAMILY_STATE_CALL: &str = "family_state_dir(";

/// **A cache is keyed by the family, never by which checkout is open.**
///
/// A linked worktree is a member of its repository's family, and every durable
/// root — the projection, the recall store, the binary of record, the registry
/// row — is keyed by the family's minted identity in the common directory. A
/// cache keyed by a checkout path instead does not merely miss: it silently
/// splits, so a temporary worktree's work never compounds with anything and the
/// saving looks like a smaller number rather than a bug.
///
/// Two rules, both over function bodies in production halves, with comments
/// masked and path literals kept:
///
/// 1. a body that builds one of `MEMBER_CACHES` does not also derive an
///    identity from a checkout (`repo_id_for`) — the per-member caches live
///    under the member's own `.rift/state/`, keyed by nothing;
/// 2. a body that builds the `FAMILY_STORE` directory — by its literal, by a
///    constant that holds it, or by calling `family_state_dir` — names a
///    `common_dir`: the family store is reached from the common directory or
///    it is not the family store.
///
/// The limits: this reads names, so a body that hands a checkout path to a
/// helper which then joins the cache is split across two bodies and seen by
/// neither rule. What it does catch is the shape the mistake actually takes,
/// which is both halves written together.
///
/// # Errors
/// If the corpus cannot be read.
pub fn family_keyed_caches(root: &Path) -> Result<Leg, String> {
    Ok(screen_family_caches(&rust_corpus(root)?))
}

/// The screen over an explicit corpus — the seam both plantings go through.
fn screen_family_caches(corpus: &[(String, String)]) -> Leg {
    let mut v = Vec::new();
    let mut cache_sites = 0_usize;
    let mut family_sites: Vec<String> = Vec::new();
    for (rel, text) in corpus {
        if is_test_surface(rel) {
            continue;
        }
        for f in fn_bodies(rel, &mask_comments(&production_half(text))) {
            for cache in MEMBER_CACHES {
                if !f.body.contains(cache) {
                    continue;
                }
                cache_sites += 1;
                if !token_offsets(&f.body, "repo_id_for").is_empty() {
                    v.push(format!(
                        "{}: `{}` builds `{cache}` and derives an id from a checkout — a member \
                         cache lives under that member's own state directory, and an id comes \
                         from the family",
                        f.site, f.name
                    ));
                }
            }
            if !builds_family_store(&f.body) {
                continue;
            }
            family_sites.push(f.name.clone());
            if !f.body.contains("common_dir") {
                v.push(format!(
                    "{}: `{}` builds the family store without a common directory — a family's \
                     store is one directory shared by every member, reached from the common dir",
                    f.site, f.name
                ));
            }
        }
    }
    Leg::new(
        "family-keyed-caches",
        format!(
            "{cache_sites} member-cache site(s) derive no id from a checkout; {} \
             family-store site(s) are reached from a common directory ({}){}",
            family_sites.len(),
            family_sites.join(", "),
            if cache_sites == 0 {
                " (no member cache is built yet, so that half is a standing guard on work \
                 that has not landed)"
            } else {
                ""
            }
        ),
        v,
    )
}

/// Whether a body builds the family's own directory, by any of the three
/// spellings a builder uses.
fn builds_family_store(body: &str) -> bool {
    body.contains(FAMILY_STORE)
        || !token_offsets(body, FAMILY_STATE_CALL).is_empty()
        || FAMILY_STORE_NAMES
            .iter()
            .any(|name| !token_offsets(body, name).is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ A cache keyed by which checkout is open, and a family store reached
    /// without the common directory, are both caught.
    #[test]
    fn a_cache_keyed_by_a_checkout_is_caught() {
        let by_checkout = vec![(
            "crates/rift-recall-doc/src/lib.rs".to_owned(),
            "fn sidecar(w: &Path) -> PathBuf {\n    \
             let id = repo_id_for(w);\n    w.join(\".rift/state/doc\").join(id)\n}\n"
                .to_owned(),
        )];
        let leg = screen_family_caches(&by_checkout);
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("derives an id from a checkout"),
            "{:#?}",
            leg.violations
        );

        let by_path = vec![(
            "crates/rift-git/src/family.rs".to_owned(),
            "fn store(w: &Path) -> PathBuf {\n    w.join(\"rift-family\")\n}\n".to_owned(),
        )];
        let leg = screen_family_caches(&by_path);
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("without a common directory"),
            "{:#?}",
            leg.violations
        );

        let correct = vec![(
            "crates/rift-git/src/family.rs".to_owned(),
            "fn store(common_dir: &Path) -> PathBuf {\n    common_dir.join(\"rift-family\")\n}\n"
                .to_owned(),
        )];
        assert!(screen_family_caches(&correct).violations.is_empty());
    }

    /// One fixture file under the hooks, as the family's run state is built.
    fn hooks_file(text: &str) -> Vec<(String, String)> {
        vec![(
            "bins/rift/src/hooks/originals.rs".to_owned(),
            text.to_owned(),
        )]
    }

    /// ★ A store joined onto `family_state_dir` from a checkout path is
    /// caught, and the same store from the common directory passes and is
    /// counted as a family-store site.
    #[test]
    fn a_store_built_through_family_state_dir_from_a_checkout_is_caught() {
        let by_checkout =
            hooks_file("fn store(w: &Path) -> PathBuf {\n    family_state_dir(w).join(\"x\")\n}\n");
        let leg = screen_family_caches(&by_checkout);
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("`store` builds the family store without a common"),
            "{:#?}",
            leg.violations
        );

        let by_common = hooks_file(
            "fn store(common_dir: &Path) -> PathBuf {\n    \
             family_state_dir(common_dir).join(\"x\")\n}\n",
        );
        let leg = screen_family_caches(&by_common);
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
        assert!(
            leg.detail.contains("1 family-store site(s)"),
            "{}",
            leg.detail
        );
    }

    /// ★ The directory's own builder, whose literal sits in a constant
    /// outside every function body, is still a family-store site.
    #[test]
    fn a_builder_over_a_named_constant_is_a_family_store_site() {
        let defined = hooks_file(
            "const FAMILY_STATE_DIR: &str = \"rift-family\";\n\
             fn family_state_dir(common_dir: &Path) -> PathBuf {\n    \
             common_dir.join(FAMILY_STATE_DIR)\n}\n",
        );
        let leg = screen_family_caches(&defined);
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
        assert!(
            leg.detail.contains("1 family-store site(s)")
                && leg.detail.contains("family_state_dir"),
            "{}",
            leg.detail
        );
    }

    /// The real tree's family-store builders, the run-state directory and
    /// every store joined onto it, are all seen and all reached from a common
    /// directory.
    #[test]
    fn the_real_tree_sees_every_family_store_builder() {
        let leg = family_keyed_caches(&crate::workspace_root()).expect("the corpus reads");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
        for name in [
            "family_state_dir",
            "outline_cache_dir",
            "linehash_cache_dir",
        ] {
            assert!(leg.detail.contains(name), "{name} unseen: {}", leg.detail);
        }
    }
}

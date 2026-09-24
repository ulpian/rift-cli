//! The `family-keyed-caches` leg: a cache is keyed by the repository family,
//! never by which checkout happened to be open.

use std::path::Path;

use crate::ci_meta::production_half;

use super::text::{fn_bodies, mask_comments, token_offsets};
use super::{is_test_surface, rust_corpus, Leg};

/// The caches this program adds, as they appear in a path expression.
const MEMBER_CACHES: [&str; 2] = [".rift/state/doc", ".rift/state/search"];

/// The family's own directory in the git common dir, as a path literal and as
/// the constant the code that joins it usually names instead.
const FAMILY_STORE: &str = "rift-family";

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
/// 2. a body that builds the `FAMILY_STORE` directory names a `common_dir` —
///    the family store is reached from the common directory or it is not the
///    family store.
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
    let mut family_sites = 0_usize;
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
            let builds_family_store =
                f.body.contains(FAMILY_STORE) || !token_offsets(&f.body, "FAMILY_DIR").is_empty();
            if !builds_family_store {
                continue;
            }
            family_sites += 1;
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
            "{cache_sites} member-cache site(s) derive no id from a checkout; {family_sites} \
             family-store site(s) are reached from a common directory{}",
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
}

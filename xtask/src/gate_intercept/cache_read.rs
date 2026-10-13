//! The `one-cache-read` leg: the family caches a decision reads, each named,
//! and no other one beside them.
//!
//! A first contact is answered from a small file the detached worker writes,
//! because a decision may open no store, and a terminal result is matched
//! against another: the line hashes of the files it named. Each is a file
//! read on the decision path, admitted on two conditions this leg holds: it
//! is one of the named readers, and it is keyed by the family's common
//! directory and the bytes' content address, so it can neither split per
//! checkout nor serve a stale entry. A further family-cache read appearing on
//! the path, or an admitted one moving or losing its key, fails the leg
//! rather than passing unnoticed.

use std::collections::BTreeSet;
use std::path::Path;

use super::gate_path::{production_fns, walk_by_name, GATE_CRATES, GATE_ROOTS};
use super::text::{token_offsets, FnBody};
use super::{rust_corpus, Leg};

/// One admitted reader: its name, the file it is defined in, and what its
/// key is built from — the family's common directory, from the git dir's own
/// pointer file, and the content address.
struct Admitted {
    name: &'static str,
    file: &'static str,
    keyed_by: [&'static str; 2],
}

/// The admitted readers.
const ADMITTED: [Admitted; 2] = [
    Admitted {
        name: "load_outline_cache",
        file: "bins/rift/src/hooks/gate/outline_cache.rs",
        keyed_by: ["read_common_dir", "outline_cache_name"],
    },
    Admitted {
        name: "load_linehash_cache",
        file: "bins/rift/src/hooks/gate/linehash_cache.rs",
        keyed_by: ["read_common_dir", "linehash_cache_name"],
    },
];

/// The names a body uses to reach the family's own directory, as code (string
/// bodies are masked): the constant, and each cache's directory.
const FAMILY_STORE: [&str; 3] = ["FAMILY_DIR", "outline_cache_dir", "linehash_cache_dir"];

/// A read of a file's bytes, by any of the spellings a body would use.
const FILE_READS: [&str; 4] = ["read_to_string", "read", "File::open", "OpenOptions"];

/// **The decision path reads one family cache, through one named function.**
///
/// The walk is the `no-store-on-gate-path` leg's, from the same roots. A
/// reached function that reaches the family's directory and reads a file is a
/// family-cache read; the leg holds when the only one is the admitted reader,
/// defined where it is named, and its body builds the key from the common
/// directory and the content address.
///
/// # Errors
/// If the corpus cannot be read, or a named root no longer exists.
pub fn one_cache_read(root: &Path) -> Result<Leg, String> {
    let corpus: Vec<(String, String)> = rust_corpus(root)?
        .into_iter()
        .filter(|(rel, _)| GATE_CRATES.iter().any(|c| rel.starts_with(c)))
        .collect();
    Ok(screen_cache_reads(&corpus))
}

/// The screen over an explicit corpus — the seam the plantings go through.
fn screen_cache_reads(corpus: &[(String, String)]) -> Leg {
    let fns = production_fns(corpus);
    let walk = walk_by_name(&fns, &GATE_ROOTS, &[]);
    let readers: Vec<&FnBody> = fns
        .iter()
        .filter(|f| walk.names.contains(f.name.as_str()) && reads_the_family_store(f))
        .collect();
    let mut v = Vec::new();
    for f in &readers {
        let admitted = ADMITTED
            .iter()
            .any(|a| f.name == a.name && f.site.starts_with(a.file));
        if !admitted {
            v.push(format!(
                "{}: `{}` reads a family cache on the decision path — the admitted reads are {}",
                f.site,
                f.name,
                admitted_list()
            ));
        }
    }
    for admitted in &ADMITTED {
        let found: Vec<&&FnBody> = readers.iter().filter(|f| f.name == admitted.name).collect();
        if found.is_empty() {
            v.push(format!(
                "`{}` is no longer reached from a decision entry point as a family-cache read — \
                 re-read the gate and correct this leg, never let the read move unnamed",
                admitted.name
            ));
        }
        for f in found {
            for key in admitted.keyed_by {
                if token_offsets(&f.body, key).is_empty() {
                    v.push(format!(
                        "{}: `{}` no longer builds its key from `{key}`",
                        f.site, f.name
                    ));
                }
            }
        }
    }
    let names: BTreeSet<&str> = readers.iter().map(|f| f.name.as_str()).collect();
    Leg::new(
        "one-cache-read",
        format!(
            "{} family-cache read(s) on the decision path: {}, each keyed by the family's \
             common dir and the content address (no store, no tokenizer)",
            readers.len(),
            names.into_iter().collect::<Vec<_>>().join(", ")
        ),
        v,
    )
}

/// The admitted readers as a finding names them.
fn admitted_list() -> String {
    ADMITTED
        .iter()
        .map(|a| format!("`{}` in {}", a.name, a.file))
        .collect::<Vec<_>>()
        .join(" and ")
}

/// Whether a body reaches the family's directory and reads a file.
fn reads_the_family_store(f: &FnBody) -> bool {
    let names_store = FAMILY_STORE
        .iter()
        .any(|name| !token_offsets(&f.body, name).is_empty());
    let reads = FILE_READS
        .iter()
        .any(|read| !token_offsets(&f.body, read).is_empty());
    names_store && reads
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The roots and both admitted readers, as the gate files spell them.
    fn corpus(extra: &str) -> Vec<(String, String)> {
        vec![
            (
                "bins/rift/src/hooks/gate/mod.rs".to_owned(),
                format!(
                    "fn window_journal_step() {{ read_gate(); output_match_record(); }}\n\
                     fn read_gate() {{ load_outline_cache(); helper(); }}\n\
                     fn output_match_record() {{ load_linehash_cache(); }}\n\
                     fn edit_record() {{}}\nfn gate_policy() {{}}\nfn gate_step() {{}}\n\
                     fn edit_step() {{}}\nfn gate() {{}}\nfn respond_with_decision() {{}}\n\
                     fn doc_gate() {{}}\nfn doc_gate_in() {{}}\nfn document_verify() {{}}\n\
                     fn helper() {{ let _ = 1; }}\n{extra}"
                ),
            ),
            (
                ADMITTED[0].file.to_owned(),
                "fn load_outline_cache() { let c = rift_git::read_common_dir(g); \
                 let f = outline_cache_dir(&c).join(outline_cache_name(h)); \
                 std::fs::read_to_string(&f); }\n"
                    .to_owned(),
            ),
            (
                ADMITTED[1].file.to_owned(),
                "fn load_linehash_cache() { let c = rift_git::read_common_dir(g); \
                 let f = linehash_cache_dir(&c).join(linehash_cache_name(h)); \
                 std::fs::read(&f); }\n"
                    .to_owned(),
            ),
        ]
    }

    /// ★ The admitted reads are named, and a further family-cache read on
    /// the path is caught.
    #[test]
    fn a_further_family_cache_read_on_the_decision_path_is_caught() {
        let clean = screen_cache_reads(&corpus(""));
        assert!(clean.violations.is_empty(), "{:#?}", clean.violations);
        assert!(
            clean.detail.starts_with("2 family-cache read(s)"),
            "{}",
            clean.detail
        );
        let planted =
            corpus("fn helper() { other(); }\nfn other() { std::fs::read(FAMILY_DIR); }\n");
        let leg = screen_cache_reads(&planted);
        assert!(
            leg.violations
                .iter()
                .any(|v| v.contains("`other` reads a family cache")),
            "{:#?}",
            leg.violations
        );
    }

    /// ★ A line-hash read the leg does not admit — before it was admitted,
    /// or under another name — is caught: its directory is a family store.
    #[test]
    fn an_unadmitted_line_hash_read_is_caught() {
        let mut renamed = corpus("");
        for (_, text) in &mut renamed {
            *text = text.replace("load_linehash_cache", "load_lines");
        }
        let leg = screen_cache_reads(&renamed);
        assert!(
            leg.violations
                .iter()
                .any(|v| v.contains("`load_lines` reads a family cache")),
            "{:#?}",
            leg.violations
        );
    }

    /// Either read moved to another file, or lost its content address, is a
    /// finding; so is either read no longer being reached at all.
    #[test]
    fn a_moved_unkeyed_or_vanished_read_is_caught() {
        for (at, name, key) in [
            (1, "load_outline_cache(); ", "outline_cache_name(h)"),
            (2, "load_linehash_cache(); ", "linehash_cache_name(h)"),
        ] {
            let mut moved = corpus("");
            moved[at].0 = "bins/rift/src/hooks/gate/mod2.rs".to_owned();
            assert!(!screen_cache_reads(&moved).violations.is_empty());
            let mut unkeyed = corpus("");
            unkeyed[at].1 = unkeyed[at].1.replace(key, "\"x\"");
            let leg = screen_cache_reads(&unkeyed);
            let bare = key.trim_end_matches("(h)");
            assert!(
                leg.violations.iter().any(|v| v.contains(bare)),
                "{:#?}",
                leg.violations
            );
            let mut gone = corpus("");
            gone[0].1 = gone[0].1.replace(name, "");
            assert!(!screen_cache_reads(&gone).violations.is_empty());
        }
    }

    /// The real tree's decision path holds exactly the admitted reads.
    #[test]
    fn the_real_decision_path_reads_the_admitted_family_caches() {
        let leg = one_cache_read(&crate::workspace_root()).expect("the corpus reads");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
        assert!(
            leg.detail.starts_with("2 family-cache read(s)"),
            "{}",
            leg.detail
        );
    }
}

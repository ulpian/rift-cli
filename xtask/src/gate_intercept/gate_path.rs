//! The `no-store-on-gate-path` leg: the decision path opens no store and loads
//! no tokenizer, walked as a call graph from the named decision entry points.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use crate::ci_meta::production_half;

use super::text::{calls_within, fn_bodies, mask_code, token_offsets, FnBody};
use super::{is_test_surface, rust_corpus, Leg};

/// The entry points a decision about a tool call is made from, end to end:
/// the guarded step the hook runs first, the gate inside it, the record kept
/// of what the agent itself wrote, the policy and step questions each asks,
/// and the composition of the answer.
///
/// Named rather than inferred, and deliberately NOT the whole hook: the hook
/// that runs these also runs the capture ingest afterwards, which opens a
/// store because that is its job. The decision is the part that answers while
/// the agent waits, and it is the part this leg walks.
///
/// A root that no longer exists is a hard error, not a smaller walk.
const GATE_ROOTS: [&str; 11] = [
    "window_journal_step",
    "read_gate",
    "doc_gate",
    "doc_gate_in",
    "document_verify",
    "edit_record",
    "gate_policy",
    "gate_step",
    "edit_step",
    "gate",
    "respond_with_decision",
];

/// What must not be reachable from a decision, and why each one matters.
///
/// The first two open a projection and the fourth is the store type itself;
/// the third names the recall projection, which nothing on this path has any
/// business reaching for; the fifth reads a revision's bytes back out of a
/// store; the sixth opens the git object database; the seventh is the
/// tokenizer. A decision has a hook's latency budget and the agent waiting on
/// it, so every one of these belongs to the worker that runs afterwards.
///
/// The eighth and ninth are the in-process document parsers. They are linked
/// into the binary, and a decision may still only reach them through the
/// child the binary re-spawns (`doc_helper`): a parser wedged on a hostile
/// file inside the hook process could not be killed, and a container that
/// decompresses past memory would take the hook with it.
const FORBIDDEN_ON_GATE_PATH: [&str; 9] = [
    "open_migrated",
    "open_capture_projection",
    "recall_db",
    "SqliteStore::",
    "basis_bytes",
    "GixSubstrate::open",
    "rift_tokens::",
    "anydoc::",
    "AnydocConverter",
];

/// The crates a decision's call graph is walked through.
const GATE_CRATES: [&str; 2] = ["bins/rift/src/", "crates/rift-agent/src/"];

/// **Nothing on the decision path opens a store or loads the tokenizer.**
///
/// The walk: every function defined in the production half of `bins/rift/src/`
/// and `crates/rift-agent/src/` outside their test surface, resolved by bare
/// name; breadth-first from `GATE_ROOTS`; every reachable body screened for
/// `FORBIDDEN_ON_GATE_PATH`.
///
/// **What this catches and what it cannot.** Names are resolved without types
/// or modules, so two functions sharing a name are one node and the walk is
/// WIDER than the real call graph — the conservative direction for a gate, and
/// the reason a finding here deserves reading before it is dismissed. A call
/// made through a trait object or a stored function pointer is not followed;
/// what covers that gap is the second half of the screen, which looks for the
/// forbidden names as MENTIONS rather than as edges, so a call into another
/// crate (`basis_bytes` lives in one) is caught at the site that writes it even
/// though the walk does not enter that crate. What neither half sees is a
/// forbidden operation reached under a name none of these seven spell; that is
/// a gate on the vocabulary, and the vocabulary is pinned by the list above.
///
/// # Errors
/// If the corpus cannot be read, or a named root no longer exists.
pub fn no_store_on_gate_path(root: &Path) -> Result<Leg, String> {
    let corpus: Vec<(String, String)> = rust_corpus(root)?
        .into_iter()
        .filter(|(rel, _)| GATE_CRATES.iter().any(|c| rel.starts_with(c)))
        .collect();
    if corpus.is_empty() {
        return Err("gate-intercept: no source under the crates a decision is made in".to_owned());
    }
    Ok(screen_gate_path(&corpus)?.0)
}

/// The walk over an explicit corpus, returning the leg AND the names it
/// reached — the seam a planted call is proven through, and the one a test
/// uses to check the walk still arrives where it should. A tightening that
/// quietly empties the walk is a green gate that guards nothing, so the
/// waypoints are asserted rather than assumed.
fn screen_gate_path(corpus: &[(String, String)]) -> Result<(Leg, BTreeSet<String>), String> {
    let mut fns: Vec<FnBody> = Vec::new();
    for (rel, text) in corpus {
        // A `tests/` directory under `src/` is compiled only through its
        // parent's `#[cfg(test)] mod tests;`, so its files carry no marker of
        // their own for the production cut to find. Walking them would put
        // test helpers on the decision path by bare name, and keeping them
        // off it would rest on naming discipline alone.
        if is_test_surface(rel) {
            continue;
        }
        fns.extend(fn_bodies(rel, &mask_code(&production_half(text))));
    }
    let defined: BTreeSet<String> = fns.iter().map(|f| f.name.clone()).collect();
    for root_fn in GATE_ROOTS {
        if !defined.contains(root_fn) {
            return Err(format!(
                "gate-intercept: `{root_fn}` is named as a decision entry point and no longer \
                 exists — re-read the hook and correct the list, never shrink the walk silently"
            ));
        }
    }
    // Breadth-first over bare names. One name can have several definitions;
    // every one of them is walked, which is the wide direction.
    let mut by_name: BTreeMap<&str, Vec<&FnBody>> = BTreeMap::new();
    for f in &fns {
        by_name.entry(f.name.as_str()).or_default().push(f);
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    for root_fn in GATE_ROOTS {
        if seen.insert(root_fn) {
            queue.push_back(root_fn);
        }
    }
    let mut v = Vec::new();
    let mut reached = 0_usize;
    while let Some(name) = queue.pop_front() {
        for f in by_name.get(name).into_iter().flatten() {
            reached += 1;
            for needle in FORBIDDEN_ON_GATE_PATH {
                if !token_offsets(&f.body, needle).is_empty() {
                    v.push(format!(
                        "{}: `{}` reaches `{needle}` — a decision runs inside a hook, with the \
                         agent waiting on it; leave this to the worker that runs afterwards",
                        f.site, f.name
                    ));
                }
            }
            for callee in calls_within(&f.body, &defined) {
                if let Some(next) = by_name.keys().find(|k| ***k == *callee) {
                    if seen.insert(next) {
                        queue.push_back(next);
                    }
                }
            }
        }
    }
    Ok((
        Leg::new(
            "no-store-on-gate-path",
            format!(
                "{} function(s) reachable from {} decision entry point(s), out of {} defined; \
                 none opens a store or loads the tokenizer",
                reached,
                GATE_ROOTS.len(),
                fns.len()
            ),
            v,
        ),
        seen.iter().map(|n| (*n).to_owned()).collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ A store opened on the decision path is caught, wherever on it.
    #[test]
    fn a_planted_store_call_on_the_decision_path_is_caught() {
        let clean = vec![(
            "bins/rift/src/hooks/gate/mod.rs".to_owned(),
            "fn window_journal_step() { read_gate(); }\n\
             fn read_gate() { helper(); }\n\
             fn edit_record() {}\nfn gate_policy() {}\nfn gate_step() {}\nfn edit_step() {}\n\
             fn gate() {}\nfn respond_with_decision() {}\n\
             fn doc_gate() {}\nfn doc_gate_in() {}\nfn document_verify() {}\n\
             fn helper() { let _ = 1; }\n"
                .to_owned(),
        )];
        let (leg, reached) = screen_gate_path(&clean).expect("the walk runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
        assert!(reached.contains("helper"), "the walk arrives: {reached:?}");

        let planted = vec![(
            clean[0].0.clone(),
            clean[0].1.replace(
                "fn helper() { let _ = 1; }",
                "fn helper() { open_migrated(); }",
            ),
        )];
        let (leg, _) = screen_gate_path(&planted).expect("the walk runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("open_migrated"),
            "{:#?}",
            leg.violations
        );
    }

    /// ★ The in-process parsers are linked into the binary and a decision may
    /// still not call them: only the child the binary re-spawns may. A
    /// planted engine call on the decision path is a finding, by either of
    /// the two spellings a caller could reach it under.
    #[test]
    fn a_planted_in_process_parser_on_the_decision_path_is_caught() {
        let clean = vec![(
            "bins/rift/src/hooks/gate/mod.rs".to_owned(),
            "fn window_journal_step() {}\nfn read_gate() {}\nfn edit_record() {}\n\
             fn gate_policy() {}\nfn gate_step() {}\nfn edit_step() {}\nfn gate() {}\n\
             fn respond_with_decision() {}\n\
             fn doc_gate() { doc_gate_in(); }\nfn doc_gate_in() { sidecar(); }\n\
             fn document_verify() {}\n\
             fn sidecar() { let _ = 1; }\n"
                .to_owned(),
        )];
        let (leg, reached) = screen_gate_path(&clean).expect("the walk runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
        assert!(reached.contains("sidecar"), "the walk arrives: {reached:?}");

        for plant in [
            "fn sidecar() { let _ = AnydocConverter.convert(b, e, c); }",
            "fn sidecar() { let _ = anydoc::to_markdown_bytes(b, f); }",
        ] {
            let planted = vec![(
                clean[0].0.clone(),
                clean[0].1.replace("fn sidecar() { let _ = 1; }", plant),
            )];
            let (leg, _) = screen_gate_path(&planted).expect("the walk runs");
            assert_eq!(leg.violations.len(), 1, "{plant}: {:#?}", leg.violations);
            assert!(
                leg.violations[0].contains("sidecar"),
                "{plant}: {:#?}",
                leg.violations
            );
        }
    }

    /// ★ The tokenizer loaded one call below a decision is caught at the
    /// function that loads it, not only when a root spells it itself.
    #[test]
    fn a_planted_tokenizer_load_below_a_decision_entry_point_is_caught() {
        let clean = vec![(
            "bins/rift/src/hooks/gate/mod.rs".to_owned(),
            "fn window_journal_step() {}\nfn read_gate() { budget(); }\nfn edit_record() {}\n\
             fn gate_policy() {}\nfn gate_step() {}\nfn edit_step() {}\nfn gate() {}\n\
             fn respond_with_decision() {}\n\
             fn doc_gate() {}\nfn doc_gate_in() {}\nfn document_verify() {}\n\
             fn budget() { let _ = 1; }\n"
                .to_owned(),
        )];
        let (leg, reached) = screen_gate_path(&clean).expect("the walk runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
        assert!(reached.contains("budget"), "the walk arrives: {reached:?}");

        let planted = vec![(
            clean[0].0.clone(),
            clean[0].1.replace(
                "fn budget() { let _ = 1; }",
                "fn budget() { let _ = rift_tokens::count(t); }",
            ),
        )];
        let (leg, _) = screen_gate_path(&planted).expect("the walk runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("`budget` reaches `rift_tokens::`"),
            "{:#?}",
            leg.violations
        );
    }

    /// ★ The same planting, in the real hook this gate is written for: the
    /// synthetic proof above says the algorithm works, this one says it is
    /// pointed at the code that matters.
    #[test]
    fn a_planted_store_call_in_the_real_hook_is_caught() {
        let root = crate::workspace_root();
        let corpus: Vec<(String, String)> = rust_corpus(&root)
            .expect("the corpus reads")
            .into_iter()
            .filter(|(rel, _)| GATE_CRATES.iter().any(|c| rel.starts_with(c)))
            .collect();
        assert!(
            screen_gate_path(&corpus)
                .expect("the walk runs")
                .0
                .violations
                .is_empty(),
            "the tree is clean before anything is planted"
        );
        let planted: Vec<(String, String)> = corpus
            .into_iter()
            .map(|(rel, text)| {
                if rel != "bins/rift/src/hooks/gate/mod.rs" {
                    return (rel, text);
                }
                let at = text
                    .find("fn read_gate(")
                    .and_then(|i| text[i..].find('{').map(|j| i + j + 1))
                    .expect("the gate has a body");
                let mut planted = text.clone();
                planted.insert_str(at, "\n    let _ = open_migrated();");
                (rel, planted)
            })
            .collect();
        let (leg, _) = screen_gate_path(&planted).expect("the walk runs");
        assert!(
            leg.violations
                .iter()
                .any(|f| f.contains("read_gate") && f.contains("open_migrated")),
            "{:#?}",
            leg.violations
        );
    }

    /// ★ A `tests/` directory under `src/`, compiled only through its
    /// parent's `mod tests;`, is not on any decision path: a helper there that
    /// a root names by accident and that opens a store is not a finding, while
    /// the same function in a production file is.
    #[test]
    fn a_test_directory_gated_by_its_parent_module_is_not_walked() {
        let roots = "fn window_journal_step() { helper(); }\nfn read_gate() {}\n\
                     fn edit_record() {}\nfn gate_policy() {}\nfn gate_step() {}\n\
                     fn edit_step() {}\nfn gate() {}\nfn respond_with_decision() {}\n\
                     fn doc_gate() {}\nfn doc_gate_in() {}\nfn document_verify() {}\n";
        let helper = "fn helper() { open_migrated(); }\n";
        let in_tests = vec![
            ("bins/rift/src/hooks/x/mod.rs".to_owned(), roots.to_owned()),
            (
                "bins/rift/src/hooks/x/tests/helper.rs".to_owned(),
                helper.to_owned(),
            ),
        ];
        let (leg, reached) = screen_gate_path(&in_tests).expect("the walk runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
        assert!(!reached.contains("helper"), "{reached:?}");

        let in_production = vec![
            ("bins/rift/src/hooks/x/mod.rs".to_owned(), roots.to_owned()),
            (
                "bins/rift/src/hooks/x/helper.rs".to_owned(),
                helper.to_owned(),
            ),
        ];
        let (leg, _) = screen_gate_path(&in_production).expect("the walk runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("`helper` reaches `open_migrated`"),
            "{:#?}",
            leg.violations
        );
    }

    /// The walk over the real tree still arrives at the decision's own
    /// vocabulary. Without this, a tightening that shrank the graph to the
    /// roots would pass and guard nothing.
    #[test]
    fn the_walk_reaches_the_functions_a_decision_is_actually_made_of() {
        let root = crate::workspace_root();
        let corpus: Vec<(String, String)> = rust_corpus(&root)
            .expect("the corpus reads")
            .into_iter()
            .filter(|(rel, _)| GATE_CRATES.iter().any(|c| rel.starts_with(c)))
            .collect();
        let (_, reached) = screen_gate_path(&corpus).expect("the walk runs");
        for waypoint in [
            "decide",
            "deny_channel",
            "gate_limits",
            "gate_frame",
            "journal_decision",
            "read_decision_audit",
        ] {
            assert!(reached.contains(waypoint), "the walk reaches {waypoint}");
        }
    }

    /// A root that has been renamed away stops the gate rather than shrinking
    /// the walk in silence.
    #[test]
    fn a_decision_entry_point_that_no_longer_exists_is_a_hard_error() {
        let err = screen_gate_path(&[(
            "bins/rift/src/x.rs".to_owned(),
            "fn other() {}\n".to_owned(),
        )])
        .expect_err("a missing root stops the gate");
        assert!(err.contains("window_journal_step"), "{err}");
    }

    /// A store opened by a function nothing on the decision path calls is not
    /// this leg's business.
    #[test]
    fn a_store_opened_off_the_decision_path_is_not_a_finding() {
        let corpus = vec![(
            "bins/rift/src/hooks/gate/mod.rs".to_owned(),
            "fn window_journal_step() {}\nfn read_gate() {}\nfn edit_record() {}\n\
             fn gate_policy() {}\nfn gate_step() {}\nfn edit_step() {}\nfn gate() {}\n\
             fn respond_with_decision() {}\n\
             fn doc_gate() {}\nfn doc_gate_in() {}\nfn document_verify() {}\n\
             fn ingest() { open_capture_projection(); }\n"
                .to_owned(),
        )];
        let (leg, _) = screen_gate_path(&corpus).expect("the walk runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
    }
}

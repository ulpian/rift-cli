//! The `no-store-on-gate-path` leg: the decision path opens no store and loads
//! no tokenizer, walked as a call graph from the named decision entry points.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use crate::ci_meta::production_half;

use super::text::{
    calls_within, fn_bodies, mask_code, qualified_calls_within, token_offsets, FnBody,
};
use super::{is_test_surface, rust_corpus, Leg};

/// What [`walk_by_name`] reached from its roots.
pub(super) struct Walk<'a> {
    /// Definitions visited, counting every definition of a shared name.
    pub(super) reached: usize,
    /// The names the walk arrived at, its roots included.
    pub(super) names: BTreeSet<&'a str>,
    /// Each reached definition that mentions a forbidden name, with the name.
    pub(super) hits: Vec<(&'a FnBody, &'static str)>,
}

/// The crates a decision's call graph is walked through, and the crates the
/// hook and worker entry points are walked through for the same reason: the
/// hooks and the worker are defined there.
pub(super) const GATE_CRATES: [&str; 2] = ["bins/rift/src/", "crates/rift-agent/src/"];

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
pub(super) const GATE_ROOTS: [&str; 11] = [
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

/// **Nothing on the decision path opens a store or loads the tokenizer.**
///
/// The walk: every function defined in the production half of `bins/rift/src/`
/// and `crates/rift-agent/src/` outside their test surface and outside every
/// module declared for tests, resolved by bare name; breadth-first from
/// `GATE_ROOTS`; every reachable body screened for `FORBIDDEN_ON_GATE_PATH`.
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
    let fns = production_fns(corpus);
    if let Some(root_fn) = missing_root(&fns, &GATE_ROOTS) {
        return Err(format!(
            "gate-intercept: `{root_fn}` is named as a decision entry point and no longer \
             exists — re-read the hook and correct the list, never shrink the walk silently"
        ));
    }
    let walk = walk_by_name(&fns, &GATE_ROOTS, &FORBIDDEN_ON_GATE_PATH);
    let v = walk
        .hits
        .iter()
        .map(|(f, needle)| {
            format!(
                "{}: `{}` reaches `{needle}` — a decision runs inside a hook, with the \
                 agent waiting on it; leave this to the worker that runs afterwards",
                f.site, f.name
            )
        })
        .collect();
    Ok((
        Leg::new(
            "no-store-on-gate-path",
            format!(
                "{} function(s) reachable from {} decision entry point(s), out of {} defined; \
                 none opens a store or loads the tokenizer",
                walk.reached,
                GATE_ROOTS.len(),
                fns.len()
            ),
            v,
        ),
        walk.names.iter().map(|n| (*n).to_owned()).collect(),
    ))
}

/// Breadth-first over bare names from `roots`, screening every reached body
/// for a mention of each of `forbidden`.
///
/// One name can have several definitions; every one of them is walked, which
/// is the wide direction.
pub(super) fn walk_by_name<'a>(
    fns: &'a [FnBody],
    roots: &[&str],
    forbidden: &[&'static str],
) -> Walk<'a> {
    let by_name = definitions_by_name(fns);
    let defined: BTreeSet<String> = by_name.keys().map(|name| (*name).to_owned()).collect();
    walk(fns, roots, forbidden, |caller| {
        calls_within(&caller.body, &defined)
            .iter()
            .flat_map(|callee| by_name.get(callee.as_str()).into_iter().flatten().copied())
            .collect()
    })
}

/// [`walk_by_name`], except that a call qualified by a module
/// (`crate::search_measure::run(`) reaches only the definitions filed under
/// that module.
///
/// Every subcommand's entry point is called `run`, so a hook that runs one
/// module's `run` would otherwise reach all of them, and with them every
/// command the binary has. The narrowing errs wide wherever it cannot be
/// sure: a bare or `crate::`/`super::`-qualified call reaches every
/// definition of the name, and so does a qualifier no walked file is filed
/// under (a re-export from elsewhere, or another crate).
pub(super) fn walk_by_qualified_name<'a>(
    fns: &'a [FnBody],
    roots: &[&str],
    forbidden: &[&'static str],
) -> Walk<'a> {
    let by_name = definitions_by_name(fns);
    let defined: BTreeSet<String> = by_name.keys().map(|name| (*name).to_owned()).collect();
    walk(fns, roots, forbidden, |caller| {
        let mut callees = Vec::new();
        for (callee, module) in qualified_calls_within(&caller.body, &defined) {
            let all = by_name.get(callee).map_or(&[][..], Vec::as_slice);
            let in_module: Vec<&FnBody> = module
                .map(|module| {
                    all.iter()
                        .copied()
                        .filter(|f| filed_under(&f.site, module))
                        .collect()
                })
                .unwrap_or_default();
            if in_module.is_empty() {
                callees.extend(all.iter().copied());
            } else {
                callees.extend(in_module);
            }
        }
        callees
    })
}

/// The first of `roots` that `fns` no longer defines.
pub(super) fn missing_root<'r>(fns: &[FnBody], roots: &[&'r str]) -> Option<&'r str> {
    let defined: BTreeSet<&str> = fns.iter().map(|f| f.name.as_str()).collect();
    roots
        .iter()
        .copied()
        .find(|root_fn| !defined.contains(root_fn))
}

/// Every definition, grouped under its bare name.
fn definitions_by_name(fns: &[FnBody]) -> BTreeMap<&str, Vec<&FnBody>> {
    let mut by_name: BTreeMap<&str, Vec<&FnBody>> = BTreeMap::new();
    for f in fns {
        by_name.entry(f.name.as_str()).or_default().push(f);
    }
    by_name
}

/// The breadth-first walk both resolutions share: every definition of every
/// root first, then whatever `callees` says each reached definition calls,
/// each definition visited once.
fn walk<'a>(
    fns: &'a [FnBody],
    roots: &[&str],
    forbidden: &[&'static str],
    callees: impl Fn(&'a FnBody) -> Vec<&'a FnBody>,
) -> Walk<'a> {
    let mut walk = Walk {
        reached: 0,
        names: BTreeSet::new(),
        hits: Vec::new(),
    };
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let mut queue: VecDeque<&FnBody> = VecDeque::new();
    for root_fn in roots {
        for f in fns.iter().filter(|f| f.name == *root_fn) {
            if visited.insert(&f.site) {
                queue.push_back(f);
            }
        }
    }
    while let Some(f) = queue.pop_front() {
        walk.reached += 1;
        walk.names.insert(&f.name);
        for needle in forbidden {
            if !token_offsets(&f.body, needle).is_empty() {
                walk.hits.push((f, needle));
            }
        }
        for next in callees(f) {
            if visited.insert(&next.site) {
                queue.push_back(next);
            }
        }
    }
    walk
}

/// True when the definition at `site` (`path:line`) lives in the module
/// `module`: its file is `module.rs` or sits below a `module/` directory.
fn filed_under(site: &str, module: &str) -> bool {
    let path = site.rsplit_once(':').map_or(site, |(path, _)| path);
    path.ends_with(&format!("/{module}.rs")) || path.contains(&format!("/{module}/"))
}

/// Every function the walk may reach: the production half of each file that
/// is neither on the test surface nor built only for tests.
///
/// A file under `src/` is compiled only through its parent's `mod` line, so a
/// module its parent declares under `#[cfg(test)]` carries no marker of its
/// own for the production cut to find. That declaration is what makes the file
/// test code to the compiler, and it is what is read here: a file's name says
/// only what its author meant, and a seeder called something other than
/// `tests` or `fixtures` would otherwise put test helpers on the decision path
/// by bare name.
pub(super) fn production_fns(corpus: &[(String, String)]) -> Vec<FnBody> {
    let for_tests = declared_for_tests(corpus);
    let mut fns = Vec::new();
    for (rel, text) in corpus {
        if is_test_surface(rel) || for_tests.contains(rel.as_str()) {
            continue;
        }
        fns.extend(fn_bodies(rel, &mask_code(&production_half(text))));
    }
    fns
}

/// The files in `corpus` the compiler builds only for tests: each one whose
/// module, or any module above it, is declared under `#[cfg(test)]` in its
/// parent.
///
/// Read from the whole parent, not its production half: the declaration is
/// itself the column-zero marker the production cut stops at. A parent that
/// is not in the corpus ends the climb, and the file is walked.
fn declared_for_tests(corpus: &[(String, String)]) -> BTreeSet<&str> {
    let masked: BTreeMap<&str, String> = corpus
        .iter()
        .map(|(rel, text)| (rel.as_str(), mask_code(text)))
        .collect();
    let mut out = BTreeSet::new();
    for (rel, _) in corpus {
        let mut module = rel.as_str();
        while let Some((parent, name)) = parent_declaration(&masked, module) {
            let parent_text = masked.get(parent).map_or("", String::as_str);
            if declared_under_cfg_test(parent_text, name) {
                out.insert(rel.as_str());
                break;
            }
            module = parent;
        }
    }
    out
}

/// The file that declares the module `rel` defines, and the module's name.
///
/// The parent of `a/b/x.rs` or `a/b/x/mod.rs` is `a/b/mod.rs` or `a/b.rs`,
/// or the crate root `lib.rs`/`main.rs` when `a/b` is a `src` directory. No
/// `#[path]` attribute appears in the crates this leg walks, so a module is
/// found only where the compiler looks by default.
fn parent_declaration<'a>(
    masked: &BTreeMap<&'a str, String>,
    rel: &'a str,
) -> Option<(&'a str, &'a str)> {
    let module = rel.strip_suffix(".rs")?;
    let module = module.strip_suffix("/mod").unwrap_or(module);
    let (dir, name) = module.rsplit_once('/')?;
    let in_src = dir.ends_with("/src") || dir == "src";
    if in_src && (name == "lib" || name == "main") {
        return None;
    }
    let mut candidates = vec![format!("{dir}/mod.rs"), format!("{dir}.rs")];
    if in_src {
        candidates.extend([format!("{dir}/lib.rs"), format!("{dir}/main.rs")]);
    }
    let parent = candidates
        .iter()
        .find_map(|candidate| masked.get_key_value(candidate.as_str()))?;
    Some((*parent.0, name))
}

/// True when `masked` declares `mod <name>;` and every declaration of it
/// carries `#[cfg(test)]`.
///
/// A declaration's attributes are those on its own line and the unbroken run
/// of lines directly above it that hold only attributes or nothing (a comment
/// is already blank in masked text), which is the run Rust attaches to the
/// item. Any other line ends the run, so a `#[cfg(test)]` on an item above
/// does not reach the declaration, and neither does an attribute this reading
/// cannot follow (one spread over several lines): the file is then walked,
/// which is the direction a gate should err in.
fn declared_under_cfg_test(masked: &str, name: &str) -> bool {
    let lines: Vec<&str> = masked.lines().collect();
    let mut declared = false;
    for (idx, line) in lines.iter().enumerate() {
        let Some(mut attributes) = module_declaration(line, name) else {
            continue;
        };
        declared = true;
        for above in lines.get(..idx).unwrap_or_default().iter().rev() {
            let (line_attributes, rest) = split_attributes(above);
            if !rest.is_empty() {
                break;
            }
            attributes.extend(line_attributes);
        }
        let gated = attributes
            .iter()
            .any(|attr| attr.split_whitespace().collect::<String>() == "#[cfg(test)]");
        if !gated {
            return false;
        }
    }
    declared
}

/// The attributes written on `line` when the line declares `mod <name>;`,
/// with or without a visibility.
fn module_declaration<'a>(line: &'a str, name: &str) -> Option<Vec<&'a str>> {
    let (attributes, rest) = split_attributes(line);
    let rest = match rest.strip_prefix("pub") {
        Some(after) => match after.strip_prefix('(') {
            Some(inner) => inner.split_once(')')?.1.trim_start(),
            None => after.trim_start(),
        },
        None => rest,
    };
    let declared = rest.strip_prefix("mod ")?.strip_suffix(';')?.trim();
    if declared != name {
        return None;
    }
    Some(attributes)
}

/// The outer attributes that open `line`, and what follows them.
fn split_attributes(line: &str) -> (Vec<&str>, &str) {
    let mut attributes = Vec::new();
    let mut rest = line.trim();
    while rest.starts_with("#[") {
        let Some(end) = rest.find(']') else { break };
        let (attribute, tail) = rest.split_at(end + 1);
        attributes.push(attribute);
        rest = tail.trim_start();
    }
    (attributes, rest)
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

    /// The roots of a synthetic walk, with `window_journal_step` calling
    /// `helper`, followed by `declaration` as the file's last lines.
    fn roots_declaring(declaration: &str) -> String {
        format!(
            "fn window_journal_step() {{ helper(); }}\nfn read_gate() {{}}\n\
             fn edit_record() {{}}\nfn gate_policy() {{}}\nfn gate_step() {{}}\n\
             fn edit_step() {{}}\nfn gate() {{}}\nfn respond_with_decision() {{}}\n\
             fn doc_gate() {{}}\nfn doc_gate_in() {{}}\nfn document_verify() {{}}\n\
             {declaration}\n"
        )
    }

    /// ★ A file is test code because its parent declares it under
    /// `#[cfg(test)]`, whatever it is called: a store-opening helper in such a
    /// module is not on the decision path, nor is one in a module beneath it,
    /// while the same helper in a module declared plainly is a finding.
    #[test]
    fn a_module_its_parent_declares_for_tests_is_not_walked() {
        let helper = "fn helper() { open_migrated(); }\n";
        for (declaration, child, gated) in [
            ("#[cfg(test)]\nmod fixtures;", "fixtures.rs", true),
            ("mod fixtures;", "fixtures.rs", false),
            ("#[cfg(test)]\nmod seeds;", "seeds/deep.rs", true),
            ("mod seeds;", "seeds/deep.rs", false),
        ] {
            let mut corpus = vec![
                (
                    "bins/rift/src/hooks/x/mod.rs".to_owned(),
                    roots_declaring(declaration),
                ),
                (format!("bins/rift/src/hooks/x/{child}"), helper.to_owned()),
            ];
            if child.starts_with("seeds/") {
                corpus.push((
                    "bins/rift/src/hooks/x/seeds.rs".to_owned(),
                    "mod deep;\n".to_owned(),
                ));
            }
            let (leg, reached) = screen_gate_path(&corpus).expect("the walk runs");
            if gated {
                assert!(leg.violations.is_empty(), "{child}: {:#?}", leg.violations);
                assert!(!reached.contains("helper"), "{child}: {reached:?}");
            } else {
                assert_eq!(leg.violations.len(), 1, "{child}: {:#?}", leg.violations);
                assert!(
                    leg.violations[0].contains("`helper` reaches `open_migrated`"),
                    "{child}: {:#?}",
                    leg.violations
                );
            }
        }
    }

    /// ★ The attributes a declaration carries are the run of attribute,
    /// comment and blank lines directly above it, plus any on its own line; a
    /// `#[cfg(test)]` that belongs to some other item above does not reach it.
    #[test]
    fn a_declaration_is_gated_only_by_the_attributes_directly_above_it() {
        let helper = "fn helper() { open_migrated(); }\n";
        for (declaration, gated) in [
            ("#[cfg(test)]\n#[allow(dead_code)]\nmod fixtures;", true),
            ("#[cfg(test)]\n/// The seeders.\nmod fixtures;", true),
            ("#[cfg(test)]\n\n// Why it is here.\n\nmod fixtures;", true),
            ("#[cfg(test)] pub(crate) mod fixtures;", true),
            ("#[cfg(test)]\nfn seed() {}\nmod fixtures;", false),
            ("#[cfg(test)]\nuse std::fs;\n\nmod fixtures;", false),
            ("#[cfg(all(test, unix))]\nmod fixtures;", false),
            ("// #[cfg(test)]\nmod fixtures;", false),
        ] {
            let corpus = vec![
                (
                    "bins/rift/src/hooks/x/mod.rs".to_owned(),
                    roots_declaring(declaration),
                ),
                (
                    "bins/rift/src/hooks/x/fixtures.rs".to_owned(),
                    helper.to_owned(),
                ),
            ];
            let (leg, _) = screen_gate_path(&corpus).expect("the walk runs");
            assert_eq!(
                leg.violations.is_empty(),
                gated,
                "{declaration:?}: {:#?}",
                leg.violations
            );
        }
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

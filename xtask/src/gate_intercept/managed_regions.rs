//! The `regions-at-boundaries` leg: no hook and no detached worker reaches
//! the writer of a Rift-managed region, walked as a call graph from the named
//! hook and worker entry points.
//!
//! A separate leg rather than two more names on the decision walk: that walk
//! starts at the decision alone and deliberately leaves out the rest of the
//! hook, while a managed region rewritten by any part of a hook, or by the
//! worker a hook detaches, is the same mid-session change to text an agent
//! holds in its prompt prefix.

use std::path::Path;

use super::gate_path::{missing_root, production_fns, walk_by_qualified_name, GATE_CRATES};
use super::{rust_corpus, Leg};

/// Every process entry a hook or the worker a hook detaches starts from: the
/// agent hooks, the git hooks (through `rift hooks git` and through a direct
/// hook link), the audit hook, the document converter a hook re-spawns, the
/// detached worker, and the queue drain it and the inline drains share.
///
/// The whole hook rather than its decision, because a region rewritten after
/// the answer is as mid-session as one rewritten before it. A root that no
/// longer exists is a hard error, not a smaller walk.
const HOOK_AND_WORKER_ROOTS: [&str; 7] = [
    "run_agent_hook",
    "run_git_hook",
    "run_direct_git_hook",
    "run_memd_hook",
    "run_doc_helper",
    "run_capture_worker",
    "drain_queues",
];

/// The functions that write a managed region: the installer that renders
/// and writes it, and the removal that cuts it back out. Adoption's
/// `recall_skill_present` only reads, and is not here on purpose.
const REGION_WRITERS: [&str; 2] = ["install_recall_skill", "remove_recall_skill"];

/// **A managed region changes only at a boundary.**
///
/// The walk is the decision leg's, narrowed at module-qualified calls
/// (`walk_by_qualified_name`): every production function in `bins/rift/src/`
/// and `crates/rift-agent/src/`, resolved by name, breadth-first from
/// `HOOK_AND_WORKER_ROOTS`, each reached body screened for a mention of a
/// region writer. The writers live in
/// `rift-agent-install`, which the walk does not enter; it does not need to,
/// because the screen finds the call where a walked function writes it, and
/// every call of either writer is in the binary.
///
/// # Errors
/// If the corpus cannot be read, or a named root no longer exists.
pub fn regions_at_boundaries(root: &Path) -> Result<Leg, String> {
    let corpus: Vec<(String, String)> = rust_corpus(root)?
        .into_iter()
        .filter(|(rel, _)| GATE_CRATES.iter().any(|c| rel.starts_with(c)))
        .collect();
    screen_region_writers(&corpus)
}

/// The walk over an explicit corpus, the seam a planted call is proven
/// through.
fn screen_region_writers(corpus: &[(String, String)]) -> Result<Leg, String> {
    let fns = production_fns(corpus);
    if let Some(root_fn) = missing_root(&fns, &HOOK_AND_WORKER_ROOTS) {
        return Err(format!(
            "gate-intercept: `{root_fn}` is named as a hook or worker entry point and no \
             longer exists — re-read the hook dispatch and correct the list, never shrink \
             the walk silently"
        ));
    }
    let walk = walk_by_qualified_name(&fns, &HOOK_AND_WORKER_ROOTS, &REGION_WRITERS);
    let violations = walk
        .hits
        .iter()
        .map(|(f, writer)| {
            format!(
                "{}: `{}` reaches `{writer}` — a managed region is written at init, enable \
                 and disable, never by a hook or worker while a session holds it",
                f.site, f.name
            )
        })
        .collect();
    Ok(Leg::new(
        "regions-at-boundaries",
        format!(
            "{} function(s) reachable from {} hook and worker entry point(s), out of {} \
             defined; none writes a managed region",
            walk.reached,
            HOOK_AND_WORKER_ROOTS.len(),
            fns.len()
        ),
        violations,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every root defined, the agent hook calling `helper`, then `rest`.
    fn roots_then(rest: &str) -> Vec<(String, String)> {
        vec![(
            "bins/rift/src/hooks/mod.rs".to_owned(),
            format!(
                "fn run_agent_hook() {{ helper(); }}\nfn run_git_hook() {{}}\n\
                 fn run_direct_git_hook() {{}}\nfn run_memd_hook() {{}}\n\
                 fn run_doc_helper() {{}}\nfn run_capture_worker() {{}}\n\
                 fn drain_queues() {{}}\n{rest}\n"
            ),
        )]
    }

    /// ★ A hook that reaches the region writer through a helper is caught at
    /// the helper that writes the call, and the same helper off every hook
    /// path is not a finding.
    #[test]
    fn a_hook_that_calls_the_region_writer_through_a_helper_is_caught() {
        let clean = roots_then("fn helper() { let _ = 1; }");
        let leg = screen_region_writers(&clean).expect("the walk runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);

        let planted = roots_then(
            "fn helper() { refresh(); }\n\
             fn refresh() { native::install_recall_skill(repo, token, body); }",
        );
        let leg = screen_region_writers(&planted).expect("the walk runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("`refresh` reaches `install_recall_skill`"),
            "{:#?}",
            leg.violations
        );

        let off_path = roots_then(
            "fn helper() {}\n\
             fn recall_init() { native::install_recall_skill(repo, token, body); }",
        );
        let leg = screen_region_writers(&off_path).expect("the walk runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
    }

    /// ★ The detached worker is a root too: a removal it reaches is as much a
    /// mid-session change as an install.
    #[test]
    fn a_worker_that_removes_a_region_is_caught() {
        let mut corpus = roots_then("fn helper() {}");
        corpus[0].1 = corpus[0].1.replace(
            "fn drain_queues() {}",
            "fn drain_queues() { native::remove_recall_skill(repo, token); }",
        );
        let leg = screen_region_writers(&corpus).expect("the walk runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("`drain_queues` reaches `remove_recall_skill`"),
            "{:#?}",
            leg.violations
        );
    }

    /// ★ A call qualified by a module reaches that module's definition and
    /// no other of the same name, while a bare call still reaches every one:
    /// the narrowing closes the collision between subcommand entry points
    /// without opening a path a real call takes.
    #[test]
    fn a_module_qualified_call_reaches_only_that_modules_definition() {
        let modules = |call: &str| {
            let mut corpus = roots_then(&format!("fn helper() {{ {call} }}"));
            corpus.push((
                "bins/rift/src/search_measure.rs".to_owned(),
                "fn run() { let _ = 1; }\n".to_owned(),
            ));
            corpus.push((
                "bins/rift/src/recall/mod.rs".to_owned(),
                "fn run() { native::install_recall_skill(repo, token, body); }\n".to_owned(),
            ));
            screen_region_writers(&corpus).expect("the walk runs")
        };
        let qualified = modules("crate::search_measure::run();");
        assert!(
            qualified.violations.is_empty(),
            "{:#?}",
            qualified.violations
        );

        for call in ["crate::recall::run();", "run();", "crate::run();"] {
            let leg = modules(call);
            assert_eq!(leg.violations.len(), 1, "{call}: {:#?}", leg.violations);
            assert!(
                leg.violations[0].starts_with("bins/rift/src/recall/mod.rs"),
                "{call}: {:#?}",
                leg.violations
            );
        }
    }

    /// A root that has been renamed away stops the gate rather than shrinking
    /// the walk in silence.
    #[test]
    fn a_hook_entry_point_that_no_longer_exists_is_a_hard_error() {
        let err = screen_region_writers(&[(
            "bins/rift/src/x.rs".to_owned(),
            "fn other() {}\n".to_owned(),
        )])
        .expect_err("a missing root stops the gate");
        assert!(err.contains("run_agent_hook"), "{err}");
    }

    /// ★ The same planting in the real agent hook: the synthetic proof says
    /// the walk works, this one says it is pointed at the hook that matters.
    #[test]
    fn a_region_write_planted_in_the_real_agent_hook_is_caught() {
        let corpus: Vec<(String, String)> = rust_corpus(&crate::workspace_root())
            .expect("the corpus reads")
            .into_iter()
            .filter(|(rel, _)| GATE_CRATES.iter().any(|c| rel.starts_with(c)))
            .collect();
        let clean = screen_region_writers(&corpus).expect("the walk runs");
        assert!(clean.violations.is_empty(), "{:#?}", clean.violations);

        let planted: Vec<(String, String)> = corpus
            .into_iter()
            .map(|(rel, text)| {
                let Some(at) = text.find("fn run_agent_hook(") else {
                    return (rel, text);
                };
                let open = at + text[at..].find('{').expect("the hook has a body") + 1;
                let mut planted = text.clone();
                planted.insert_str(open, "\n    let _ = install_recall_skill(r, t, b);");
                (rel, planted)
            })
            .collect();
        let leg = screen_region_writers(&planted).expect("the walk runs");
        assert!(
            leg.violations
                .iter()
                .any(|f| f.contains("`run_agent_hook` reaches `install_recall_skill`")),
            "{:#?}",
            leg.violations
        );
    }
}

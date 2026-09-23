//! **Which calls are a search for file names, and which are not.**
//!
//! The reading half of the name-search arm: what the agent wrote, read through
//! the family's own pinned row into the one shape an index can answer — a
//! directory, a pattern, and which of the two ways the pattern is matched.
//! Every refusal here leaves the call exactly as it was written, which is why
//! this reading is kept apart from the answer it feeds.

#[cfg(feature = "search")]
use std::path::Path;

use rift_shadow::hooks::AdapterRegistry;

#[cfg(feature = "search")]
use super::answer::answer_name_search;
#[cfg(feature = "search")]
use super::cap::search_args;
#[cfg(feature = "search")]
use super::window::SearchWindow;
use super::SearchOutcome;

/// A search for file **names** that Rift can answer from the index it holds.
#[cfg(feature = "search")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NameSearch {
    /// The repository-relative directory the search is scoped to; empty for
    /// the whole tree.
    pub(super) dir: String,
    /// The pattern, exactly as the agent wrote it.
    pub(super) pattern: String,
    /// Which native call this is, which decides two separate things: how the
    /// pattern is matched (see [`NameSearch::whole_path`]) and what ceiling
    /// the listing it stands in for is priced at (see
    /// `rift_recall_serve::SearchCeiling`). One fact rather than two, so a
    /// search can never be matched as one shape and priced as the other.
    pub(super) shape: rift_recall_serve::ListingShape,
}

#[cfg(feature = "search")]
impl NameSearch {
    /// Which native call this is — the axis its declared baseline is priced
    /// on.
    pub(super) fn shape(&self) -> rift_recall_serve::ListingShape {
        self.shape
    }

    /// Whether the pattern is matched against the whole repository-relative
    /// path — which is what a glob means — or against the file's name alone,
    /// which is what the listing grammar's `-name` means. The two are
    /// different questions and answering one with the other would list files
    /// the agent did not ask for.
    fn whole_path(&self) -> bool {
        matches!(self.shape, rift_recall_serve::ListingShape::Glob)
    }

    /// The identity of one name search: what it looks in, what it looks for,
    /// and which of the two ways it is matched.
    ///
    /// A digest, for the reason [`search_key`](super::cap::search_key) is one:
    /// it becomes a file name, and the pattern is the agent's own text, which
    /// Rift never writes down in the clear.
    pub(super) fn key(&self) -> String {
        let mut bytes = Vec::with_capacity(self.dir.len() + self.pattern.len() + 3);
        bytes.push(u8::from(self.whole_path()));
        bytes.extend_from_slice(self.dir.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(self.pattern.as_bytes());
        rift_hashchain::leaf_hash(&bytes).to_hex()
    }

    /// Whether one indexed path is one of this search's matches.
    pub(super) fn matches(&self, path: &str) -> bool {
        let Some(rest) = self.under_scope(path) else {
            return false;
        };
        if self.whole_path() {
            return rift_config::glob_match(&self.pattern, rest);
        }
        // The listing grammar's pattern is a file name, and `find -name` walks
        // the whole subtree: every depth, matched on the last segment.
        rest.rsplit('/')
            .next()
            .is_some_and(|name| rift_config::glob_match(&self.pattern, name))
    }

    /// The path relative to this search's directory, or `None` when it is not
    /// under it at all.
    fn under_scope<'a>(&self, path: &'a str) -> Option<&'a str> {
        if self.dir.is_empty() {
            return Some(path);
        }
        path.strip_prefix(&self.dir)?.strip_prefix('/')
    }
}

/// The name search this call is, or `None` when it is not one this gate can
/// answer.
///
/// Every refusal leaves the call untouched, and each closes a way an answer
/// could be about something other than what the agent asked for:
///
/// * a **glob** carrying an argument its pinned row does not name is not a
///   glob over the whole tree — the family's own page names the pattern and
///   nothing else, so a call that scopes itself with an argument no artifact
///   pins is a call this gate does not understand;
/// * a **listing with no pattern** is a request for the contents of a
///   directory, and directories are exactly what an index of files cannot
///   enumerate: it would answer `ls` with no subdirectory names and a total
///   that counted none of them;
/// * a directory this checkout has no name for belongs to no checkout this
///   gate can answer about;
/// * a pattern longer than the matcher's own bound is a payload rather than a
///   pattern, and an empty one is not a search.
#[cfg(feature = "search")]
fn name_search(
    agent: &rift_core::AgentId,
    payload: &serde_json::Value,
    facts: &rift_agent::HookFacts,
    kind: &rift_agent::SearchKind,
    work_tree: &Path,
) -> Option<NameSearch> {
    use rift_agent::SearchKind;
    let bounded = |dir: String, pattern: &str, shape: rift_recall_serve::ListingShape| {
        (!pattern.is_empty() && pattern.len() <= rift_recall_search::query::MAX_GLOB_BYTES).then(
            || NameSearch {
                dir,
                pattern: pattern.to_owned(),
                shape,
            },
        )
    };
    match kind {
        SearchKind::Glob { pattern } => {
            let row = rift_agent::tool_fields::tool_field_row(agent, facts.tool_name.as_deref()?)?;
            let named = row.fact("pattern")?;
            let args = search_args(row, payload)?;
            if args.keys().any(|key| key != named) {
                return None;
            }
            bounded(
                String::new(),
                pattern,
                rift_recall_serve::ListingShape::Glob,
            )
        }
        SearchKind::Listing { dir, pattern } => {
            let root = work_tree.canonicalize().ok()?;
            let scope = scope_dir(&root, dir, crate::hooks::payload_cwd(facts).as_deref())?;
            bounded(
                scope,
                pattern.as_deref()?,
                rift_recall_serve::ListingShape::Listing,
            )
        }
        SearchKind::Grep { .. } => None,
    }
}

/// One directory, as this checkout names it: repository-relative, `/`-spelled,
/// empty for the root itself.
///
/// One spelling, for the reason the window journal has one: a relative name
/// resolved against some other directory, or a symlinked prefix, would make
/// two answers about two different trees look like one. A relative name is
/// resolved against the directory the payload itself states and against
/// nothing else — the hook process's own is where the client launched it, not
/// where the agent stood.
#[cfg(feature = "search")]
fn scope_dir(root: &Path, named: &str, cwd: Option<&Path>) -> Option<String> {
    let dir = Path::new(named);
    let joined;
    let dir = if dir.is_absolute() {
        dir
    } else {
        joined = cwd.filter(|at| at.is_absolute())?.join(dir);
        joined.as_path()
    };
    let canonical = dir.canonicalize().ok()?;
    if !canonical.is_dir() {
        return None;
    }
    // The root itself is the empty scope, which the chokepoint spells as no
    // name at all; a directory outside the root is no scope.
    canonical.strip_prefix(root).ok()?;
    Some(rift_recall_substrate::worktree::repo_relative_slash(root, &canonical).unwrap_or_default())
}

/// The name-search arm, or `None` when this call is not one — which is every
/// call in a build without the search seam, where there is no index to answer
/// from and the search runs exactly as it was written.
#[cfg(feature = "search")]
pub(super) fn name_answer(
    call: &super::SearchCall<'_>,
    registry: &AdapterRegistry,
    policy: &rift_recall_policy::ServePolicy,
    facts: &rift_agent::HookFacts,
) -> Option<SearchOutcome> {
    let raw = call.raw;
    let request = registry
        .get(&raw.agent)?
        .search_request(&raw.verb, &raw.payload)?;
    let named = name_search(
        &raw.agent,
        &raw.payload,
        facts,
        &request.kind,
        call.work_tree,
    )?;
    // A decision, and only a decision: an answered call never runs, so there
    // is no result to check it against, and a result that arrives anyway is
    // the repeat this window already allows.
    if !matches!(
        crate::hooks::gate_step(&raw.agent, &raw.verb, &raw.payload),
        Some(crate::hooks::GateStep::Decide)
    ) {
        return Some(SearchOutcome::default());
    }
    let window = SearchWindow::at(call.work_tree, &raw.session, facts.child_session.as_deref());
    Some(answer_name_search(call, policy, &window, &named))
}

/// The same arm in a build without the search seam.
#[cfg(not(feature = "search"))]
pub(super) fn name_answer(
    _call: &super::SearchCall<'_>,
    _registry: &AdapterRegistry,
    _policy: &rift_recall_policy::ServePolicy,
    _facts: &rift_agent::HookFacts,
) -> Option<SearchOutcome> {
    None
}

#[cfg(feature = "search")]
#[cfg(test)]
mod tests {
    use std::path::Path;

    use rift_shadow::hooks::RawHook;
    use serde_json::json;

    use crate::search_gate::tests::{frame_of, gate, hook, indexed, levers, markers};

    /// A checkout of thirty leaves under one deep directory, and one file
    /// outside it, indexed the way the detached worker builds it.
    fn indexed_leaves() -> tempfile::TempDir {
        let mut files: Vec<(String, String)> = (0..30)
            .map(|n| {
                (
                    format!("src/deep/nested{n}/leaf.rs"),
                    "fn leaf() {}\n".to_owned(),
                )
            })
            .collect();
        files.push(("tests/c.rs".to_owned(), "fn c() {}\n".to_owned()));
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        indexed(&borrowed)
    }

    /// One shell call, as an agent standing in `cwd` would have written it.
    fn shell(cwd: &Path, command: &str) -> serde_json::Value {
        json!({
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "cwd": cwd.to_str().expect("a utf-8 path"),
        })
    }

    /// **A listing is answered inside the directory it named**, at every depth
    /// under it and nowhere outside it.
    #[cfg(feature = "search")]
    #[test]
    fn a_listing_is_answered_within_the_directory_it_named() {
        let tmp = indexed_leaves();
        let cwd = tmp.path().canonicalize().expect("a real path");
        let frame = frame_of(&gate(
            &hook("PreToolUse", 1, shell(&cwd, "find src -name '*.rs'")),
            tmp.path(),
            &levers(),
        ));
        assert!(
            frame.starts_with("[rift] Search answered: 8 of 30 paths matching"),
            "the subtree it named, and nothing outside it: {frame}"
        );
        assert!(
            frame.contains("src/deep/nested0/leaf.rs"),
            "every depth: {frame}"
        );
        assert!(!frame.contains("tests/c.rs"), "and no other: {frame}");
    }

    /// **The same search, as each other family spells it.**
    ///
    /// The other two families, on the shapes their own pinned rows name: one
    /// nests its arguments and takes the pattern as an argument, one spells the
    /// same search as a command line.
    #[cfg(feature = "search")]
    #[test]
    fn each_family_names_the_same_listing_its_own_way() {
        let tmp = indexed_leaves();
        let cwd = tmp.path().canonicalize().expect("a real path");
        let elsewhere = [
            (
                rift_core::AgentId::CODEX,
                shell(&cwd, "find src -name '*.rs'"),
            ),
            (
                rift_core::AgentId::ANTIGRAVITY,
                json!({
                    "toolCall": {
                        "name": "find_by_name",
                        "args": {
                            "SearchDirectory": cwd.join("src").to_str().expect("a utf-8 path"),
                            "Pattern": "*.rs",
                        },
                    },
                }),
            ),
        ];
        for (agent, payload) in elsewhere {
            let call = RawHook {
                agent: agent.clone(),
                session: format!("s-{}", agent.as_str()),
                ..hook("PreToolUse", 1, payload)
            };
            let levers = rift_recall_policy::resolve(
                &rift_recall_policy::ServePolicy::default(),
                agent.as_str(),
                crate::recall::family_capability(&agent),
            );
            let frame = frame_of(&gate(&call, tmp.path(), &levers));
            assert!(
                frame.starts_with("[rift] Search answered: 8 of 30 paths matching"),
                "{} names the same search another way: {frame}",
                agent.as_str()
            );
        }
    }

    /// **A listing that named no pattern is not answered at all.**
    ///
    /// A directory listing enumerates directories too, and an index of files
    /// cannot: answering `ls` from one would drop every subdirectory name and
    /// state a total that counted none of them.
    #[cfg(feature = "search")]
    #[test]
    fn a_listing_that_named_no_pattern_is_not_answered() {
        let tmp = indexed_leaves();
        let cwd = tmp.path().canonicalize().expect("a real path");
        for left_alone in ["ls src", "ls -la src", "rg --files src"] {
            let outcome = gate(
                &hook("PreToolUse", 2, shell(&cwd, left_alone)),
                tmp.path(),
                &levers(),
            );
            assert_eq!(
                outcome.answer, None,
                "a directory's contents are not this index's to state: {left_alone}"
            );
        }
    }

    /// **A glob that scopes itself with an argument no artifact pins is not a
    /// glob this gate understands**, and runs exactly as it was written.
    #[cfg(feature = "search")]
    #[test]
    fn a_glob_carrying_an_unpinned_argument_is_left_alone() {
        let tmp = indexed(&[("src/a.rs", "fn a() {}\n")]);
        let scoped = json!({
            "tool_name": "Glob",
            "tool_input": { "pattern": "**/*.rs", "path": "/somewhere/else" },
        });
        let outcome = gate(&hook("PreToolUse", 1, scoped), tmp.path(), &levers());
        assert_eq!(outcome.answer, None, "the call this gate cannot read");
        assert_eq!(markers(tmp.path(), "g-"), 0);
    }
}

//! **A content search that declared no result cap is given one, once.**
//!
//! The whole of what Rift does to a content search. It is not answered, not
//! substituted and not declined — a search inside file contents has an answer
//! no index of Rift's can stand in for, and a decline would send the model
//! looking for one that does not exist. What a search that named no bound can
//! be given is the bound its own tool documents: the family's pinned rows
//! carry an argument for it, the tool states its true total even when that
//! bound truncates the listing, and the model reads that total and asks again
//! when it wants the rest. Rift writes nothing into the result.
//!
//! Four rules hold the lever honest, and each is a fact this module keeps:
//!
//! 1. **A cap is only ever added.** A call that named its own bound — an
//!    explicit `0` included — chose one, and a choice already made is not one
//!    to make again.
//! 2. **One cap per key per context window.** The key is the search itself
//!    with the two parameters that page through its results removed, so the
//!    follow-up the tool's own total invites — the same search, with or
//!    without an offset — runs with no bound at all. That is the escape hatch,
//!    and it is why the cap costs the agent nothing it cannot take back.
//! 3. **Verified where the result proves it.** The executed call carries the
//!    input the tool really ran with, so a cap another hook's amendment
//!    displaced is a fact rather than an inference. A displaced cap is
//!    recorded and this window stops amending; it is never escalated to a
//!    decline, because a content search is never substituted.
//! 4. **Nothing here opens a store or counts a token.** The window's memory is
//!    marker files under this member's own run state, and the accounting row is
//!    journaled for the detached worker.
//!
//! **A name search is answered from the index Rift already holds.** The other
//! half of this module, and the mirror image of the rule above: a search for
//! file *names* — a glob, or a `find … -name` listing under the published
//! grammar — has an answer that fits in a sentence, so it gets one. The
//! ranked head of the matches, the total the head is a head of, and the fact
//! that repeating the call runs it in full. Four rules again:
//!
//! 1. **From the index or not at all.** The index is a file this member keeps
//!    and the detached worker writes. Warm, it is loaded and matched inside
//!    the hook's budget; cold or stale, the search runs exactly as written and
//!    the walk is asked of the worker. A hook never walks a tree.
//! 2. **One answer per name search per context window**, on a family whose
//!    reason channel a recorded run has confirmed. The repeat is allowed —
//!    that is what makes the frame's own escape hatch true — and a repeat
//!    inside the turn that was answered is counted, because a lever that sends
//!    the model straight back for the full list has saved nothing.
//! 3. **Names, never contents, and never a total nobody counted.** The frame
//!    carries paths and a count. A content search is never answered this way
//!    and never substituted by a name search.
//! 4. **Nothing here opens a store or counts a token either.** The frame is
//!    fitted to the operator's budget by characters, and the tokens it cost —
//!    and the listing it stood in for — are counted by the worker.
//!
//! The re-query rate — the honesty figure this lever is judged by — is defined
//! in exactly one place: this window directory. `c-<key>` is a search this
//! window capped, `q-<key>` is one that was asked again *in the same turn*
//! after being capped, and the rate is the second count over the first. A
//! reader outside this binary counts the files; nothing recomputes the
//! definition from a different set of lines.
//!
//! What is here is the gate itself and the two kinds of thing it hands back.
//! The bound a content search is given is in `cap`; what makes a call a name
//! search is in `name` and what one is answered with is in `answer`; the
//! markers one conversation decides by, and the collection of what ended
//! conversations left, are in `window`.

mod answer;
mod cap;
mod name;
mod window;

use std::path::Path;

use rift_recall_policy::Mode;
use rift_shadow::hooks::{AdapterRegistry, RawHook};

use self::cap::{decide, search_args, search_key, verify, GREP_KIND};
use self::name::name_answer;
use self::window::SearchWindow;

pub(crate) use self::answer::priced_paths;
pub(crate) use self::window::sweep;

/// The amendment one call is owed: the call as the agent wrote it, and the cap
/// to add to it.
///
/// The whole input travels because the amendment replaces the object rather
/// than patching it, and carrying the original is what lets the one changed
/// field be proved to be the only difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrepCap {
    /// The call as the agent wrote it.
    pub(crate) tool_input: serde_json::Value,
    /// The bound to add.
    pub(crate) head_limit: u32,
}

/// What one hook's search gate concluded.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct SearchOutcome {
    /// An accounting row was journaled, so the detached worker is owed a run.
    pub(crate) journaled: bool,
    /// What the call is owed, `None` for every call this gate leaves alone.
    pub(crate) answer: Option<SearchAnswer>,
}

/// The two things this gate may hand back, which are deliberately different
/// kinds of thing.
///
/// A content search is **amended**: it runs, and Rift adds the bound the
/// caller left out. A name search is **answered**: it does not run, and the
/// model reads the answer instead. Nothing in this module ever does the first
/// to a name search or the second to a content one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SearchAnswer {
    /// The bound to add to a content search that declared none.
    Cap(GrepCap),
    /// The rendered answer a name search is declined with. A build without the
    /// search seam has no index to answer one from, so nothing constructs it
    /// there and the response arm below simply never matches.
    #[cfg_attr(not(feature = "search"), allow(dead_code))]
    Listing(String),
}

/// The hook one decision is being made for.
///
/// One struct rather than five arguments repeated down every path: both halves
/// of this gate, and every accounting row either of them journals, work from
/// exactly these facts, and passing them together is what keeps a row about
/// one call from being written with another call's member or clock.
pub(super) struct SearchCall<'a> {
    /// The phase timer this hook is measured by.
    t: &'a crate::hooktime::HookTimer,
    /// The call as the client sent it.
    raw: &'a RawHook,
    /// This member's git directory, where the worker's queue lives.
    git_dir: &'a Path,
    /// The checkout the search is about.
    work_tree: &'a Path,
    /// The levers, as they resolved for this family.
    effective: &'a rift_recall_policy::Effective,
}

/// **The gate.** Runs beside the read and document gates and under the same
/// rules: it opens no store, initialises no tokenizer, reads and writes files
/// under this member's own run state, and every way of failing to decide
/// leaves the agent's call exactly as it was written.
#[allow(clippy::too_many_arguments)]
pub(crate) fn search_gate(
    t: &crate::hooktime::HookTimer,
    registry: &AdapterRegistry,
    raw: &RawHook,
    git_dir: &Path,
    work_tree: &Path,
    policy: &rift_recall_policy::ServePolicy,
    effective: &rift_recall_policy::Effective,
) -> SearchOutcome {
    let call = SearchCall {
        t,
        raw,
        git_dir,
        work_tree,
        effective,
    };
    let quiet = SearchOutcome::default();
    // Two shut tables and a hook that names no context window: three ways of
    // having nothing to decide, and none of them costs a file read.
    if raw.session.is_empty()
        || (effective.grep_cap == Mode::Off && effective.search_answer == Mode::Off)
    {
        return quiet;
    }
    let Some(adapter) = registry.get(&raw.agent) else {
        return quiet;
    };
    let facts = adapter.facts(&raw.verb, &raw.payload);
    // A NAME search is decided before a content search is amended, and the two
    // arms never see the same call: what a family calls each of them is that
    // family's own pinned fact, read here through the adapter that holds it.
    if let Some(outcome) = call.t.phase("search_answer", || {
        name_answer(&call, registry, policy, &facts)
    }) {
        return outcome;
    }
    if effective.grep_cap == Mode::Off {
        return quiet;
    }
    let Some(row) = facts
        .tool_name
        .as_deref()
        .and_then(|tool| rift_agent::tool_fields::tool_field_row(&raw.agent, tool))
    else {
        return quiet;
    };
    if row.fact("kind") != Some(GREP_KIND) {
        return quiet;
    }
    // The argument the cap is added to, named by the row and by nothing here.
    // No pinned argument, no amendment: a family whose row states no bound has
    // no bound to be given.
    let (Some(capped_field), Some(args)) = (row.fact("head_limit"), search_args(row, &raw.payload))
    else {
        return quiet;
    };
    let Some(key) = search_key(args, capped_field) else {
        return quiet;
    };
    // The context window, not the session: a subagent runs under its parent's
    // session id with its own agent id, and one of them having been given a
    // bound says nothing about the other.
    let window = SearchWindow::at(work_tree, &raw.session, facts.child_session.as_deref());
    let Some(step) = crate::hooks::gate_step(&raw.agent, &raw.verb, &raw.payload) else {
        return quiet;
    };
    match step {
        crate::hooks::GateStep::Decide => decide(&call, &window, args, capped_field, &key),
        crate::hooks::GateStep::CrossCheck => verify(&call, &window, args, capped_field, &key),
        crate::hooks::GateStep::Evict(_) => quiet,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rift_shadow::hooks::{AdapterRegistry, RawHook};
    use serde_json::json;

    use super::{search_gate, GrepCap, SearchAnswer, SearchOutcome};

    /// The family this lever exists for, and the levers as this repository
    /// ships them for it.
    pub(super) fn levers() -> rift_recall_policy::Effective {
        rift_recall_policy::resolve(
            &rift_recall_policy::ServePolicy::default(),
            "claude-code",
            crate::recall::family_capability(&rift_core::AgentId::CLAUDE_CODE),
        )
    }

    /// One hook as a client sends it, in the turn `turn`.
    pub(super) fn hook(verb: &str, turn: u32, payload: serde_json::Value) -> RawHook {
        RawHook {
            agent: rift_core::AgentId::CLAUDE_CODE,
            verb: verb.to_owned(),
            session: "s-1".to_owned(),
            turn: Some(turn),
            occurred_at_ms: 1_700_000_000_000,
            payload,
        }
    }

    /// A content search, spelled with the argument names the family's pinned
    /// row and its own published page name.
    pub(super) fn grep(input: serde_json::Value) -> serde_json::Value {
        json!({ "tool_name": "Grep", "tool_input": input })
    }

    /// One call through the gate, against a checkout the test owns.
    pub(super) fn gate(
        raw: &RawHook,
        work_tree: &Path,
        effective: &rift_recall_policy::Effective,
    ) -> SearchOutcome {
        gate_under(
            raw,
            work_tree,
            &rift_recall_policy::ServePolicy::default(),
            effective,
        )
    }

    /// The same, under a repository policy of the test's own — the deny list
    /// is the one thing about a name search that a default cannot express.
    pub(super) fn gate_under(
        raw: &RawHook,
        work_tree: &Path,
        policy: &rift_recall_policy::ServePolicy,
        effective: &rift_recall_policy::Effective,
    ) -> SearchOutcome {
        search_gate(
            &crate::hooktime::HookTimer::new("agent-hook", false),
            &AdapterRegistry::with_builtin(),
            raw,
            &work_tree.join(".git"),
            work_tree,
            policy,
            effective,
        )
    }

    /// The amendment an outcome carries, or a failure naming what it carried
    /// instead: every test below is about one of the two kinds of answer, and
    /// reading one as the other would pass for the wrong reason.
    pub(super) fn cap_of(outcome: &SearchOutcome) -> GrepCap {
        match outcome.answer.clone() {
            Some(SearchAnswer::Cap(cap)) => cap,
            other => panic!("a bound was owed, not {other:?}"),
        }
    }

    /// The whole response the client would receive for this amendment.
    pub(super) fn response(cap: &GrepCap) -> serde_json::Value {
        rift_agent::respond::gate(
            &rift_core::AgentId::CLAUDE_CODE,
            "PreToolUse",
            &rift_agent::respond::GateDecision::Rewrite(
                rift_agent::respond::Rewrite::GrepHeadLimit {
                    tool_input: &cap.tool_input,
                    head_limit: cap.head_limit,
                },
            ),
        )
        .expect("the family's pinned row documents this amendment")
    }

    /// The accounting rows this hook journaled for the detached worker.
    pub(super) fn journaled(work_tree: &Path) -> Vec<crate::agent_queue::AgentJob> {
        crate::agent_queue::jobs(&work_tree.join(".git"))
            .into_iter()
            .map(|(_, job)| job)
            .collect()
    }

    /// Files under this window's directory whose names begin with `prefix` —
    /// the re-query figure's one definition, counted the way a reader outside
    /// this binary counts it.
    pub(super) fn markers(work_tree: &Path, prefix: &str) -> usize {
        let dir = work_tree.join(".rift/state/search/w");
        let Ok(windows) = std::fs::read_dir(&dir) else {
            return 0;
        };
        windows
            .flatten()
            .filter_map(|window| std::fs::read_dir(window.path()).ok())
            .flatten()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
            .count()
    }

    /// A checkout with an index over `files`, built the way the detached
    /// worker builds it.
    ///
    /// The skeleton comes first and the walk second, on purpose: the index is
    /// keyed by the newest directory modification time it saw, and a directory
    /// this hook is about to create — git's own log under `.git`, the window
    /// journals under `.rift` — would move that key out from under the index the
    /// test just built. A real checkout has both long before an agent runs.
    #[cfg(feature = "search")]
    pub(super) fn indexed(files: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("a work tree");
        let root = tmp.path();
        for dir in [".git/rift-agent-queue", ".rift/state/reads"] {
            std::fs::create_dir_all(root.join(dir)).expect("the skeleton");
        }
        for (rel, body) in files {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("a directory");
            }
            std::fs::write(path, body).expect("a file");
        }
        rift_recall_search::rebuild(root, &rift_recall_search::head_oid(root), &[])
            .expect("the worker's own pass");
        tmp
    }

    /// One name search, as each family spells it.
    #[cfg(feature = "search")]
    pub(super) fn glob(pattern: &str) -> serde_json::Value {
        json!({ "tool_name": "Glob", "tool_input": { "pattern": pattern } })
    }

    /// The answer an outcome carries, or a failure naming what it carried.
    #[cfg(feature = "search")]
    pub(super) fn frame_of(outcome: &SearchOutcome) -> String {
        match outcome.answer.clone() {
            Some(SearchAnswer::Listing(frame)) => frame,
            other => panic!("an answer was owed, not {other:?}"),
        }
    }
}

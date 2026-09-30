//! **What a name search is answered with.**
//!
//! The answering half: the index this member already holds, the ranked head of
//! what matched, the total that head is a head of, and the accounting the
//! detached worker is left. Apart from the reading that recognises a name
//! search, because every screen and every bound here is about the sentence a
//! model is handed rather than about what the agent asked for.

use std::path::Path;

#[cfg(feature = "search")]
use super::name::NameSearch;
#[cfg(feature = "search")]
use super::window::{Claim, SearchWindow, ANSWER_PREFIX, REPEAT_PREFIX};
#[cfg(feature = "search")]
use super::{SearchAnswer, SearchOutcome};
#[cfg(feature = "search")]
use rift_recall_policy::Mode;

/// How many paths a native listing of this repository would have put in front
/// of the model before it truncated.
///
/// The pinned figure, not a round one: the `Glob` tool's own page says
/// "Results are sorted by modification time and capped at 100 files", recorded
/// verbatim in this family's tool-field pin. It bounds the list the worker
/// prices, because that list has to be the one the model would really have
/// received or the figure is about a listing nobody would have got.
#[cfg(feature = "search")]
use rift_recall_serve::NATIVE_LISTING_PATHS;

/// The characters one token of the answer budget is worth.
///
/// The same conversion the boundary map makes, and here it is the bound rather
/// than a starting point: a decision runs inside a hook, and the tokenizer
/// belongs to the worker that runs afterwards. It bites only on unusually long
/// paths — the operator's real bound on this frame is how many paths it names.
#[cfg(feature = "search")]
const ANSWER_CHARS_PER_TOKEN: usize = 4;

/// The directory the paths one answer stood in for are left in for the worker.
///
/// Named in every build, because a checkout written to by one that answers
/// name searches is still swept by one that does not.
pub(super) const ANSWERS_DIR: &str = "answers";

/// The neutral tag on an answer that was decided and not made.
#[cfg(feature = "search")]
const TAG_ANSWER_OBSERVED: &str = "answer_observed";

/// What the answer may say and what it may claim, screened together.
#[cfg(feature = "search")]
struct Screened {
    /// The paths an answer may name: the ranked head, screened.
    nameable: Vec<String>,
    /// The paths an answer may claim to have stood in for, which for a shell
    /// listing is very much the larger set.
    priced: Vec<String>,
}

/// **Answer.** The call is a name search on a family that may be declined; what
/// it is answered with, and whether it is answered at all, is the whole of what
/// follows.
#[cfg(feature = "search")]
pub(super) fn answer_name_search(
    call: &super::SearchCall<'_>,
    policy: &rift_recall_policy::ServePolicy,
    window: &SearchWindow,
    named: &NameSearch,
) -> SearchOutcome {
    let quiet = SearchOutcome::default();
    if call.effective.search_answer == Mode::Off {
        return quiet;
    }
    // The index or nothing. A hook that walked a tree to answer a search would
    // spend more than the search costs, so a cold or stale index is a search
    // that runs exactly as it was written and a walk the worker is asked for.
    let Some(index) = warm_index(call) else {
        return SearchOutcome {
            journaled: crate::hooks::owe_search_index(call.git_dir, call.work_tree),
            answer: None,
        };
    };
    let matched: Vec<rift_recall_search::FileEntry> = index
        .entries()
        .iter()
        .filter(|entry| named.matches(&entry.path))
        .cloned()
        .collect();
    // A search this index has no match for is not a search Rift knows the
    // answer to. The index skips what git ignores and the tool does not, so
    // "nothing here" is a statement about the index and never about the tree —
    // and a model told there is no such file stops looking for it.
    if matched.is_empty() {
        return quiet;
    }
    let total = matched.len();
    let shape = named.shape();
    let would_have_printed = native_listing(shape, &matched);
    let Some(ranked) = rank(call, window, index.key().clone(), matched) else {
        return quiet;
    };
    let listed: Vec<String> = ranked.hits.iter().map(|hit| hit.path.clone()).collect();
    let config = crate::hooks::effective_config(call.work_tree);
    let screened = screened_paths(
        &config,
        policy,
        call.raw.agent.as_str(),
        shape,
        &listed,
        &would_have_printed,
    );
    let Some(reason) = answer_frame(&config, &screened.nameable, total, named, call.effective)
    else {
        return quiet;
    };
    // The whole point is to hand the model less than the listing would have.
    // A short listing is one the answer cannot beat — the frame's own
    // sentence costs more than the handful of paths it stands in for — and a
    // lever that fires there spends tokens to save none. Characters rather
    // than tokens, because the tokenizer belongs to the worker; the
    // comparison errs against answering, because the listing this is measured
    // against is spelled the short way and the native one is not.
    if reason.chars().count() >= screened.priced.join("\n").chars().count() {
        return quiet;
    }
    answer_once(call, window, named, shape, reason, &screened.priced)
}

/// The one answer this window may make for this search, or nothing.
///
/// One answer per name search per window is what makes the frame's own escape
/// hatch true: the repeat runs the search in full. The claim is taken in both
/// modes, so what `observe` records is the decision `enforce` would have made
/// and not one row per glob of the same pattern.
#[cfg(feature = "search")]
fn answer_once(
    call: &super::SearchCall<'_>,
    window: &SearchWindow,
    named: &NameSearch,
    shape: rift_recall_serve::ListingShape,
    reason: String,
    priced: &[String],
) -> SearchOutcome {
    let turn = crate::hooks::gate_turn(call.raw, call.git_dir);
    if let Claim::Repeat { capped_in_turn } = window.claim(ANSWER_PREFIX, &named.key(), turn) {
        // Asked again inside the turn it was answered in: the escape hatch
        // taken immediately, which is the figure this lever is judged by.
        if capped_in_turn == turn {
            window.note_requery(REPEAT_PREFIX, &named.key());
        }
        return SearchOutcome::default();
    }
    if call.effective.search_answer != Mode::Enforce {
        // Reached, decided, and not acted on. Every check above ran, so the
        // row sizes what this lever would be working on; the frame is left out
        // because nothing was put in front of a model, and a sentence nobody
        // read is not a cost this row may book against itself.
        return SearchOutcome {
            journaled: journal_answer(call, shape, "", None, Some(0)),
            answer: None,
        };
    }
    book_answer(call, shape, reason, priced)
}

/// This checkout's index, **loaded and never built**.
///
/// The probe that decides whether the saved index still answers is bounded by
/// the same rule a cold build is: a tree big enough for the walk to be
/// unaffordable is one where deciding whether the file is stale is unaffordable
/// too, and a hook that spent its budget finding that out would have spent it
/// for nothing. Either way the answer here is `None`, and `None` is a search
/// that runs.
#[cfg(feature = "search")]
fn warm_index(call: &super::SearchCall<'_>) -> Option<rift_recall_search::PathIndex> {
    let work_tree = call.work_tree;
    let path = rift_recall_search::index_path(work_tree);
    // One `stat` before anything expensive. A checkout the worker has not been
    // round yet is the common cold case — every session before its first pass,
    // and every tree too large for one — and asking git and the directory tree
    // whether a file that is not there is stale costs the whole answer's
    // budget to learn nothing.
    if !path.is_file() {
        return None;
    }
    let head = call
        .t
        .phase("search_head", || rift_recall_search::head_oid(work_tree));
    let deadline = std::time::Instant::now() + rift_recall_search::COLD_BUDGET;
    let key = call.t.phase("search_probe", || {
        rift_recall_search::walk::current_key_within(work_tree, &head, Some(deadline))
    })?;
    call.t.phase("search_load", || {
        rift_recall_search::PathIndex::load(&path, &key)
    })
}

/// What the native call itself would have printed, which is a different set
/// from the one Rift is about to name.
///
/// A glob tool stops after a hundred files, so the head this hook ranked is
/// the whole of it and there is nothing else it would have shown; a shell
/// listing prints every match and is cut only by how much of the result the
/// client reads back — so that shape is priced over the match set, not over
/// the ranked head, or the claim describes a listing nobody would have
/// received.
#[cfg(feature = "search")]
fn native_listing(
    shape: rift_recall_serve::ListingShape,
    matched: &[rift_recall_search::FileEntry],
) -> Vec<String> {
    match shape {
        rift_recall_serve::ListingShape::Glob => Vec::new(),
        rift_recall_serve::ListingShape::Listing => {
            matched.iter().map(|entry| entry.path.clone()).collect()
        }
    }
}

/// The matches, best first, with the warmth of this conversation's own reading
/// folded in.
///
/// The ranking is the engine's and not this module's: same index, same facts,
/// same query, same order. What this supplies is the two arguments a hook can
/// afford — the matched paths, and the window's own journal, which is one
/// bounded file the page cache is already holding. What it deliberately does
/// not supply is git status: a full status of a working tree is not a hook's
/// to run, and a frame that tagged nothing is honest where a frame that
/// guessed would not be.
#[cfg(feature = "search")]
fn rank(
    call: &super::SearchCall<'_>,
    window: &SearchWindow,
    key: rift_recall_search::IndexKey,
    matched: Vec<rift_recall_search::FileEntry>,
) -> Option<rift_recall_search::Ranked> {
    use rift_recall_search::SearchEngine as _;
    let index = rift_recall_search::PathIndex::new(key, matched).ok()?;
    let mut frecency = rift_recall_search::FrecencyFacts::new();
    frecency.absorb_journal(&window.reads(call.work_tree));
    Some(
        rift_recall_search::NativeEngine::new(index)
            .with_frecency(frecency)
            .rank_paths(&rift_recall_search::PathQuery {
                // No fuzzy half at all: the pattern already said which paths
                // are answers, and every one of them answers it equally well.
                // What is left to decide is the order, which is warmth.
                text: String::new(),
                limit: NATIVE_LISTING_PATHS,
                now_ms: u64::try_from(call.raw.occurred_at_ms).unwrap_or_default(),
            }),
    )
}

/// The sentence the model reads, fitted to the operator's budget.
///
/// Written by the one renderer allowed to write one; this function chooses the
/// paths and states the total. Two bounds apply, in this order: the operator's
/// `glob_top_n`, which is how many paths an answer names, and the character
/// budget, which drops from the ranked tail — the coldest end — until the
/// frame fits. The total never moves: it is the count of what matched, and a
/// frame that shrank its list while restating its total is the honest shape.
#[cfg(feature = "search")]
fn answer_frame(
    config: &rift_config::EffectiveConfig,
    listed: &[String],
    total: usize,
    named: &NameSearch,
    effective: &rift_recall_policy::Effective,
) -> Option<String> {
    let top_n = usize::try_from(effective.glob_top_n).unwrap_or_default();
    // The index is built without the paths this repository calls sensitive —
    // but a list edited after the index was written moves no directory mtime
    // and no HEAD, so the saved index stays fresh and can still hold a path
    // that has since become sensitive. This is the belt on top of that brace:
    // a name is the one thing an answer says out loud. The caller answered the
    // same screen before anything was priced, so a path this drops was never
    // billed for either.
    let mut shown: Vec<rift_recall_serve::SearchHit<'_>> = listed
        .iter()
        .filter(|path| !config.is_sensitive_path(path))
        .take(top_n)
        .map(|path| rift_recall_serve::SearchHit {
            path,
            // Nothing is claimed about the working tree: see `rank`.
            status: rift_recall_serve::PathStatus::Tracked,
        })
        .collect();
    let budget_chars = usize::try_from(effective.answer_budget_tokens)
        .unwrap_or_default()
        .saturating_mul(ANSWER_CHARS_PER_TOKEN);
    while !shown.is_empty() {
        let text = rift_recall_serve::render_search_answer(&shown, total, &named.pattern)?;
        if text.chars().count() <= budget_chars {
            return Some(text);
        }
        shown.pop();
    }
    None
}

/// Two sets, screened by one screen, because they answer two questions.
///
/// What Rift may **name** is the ranked head, which is bounded by what a frame
/// can hold. What Rift may **price** is what the client would have shown for
/// this shape, which for a listing is very much larger. Both are answered
/// before the frame is written and before anything is priced, because a path
/// Rift will not name is one it may not claim to have saved either — and
/// because a name it will not say is not a name it writes into its own run
/// state on the way to the worker.
///
/// Three screens, not two. The repository's own `denied_paths` and the
/// structural safety of the name are the serve crate's; the repository's
/// *sensitive* list is this gate's, because the policy engine keeps it private
/// and the one other caller of that function — the `search` verb —
/// deliberately does name sensitive paths to the operator who asked. So it is
/// passed in here, where a hook answers for a model, and it runs before the
/// price and not only before the frame.
#[cfg(feature = "search")]
fn screened_paths(
    config: &rift_config::EffectiveConfig,
    policy: &rift_recall_policy::ServePolicy,
    agent: &str,
    shape: rift_recall_serve::ListingShape,
    listed: &[String],
    would_have_printed: &[String],
) -> Screened {
    let sensitive = |path: &str| config.is_sensitive_path(path);
    let engine = rift_recall_policy::PolicyEngine::new(config).with_serve_policy(policy.clone());
    let nameable = rift_recall_serve::priceable_listing(
        &engine,
        rift_recall_serve::ListingShape::Glob,
        agent,
        listed,
        sensitive,
    );
    let priced = match shape {
        rift_recall_serve::ListingShape::Glob => nameable.clone(),
        rift_recall_serve::ListingShape::Listing => rift_recall_serve::priceable_listing(
            &engine,
            shape,
            agent,
            would_have_printed,
            sensitive,
        ),
    };
    Screened { nameable, priced }
}

/// Book the answer: the paths it stood in for left where the worker will find
/// them, the accounting row queued, and the frame handed back to the model.
///
/// The paths are best effort and the row is written either way: a frame the
/// model read is a cost whether or not the listing behind it can be priced,
/// and a decision whose paths are missing books that cost with no claim beside
/// it rather than none at all.
#[cfg(feature = "search")]
fn book_answer(
    call: &super::SearchCall<'_>,
    shape: rift_recall_serve::ListingShape,
    reason: String,
    priced: &[String],
) -> SearchOutcome {
    // The paths the answer stood in for, priced by the worker with the real
    // tokenizer under the identity of this decision. Cut where the native tool
    // cuts, because that is the listing the model would have received.
    let call_id = {
        use rift_core::UlidSource as _;
        crate::hooks::AUDIT_IDS.next_ulid().to_hex()
    };
    let _ = leave_priced_paths(call.work_tree, &call_id, priced);
    let journaled = journal_answer(call, shape, &reason, Some(&call_id), None);
    SearchOutcome {
        journaled,
        answer: Some(SearchAnswer::Listing(reason)),
    }
}

/// Leave the paths this answer stood in for where the worker will find them.
///
/// A file rather than a field of the journaled row: what the listing would have
/// cost is a tokenizer question, the tokenizer belongs to the worker, and the
/// worker needs the paths themselves to ask it. Keyed by the decision's own id,
/// under this member's own run state, and collected by `rift clean` with the
/// window markers beside it if the worker never comes.
#[cfg(feature = "search")]
fn leave_priced_paths(work_tree: &Path, call_id: &str, paths: &[String]) -> bool {
    let dir = work_tree.join(".rift/state/search").join(ANSWERS_DIR);
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    std::fs::write(dir.join(call_id), paths.join("\n")).is_ok()
}

/// Journal the accounting one answer owes.
///
/// `frame` is what the model was handed, which the worker counts; `call_id`
/// names the paths it stood in for, which the worker prices; `shape` is which
/// native call they stood in for, which is the ceiling the worker prices them
/// at; `declared` is the figure a decision that was only observed books
/// instead — zero, because nothing was answered and nothing was avoided.
#[cfg(feature = "search")]
fn journal_answer(
    call: &super::SearchCall<'_>,
    shape: rift_recall_serve::ListingShape,
    frame: &str,
    call_id: Option<&str>,
    declared: Option<u32>,
) -> bool {
    let call_id = call_id.map_or_else(
        || {
            use rift_core::UlidSource as _;
            crate::hooks::AUDIT_IDS.next_ulid().to_hex()
        },
        str::to_owned,
    );
    crate::agent_queue::enqueue(
        call.git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: call.work_tree.to_path_buf(),
            queued_at_ms: call.raw.occurred_at_ms,
            attempts: 0,
            kind: crate::agent_queue::JobKind::ServeAudit(crate::agent_queue::ServeAudit {
                call_id,
                decision: crate::agent_queue::ServeAuditKind::SearchAnswered,
                agent: call.raw.agent.as_str().to_owned(),
                // A listing is about no file and no revision.
                basis: None,
                frame: frame.to_owned(),
                tag: declared.map(|_| TAG_ANSWER_OBSERVED.to_owned()),
                total: None,
                declared_avoided: declared,
                shape: Some(shape),
                session: Some(call.raw.session.clone()),
                window: None,
            }),
        },
    )
    .is_ok()
}

/// The paths one answer stood in for, as the worker reads them back.
///
/// `None` when the file is gone — a `clean` between the decision and the
/// drain, a member removed — which the worker books as a figure it could not
/// establish rather than as a saving of nothing.
#[cfg(feature = "search")]
pub(crate) fn priced_paths(work_tree: &Path, call_id: &str) -> Option<Vec<String>> {
    let path = work_tree
        .join(".rift/state/search")
        .join(ANSWERS_DIR)
        .join(call_id);
    let text = std::fs::read_to_string(&path).ok()?;
    // Read once and taken away: the figure is booked once, and a file left
    // behind would be collected later as an answer nobody priced.
    let _ = std::fs::remove_file(&path);
    Some(text.lines().map(str::to_owned).collect())
}

/// The same, in a build that answers no name search: there is nothing to
/// price, because nothing here ever stood in for a listing.
#[cfg(not(feature = "search"))]
pub(crate) fn priced_paths(_work_tree: &Path, _call_id: &str) -> Option<Vec<String>> {
    None
}

#[cfg(feature = "search")]
#[cfg(test)]
mod tests {
    use rift_recall_policy::Mode;
    use rift_shadow::hooks::RawHook;
    use serde_json::json;

    use crate::search_gate::tests::{
        frame_of, gate, gate_under, glob, grep, hook, indexed, journaled, levers, markers,
    };
    use crate::search_gate::SearchAnswer;

    /// A checkout of `count` module sources, indexed, with `extra` beside
    /// them — the shape a glob over this tree is answered from.
    fn indexed_modules(count: usize, extra: &[(&str, &str)]) -> tempfile::TempDir {
        let files: Vec<(String, String)> = (0..count)
            .map(|n| {
                (
                    format!("src/module{n}/handler.rs"),
                    format!("fn f{n}() {{}}\n"),
                )
            })
            .collect();
        let mut borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        borrowed.extend_from_slice(extra);
        indexed(&borrowed)
    }

    /// ★ **A name search is answered with the ranked head and the total it is
    /// a head of**, in the one channel this family's pinned row names.
    ///
    /// The answer is a strict subset that says so, and the sentence the client
    /// receives carries it whole and amends nothing.
    #[cfg(feature = "search")]
    #[test]
    fn a_glob_is_answered_with_the_top_n_and_the_total_it_is_a_head_of() {
        let tmp = indexed_modules(60, &[("README.md", "prose\n")]);
        let call = hook("PreToolUse", 1, glob("**/*.rs"));

        let outcome = gate(&call, tmp.path(), &levers());
        let frame = frame_of(&outcome);
        assert!(
            frame.starts_with("[rift] Search answered: 8 of 60 paths matching"),
            "the head, and the total it is a head of: {frame}"
        );
        assert_eq!(
            frame.lines().filter(|l| l.starts_with("- ")).count(),
            8,
            "the operator's top-n, and no more: {frame}"
        );
        assert!(
            frame.contains("Repeat this exact call for the full list."),
            "the escape hatch is stated: {frame}"
        );
        assert!(
            !frame.contains("README.md"),
            "only what the pattern matched: {frame}"
        );
        assert!(
            outcome.journaled,
            "the decision is journaled for the worker"
        );
        assert_eq!(markers(tmp.path(), "g-"), 1, "one answer, one marker");

        // The whole response the client receives: a decision with the reason
        // in the field this family's pinned row names, and no amendment.
        let response = rift_agent::respond::gate(
            &rift_core::AgentId::CLAUDE_CODE,
            "PreToolUse",
            &rift_agent::respond::GateDecision::Deny { reason: &frame },
        )
        .expect("the family's confirmed reason channel");
        assert_eq!(
            response.pointer("/hookSpecificOutput/permissionDecision"),
            Some(&serde_json::Value::from("deny")),
            "{response}"
        );
        assert_eq!(
            response.pointer("/hookSpecificOutput/permissionDecisionReason"),
            Some(&serde_json::Value::from(frame.as_str())),
            "the frame travels whole: {response}"
        );
        assert!(
            response
                .pointer("/hookSpecificOutput/updatedInput")
                .is_none(),
            "a decline carries no amendment: {response}"
        );
    }

    /// ★ **The repeat runs the search in full, and is counted.**
    ///
    /// The escape hatch is the identical call, and it is what makes hiding the
    /// tail safe: one answer per search per window, and a conversation of its
    /// own is a window of its own.
    #[cfg(feature = "search")]
    #[test]
    fn the_repeat_of_an_answered_glob_runs_in_full_and_is_counted() {
        let tmp = indexed_modules(60, &[("README.md", "prose\n")]);
        let call = hook("PreToolUse", 1, glob("**/*.rs"));
        assert!(matches!(
            gate(&call, tmp.path(), &levers()).answer,
            Some(SearchAnswer::Listing(_))
        ));

        // The identical call, in the same window: it runs, exactly as written.
        let again = gate(&call, tmp.path(), &levers());
        assert_eq!(again.answer, None, "the repeat runs the search in full");
        assert_eq!(markers(tmp.path(), "g-"), 1, "and no second answer");
        assert_eq!(
            markers(tmp.path(), "r-"),
            1,
            "a repeat inside the answered turn is counted, and apart from a bound's"
        );
        assert_eq!(
            markers(tmp.path(), "q-"),
            0,
            "which is not the bounded search's re-query rate"
        );
        // A different conversation is a different window, and is answered.
        let elsewhere = RawHook {
            session: "s-2".to_owned(),
            ..hook("PreToolUse", 1, glob("**/*.rs"))
        };
        assert!(matches!(
            gate(&elsewhere, tmp.path(), &levers()).answer,
            Some(SearchAnswer::Listing(_))
        ));
    }

    /// ★ **A content search is never answered from an index.**
    ///
    /// The name search and the content search reach this gate through the same
    /// door and leave through different ones: one is answered, the other is
    /// bounded, and neither is ever given the other's treatment.
    #[cfg(feature = "search")]
    #[test]
    fn grep_is_never_substituted() {
        // Forty files, so an answer would beat the listing it stands in for:
        // on a one-file tree the cost guard stands every answer down and the
        // assertions below could not fail for the defect they exist for.
        let files: Vec<(String, String)> = (0..40)
            .map(|n| {
                (
                    format!("src/module{n}/handler.rs"),
                    format!("fn needle{n}() {{}}\n"),
                )
            })
            .chain([("src/a.rs".to_owned(), "fn needle() {}\n".to_owned())])
            .collect();
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let tmp = indexed(&borrowed);
        let content = [
            grep(json!({ "pattern": "needle", "path": "." })),
            // The same search on the shell channel, under the published
            // grammar: a `grep` on one file is a read of lines, never a
            // listing, and this gate answers no part of it.
            json!({ "tool_name": "Bash", "tool_input": { "command": "grep needle src/a.rs" } }),
            json!({ "tool_name": "Bash", "tool_input": { "command": "rg needle src" } }),
        ];
        for payload in content {
            let outcome = gate(
                &hook("PreToolUse", 1, payload.clone()),
                tmp.path(),
                &levers(),
            );
            assert!(
                !matches!(outcome.answer, Some(SearchAnswer::Listing(_))),
                "a content search is never answered by a list of names: {payload}"
            );
        }
        assert_eq!(markers(tmp.path(), "g-"), 0, "and no answer was claimed");
    }

    /// ★ **A family whose reason channel no recorded run has confirmed is
    /// never answered — the search runs, and the decision is recorded.**
    #[cfg(feature = "search")]
    #[test]
    fn an_unconfirmed_family_observes() {
        let files: Vec<(String, String)> = (0..40)
            .map(|n| {
                (
                    format!("src/module{n}/handler.rs"),
                    "fn f() {}\n".to_owned(),
                )
            })
            .collect();
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let tmp = indexed(&borrowed);
        let unconfirmed = rift_recall_policy::Effective {
            search_answer: Mode::Observe,
            search_answer_cap: Some(rift_recall_policy::CapReason::UnconfirmedReasonChannel),
            ..levers()
        };
        let call = hook("PreToolUse", 1, glob("**/*.rs"));

        let outcome = gate(&call, tmp.path(), &unconfirmed);
        assert_eq!(outcome.answer, None, "nothing was put to the model");
        assert!(outcome.journaled, "and the decision was recorded");
        let rows = journaled(tmp.path());
        assert_eq!(rows.len(), 1);
        let crate::agent_queue::JobKind::ServeAudit(audit) = &rows[0].kind else {
            panic!("an accounting row");
        };
        assert_eq!(
            audit.decision,
            crate::agent_queue::ServeAuditKind::SearchAnswered
        );
        assert!(
            audit.frame.is_empty(),
            "a sentence nobody read costs nothing"
        );
        assert_eq!(audit.declared_avoided, Some(0), "and claims nothing");
        assert_eq!(audit.tag.as_deref(), Some("answer_observed"));
        assert!(
            !std::fs::exists(tmp.path().join(".rift/state/search/answers")).unwrap_or(false),
            "an observed decision leaves the worker no listing to price"
        );

        // The same search still runs afterwards, and books no second row.
        let again = gate(&call, tmp.path(), &unconfirmed);
        assert_eq!(again.answer, None);
        assert_eq!(journaled(tmp.path()).len(), 1);
    }

    /// **A cold index is a search that runs, and a walk the worker is asked
    /// for.** A hook never walks a tree while an agent waits on it.
    #[cfg(feature = "search")]
    #[test]
    fn a_cold_index_lets_the_glob_run_and_asks_for_a_rebuild() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).expect("a directory");
        std::fs::write(root.join("src/a.rs"), "fn a() {}\n").expect("a file");

        let outcome = gate(&hook("PreToolUse", 1, glob("**/*.rs")), root, &levers());
        assert_eq!(outcome.answer, None, "the search runs as it was written");
        assert!(outcome.journaled, "and the walk is asked of the worker");
        assert!(
            journaled(root)
                .iter()
                .any(|job| matches!(job.kind, crate::agent_queue::JobKind::SearchIndex)),
            "the request is for the index and not for an accounting row"
        );
        assert!(
            !rift_recall_search::index_path(root).exists(),
            "and nothing was walked here"
        );
        assert_eq!(markers(root, "g-"), 0, "no answer was claimed either");
    }

    /// **A path this repository refuses to serve is neither named in an answer
    /// nor priced into what the answer claims.**
    ///
    /// The two halves are one screen, and the order matters: the listing that
    /// may be priced is decided before the frame is written, so a denied path
    /// cannot be counted into the saving and then left out of the sentence. It
    /// is also never written into this member's own run state on the way to the
    /// worker — the paths left there are the ones an answer would have said.
    #[cfg(feature = "search")]
    #[test]
    fn a_denied_path_is_neither_named_nor_priced() {
        let mut files: Vec<(String, String)> = (0..28)
            .map(|n| {
                (
                    format!("src/module{n}/handler.rs"),
                    "fn a() {}\n".to_owned(),
                )
            })
            .collect();
        files.extend((0..12).map(|n| {
            (
                format!("internal/plan{n}/notes.rs"),
                "fn b() {}\n".to_owned(),
            )
        }));
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let tmp = indexed(&borrowed);
        let policy = rift_recall_policy::ServePolicy {
            denied_paths: vec!["internal/**".to_owned()],
            ..rift_recall_policy::ServePolicy::default()
        };
        let outcome = gate_under(
            &hook("PreToolUse", 1, glob("**/*.rs")),
            tmp.path(),
            &policy,
            &levers(),
        );
        let frame = frame_of(&outcome);
        assert!(
            !frame.contains("internal/"),
            "a denied path is not a name an answer says: {frame}"
        );
        assert!(
            frame.starts_with("[rift] Search answered: 8 of 40 paths matching"),
            "and the total is still what matched, so the count is not a claim \
             about the tree: {frame}"
        );

        let priced = std::fs::read_dir(tmp.path().join(".rift/state/search/answers"))
            .expect("one answer was left for the worker to price")
            .flatten()
            .map(|entry| std::fs::read_to_string(entry.path()).expect("the paths"))
            .collect::<String>();
        let paths: Vec<&str> = priced.lines().collect();
        assert_eq!(
            paths.len(),
            28,
            "the twelve the screen refused are not in the pool: {paths:?}"
        );
        assert!(
            !priced.contains("internal/"),
            "and their names were never written down: {priced}"
        );
    }

    /// ★ **A path this repository calls sensitive is neither named in an
    /// answer nor priced into what the answer claims — including one the
    /// saved index still holds.**
    ///
    /// The index is built without the sensitive paths, which closes the case
    /// where the list was already there. It does not close the other one:
    /// adding a glob to `.rift/config.toml` moves no directory modification
    /// time and no HEAD, so the saved index stays fresh by its own key and
    /// still holds a path that has since become sensitive. The screen that
    /// answers that runs at this call site, before the price and before the
    /// frame — so the path is left out of the pool as well as out of the
    /// sentence, and is never written into this member's run state on the way
    /// to the worker.
    #[cfg(feature = "search")]
    #[test]
    fn a_path_that_became_sensitive_is_neither_named_nor_priced() {
        let mut files: Vec<(String, String)> = (0..28)
            .map(|n| {
                (
                    format!("src/module{n}/handler.rs"),
                    "fn a() {}\n".to_owned(),
                )
            })
            .collect();
        files
            .extend((0..12).map(|n| (format!("vault/plan{n}/notes.rs"), "fn b() {}\n".to_owned())));
        // The policy file exists before the walk, so that adding the glob
        // below writes into it rather than creating it: creating a file would
        // move `.rift`'s own modification time, which the index IS keyed by,
        // and the test would then be measuring a stale index instead of a
        // stale sensitive list.
        let plain = "schema_version = 1\n\n[project]\nname = \"t\"\n";
        files.push((".rift/config.toml".to_owned(), plain.to_owned()));
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let tmp = indexed(&borrowed);
        // Written AFTER the index, which is the whole point: the index's own
        // key takes no sensitive glob, so it is still fresh and still holds
        // every one of the forty paths.
        std::fs::write(
            tmp.path().join(".rift/config.toml"),
            format!("{plain}\n[paths]\nsensitive_paths = [\"vault/**\"]\n"),
        )
        .expect("the repository's own policy");

        let outcome = gate(
            &hook("PreToolUse", 1, glob("**/*.rs")),
            tmp.path(),
            &levers(),
        );
        let frame = frame_of(&outcome);
        assert!(
            !frame.contains("vault/"),
            "a sensitive path is not a name an answer says: {frame}"
        );
        assert!(
            frame.starts_with("[rift] Search answered: 8 of 40 paths matching"),
            "and the total is still what matched: {frame}"
        );

        let priced = std::fs::read_dir(tmp.path().join(".rift/state/search/answers"))
            .expect("one answer was left for the worker to price")
            .flatten()
            .map(|entry| std::fs::read_to_string(entry.path()).expect("the paths"))
            .collect::<String>();
        let paths: Vec<&str> = priced.lines().collect();
        assert_eq!(
            paths.len(),
            28,
            "the twelve the screen refused are not in the pool: {paths:?}"
        );
        assert!(
            !priced.contains("vault/"),
            "and their names were never written down: {priced}"
        );
    }

    /// **An answer carries names and a count, and never a byte of a file.**
    #[cfg(feature = "search")]
    #[test]
    fn a_listing_answer_never_carries_file_contents() {
        let secretish = "SUPERSECRETBODYTEXT";
        let body = format!("fn a() {{ \"{secretish}\" }}\n");
        let mut files: Vec<(String, String)> = (0..40)
            .map(|n| (format!("src/module{n}/handler.rs"), body.clone()))
            .collect();
        files.push(("src/module0/named.rs".to_owned(), body.clone()));
        let borrowed: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_str()))
            .collect();
        let tmp = indexed(&borrowed);
        let frame = frame_of(&gate(
            &hook("PreToolUse", 1, glob("**/*.rs")),
            tmp.path(),
            &levers(),
        ));
        assert!(!frame.contains(secretish), "no file content: {frame}");
        assert!(!frame.contains("fn a()"), "and no line of one: {frame}");
        assert_eq!(
            frame.lines().filter(|l| l.starts_with("- ")).count(),
            8,
            "one line per path and nothing else: {frame}"
        );
        for line in frame.lines().filter(|l| l.starts_with("- ")) {
            assert!(
                line.ends_with("handler.rs") || line.ends_with("named.rs"),
                "a path, whole, and nothing after it: {line}"
            );
        }
    }

    /// **A listing an answer could not beat is not answered.**
    ///
    /// The frame carries a sentence of its own, so a handful of paths costs
    /// less as a listing than as an answer to one — and a lever that fired
    /// there would spend tokens to save none.
    #[cfg(feature = "search")]
    #[test]
    fn an_answer_that_would_cost_more_than_the_listing_is_not_made() {
        let tmp = indexed(&[("src/a.rs", "fn a() {}\n"), ("src/b.rs", "fn b() {}\n")]);
        let outcome = gate(
            &hook("PreToolUse", 1, glob("**/*.rs")),
            tmp.path(),
            &levers(),
        );
        assert_eq!(
            outcome.answer, None,
            "two paths are cheaper said than framed"
        );
        assert!(
            !outcome.journaled,
            "and nothing is claimed for not saying it"
        );
        assert_eq!(markers(tmp.path(), "g-"), 0, "no answer was spent either");
    }
}

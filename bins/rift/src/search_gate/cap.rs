//! **The bound a content search that declared none is given.**
//!
//! Its own module because it is one decision made twice: once before the call
//! runs, where the bound is asked for, and once after, where the executed call
//! says whether the bound arrived. Nothing here answers a search; the other
//! half of this gate does that, and never to a content search.

use rift_recall_policy::Mode;

use super::window::{Claim, SearchWindow, CAP_PREFIX, REQUERY_PREFIX};
use super::{GrepCap, SearchAnswer, SearchOutcome};

/// The pinned fact that says a row is the **content** search.
///
/// Read off the family's own tool-field row rather than matched by tool name:
/// what a family calls its content search is that family's to say, and the
/// row is where it is already pinned to an artifact.
pub(super) const GREP_KIND: &str = "grep";

/// The second of the two parameters that page through a search's results.
///
/// The first is the row's own `head_limit`. Both are named verbatim by the
/// pinned tool-field artifact, in the sentence that says what the tool's total
/// still covers: "The total covers every match even when the tool's
/// `head_limit` or `offset` parameters truncate the listed per-file entries."
/// They are the two arguments a caller changes to walk through one search's
/// results, so neither belongs in the identity of the search itself — a
/// follow-up that pages is the same search asked again, and the rule that lets
/// it run uncapped depends on exactly that.
const PAGING_FIELD: &str = "offset";

/// The field a family's call carries its arguments in, when its row names no
/// other.
const ARGS_FIELD: &str = "tool_input";

/// **Decide.** The call is a content search this family can be amended on;
/// whether it is amended is the whole of what follows.
pub(super) fn decide(
    call: &super::SearchCall<'_>,
    window: &SearchWindow,
    args: &serde_json::Map<String, serde_json::Value>,
    capped_field: &str,
    key: &str,
) -> SearchOutcome {
    let quiet = SearchOutcome::default();
    // Rule 1: a caller that named its own bound — including the explicit "no
    // bound" of a zero — chose it, and this gate leaves every such call
    // untouched. Presence, never value: the argument being written at all is
    // the choice.
    if args.contains_key(capped_field) {
        return quiet;
    }
    // A window whose amendment was verified to have been dropped is a window
    // sharing the channel with somebody else's hook. Rift does not fight over
    // it: the fact was recorded once and this window's searches run as they
    // were written.
    if window.is_dropped() {
        return quiet;
    }
    let turn = crate::hooks::gate_turn(call.raw, call.git_dir);
    match window.claim(CAP_PREFIX, key, turn) {
        // Rule 2: this search was already capped in this window, so this one
        // runs with no bound at all — the escape hatch the tool's own total
        // invites the model to take. Taken inside the turn that capped it, the
        // repeat is also the honesty figure this lever is judged by.
        Claim::Repeat { capped_in_turn } => {
            if capped_in_turn == turn {
                window.note_requery(REQUERY_PREFIX, key);
            }
            quiet
        }
        Claim::Fresh => match call.effective.grep_cap {
            // Remembered before the answer is given, so the result of this
            // very call can be checked against it. Nothing is booked here: an
            // amendment is a request to the client, and the row is written
            // where the executed call proves it happened.
            Mode::Enforce => {
                window.arm(key, call.effective.grep_head_limit);
                SearchOutcome {
                    journaled: false,
                    answer: Some(SearchAnswer::Cap(GrepCap {
                        tool_input: serde_json::Value::Object(args.clone()),
                        head_limit: call.effective.grep_head_limit,
                    })),
                }
            }
            // Reached, decided, and not acted on. Every check above ran, so
            // the row sizes what this lever would be working on; there is
            // nothing to verify, because nothing was asked of the client.
            Mode::Observe => SearchOutcome {
                journaled: journal_cap(call),
                answer: None,
            },
            Mode::Off => quiet,
        },
    }
}

/// **Did the cap this window added actually reach the tool?**
///
/// The executed call carries the input the tool really ran with, so the
/// question is answered exactly rather than inferred:
///
/// * the bound is on the executed call — the amendment happened, and this is
///   where the row is written. Once: the arming is consumed, so a second
///   result for one amendment books nothing;
/// * the bound is gone — a sibling hook amended the same call and the client
///   kept its amendment instead. The search ran in full, there is nothing to
///   record about it but the fact of the drop, and **no decline follows**: a
///   content search is never substituted, so the honest response to losing the
///   channel is to stop using it for this window.
pub(super) fn verify(
    call: &super::SearchCall<'_>,
    window: &SearchWindow,
    args: &serde_json::Map<String, serde_json::Value>,
    capped_field: &str,
    key: &str,
) -> SearchOutcome {
    let quiet = SearchOutcome::default();
    // A search this window never amended is a search whose result contradicts
    // nothing. Costs one `stat`.
    let Some(armed) = window.take_armed(key) else {
        return quiet;
    };
    // The bound this window asked for, not merely *a* bound. The key is the
    // search without its paging arguments, so a call that was amended and
    // never came back — a rejected call, a session that ended — leaves an
    // arming that the same search, re-issued later with a bound the agent
    // chose for itself, would otherwise consume: a row saying Rift capped a
    // search it did not cap. Equality closes that, and it costs one comparison
    // against a value this module wrote down.
    if args.get(capped_field).and_then(serde_json::Value::as_u64) == Some(u64::from(armed)) {
        return SearchOutcome {
            journaled: journal_cap(call),
            answer: None,
        };
    }
    if args.contains_key(capped_field) {
        // A bound is on the executed call and it is not the one this window
        // asked for. Nothing here can tell a sibling hook's amendment from the
        // agent's own second thoughts, and neither is a cap Rift made, so
        // nothing is booked — and the window stops amending, because the one
        // reading that would matter is the one where the channel is shared.
        window.mark_dropped();
        return quiet;
    }
    window.mark_dropped();
    quiet
}

/// The arguments object this family's pinned row says a call carries them in.
///
/// Read off the row rather than known here: two families put them flat under
/// the envelope field beside `tool_name`, one nests them, and the row is where
/// that fact is already pinned to an artifact.
pub(super) fn search_args(
    row: rift_agent::tool_fields::ToolFieldRow,
    payload: &serde_json::Value,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    let mut node = payload;
    for segment in row.fact("args").unwrap_or(ARGS_FIELD).split('.') {
        node = node.get(segment)?;
    }
    node.as_object()
}

/// **The identity of one search**: everything the caller wrote except the two
/// parameters that page through the results.
///
/// A digest rather than the arguments themselves, for two reasons that point
/// the same way: it is a file name in a directory this member keeps, and a
/// pattern is the agent's own text — which never becomes a path, a marker name
/// or anything else Rift writes down in the clear.
///
/// Sorted before it is hashed, so two spellings of one call in different
/// argument orders are one key.
pub(super) fn search_key(
    args: &serde_json::Map<String, serde_json::Value>,
    capped_field: &str,
) -> Option<String> {
    let mut fields: Vec<(&str, String)> = args
        .iter()
        .filter(|(name, _)| name.as_str() != capped_field && name.as_str() != PAGING_FIELD)
        .map(|(name, value)| (name.as_str(), value.to_string()))
        .collect();
    if fields.is_empty() {
        return None;
    }
    fields.sort_unstable();
    let mut bytes = Vec::new();
    for (name, value) in fields {
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(0);
    }
    Some(rift_hashchain::leaf_hash(&bytes).to_hex())
}

/// Journal the accounting one cap owes, and say whether the worker was left
/// anything.
///
/// The row books **no figures at all**: the search ran, Rift added nothing to
/// its output and withheld nothing from it, so there is neither a cost nor a
/// saving to claim. What the row carries is that a cap happened, which is the
/// count the search fixture divides by.
///
/// The total the tool reports is deliberately absent. The pinned artifact says
/// the total is stated in the `count` mode's own rendered output — text, in a
/// shape no artifact pins — and the fields a result carries beside it are the
/// *returned* counts, as the recorded observation of them says in so many
/// words. Rift reads pinned fields and never a rendering, so the count this
/// row carries is unknown rather than a number read out of a format nobody
/// pinned. The total's real audience is the model, which is handed it by the
/// tool itself.
fn journal_cap(call: &super::SearchCall<'_>) -> bool {
    crate::agent_queue::enqueue(
        call.git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: call.work_tree.to_path_buf(),
            queued_at_ms: call.raw.occurred_at_ms,
            attempts: 0,
            kind: crate::agent_queue::JobKind::ServeAudit(crate::agent_queue::ServeAudit {
                call_id: {
                    use rift_core::UlidSource as _;
                    crate::hooks::AUDIT_IDS.next_ulid().to_hex()
                },
                decision: crate::agent_queue::ServeAuditKind::GrepCapped,
                agent: call.raw.agent.as_str().to_owned(),
                // A search is about no file and no revision, and the frame is
                // empty because nothing was put in front of a model.
                basis: None,
                frame: String::new(),
                tag: None,
                total: None,
                declared_avoided: None,
                // A bounded content search stood in for no listing at all.
                shape: None,
                session: Some(call.raw.session.clone()),
                window: None,
            }),
        },
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use rift_recall_policy::Mode;
    use rift_shadow::hooks::RawHook;
    use serde_json::json;

    use crate::search_gate::tests::{
        cap_of, gate, grep, hook, journaled, levers, markers, response,
    };
    use crate::search_gate::SearchOutcome;

    /// ★ A content search that declared no result cap is given one — and the
    /// call the tool runs is otherwise the call the agent wrote, field for
    /// field, in the one channel the family's pinned rows allow.
    #[test]
    fn a_grep_without_head_limit_gets_one_and_nothing_else_changes() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let input = json!({
            "pattern": "fn main",
            "path": "crates",
            "glob": "**/*.rs",
            "type": "rust",
            "output_mode": "content",
            "multiline": true,
        });
        let call = hook("PreToolUse", 1, grep(input.clone()));

        let outcome = gate(&call, tmp.path(), &levers());
        let cap = cap_of(&outcome);
        assert_eq!(cap.head_limit, 40, "the bound this repository ships");
        assert_eq!(cap.tool_input, input, "the call travels whole");

        let response = response(&cap);
        assert_eq!(
            response.pointer("/hookSpecificOutput/permissionDecision"),
            Some(&serde_json::Value::from("allow")),
            "an amendment declines nothing: {response}"
        );
        let amended = response
            .pointer("/hookSpecificOutput/updatedInput")
            .and_then(serde_json::Value::as_object)
            .expect("the amended call");
        let mut expected = input.as_object().expect("the call").clone();
        expected.insert("head_limit".to_owned(), serde_json::Value::from(40));
        assert_eq!(amended, &expected, "one field added and nothing else moved");
        assert!(
            !outcome.journaled,
            "an amendment is a request to the client, and books nothing until the result proves it"
        );
        assert!(journaled(tmp.path()).is_empty());
    }

    /// ★ A caller that named its own bound chose one, and an explicit zero is
    /// as much a choice as any other number.
    #[test]
    fn an_explicit_head_limit_is_never_touched() {
        let tmp = tempfile::tempdir().expect("a work tree");
        for chosen in [json!(0), json!(5), json!(1000)] {
            let call = hook(
                "PreToolUse",
                1,
                grep(json!({ "pattern": "needle", "head_limit": chosen })),
            );
            let outcome = gate(&call, tmp.path(), &levers());
            assert_eq!(
                outcome,
                SearchOutcome::default(),
                "head_limit {chosen} was the caller's own choice"
            );
        }
        assert_eq!(
            markers(tmp.path(), "c-"),
            0,
            "a call this gate leaves alone is not a cap it made"
        );
    }

    /// ★ The escape hatch: the same search, asked again in this window, runs
    /// with no bound at all — with or without the offset that pages through
    /// the results the first one truncated.
    #[test]
    fn a_same_key_follow_up_grep_runs_uncapped() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let input = json!({ "pattern": "needle", "path": ".", "output_mode": "content" });
        let first = hook("PreToolUse", 1, grep(input.clone()));
        assert!(
            gate(&first, tmp.path(), &levers()).answer.is_some(),
            "the first is bounded"
        );

        let again = hook("PreToolUse", 2, grep(input.clone()));
        assert_eq!(
            gate(&again, tmp.path(), &levers()).answer,
            None,
            "the same search runs in full"
        );

        let mut paged = input.as_object().expect("the call").clone();
        paged.insert("offset".to_owned(), serde_json::Value::from(40));
        let onward = hook("PreToolUse", 2, grep(serde_json::Value::Object(paged)));
        assert_eq!(
            gate(&onward, tmp.path(), &levers()).answer,
            None,
            "paging through one search's results is that same search"
        );

        // A different search is a different key, and gets its own one bound.
        let other = hook("PreToolUse", 2, grep(json!({ "pattern": "other" })));
        assert!(
            gate(&other, tmp.path(), &levers()).answer.is_some(),
            "another search is another decision"
        );
    }

    /// A cap another hook's amendment displaced is recorded, and it is the end
    /// of this window's amendments — never the start of a decline. A content
    /// search is never substituted, so there is nothing to decline it in
    /// favour of.
    #[test]
    fn a_dropped_head_limit_is_recorded_and_never_denied() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let input = json!({ "pattern": "needle", "output_mode": "content" });
        let call = hook("PreToolUse", 1, grep(input.clone()));
        assert!(gate(&call, tmp.path(), &levers()).answer.is_some());

        // The result names the call as the agent wrote it: the bound Rift
        // asked for never reached the tool.
        let result = hook("PostToolUse", 1, grep(input.clone()));
        let outcome = gate(&result, tmp.path(), &levers());
        assert_eq!(
            outcome,
            SearchOutcome::default(),
            "a dropped bound books nothing, and answers nothing"
        );
        assert!(
            journaled(tmp.path()).is_empty(),
            "a search that ran in full saved nothing and cost nothing"
        );
        assert!(
            tmp.path()
                .join(".rift/state/search/w")
                .read_dir()
                .expect("the window")
                .flatten()
                .any(|w| w.path().join("dropped").exists()),
            "the drop is recorded"
        );

        // And the window stands down: the next search runs as written.
        let next = hook("PreToolUse", 2, grep(json!({ "pattern": "another" })));
        assert_eq!(gate(&next, tmp.path(), &levers()).answer, None);
    }

    /// The bound that reached the tool is the one moment this lever books a
    /// row: the executed call carries it, so the cap can be said to have
    /// happened. Once — a second result for one amendment claims nothing.
    #[test]
    fn a_carried_head_limit_books_the_row_the_result_proves() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let input = json!({ "pattern": "needle", "output_mode": "content" });
        let call = hook("PreToolUse", 1, grep(input.clone()));
        assert!(gate(&call, tmp.path(), &levers()).answer.is_some());

        let mut executed = input.as_object().expect("the call").clone();
        executed.insert("head_limit".to_owned(), serde_json::Value::from(40));
        let result = hook("PostToolUse", 1, grep(serde_json::Value::Object(executed)));
        assert!(
            gate(&result, tmp.path(), &levers()).journaled,
            "the executed call carried the bound"
        );
        let rows = journaled(tmp.path());
        assert_eq!(rows.len(), 1);
        let crate::agent_queue::JobKind::ServeAudit(audit) = &rows[0].kind else {
            panic!("a serving decision: {:?}", rows[0].kind);
        };
        assert_eq!(
            audit.decision,
            crate::agent_queue::ServeAuditKind::GrepCapped
        );
        assert!(
            audit.basis.is_none() && audit.frame.is_empty(),
            "a search is about no file, and nothing was put in front of a model"
        );

        let again = hook("PostToolUse", 1, grep(input));
        assert!(
            !gate(&again, tmp.path(), &levers()).journaled,
            "one amendment is answered once"
        );
    }

    /// The honesty figure: a search bounded in this turn and asked again in
    /// the same turn is a re-query, counted once, in the one place the fixture
    /// reads it from.
    #[test]
    fn a_capped_grep_followed_by_the_same_key_counts_toward_the_requery_rate() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let input = json!({ "pattern": "needle", "path": "." });
        assert!(gate(
            &hook("PreToolUse", 3, grep(input.clone())),
            tmp.path(),
            &levers()
        )
        .answer
        .is_some());
        assert_eq!(markers(tmp.path(), "c-"), 1);
        assert_eq!(markers(tmp.path(), "q-"), 0, "nothing asked again yet");

        // Same turn, same search: the model came back for the rest.
        for _ in 0..2 {
            assert_eq!(
                gate(
                    &hook("PreToolUse", 3, grep(input.clone())),
                    tmp.path(),
                    &levers()
                )
                .answer,
                None
            );
        }
        assert_eq!(
            markers(tmp.path(), "q-"),
            1,
            "how many capped searches were asked again, not how many times"
        );

        // A later turn is ordinary work, not the re-query this rate is about.
        let other = json!({ "pattern": "elsewhere" });
        assert!(gate(
            &hook("PreToolUse", 4, grep(other.clone())),
            tmp.path(),
            &levers()
        )
        .answer
        .is_some());
        assert_eq!(
            gate(&hook("PreToolUse", 5, grep(other)), tmp.path(), &levers()).answer,
            None
        );
        assert_eq!(markers(tmp.path(), "c-"), 2);
        assert_eq!(markers(tmp.path(), "q-"), 1);
    }

    /// A repository that asked to be told rather than amended is told: every
    /// check runs, the row sizes what the lever would have worked on, and the
    /// call reaches the tool exactly as the agent wrote it.
    #[test]
    fn an_observed_family_records_the_cap_it_did_not_make() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let observed = rift_recall_policy::Effective {
            grep_cap: Mode::Observe,
            ..levers()
        };
        let input = json!({ "pattern": "needle", "path": "." });
        let outcome = gate(
            &hook("PreToolUse", 1, grep(input.clone())),
            tmp.path(),
            &observed,
        );
        assert_eq!(outcome.answer, None, "nothing was amended");
        assert!(outcome.journaled, "and the decision was recorded");
        let rows = journaled(tmp.path());
        assert_eq!(rows.len(), 1);

        // The same search still runs unbounded afterwards, and books no second
        // row: what observe records is the decision enforce would have made.
        let again = gate(&hook("PreToolUse", 1, grep(input)), tmp.path(), &observed);
        assert_eq!(again, SearchOutcome::default());
        assert_eq!(journaled(tmp.path()).len(), 1);
    }

    /// Every other tool, every other verb and a family whose pinned rows
    /// document no amendment are left alone. A name search is not in the list
    /// because it is the other arm's business, and is answered rather than
    /// amended — never the reverse.
    #[test]
    fn nothing_but_a_content_search_on_an_amendable_family_is_touched() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let untouched = [
            json!({ "tool_name": "Read", "tool_input": { "file_path": "a.txt" } }),
            json!({ "tool_name": "Bash", "tool_input": { "command": "rg needle src" } }),
            json!({ "tool_name": "Grep" }),
            json!({ "tool_name": "Grep", "tool_input": {} }),
        ];
        for payload in untouched {
            let call = hook("PreToolUse", 1, payload.clone());
            assert_eq!(
                gate(&call, tmp.path(), &levers()),
                SearchOutcome::default(),
                "{payload}"
            );
        }
        for verb in ["PreCompact", "SessionStart", "Stop"] {
            let call = hook(verb, 1, grep(json!({ "pattern": "needle" })));
            assert_eq!(gate(&call, tmp.path(), &levers()), SearchOutcome::default());
        }
        // A family that documents no amendment resolves the lever to `off`,
        // and this gate never reaches a marker for it.
        let codex = rift_recall_policy::resolve(
            &rift_recall_policy::ServePolicy::default(),
            "codex",
            crate::recall::family_capability(&rift_core::AgentId::CODEX),
        );
        assert_eq!(codex.grep_cap, Mode::Off);
        let call = RawHook {
            agent: rift_core::AgentId::CODEX,
            ..hook("PreToolUse", 1, grep(json!({ "pattern": "needle" })))
        };
        assert_eq!(gate(&call, tmp.path(), &codex), SearchOutcome::default());
        assert_eq!(markers(tmp.path(), "c-"), 0);
    }

    /// **An arming outlived by its call books nothing.**
    ///
    /// The key is the search without its paging arguments, so a bound this
    /// window asked for and never saw answered — a rejected call, a session
    /// that ended — would otherwise be consumed by the same search re-issued
    /// with a bound the agent chose for itself.
    #[test]
    fn a_stale_arming_with_another_value_books_nothing() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let input = json!({ "pattern": "needle", "path": "." });
        let capped = gate(
            &hook("PreToolUse", 1, grep(input.clone())),
            tmp.path(),
            &levers(),
        );
        assert_eq!(cap_of(&capped).head_limit, 40);

        // The agent's own bound, on the same search, in the same window.
        let mut chosen = input.as_object().expect("the call").clone();
        chosen.insert("head_limit".to_owned(), json!(200));
        let executed = json!({
            "tool_name": "Grep",
            "tool_input": chosen,
            "tool_response": { "mode": "content", "numFiles": 1 },
        });
        let verified = gate(&hook("PostToolUse", 1, executed), tmp.path(), &levers());
        assert!(
            !verified.journaled,
            "a bound Rift did not ask for is not a cap Rift made"
        );
        assert!(journaled(tmp.path()).is_empty());
    }
}

//! The amended call: one family, two forms, one field changed, and never
//! beside a decline.

use rift_core::AgentId;
use serde_json::{json, Value};

use crate::respond::{gate, neutral, GateDecision, Rewrite};

/// **An amended call reaches one family, in two forms, changing one
/// field.** Swept over every family and verb: the amendment appears only
/// where a pinned row lists the form, the object it carries is the call
/// the agent wrote with exactly one key different, and every other family
/// answers as if nothing had been asked.
///
/// The closed set is the point. Both other families' own pages say a tool
/// call cannot be modified, and a third form here would stop being an
/// addition to what the agent asked for and start being a decision about
/// it — which is what the declined-call channel is for, with a reason
/// attached and a run behind it.
#[test]
fn a_rewrite_is_emitted_only_on_claude_code_and_only_in_the_two_forms() {
    let verbs = [
        "PreToolUse",
        "PostToolUse",
        "UserPromptSubmit",
        "SessionStart",
        "PreInvocation",
        "Stop",
        "",
    ];
    let call = json!({"file_path": "/w/notes.docx", "pattern": "needle"});
    let forms = [
        Rewrite::DocumentSidecar {
            tool_input: &call,
            file_path: "/w/.rift/state/doc/abc.md",
        },
        Rewrite::GrepHeadLimit {
            tool_input: &call,
            head_limit: 40,
        },
    ];
    for agent in AgentId::FIRST_PARTY {
        for verb in verbs {
            for form in forms {
                let out = gate(agent, verb, &GateDecision::Rewrite(form));
                let here = *agent == AgentId::CLAUDE_CODE && verb == "PreToolUse";
                let amended = out
                    .as_ref()
                    .and_then(|value| value["hookSpecificOutput"].get("updatedInput"))
                    .cloned();
                assert_eq!(
                    amended.is_some(),
                    here,
                    "{} {verb} amends a call on a channel no pinned row gives it",
                    agent.as_str()
                );
                let Some(amended) = amended else {
                    assert_eq!(
                        out,
                        gate(agent, verb, &GateDecision::Allow),
                        "{} {verb} answered a refused amendment visibly",
                        agent.as_str()
                    );
                    continue;
                };
                // The whole call travels, with one field different.
                let before = call.as_object().expect("a call is an object");
                let after = amended.as_object().expect("an amended call is an object");
                for key in before.keys() {
                    assert!(
                        after.contains_key(key),
                        "the amended call dropped `{key}`, which the agent wrote"
                    );
                }
                assert!(
                    after.len() <= before.len() + 1,
                    "the amended call grew by more than the one field"
                );
                let changed: Vec<&String> = after
                    .iter()
                    .filter(|(key, value)| before.get(*key) != Some(*value))
                    .map(|(key, _)| key)
                    .collect();
                assert_eq!(changed.len(), 1, "more than one field changed: {changed:?}");
                assert!(
                    matches!(changed[0].as_str(), "file_path" | "head_limit"),
                    "the changed field is {}, which is not one of the two forms",
                    changed[0]
                );
            }
        }
    }
}

/// A cap is added and never overridden, a sidecar replaces a path the call
/// actually carries, and neither is written into something that is not a
/// call. Each of these is an amendment that would change what the agent
/// asked for rather than add to it, so each answers as an allowed call.
#[test]
fn an_amendment_that_would_change_the_call_is_refused() {
    let capped = json!({"pattern": "needle", "head_limit": 5});
    let zero = json!({"pattern": "needle", "head_limit": 0});
    let read = json!({"file_path": "/w/notes.docx"});
    let not_a_call = json!(["file_path", "/w/notes.docx"]);
    let cases = [
        // A search that named its own cap keeps it, zero included.
        Rewrite::GrepHeadLimit {
            tool_input: &capped,
            head_limit: 40,
        },
        Rewrite::GrepHeadLimit {
            tool_input: &zero,
            head_limit: 40,
        },
        // A cap of nothing is not a cap.
        Rewrite::GrepHeadLimit {
            tool_input: &read,
            head_limit: 0,
        },
        // A substitution that substitutes nothing.
        Rewrite::DocumentSidecar {
            tool_input: &read,
            file_path: "/w/notes.docx",
        },
        // A sidecar with no path to point at.
        Rewrite::DocumentSidecar {
            tool_input: &read,
            file_path: "",
        },
        // A read with no path field to replace.
        Rewrite::DocumentSidecar {
            tool_input: &capped,
            file_path: "/w/.rift/state/doc/abc.md",
        },
        // Something that is not a call at all.
        Rewrite::DocumentSidecar {
            tool_input: &not_a_call,
            file_path: "/w/.rift/state/doc/abc.md",
        },
    ];
    for case in cases {
        assert_eq!(
            gate(
                &AgentId::CLAUDE_CODE,
                "PreToolUse",
                &GateDecision::Rewrite(case)
            ),
            neutral(&AgentId::CLAUDE_CODE, "PreToolUse"),
            "{case:?} was emitted"
        );
    }
}

/// A declined call carries no amendment and an amended call carries no
/// explanation. They are separate answers to separate questions, and a
/// response that held both would be telling a client to run a call it was
/// simultaneously told not to run.
#[test]
fn a_decline_and_an_amendment_never_travel_together() {
    let call = json!({"file_path": "/w/notes.docx"});
    let declined = gate(
        &AgentId::CLAUDE_CODE,
        "PreToolUse",
        &GateDecision::Deny {
            reason: "[rift] a rendered reason",
        },
    )
    .expect("a confirmed family answers a decline");
    assert!(!declined.to_string().contains("updatedInput"));
    let amended = gate(
        &AgentId::CLAUDE_CODE,
        "PreToolUse",
        &GateDecision::Rewrite(Rewrite::DocumentSidecar {
            tool_input: &call,
            file_path: "/w/.rift/state/doc/abc.md",
        }),
    )
    .expect("this family accepts both forms");
    assert!(!amended.to_string().contains("permissionDecisionReason"));
    assert_eq!(
        amended["hookSpecificOutput"]["permissionDecision"],
        Value::from("allow")
    );
}

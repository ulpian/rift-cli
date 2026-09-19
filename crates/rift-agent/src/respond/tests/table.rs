//! The capability table itself: well formed, closed at the pinned forms,
//! confirmed only by a recorded run, read only from inside a fixture's fence,
//! and the one rule the whole module exists to keep — no field on an unpinned
//! channel.

use rift_core::AgentId;
use serde_json::{json, Value};

use crate::respond::{
    capability_rows, gate, pinned_capability_rows, respond_with_decision, Channel, GateDecision,
    Rewrite, RewriteForm, CAPABILITIES,
};

/// **The rule this module exists to keep: no field on an unpinned
/// channel.** Every key `respond` can write, at the top level or inside
/// `hookSpecificOutput`, is one the family's capability rows entitle it to
/// write on that exact verb — and the rows exist only where a committed
/// pin says so.
///
/// Swept over every first-party family, so a family with no rows at all
/// can answer with an empty acknowledgement and nothing else, and a verb
/// no pin covers can carry no fact.
#[test]
fn every_response_shape_has_a_pinned_output_surface() {
    let verbs = [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "PreInvocation",
        "PostInvocation",
        "PreCompact",
        "PostCompact",
        "SubagentStart",
        "SubagentStop",
        "Stop",
        "SessionEnd",
        "BeforeModel",
        "",
    ];
    for agent in AgentId::FIRST_PARTY {
        for verb in verbs {
            let permitted = permitted_fields(agent, verb);
            let input = a_call();
            for decision in decisions(&input) {
                for inject in [None, Some("pointer")] {
                    let both = [
                        respond_with_decision(agent, verb, inject, &decision),
                        gate(agent, verb, &decision),
                    ];
                    for out in both.into_iter().flatten() {
                        let object = out.as_object().expect("a hook response is a JSON object");
                        let nested = object
                            .get("hookSpecificOutput")
                            .and_then(Value::as_object)
                            .into_iter()
                            .flat_map(|inner| inner.keys());
                        for key in object.keys().chain(nested) {
                            assert!(
                                permitted.contains(&key.as_str()),
                                "{} writes `{key}` on {verb}, which no pinned capability \
                                 row entitles it to; pin the channel or do not write the \
                                 field",
                                agent.as_str()
                            );
                        }
                    }
                }
            }
        }
    }
}

/// The table is a table: every row names one of the three families whose
/// output surface is pinned, on a non-empty verb, and no `(family, verb)`
/// carries the same kind of channel twice.
#[test]
fn the_capability_table_is_well_formed() {
    let pinned_families = [AgentId::CLAUDE_CODE, AgentId::CODEX, AgentId::ANTIGRAVITY];
    let mut seen: Vec<String> = Vec::new();
    for (agent, verb, channel) in CAPABILITIES {
        assert!(
            pinned_families.contains(agent),
            "{} has a capability row but no pinned output surface",
            agent.as_str()
        );
        assert!(!verb.is_empty(), "a capability row names no verb");
        let discriminant = channel
            .to_string()
            .split(' ')
            .next()
            .expect("a rendered channel starts with its name")
            .to_owned();
        let key = format!("{} {verb} {discriminant}", agent.as_str());
        assert!(!seen.contains(&key), "duplicate capability row: {key}");
        seen.push(key);
    }
    for agent in pinned_families {
        assert!(
            !capability_rows(&agent).is_empty(),
            "{} is an alpha family with no rows at all",
            agent.as_str()
        );
    }
}

/// A reason channel counts as confirmed on exactly the families whose
/// fixture carries a recorded run showing a model reading the reason —
/// and the fixture, not this list, is the authority: each adapter's
/// `the_capability_table_equals_the_pinned_fixtures` compares the two as
/// text in both directions, so a flag moved here without moving the
/// evidence reddens there.
///
/// Spelling the three out is what stops a fourth from arriving quietly:
/// this is the flag that decides whether Rift may decline a call at all.
#[test]
fn a_reason_channel_is_confirmed_only_where_a_run_was_recorded() {
    let expected = [
        // 11 declined reads of 11 read by the model, 0 re-issued.
        (AgentId::CLAUDE_CODE, true),
        // 10 declined reads of 10 read by the model, on the client that
        // has the surface; 10 re-issued, which is a mode's business.
        (AgentId::CODEX, true),
        // The reason reached the model 3 times of 3, synchronously.
        (AgentId::ANTIGRAVITY, true),
    ];
    for (agent, verb, channel) in CAPABILITIES {
        let Channel::DenyWithReason { confirmed, .. } = channel else {
            continue;
        };
        let Some((_, want)) = expected.iter().find(|(a, _)| a == agent) else {
            panic!("{} has a deny row this test does not list", agent.as_str())
        };
        assert_eq!(
            *confirmed,
            *want,
            "{} {verb} disagrees with the recorded runs; confirmation is an \
             observation in the family's fixture, not a table edit",
            agent.as_str()
        );
    }
}

/// Rewrites stay closed at two additive forms for Claude Code and one
/// bounded shell-read replacement for Codex.
#[test]
fn input_rewrites_are_the_three_pinned_forms_on_two_families() {
    let mut rows = Vec::new();
    for (agent, _, channel) in CAPABILITIES {
        if let Channel::UpdatedInput { forms } = channel {
            rows.push((agent.clone(), *forms));
        }
    }
    assert_eq!(
        rows,
        [
            (
                AgentId::CLAUDE_CODE,
                &[RewriteForm::DocumentSidecarPath, RewriteForm::GrepHeadLimit,][..],
            ),
            (AgentId::CODEX, &[RewriteForm::ShellReadReplacement][..],),
        ]
    );
}

/// A fixture's rows are read from inside its fence and nowhere else, so
/// prose that quotes a row while explaining it never becomes one.
#[test]
fn capability_rows_are_read_only_from_inside_the_fence() {
    let fixture = "\
verb=PreToolUse channel=Neutral
```rift-capability-rows
verb=SessionStart channel=AdditionalContext limit_field=none
```
verb=Stop channel=Neutral
";
    assert_eq!(
        pinned_capability_rows(fixture),
        ["verb=SessionStart channel=AdditionalContext limit_field=none"]
    );
}

/// The field names a family's pinned rows entitle Rift to write on this
/// verb. Derived from the rows rather than listed separately, so a channel
/// cannot be given a field the table does not carry.
fn permitted_fields(agent: &AgentId, verb: &str) -> Vec<&'static str> {
    let mut fields = Vec::new();
    for (a, v, channel) in CAPABILITIES {
        if a != agent || *v != verb {
            continue;
        }
        match channel {
            Channel::AdditionalContext { .. } => {
                fields.extend(["hookSpecificOutput", "hookEventName", "additionalContext"]);
            }
            Channel::DenyWithReason { reason_field, .. } => {
                fields.extend([
                    "hookSpecificOutput",
                    "hookEventName",
                    "decision",
                    "permissionDecision",
                    reason_field,
                ]);
            }
            Channel::UpdatedInput { .. } => {
                fields.extend([
                    "hookSpecificOutput",
                    "hookEventName",
                    "permissionDecision",
                    "updatedInput",
                ]);
            }
            Channel::InjectSteps { .. } => fields.push("injectSteps"),
            Channel::Neutral => fields.push("decision"),
        }
    }
    fields
}

/// Every decision the gate can be handed, so a sweep covers the answers as
/// well as the questions.
fn decisions(input: &Value) -> Vec<GateDecision<'_>> {
    vec![
        GateDecision::Allow,
        GateDecision::Deny {
            reason: "[rift] a rendered reason",
        },
        GateDecision::Rewrite(Rewrite::DocumentSidecar {
            tool_input: input,
            file_path: "/w/.rift/state/doc/abc.md",
        }),
        GateDecision::Rewrite(Rewrite::GrepHeadLimit {
            tool_input: input,
            head_limit: 40,
        }),
    ]
}

/// A call as an agent wrote it, carrying both fields the two rewrites
/// touch plus one they must leave alone.
fn a_call() -> Value {
    json!({"file_path": "/w/notes.docx", "pattern": "needle", "output_mode": "content"})
}

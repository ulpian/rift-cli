//! The injected text: the boundary rows it travels on, the step or context
//! field it is written into, the bound a family declares for it, and the
//! silence every other verb and family keeps.

use rift_core::AgentId;
use rift_recall_types::{ContextKind, ContextPlan};
use serde_json::{json, Value};

use crate::respond::{
    accepts_injection, capability_rows, declared_context_limit, delivers_read_replacement,
    injection_points, respond, respond_with_decision, respond_with_plan, GateDecision, InjectKind,
    DECLARED_CONTEXT_LIMIT_TOKENS,
};

/// **An allowed call is answered exactly as it was before the gate
/// existed.** The gate is new behaviour for a decision Rift did not make
/// until now; it is not a new answer to the calls it was already
/// answering, and this test is the whole set of those answers written out
/// as bytes rather than derived from the code that produces them.
#[test]
fn an_allowed_call_leaves_every_response_byte_as_it_was() {
    let cc = AgentId::CLAUDE_CODE;
    let agy = AgentId::ANTIGRAVITY;
    let expected: &[(&AgentId, &str, Option<&str>, Option<Value>)] = &[
        (
            &cc,
            "UserPromptSubmit",
            Some("hello"),
            Some(json!({"hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": "hello",
            }})),
        ),
        (&cc, "UserPromptSubmit", None, None),
        (&cc, "PreToolUse", Some("hello"), None),
        (
            &cc,
            "SessionStart",
            Some("hello"),
            Some(json!({"hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": "hello",
            }})),
        ),
        (&agy, "PreToolUse", None, Some(json!({"decision": "allow"}))),
        (&agy, "Stop", None, Some(json!({"decision": "stop"}))),
        (&agy, "PostToolUse", None, Some(json!({}))),
        (&agy, "PostInvocation", None, Some(json!({}))),
        // The one row that moved when this family's boundary was wired:
        // offered text, it now writes the step its pinned row names, and
        // offered none it is the acknowledgement it always was.
        (
            &agy,
            "PreInvocation",
            Some("hello"),
            Some(json!({"injectSteps": [{"userMessage": "hello"}]})),
        ),
        (&agy, "PreInvocation", None, Some(json!({}))),
        // And the row that moved when the bounded channel was wired: the
        // shape is the one every context field carries, and what is new is
        // that a caller now fits its text to the bound before offering it.
        (
            &AgentId::CODEX,
            "UserPromptSubmit",
            Some("hello"),
            Some(json!({"hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": "hello",
            }})),
        ),
        (&AgentId::CODEX, "UserPromptSubmit", None, None),
    ];
    for (agent, verb, inject, want) in expected {
        assert_eq!(
            respond(agent, verb, *inject),
            *want,
            "{} {verb} moved",
            agent.as_str()
        );
        assert_eq!(
            respond_with_decision(agent, verb, *inject, &GateDecision::Allow),
            *want,
            "{} {verb} moved under an explicit allow",
            agent.as_str()
        );
    }
}

#[test]
fn native_context_encoding_preserves_the_dispatchers_plan_verbatim() {
    let plan = ContextPlan::Deliver {
        kind: ContextKind::ReadReplacement,
        text: "[rift] prepared result".to_owned(),
        delivered_tokens: 5,
        avoided_tokens: 30,
        fallback: true,
    };
    assert_eq!(
        respond_with_plan(
            &AgentId::ANTIGRAVITY,
            "PreInvocation",
            &plan,
            &GateDecision::Allow,
        ),
        Some(json!({"injectSteps": [{"userMessage": "[rift] prepared result"}]}))
    );
    assert_eq!(
        respond_with_plan(
            &AgentId::CODEX,
            "UserPromptSubmit",
            &plan,
            &GateDecision::Allow,
        ),
        Some(json!({"hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "additionalContext": "[rift] prepared result",
        }}))
    );
    let shell_plan = ContextPlan::Deliver {
        kind: ContextKind::ReadReplacement,
        text: "[rift] it's already held".to_owned(),
        delivered_tokens: 6,
        avoided_tokens: 30,
        fallback: false,
    };
    assert_eq!(
        respond_with_plan(
            &AgentId::CODEX,
            "PreToolUse",
            &shell_plan,
            &GateDecision::Deny {
                reason: "the raw read is redundant",
            },
        ),
        Some(json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": {
                "command": "printf '%s\\n' '[rift] it'\\''s already held'",
            },
        }}))
    );
    assert!(delivers_read_replacement(
        &respond_with_plan(
            &AgentId::CODEX,
            "PreToolUse",
            &shell_plan,
            &GateDecision::Deny {
                reason: "the raw read is redundant",
            },
        )
        .expect("the replacement is encoded")
    ));
    assert!(!delivers_read_replacement(
        &json!({"hookSpecificOutput": {"additionalContext": "not a replacement"}})
    ));
}

/// **A step channel writes one step, of the kind its row names, carrying
/// nothing else.** The array holds exactly one object, that object holds
/// exactly the one text field the recorded run chose, and no response on
/// this family ever carries the field that would steer the agent's loop.
///
/// The kind is the load-bearing half. Both text kinds reach the model at
/// the invocation they are injected into; a few invocations later only the
/// user kind is still in its reach, and every fact this channel carries —
/// a session pointer, what the context holds, what it stopped holding — is
/// read later than that. A row that named the transient kind would inject
/// on time and be gone by the time it mattered.
#[test]
fn a_step_channel_writes_one_step_of_the_kind_its_row_names() {
    let agy = AgentId::ANTIGRAVITY;
    let out = respond(&agy, "PreInvocation", Some("[rift] a pointer")).expect("a response");
    let steps = out["injectSteps"]
        .as_array()
        .expect("the steps travel in an array");
    assert_eq!(steps.len(), 1, "one boundary, one step: {out}");
    let step = steps[0].as_object().expect("a step is an object");
    assert_eq!(
        step.keys().collect::<Vec<_>>(),
        ["userMessage"],
        "the kind the recorded run found still readable, and no other field"
    );
    assert_eq!(step["userMessage"], "[rift] a pointer");
    assert!(
        !out.to_string().contains("terminationBehavior"),
        "a hook that steers the agent's loop is a hook that blocks it: {out}"
    );
    // A kind that is not text has nothing to write, so the family answers
    // its acknowledgement instead of a step Rift composed.
    assert_eq!(InjectKind::ToolCall.message_field(), None);
    assert_eq!(InjectKind::UserMessage.message_field(), Some("userMessage"));
    assert_eq!(
        InjectKind::EphemeralMessage.message_field(),
        Some("ephemeralMessage")
    );
}

/// Text offered to a family with no channel is dropped, never emitted in
/// an invented shape — the caller may compute a pointer without knowing
/// the table, and no hook API ever sees JSON Rift guessed at.
#[test]
fn offering_a_pointer_to_a_channelless_family_never_invents_a_shape() {
    for agent in AgentId::FIRST_PARTY {
        if accepts_injection(agent, "UserPromptSubmit") {
            continue;
        }
        let out = respond(agent, "UserPromptSubmit", Some("pointer"));
        let neutral_out = respond(agent, "UserPromptSubmit", None);
        assert_eq!(
            out,
            neutral_out,
            "{} responded differently because a pointer was offered",
            agent.as_str()
        );
        if let Some(v) = out {
            assert!(
                !v.to_string().contains("pointer"),
                "{} leaked the pointer into a response shape it does not have",
                agent.as_str()
            );
        }
    }
}

/// An empty pointer is not a pointer: the response falls back to the
/// family's neutral contract rather than injecting nothing loudly.
#[test]
fn an_empty_pointer_injects_nothing() {
    assert_eq!(
        respond(&AgentId::CLAUDE_CODE, "UserPromptSubmit", Some("")),
        None
    );
}

/// Every family's injection points are exactly the boundary rows its
/// pinned surface carries — including the family whose channel this module
/// has a row for and no writer yet.
///
/// The two halves are stated apart because they diverge on purpose: the
/// **row** exists as soon as the fixture pins it, which is what the
/// per-family wiring is built against; the **writer** exists only where the
/// shape it would write is the shape the family documented. A family whose
/// channel bounds its text is a row here and silence on the wire until its
/// own ticket fits the text to that bound.
#[test]
fn injection_points_are_the_pinned_boundary_rows_and_a_result_tag_is_not_one() {
    let rows = |agent: &AgentId| -> Vec<String> {
        injection_points(agent)
            .iter()
            .map(|(verb, channel)| format!("verb={verb} {channel}"))
            .collect()
    };
    assert_eq!(
        rows(&AgentId::CLAUDE_CODE),
        [
            "verb=UserPromptSubmit channel=AdditionalContext limit_field=none",
            "verb=SessionStart channel=AdditionalContext limit_field=none",
            "verb=SubagentStart channel=AdditionalContext limit_field=none",
        ]
    );
    assert_eq!(
        rows(&AgentId::CODEX),
        [
            "verb=UserPromptSubmit channel=AdditionalContext \
             limit_field=additionalContextLimit",
            "verb=SessionStart channel=AdditionalContext limit_field=additionalContextLimit",
        ]
    );
    assert_eq!(
        rows(&AgentId::ANTIGRAVITY),
        ["verb=PreInvocation channel=InjectSteps kind=user_message"]
    );
    // Every row this returns is a row the family's fixture pins, because
    // it is the same table read twice.
    for agent in [AgentId::CLAUDE_CODE, AgentId::CODEX, AgentId::ANTIGRAVITY] {
        let pinned = capability_rows(&agent);
        for row in rows(&agent) {
            assert!(pinned.contains(&row), "{row} is not a pinned row");
        }
        assert!(
            !injection_points(&agent)
                .iter()
                .any(|(verb, _)| *verb == "PostToolUse"),
            "a context field beside a result is a tag, not a boundary: {}",
            agent.as_str()
        );
    }
    // The writer is narrower than the table, and says so in both
    // directions.
    assert!(accepts_injection(&AgentId::CLAUDE_CODE, "SessionStart"));
    assert!(accepts_injection(&AgentId::CLAUDE_CODE, "UserPromptSubmit"));
    assert!(accepts_injection(&AgentId::CLAUDE_CODE, "SubagentStart"));
    assert!(!accepts_injection(&AgentId::CLAUDE_CODE, "PostToolUse"));
    assert!(accepts_injection(&AgentId::CODEX, "SessionStart"));
    assert!(accepts_injection(&AgentId::ANTIGRAVITY, "PreInvocation"));
}

/// A limit is stated for exactly the rows whose pinned surface names the
/// key it is written under, and for nothing else.
///
/// The two directions matter separately. A family that declares no limit
/// must answer `None` rather than a default, or a caller would fit text to
/// a bound nobody set; and a channel that is not a context field has no
/// limit to state at all, however bounded its own contents are.
#[test]
fn a_context_limit_is_declared_where_the_pinned_row_names_its_key() {
    // The shape first: the same context field Claude Code is written on,
    // on both of this family's boundaries, exactly as its own generated
    // schemas pin it — a bounded channel is not a different channel.
    for verb in ["UserPromptSubmit", "SessionStart"] {
        let out = respond(&AgentId::CODEX, verb, Some("hello")).expect("a response");
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], verb);
        assert_eq!(out["hookSpecificOutput"]["additionalContext"], "hello");
    }
    assert_eq!(respond(&AgentId::CODEX, "Stop", Some("hello")), None);
    for verb in ["UserPromptSubmit", "SessionStart"] {
        assert_eq!(
            declared_context_limit(&AgentId::CODEX, verb),
            Some(DECLARED_CONTEXT_LIMIT_TOKENS),
            "codex {verb}"
        );
        assert_eq!(
            declared_context_limit(&AgentId::CLAUDE_CODE, verb),
            None,
            "claude-code {verb} pins the field and no bound on it"
        );
    }
    assert_eq!(
        declared_context_limit(&AgentId::ANTIGRAVITY, "PreInvocation"),
        None,
        "a trajectory step is not a context field"
    );
    for verb in ["PreToolUse", "PostToolUse", "Stop", ""] {
        assert_eq!(
            declared_context_limit(&AgentId::CODEX, verb),
            None,
            "{verb}"
        );
    }
    assert_eq!(
        declared_context_limit(&AgentId::new("some-third-party-agent"), "UserPromptSubmit"),
        None
    );
}

/// An unknown third-party agent is silent on every verb: Rift never
/// answers a hook API it has no contract with.
#[test]
fn an_unknown_agent_is_always_silent() {
    let stranger = AgentId::new("some-third-party-agent");
    assert!(injection_points(&stranger).is_empty());
    for verb in ["UserPromptSubmit", "PreToolUse", "Stop", ""] {
        assert_eq!(respond(&stranger, verb, Some("pointer")), None);
    }
}

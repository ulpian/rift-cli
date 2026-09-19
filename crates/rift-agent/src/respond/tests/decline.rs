//! The declined call: emitted only where a recorded run confirmed the model
//! reads the reason, passed through byte for byte, and written on the one
//! route each family's fixture pins.

use rift_core::AgentId;
use serde_json::{json, Value};

use crate::respond::{
    capability_rows, declared_context_limit, gate, neutral, respond_with_decision, GateDecision,
};

/// The families whose fixture carries a run showing a model reading a
/// declined call's reason, and the verb it was seen on.
const CONFIRMED_DENY: &[(AgentId, &str)] = &[
    (AgentId::CLAUDE_CODE, "PreToolUse"),
    (AgentId::CODEX, "PreToolUse"),
    (AgentId::ANTIGRAVITY, "PreToolUse"),
];

/// **A call is declined only where a model will read why.** Swept over
/// every family and every verb: the reason appears in a response exactly
/// on the pairs whose pinned row is a confirmed explanation channel, and
/// on every other pair the answer is indistinguishable from the one an
/// allowed call gets — the same bytes, carrying none of the reason.
///
/// This is the rule the whole design rests on. A family that declines a
/// call whose reason its model never sees has not made a decision; it has
/// produced a tool failure, which a model is right to retry, and the
/// retries are the pool this program is trying to shrink.
#[test]
fn a_deny_is_emitted_only_on_a_family_with_a_confirmed_reason_channel() {
    let reason = "[rift] a rendered reason";
    let verbs = [
        "PreToolUse",
        "PostToolUse",
        "UserPromptSubmit",
        "SessionStart",
        "PreInvocation",
        "PreCompact",
        "Stop",
        "",
    ];
    for agent in AgentId::FIRST_PARTY {
        for verb in verbs {
            let out = gate(agent, verb, &GateDecision::Deny { reason });
            let carried = out
                .as_ref()
                .is_some_and(|value| value.to_string().contains(reason));
            let confirmed = CONFIRMED_DENY.iter().any(|(a, v)| a == agent && *v == verb);
            assert_eq!(
                carried,
                confirmed,
                "{} {verb}: a decline is emitted exactly where a recorded run \
                 confirms the model reads the reason",
                agent.as_str()
            );
            if !confirmed {
                assert_eq!(
                    out,
                    gate(agent, verb, &GateDecision::Allow),
                    "{} {verb} answers a refused decline differently from an \
                     allowed call, which tells a client something happened",
                    agent.as_str()
                );
            }
        }
    }
}

/// The reason travels as it was rendered. One renderer writes what a model
/// reads; a second place that could trim, wrap or prefix those bytes is a
/// second renderer, and the red-team corpus only covers the first.
#[test]
fn the_reason_is_passed_through_byte_for_byte() {
    let reason = "[rift] Read skipped: a file you read L1-40 are unchanged.  \n\ttrailing";
    for (agent, verb) in CONFIRMED_DENY {
        let out = gate(agent, verb, &GateDecision::Deny { reason })
            .expect("a confirmed family answers a decline");
        let field = match agent {
            a if *a == AgentId::ANTIGRAVITY => out["reason"].clone(),
            _ => out["hookSpecificOutput"]["permissionDecisionReason"].clone(),
        };
        assert_eq!(field, Value::from(reason), "{}", agent.as_str());
    }
}

/// Anything the gate cannot do is the family's neutral response — the
/// answer an allowed call gets. An unknown family, a verb with no row, a
/// decline with nothing to say: each is a mistake somewhere upstream, and
/// the shape of the mistake is that the call simply runs.
#[test]
fn an_error_in_the_gate_is_the_neutral_response() {
    let cases = [
        // A family with no pinned surface at all.
        (AgentId::new("some-third-party-agent"), "PreToolUse"),
        // A verb this family has no explanation channel on.
        (AgentId::CLAUDE_CODE, "PostToolUse"),
        (AgentId::CODEX, "UserPromptSubmit"),
        (AgentId::ANTIGRAVITY, "Stop"),
        // A verb nobody declares.
        (AgentId::CLAUDE_CODE, ""),
    ];
    for (agent, verb) in &cases {
        assert_eq!(
            gate(agent, verb, &GateDecision::Deny { reason: "why" }),
            neutral(agent, verb),
            "{} {verb} answered a decline it cannot make",
            agent.as_str()
        );
    }
    // An empty explanation is the unexplained failure the confirmation
    // requirement exists to prevent, even on a confirmed family.
    for (agent, verb) in CONFIRMED_DENY {
        for reason in ["", "   ", "\n\t "] {
            assert_eq!(
                gate(agent, verb, &GateDecision::Deny { reason }),
                neutral(agent, verb),
                "{} {verb} declined a call with nothing to say for it",
                agent.as_str()
            );
        }
    }
}

/// ★ **A declined call on the family that publishes two decline routes is
/// written on the supported one, and only on it.**
///
/// The pinned schema declares both: a nested `permissionDecision` pair, and
/// a top-level `decision` of `"block"` with a `reason` beside it that the
/// client's own test names deprecated. A recorded run found both still
/// blocking, which is exactly the situation in which a writer drifts onto
/// the older one and nobody notices — so the object is written out here
/// key by key, and the deprecated keys are asserted absent rather than
/// merely not mentioned.
#[test]
fn a_decline_is_written_on_the_supported_route_and_never_the_deprecated_one() {
    let reason = "[rift] Read skipped: notes.md@sha256:abcdef01 lines 1-40 are unchanged.";
    let out = gate(
        &AgentId::CODEX,
        "PreToolUse",
        &GateDecision::Deny { reason },
    )
    .expect("the run recorded in this family's fixture confirmed the channel");
    assert_eq!(
        out,
        json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }}),
        "the bytes this family's client parses, and no others"
    );
    let object = out.as_object().expect("one object");
    assert_eq!(object.len(), 1, "exactly one top-level key");
    for deprecated in ["decision", "reason", "continue", "stopReason"] {
        assert!(
            !object.contains_key(deprecated),
            "the deprecated route's {deprecated} was written"
        );
    }
    // The client refuses a decline with nothing to say, in as many words
    // (the fixture quotes the message). Composing one would spend a round
    // trip to be told so, which is why nothing composes one.
    for empty in ["", "   ", "\n"] {
        assert_eq!(
            gate(
                &AgentId::CODEX,
                "PreToolUse",
                &GateDecision::Deny { reason: empty }
            ),
            None,
            "a decline with nothing to say is not written"
        );
    }
    // The separately planned bounded replacement uses Codex's confirmed
    // updatedInput route. A raw decline remains exactly the object above:
    // the two response forms are selected before encoding and never travel
    // together.
    assert!(capability_rows(&AgentId::CODEX)
        .iter()
        .any(|row| { row == "verb=PreToolUse channel=UpdatedInput forms=shell_read_replacement" }));
    // The bound Rift declares for its own boundary text belongs to the two
    // boundary verbs and not to this one. A decline is a different message
    // at a different moment: it is never fitted to that number, never
    // counted against it, and never travels beside the text that is.
    assert_eq!(declared_context_limit(&AgentId::CODEX, "PreToolUse"), None);
    assert!(declared_context_limit(&AgentId::CODEX, "UserPromptSubmit").is_some());
    assert!(!out.to_string().contains("additionalContext"));
    assert_eq!(
        respond_with_decision(
            &AgentId::CODEX,
            "PreToolUse",
            Some("a session pointer"),
            &GateDecision::Deny { reason },
        ),
        Some(out),
        "a decline is the whole answer; the pointer is dropped"
    );
}

/// **The declined shapes as bytes**, because a client reads bytes and not
/// a value tree.
///
/// Two families are declined and each has its own envelope: one nests the
/// decision beside the event name, the other answers a flat object. A
/// field in the right place with the wrong shape around it is a call that
/// runs where Rift meant to decline it, or an agent handed a document it
/// cannot parse — neither of which a comparison of parsed values would
/// catch. Each family's own artifact pins the same object beside the
/// recorded run it came from and its adapter asserts the equality against
/// that pin; the literals here are what makes this table readable as one
/// page.
#[test]
fn the_declined_envelopes_are_the_bytes_each_family_is_pinned_to() {
    let reason = "[rift] a rendered reason";
    let quoted = serde_json::to_string(reason).expect("a string serialises");
    let bytes = |agent: &AgentId| {
        let value = gate(agent, "PreToolUse", &GateDecision::Deny { reason })
            .expect("a confirmed reason channel is answered on");
        serde_json::to_string(&value).expect("a response serialises")
    };
    assert_eq!(
        bytes(&AgentId::ANTIGRAVITY),
        format!("{{\"decision\":\"deny\",\"reason\":{quoted}}}")
    );
    assert_eq!(
        bytes(&AgentId::CLAUDE_CODE),
        format!(
            "{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\
             \"permissionDecision\":\"deny\",\"permissionDecisionReason\":{quoted}}}}}"
        )
    );
}

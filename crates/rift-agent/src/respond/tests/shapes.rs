//! The families Rift configures, and the response contract each one has,
//! written out as bytes: one row per family, walked by one test per family.
//!
//! This is the conformance table the injection design asks for: adding a
//! family without deciding its response shape fails here. A row states the
//! whole of what a family is answered — its boundary verbs, whether a pointer
//! offered at a verb is written, the rows its pin must carry, the response on
//! every verb worth naming, and the declined-call envelope where a recorded
//! run confirmed one — so one family's contract reads as one page.
//!
//! A pinned channel is not automatically a wired one. Every boundary row in
//! the table is written on — including the bounded one, whose caller fits its
//! text to the number Rift declared — and every declined call here is
//! composed by the gate; whether one is ever *asked* for is the repository's
//! policy to say, and no part of that decision is here.

use rift_core::AgentId;
use serde_json::{json, Value};

use crate::respond::{
    accepts_injection, capability_rows, gate, injection_points, respond, GateDecision,
};

/// The reason every declined call in the table carries. It travels byte for
/// byte, so any string proves the envelope.
const REASON: &str = "[rift] a rendered reason";

/// What one family's pinned rows entitle `respond` and `gate` to write.
struct PinnedShape {
    agent: AgentId,
    /// The verbs `injection_points` lists, in table order.
    boundaries: &'static [&'static str],
    /// `(verb, accepted)`: whether a pointer offered at the verb is written.
    accepts: &'static [(&'static str, bool)],
    /// Rows the family's table must carry, spelled as its fixture spells them.
    rows: &'static [&'static str],
    /// `(verb, offered text, answer)`: the bytes `respond` writes.
    responses: Vec<(&'static str, Option<&'static str>, Option<Value>)>,
    /// The declined-call envelope on `PreToolUse`, or `None` where no recorded
    /// run confirmed the family's model reads a reason.
    declined: Option<Value>,
}

/// Claude Code: the verified context channel on every boundary it pins, and
/// nothing at all on any other verb, injected text or not.
#[test]
fn claude_code_answers_the_shapes_its_pin_names() {
    assert_pinned(&claude_code_shape());
}

/// Codex: both boundaries on the same context field, and the nested decline
/// envelope its client parses.
#[test]
fn codex_answers_the_shapes_its_pin_names() {
    assert_pinned(&codex_shape());
}

/// Antigravity: a decision object is required, Rift always allows, and the
/// one boundary writes the step its pinned row names.
#[test]
fn antigravity_answers_the_shapes_its_pin_names() {
    assert_pinned(&antigravity_shape());
}

/// Gemini: an observational family with no pinned context channel — its
/// guidance ships in the managed instructions file instead.
#[test]
fn gemini_stays_silent_on_every_verb() {
    assert_pinned(&gemini_shape());
}

/// Every assertion one row makes, in the order the row states them.
fn assert_pinned(shape: &PinnedShape) {
    let agent = &shape.agent;
    let family = agent.as_str();
    let boundaries: Vec<&str> = injection_points(agent)
        .iter()
        .map(|(verb, _)| *verb)
        .collect();
    assert_eq!(
        boundaries, shape.boundaries,
        "{family}: every boundary this family pins, and the post-result tag is not one"
    );
    for (verb, accepted) in shape.accepts {
        assert_eq!(
            accepts_injection(agent, verb),
            *accepted,
            "{family} {verb}: a pinned channel is written on exactly where the row says"
        );
    }
    let pinned = capability_rows(agent);
    for row in shape.rows {
        assert!(
            pinned.iter().any(|pinned| pinned == row),
            "{family} carries no row {row}"
        );
    }
    for (verb, inject, want) in &shape.responses {
        assert_eq!(
            &respond(agent, verb, *inject),
            want,
            "{family} {verb} must answer the shape its pin names"
        );
    }
    // Which envelope a family takes is a fact about that family, so each is
    // written out as bytes rather than derived from the field names.
    assert_eq!(
        gate(agent, "PreToolUse", &GateDecision::Deny { reason: REASON }),
        shape.declined,
        "{family}: the declined-call envelope its fixture pins"
    );
}

fn claude_code_shape() -> PinnedShape {
    PinnedShape {
        agent: AgentId::CLAUDE_CODE,
        boundaries: &["UserPromptSubmit", "SessionStart", "SubagentStart"],
        accepts: &[],
        rows: &["verb=SessionStart channel=AdditionalContext limit_field=none"],
        responses: vec![
            (
                "UserPromptSubmit",
                Some("hello"),
                Some(json!({"hookSpecificOutput": {
                    "hookEventName": "UserPromptSubmit",
                    "additionalContext": "hello",
                }})),
            ),
            // …and nothing at all on any other verb, injected text or not.
            ("PostToolUse", Some("hello"), None),
            ("UserPromptSubmit", None, None),
            (
                "SessionStart",
                Some("hello"),
                Some(json!({"hookSpecificOutput": {
                    "hookEventName": "SessionStart",
                    "additionalContext": "hello",
                }})),
            ),
        ],
        declined: Some(json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": REASON,
        }})),
    }
}

/// The bounded channel is asserted where the bound is
/// (`a_context_limit_is_declared_…`); this row pins the shape, which is the
/// one every context field carries.
fn codex_shape() -> PinnedShape {
    PinnedShape {
        agent: AgentId::CODEX,
        boundaries: &["UserPromptSubmit", "SessionStart"],
        accepts: &[],
        rows: &[
            "verb=UserPromptSubmit channel=AdditionalContext limit_field=additionalContextLimit",
        ],
        responses: vec![(
            "UserPromptSubmit",
            Some("hello"),
            Some(json!({"hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": "hello",
            }})),
        )],
        declined: Some(json!({"hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": REASON,
        }})),
    }
}

/// Offered text, the boundary writes the step its pinned row names; offered
/// none, and after an invocation, it is the acknowledgement it always was.
fn antigravity_shape() -> PinnedShape {
    PinnedShape {
        agent: AgentId::ANTIGRAVITY,
        boundaries: &["PreInvocation"],
        accepts: &[("PreInvocation", true)],
        rows: &["verb=PreInvocation channel=InjectSteps kind=user_message"],
        responses: vec![
            ("PreToolUse", None, Some(json!({ "decision": "allow" }))),
            ("Stop", None, Some(json!({ "decision": "stop" }))),
            ("PostToolUse", None, Some(json!({}))),
            (
                "PreInvocation",
                Some("hello"),
                Some(json!({"injectSteps": [{"userMessage": "hello"}]})),
            ),
            ("PostInvocation", Some("hello"), Some(json!({}))),
        ],
        declined: Some(json!({"decision": "deny", "reason": REASON})),
    }
}

fn gemini_shape() -> PinnedShape {
    PinnedShape {
        agent: AgentId::GEMINI,
        boundaries: &[],
        accepts: &[("UserPromptSubmit", false)],
        rows: &[],
        responses: vec![
            ("SessionStart", Some("hello"), None),
            ("UserPromptSubmit", Some("hello"), None),
            ("BeforeModel", Some("hello"), None),
            ("Stop", Some("hello"), None),
        ],
        declined: None,
    }
}

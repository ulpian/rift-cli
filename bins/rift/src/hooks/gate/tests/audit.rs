//! The accounting row a decision owes the detached worker — and the calls
//! that owe none.

use super::fixtures::*;
use crate::hooks::gate::answer::read_decision_audit;

/// The row a repeat owes is journaled, and only for the repeat: the first
/// read decided nothing, so it owes nothing. What is written names the
/// bytes (path, revision, blob, lines) and no content, because the worker
/// must be able to count exactly those bytes or none.
#[test]
fn the_repeat_the_gate_observes_journals_the_row_it_owes() {
    let (tmp, file) = checkout(120);
    let here = tmp.path();
    let payload = read_call(&file, Some(10), Some(20));
    let call = hook("claude-code", "PreToolUse", "s-8", payload);
    assert!(
        !gate_observing(&call, here).journaled,
        "an allowed read owes no accounting"
    );
    assert!(journaled(here).is_empty());

    assert!(
        gate_observing(&call, here).journaled,
        "the repeat is what sizes the pool"
    );
    let jobs = journaled(here);
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    let crate::agent_queue::JobKind::ServeAudit(audit) = &jobs[0].kind else {
        panic!("the gate journals an accounting row: {:?}", jobs[0].kind);
    };
    assert_eq!(
        audit.decision,
        crate::agent_queue::ServeAuditKind::ReadRepeatObserved
    );
    assert_eq!(audit.agent, "claude-code");
    assert!(audit.frame.is_empty(), "nothing was sent, so nothing cost");
    let basis = audit.basis.as_ref().expect("the bytes to count");
    assert_eq!(basis.rel_path, "src.rs");
    assert_eq!(
        basis.lines,
        Some(crate::agent_queue::AuditLines { lo: 10, hi: 29 })
    );
    assert_eq!(basis.content_sha256, revision(here, "src.rs").to_hex());
    assert!(!basis.blob_oid.is_empty(), "the fallback source is named");
    assert_eq!(jobs[0].queued_at_ms, 1_700_000_000_000, "the hook\'s clock");
    assert_eq!(
        jobs[0].work_tree, here,
        "the checkout the worker resolves the family from"
    );
}

/// The decline arm of the same mapping, reached through the pure function
/// because no family is declined on while every lever is in observe. A
/// decline books what it declined AND what its reason cost; an allowed
/// read books nothing at all.
#[test]
fn a_decline_books_its_frame_and_an_allowed_read_books_nothing() {
    use rift_agent::window_journal::{Decision, DenyShape, ResolvedRead};
    let read = ResolvedRead::new(
        &rift_agent::ReadRequest {
            path: "src.rs".to_owned(),
            span: rift_agent::ReadSpan::lines(1, rift_agent::HiKind::Explicit(10)),
            channel: rift_agent::ReadChannel::Tool,
        },
        rift_core::Sha256Id::parse_hex(&"ab".repeat(32)).expect("a revision"),
        80,
    )
    .expect("a resolved read");
    let agent = rift_core::AgentId::CLAUDE_CODE;
    let id = || "0123456789abcdef0123456789abcdef".to_owned();
    assert!(
        read_decision_audit(&agent, &read, "oid", &Decision::Allow, "", false, id()).is_none(),
        "nothing was decided, so nothing is owed"
    );
    let denied = read_decision_audit(
        &agent,
        &read,
        "oid",
        &Decision::Deny(DenyShape::Plain),
        "you already hold these lines",
        false,
        id(),
    )
    .expect("the row a decline owes");
    assert_eq!(
        denied.decision,
        crate::agent_queue::ServeAuditKind::ReadIntercepted
    );
    assert_eq!(denied.frame, "you already hold these lines");
    assert_eq!(denied.call_id, id());
}

/// A tool call that FAILED is not a result this window holds. The family
/// publishes its own verb for one, and it is not a step of this gate, so
/// no line is written and no coverage is credited — which is what stops a
/// later decline resting on bytes the model never received.
#[test]
fn a_failed_tool_call_leaves_the_window_untouched() {
    let (tmp, file) = checkout(40);
    let here = tmp.path();
    let payload = read_call(&file, None, Some(10));
    gate_observing(
        &hook("claude-code", "PostToolUseFailure", "s-9", payload),
        here,
    );
    assert!(
        lines_of(here, "s-9", None).is_empty(),
        "a failed call hands the window nothing"
    );
    assert!(journaled(here).is_empty());
}

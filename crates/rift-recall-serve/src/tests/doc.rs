//! The document verb: the sidecar through the chokepoint, the gates in front
//! of it, and the two answers when there is no sidecar yet.

use rift_recall_policy::{PolicyEngine, TurnBudget};
use rift_recall_substrate::MemorySubstrate;
use rift_recall_types::OmitReason;
use serde_json::json;

use super::fixtures::*;
use crate::*;

/// One document, as the binary resolves it: the sidecar's Markdown, the
/// document's own content address, and the declared cost of the read it stands
/// in for.
fn doc_request(path: &str, markdown: &str, baseline: u32) -> ToolRequest {
    ToolRequest {
        target: Some(path.to_owned()),
        document: Some(Document {
            source_sha256: SHA_A.to_owned(),
            declared_baseline: baseline,
            sidecar: SidecarState::Held(Sidecar {
                path: ".rift/state/doc/deadbeef.md".to_owned(),
                markdown: markdown.to_owned(),
            }),
        }),
        ..Default::default()
    }
}

/// ★ **The sidecar is served as raw Markdown, through the chokepoint, and the
/// figures are the declared baseline minus what it cost.**
///
/// Everything this verb exists for is in one response: the Markdown arrives
/// verbatim (it is the read it replaces, so it is raw and it is whole), the
/// state is the sealed constructor's `exact_included` rather than a word this
/// handler chose, the audit row is a `doc_served` naming the DOCUMENT — not the
/// sidecar — and the avoided figure is the declared baseline net of what was
/// returned, tagged as a declaration so nobody reads it as a measurement.
#[test]
fn doc_read_serves_the_sidecar_through_the_chokepoint() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let markdown = "<!-- rift: converted -->\n# Quarterly plan\n\n| a | b |\n| - | - |\n";
    let mut budget = TurnBudget::new(4, 4000);
    let resp = block(s.dispatch(
        ToolName::DocRead,
        &doc_request("docs/plan.docx", markdown, 9_000),
        &ctx(&store, &reader),
        &mut budget,
    ))
    .unwrap();

    assert_eq!(resp.tool, ToolName::DocRead);
    assert_eq!(resp.payload["state"], serde_json::json!("exact_included"));
    assert_eq!(resp.payload["content"], serde_json::json!(markdown));
    assert_eq!(resp.payload["path"], serde_json::json!("docs/plan.docx"));
    assert_eq!(
        resp.payload["sidecar"],
        serde_json::json!(".rift/state/doc/deadbeef.md"),
        "the answer names where this member sees the Markdown"
    );
    assert_eq!(
        resp.payload["reason"],
        serde_json::json!(TAG_DECLARED_BASELINE),
        "the figure is a declaration and the row says so"
    );

    // The seam's own figures, not the handler's.
    assert_eq!(
        resp.estimated_tokens_returned,
        rift_tokens::count(&resp.payload.to_string())
    );
    assert_eq!(
        resp.estimated_tokens_avoided,
        9_000_u32.saturating_sub(resp.estimated_tokens_returned)
    );
    assert_eq!(resp.event.verb(), "doc_served");
    assert_eq!(resp.event.tokens_avoided(), resp.estimated_tokens_avoided);
    assert_eq!(resp.event.tokens_returned(), resp.estimated_tokens_returned);
    let audited = resp.event.to_hook_json_keyed("call-1");
    assert!(
        audited.contains(&file_handle("docs/plan.docx", SHA_A)),
        "the row names the document, which is the thing that was not read: {audited}"
    );
}

/// **A denied path is refused, and the refusal carries none of the document.**
///
/// The sidecar is durable text derived from a file the repository declared
/// off-limits, so the verb that hands sidecars over is exactly where a deny
/// list has to be enforced again — a caller that could read `secrets/**`
/// through a Markdown re-encoding would have found the cheapest way around a
/// gate `retrieve_exact` already closes.
#[test]
fn a_denied_path_is_refused() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(
        PolicyEngine::new(&cfg).with_denied_paths(vec!["secrets/**".to_owned()]),
    );
    let mut budget = TurnBudget::new(4, 4000);
    let secret = "# Q4 numbers\n\nAKIA-DEADBEEF-PROD\n";
    let resp = block(s.dispatch(
        ToolName::DocRead,
        &doc_request("secrets/q4.xlsx", secret, 9_000),
        &ctx(&store, &reader),
        &mut budget,
    ))
    .unwrap();

    assert_eq!(resp.payload["state"], serde_json::json!("refused"));
    assert_eq!(
        resp.payload["reason"],
        serde_json::json!(omit_tag(OmitReason::PolicyDeniedExact))
    );
    assert_eq!(resp.payload["content"], serde_json::Value::Null);
    assert_eq!(resp.payload["sidecar"], serde_json::Value::Null);
    let rendered = resp.payload.to_string();
    assert!(
        !rendered.contains("AKIA-DEADBEEF-PROD") && !rendered.contains("Q4 numbers"),
        "a denied document's Markdown escaped through the refusal: {rendered}"
    );
    assert_eq!(resp.estimated_tokens_avoided, 0);
    assert_eq!(resp.event.verb(), "refused");

    // The same document under a policy nobody can read: an unreadable policy is
    // not a permissive one — it denies nothing BY NAME, so the deny list cannot
    // be what refuses this — and it refuses under the same tag.
    let unreadable = rift_recall_policy::ServePolicy {
        fail_closed: true,
        ..rift_recall_policy::ServePolicy::default()
    };
    assert!(unreadable.denied_paths.is_empty());
    let closed = ServeDispatcher::new(PolicyEngine::new(&cfg).with_serve_policy(unreadable));
    let mut budget = TurnBudget::new(4, 4000);
    let resp = block(closed.dispatch(
        ToolName::DocRead,
        &doc_request("docs/plan.docx", secret, 9_000),
        &ctx(&store, &reader),
        &mut budget,
    ))
    .unwrap();
    assert_eq!(resp.payload["state"], serde_json::json!("refused"));
    assert!(!resp.payload.to_string().contains("AKIA-DEADBEEF-PROD"));
}

/// A sensitive document is not handed over by a verb that asks for no reason:
/// the audited route that demands one is the way in, and the refusal names
/// which gate held it back so a caller can act on it.
#[test]
fn a_sensitive_document_is_refused_rather_than_served_without_a_reason() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    assert!(
        PolicyEngine::new(&cfg).exact_source_required(".env.docx"),
        "the fixture's premise: the shipped config treats this path as sensitive"
    );
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::new(4, 4000);
    let resp = block(s.dispatch(
        ToolName::DocRead,
        &doc_request(".env.docx", "# secrets\n\nTOKEN=hunter2\n", 9_000),
        &ctx(&store, &reader),
        &mut budget,
    ))
    .unwrap();
    assert_eq!(resp.payload["state"], serde_json::json!("refused"));
    assert_eq!(
        resp.payload["reason"],
        serde_json::json!(omit_tag(OmitReason::SensitivePathRequiresReason))
    );
    assert!(!resp.payload.to_string().contains("hunter2"));
}

/// A document the family holds no Markdown for is answered — with which of the
/// two silences it is. Owed means the worker will convert it and the next call
/// is answered; declined means it never will and the caller can stop asking.
/// Neither claims a saving, and both book a row.
#[test]
fn a_document_without_a_sidecar_is_answered_pending_and_avoids_nothing() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    for (state, tag) in [
        (SidecarState::Owed, TAG_SIDECAR_OWED),
        (SidecarState::Declined, TAG_SIDECAR_DECLINED),
    ] {
        let req = ToolRequest {
            target: Some("docs/plan.pdf".to_owned()),
            document: Some(Document {
                source_sha256: SHA_A.to_owned(),
                declared_baseline: 0,
                sidecar: state,
            }),
            ..Default::default()
        };
        let mut budget = TurnBudget::new(4, 4000);
        let resp =
            block(s.dispatch(ToolName::DocRead, &req, &ctx(&store, &reader), &mut budget)).unwrap();
        assert_eq!(resp.payload["state"], serde_json::json!("pending"));
        assert_eq!(resp.payload["reason"], serde_json::json!(tag));
        assert_eq!(resp.payload["content"], serde_json::Value::Null);
        assert_eq!(resp.estimated_tokens_avoided, 0);
        assert_eq!(resp.event.verb(), "doc_deferred");
        // The answer is still an answer the caller paid for, and the row says
        // what it cost: a silence booked as free would take the price of
        // asking out of every honesty figure this plane publishes.
        assert!(resp.estimated_tokens_returned > 0);
        assert_eq!(
            resp.event.tokens_returned(),
            resp.estimated_tokens_returned,
            "the row books what the caller received"
        );
    }
}

/// ★ Markdown that costs more than the document it replaces is not inlined:
/// the answer names the file instead, and claims nothing for having done so.
///
/// The ceiling is the document's own declared cost, capped by what is left of
/// the turn — and the first arm here is the one that matters, because a turn
/// with room to spare is exactly where a dearer sidecar used to be handed over
/// whole. The second arm is the same answer for the other reason: a document
/// that declares nothing is weighed against the turn alone.
#[test]
fn a_sidecar_dearer_than_the_document_is_named_rather_than_inlined() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let markdown = "# plan\n".repeat(400);
    let named = |resp: &ToolResponse| {
        assert_eq!(
            resp.payload["state"],
            serde_json::json!("available_by_handle")
        );
        assert_eq!(resp.payload["content"], serde_json::Value::Null);
        assert_eq!(
            resp.payload["sidecar"],
            serde_json::json!(".rift/state/doc/deadbeef.md"),
            "the caller is told where to read it itself"
        );
        assert_eq!(
            resp.estimated_tokens_avoided, 0,
            "nothing was handed over, so nothing was avoided"
        );
        assert!(!resp.payload.to_string().contains("# plan"));
    };

    // A turn with thousands of tokens left, and a document declared at ten:
    // the allowance is not what decides it.
    let mut fat = TurnBudget::new(4, 8_000);
    named(
        &block(s.dispatch(
            ToolName::DocRead,
            &doc_request("docs/plan.docx", &markdown, 10),
            &ctx(&store, &reader),
            &mut fat,
        ))
        .unwrap(),
    );

    let mut thin = TurnBudget::new(4, 20);
    named(
        &block(s.dispatch(
            ToolName::DocRead,
            &doc_request("docs/plan.docx", &markdown, 0),
            &ctx(&store, &reader),
            &mut thin,
        ))
        .unwrap(),
    );
}

/// The document verb reads neither the projection nor the object database, and
/// a request that reached it without a resolved sidecar is an internal error
/// rather than a lookup: this crate cannot open the family's store and must not
/// learn to.
#[test]
fn the_document_verb_answers_from_neither_the_projection_nor_the_substrate() {
    assert!(!ToolName::DocRead.may_read_the_substrate());
    assert!(!ToolName::DocRead.is_scope_bound());
    assert!(!ToolName::DocRead.is_exact(), "it asks for no reason");

    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::new(4, 4000);
    let (tool, decoded) = decode_verb(
        "doc-read",
        &json!({ "target": "docs/plan.docx", "document": { "markdown": "# forged" } }),
    )
    .expect("the verb decodes");
    assert_eq!(tool, ToolName::DocRead);
    assert_eq!(
        decoded.document, None,
        "no params object can hand this surface the bytes it is asking it to serve"
    );
    assert!(matches!(
        block(s.dispatch(tool, &decoded, &ctx(&store, &reader), &mut budget)),
        Err(ToolError::Internal(_))
    ));
}

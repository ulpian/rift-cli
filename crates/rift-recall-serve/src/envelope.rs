//! The view of a sealed envelope, and the vocabulary every verb answers in:
//! how a target's trust is derived from its provenance, how a turn is charged,
//! and how a serve — requested, injected or granted — is audited.
//!
//! [`envelope_view`] is the **one** reader of the four-state
//! [`ArtifactState`] in this crate and sits last in this file on purpose: the
//! sealed-constructor screen in the tests exempts nothing but what follows its
//! definition, so a second reader cannot grow up beside it unseen.

use rift_recall_events::MemdEvent;
use rift_recall_ledger::grants::CROSS_BRANCH_GRANT_TAG;
use rift_recall_policy::TurnBudget;
use rift_recall_types::{ArtifactState, Envelope, Handle, OmitReason, Trust};
use serde_json::{json, Value};

/// The content hash a `file:<path>@sha256:<hash>` handle/path encodes (for blob
/// fetch); falls back to the whole string when no `sha256:` marker is present.
#[must_use]
pub fn exact_sha(target: &str) -> &str {
    target
        .rsplit_once("@sha256:")
        .map_or(target, |(_, h)| h.split(':').next().unwrap_or(h))
}

/// Derive the trust class of an exact-retrieval target from **provenance**, not
/// the caller: a `file:`/`summary:` handle is a structural code fact;
/// a `log:` handle is untrusted tool output; anything that does not parse as a
/// known handle fails closed to untrusted (so unknown provenance is never raw).
#[must_use]
pub fn provenance_trust(target: &str) -> Trust {
    match target.parse::<Handle>() {
        Ok(Handle::File { .. } | Handle::Summary { .. }) => Trust::CodeFact,
        // a log handle is untrusted tool output; anything unparseable fails closed.
        Ok(Handle::Log { .. }) | Err(_) => Trust::ToolOutputUntrusted,
    }
}

/// The repo-relative path a target names, for policy matching (denied/sensitive
/// globs match the artifact path, not the raw handle string). Non-`file:` handles
/// have no repo path; their id stands in (policy denies untrusted regardless).
#[must_use]
pub fn handle_display_path(target: &str) -> String {
    match target.parse::<Handle>() {
        Ok(Handle::File { path, .. }) => path,
        Ok(Handle::Log { tool_use_id }) => tool_use_id,
        Ok(Handle::Summary { key }) => key,
        Err(_) => target.to_string(),
    }
}

/// The audit envelope for an **injected** serve — one that carries no tool
/// response because the agent never asked for it (the session pointer, or the
/// per-turn delta manifest). Injection is pure
/// spend: `tokens` were measured by the real tokenizer and nothing was avoided,
/// so the savings ledger records the cost and claims no credit. The envelope is
/// authored by the events crate, so an injected serve and a requested one are
/// audited through the same contract.
///
/// It is audited under its **own** verb, `memd.manifest_injected`. Sharing
/// `memd.manifest_served` with the pull side made the two indistinguishable in
/// `memd_audit_event`, and *by construction* rather than by bad luck: a row's
/// discriminating columns are the verb, the two token figures, a handle and a
/// reason; an injection sets none of the last three, so under a shared verb it
/// is a pull row that avoided nothing, and the relation holds no fact that says
/// otherwise. Netting injected spend against served credit therefore meant
/// inferring the side from *outside* the row — a missing per-call
/// sibling, `tokens_avoided = 0` — which is a guess about history, not a
/// measurement, however well it scores on any one database. One verb had been
/// spent on two meanings, and no query can un-spend it. The split makes the
/// netting a query.
///
/// The split is new rows only; the relation is append-only and nothing existing
/// was rewritten, so a `manifest_served` row predating the split may still be
/// either side and no query can tell. What is checked, rather than asserted, is
/// the going-forward half: this is the one envelope builder both injection
/// sites use (the session pointer and the per-turn manifest),
/// and each site has a test that reads back the row its **production** path
/// wrote and asserts the verb on it.
///
/// `occurrence` keys the row. An injected pointer is byte-identical in every
/// session, so a payload-only key would count the first injection and silently
/// discard every later one — understating spend, which inflates net savings.
/// Passing the session (stable within one injection, distinct across sessions)
/// keeps retries idempotent while letting each session's cost land.
#[must_use]
pub fn injection_audit_envelope(tokens: u32, occurrence: &str) -> String {
    MemdEvent::ManifestInjected {
        estimated_tokens_returned: tokens,
    }
    .to_hook_json_keyed(occurrence)
}

/// The audit envelope for a serve a **cross-branch grant** widened.
///
/// It is the ordinary per-invocation envelope with one member added to its
/// payload: `reason`, set to the fixed tag [`CROSS_BRANCH_GRANT_TAG`], which is
/// the column `memd_audit_event.reason` — a frozen v4 relation whose contract
/// says that column holds a **neutral tag** — can carry. That is the whole
/// point of doing it here rather than at the writer: the value is a constant of
/// this crate, so no path exists by which a caller's text could reach a
/// neutral-tag column. The person's own words go to `serve_grant_event`
/// instead, through `rift_recall_ledger::grants`, and are never re-served.
///
/// The idempotency key stays the one the event minted, over the untagged
/// payload. Deliberately: the key's job is to make one invocation one row —
/// the CLI channel supplies a fresh id per invocation for exactly that — and a
/// fixed tag adds no identity for it to carry.
#[must_use]
pub fn grant_tagged_envelope(event: &MemdEvent, occurrence: &str) -> String {
    let envelope = event.to_hook_json_keyed(occurrence);
    let Ok(mut doc) = serde_json::from_str::<Value>(&envelope) else {
        return envelope;
    };
    if let Some(payload) = doc.get_mut("payload").and_then(Value::as_object_mut) {
        payload.insert(
            "reason".to_owned(),
            Value::String(CROSS_BRANCH_GRANT_TAG.to_owned()),
        );
    }
    serde_json::to_string(&doc).unwrap_or(envelope)
}

/// A stable, neutral tag for an omit reason (never leaks content). Public
/// because it is the audit/CLI vocabulary for a refusal: every transport
/// reports the same tag for the same decision.
#[must_use]
pub fn omit_tag(reason: OmitReason) -> &'static str {
    match reason {
        OmitReason::BudgetExhausted => "budget_exhausted",
        OmitReason::PolicyDeniedExact => "policy_denied_exact",
        OmitReason::SensitivePathRequiresReason => "sensitive_path_requires_reason",
        OmitReason::SummaryInvalidSourceChanged => "summary_invalid_source_changed",
        OmitReason::SourceDeletedOrRedacted => "source_deleted_or_redacted",
        OmitReason::NotRequested => "not_requested",
    }
}

/// Charge `cost` against the turn budget, returning the amount actually charged
/// (clamped to the remaining budget so the ceiling is never breached). Called
/// from exactly one place — the respond seam (structurally tested).
pub(crate) fn charge(budget: &mut TurnBudget, cost: u32) -> u32 {
    let allowed = cost.min(budget.tokens_remaining());
    let _ = budget.try_tokens(allowed);
    allowed
}

/// The manifest-shaped event builder every list/awareness tool hands to the
/// respond seam (the figures are supplied by the seam).
pub(crate) fn manifest_event(returned: u32, avoided: u32) -> MemdEvent {
    MemdEvent::ManifestServed {
        estimated_tokens_returned: returned,
        estimated_tokens_avoided: avoided,
    }
}

/// Render an [`Envelope`] to a JSON view (state tag + hash, never raw bytes for
/// the manifest states).
pub(crate) fn envelope_view(e: &Envelope) -> Value {
    let (state, basis): (&str, Option<String>) = match &e.state {
        ArtifactState::KnownUnchanged { basis_sha256, .. } => {
            ("known_unchanged", Some(basis_sha256.clone()))
        }
        ArtifactState::SummaryIncluded { basis_sha256, .. } => {
            ("summary_included", Some(basis_sha256.clone()))
        }
        ArtifactState::ExactIncluded { basis_sha256, .. } => {
            ("exact_included", Some(basis_sha256.clone()))
        }
        ArtifactState::AvailableByHandle { basis_sha256, .. } => {
            ("available_by_handle", basis_sha256.clone())
        }
    };
    json!({ "path": e.path, "state": state, "basis_sha256": basis })
}

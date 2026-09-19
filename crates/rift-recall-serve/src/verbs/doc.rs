//! The document verb: the Markdown sidecar Rift holds for a document the
//! caller named, served through the trust chokepoint in the position of the
//! read it replaces.

use rift_recall_events::{file_handle, MemdEvent};
use rift_recall_policy::TurnBudget;
use rift_recall_types::{OmitReason, Trust};
use rift_recall_validity::trust::{IntoRawBody, TrustedPayload};
use rift_recall_validity::{Budget, SeenStatus};
use serde_json::{json, Value};

use crate::envelope::{envelope_view, omit_tag, provenance_trust};
use crate::{
    Document, ServeDispatcher, Sidecar, SidecarState, ToolError, ToolName, ToolRequest,
    ToolResponse, TAG_DECLARED_BASELINE, TAG_SIDECAR_DECLINED, TAG_SIDECAR_OWED,
};

impl ServeDispatcher<'_> {
    /// Serve the Markdown sidecar Rift holds for a document the caller named.
    ///
    /// The verb is the pull side of the same substitution the hook makes: an
    /// agent that opens `spec.docx` should be reading Markdown, and where no
    /// hook can rewrite the call — Codex, Antigravity, a person at a terminal —
    /// this is how it asks for the same thing. What it hands back is the
    /// **raw** Markdown, in the position of the read it replaces, which is why
    /// it goes through the trust chokepoint rather than around it.
    ///
    /// Four things happen here and nowhere else on this path:
    ///
    /// 1. **The path is gated before the bytes are touched.** A denied path, an
    ///    unreadable policy and a sensitive path are refused with a neutral
    ///    tag; a sensitive document's exact source is reached through the
    ///    audited `retrieve_exact`, which asks for a reason, or not at all.
    /// 2. **The bytes go through the chokepoint.** A `TrustedPayload` is the
    ///    only way raw text leaves this crate, and it cannot be built from an
    ///    untrusted class.
    /// 3. **The state is named by the sealed constructor**, not by this
    ///    handler: the sidecar rides inline only as `exact_included`, and when
    ///    it does not fit, the answer names the file so the caller can read it
    ///    itself. The ceiling for that decision is the document's own declared
    ///    cost — a sidecar is worth inlining exactly while it is cheaper than
    ///    the document it replaces — never below the turn's own allowance.
    /// 4. **The figures are sealed at the accounting seam**, with the declared
    ///    baseline as the gross figure and [`TAG_DECLARED_BASELINE`] on the row
    ///    saying it is a declaration. Nothing is claimed for an answer that
    ///    carried no Markdown.
    pub(crate) fn doc_read(
        &self,
        req: &ToolRequest,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let path = req.target.clone().unwrap_or_default();
        let Some(doc) = req.document.as_ref() else {
            // The wire decoder cannot reach the family's sidecar store, so a
            // request that arrived without one was not resolved by anybody.
            // Answering it would mean this crate going looking for a file.
            return Err(ToolError::Internal(
                "doc_read: no resolved sidecar on the request".to_owned(),
            ));
        };
        let handle = file_handle(&path, &doc.source_sha256);
        if let Some(reason) = self.doc_gate(&path) {
            return Ok(Self::refuse_doc(&handle, &path, doc, reason, budget));
        }
        let sidecar = match &doc.sidecar {
            SidecarState::Held(sidecar) => sidecar,
            SidecarState::Owed => {
                return Ok(Self::doc_without_a_sidecar(
                    &handle,
                    &path,
                    doc,
                    TAG_SIDECAR_OWED,
                    budget,
                ))
            }
            SidecarState::Declined => {
                return Ok(Self::doc_without_a_sidecar(
                    &handle,
                    &path,
                    doc,
                    TAG_SIDECAR_DECLINED,
                    budget,
                ))
            }
        };
        Ok(Self::serve_sidecar(&handle, &path, doc, sidecar, budget))
    }

    /// Which gate holds a document back, before its bytes are touched.
    ///
    /// An unreadable policy is not a permissive one, and a denied path is
    /// not served — not the document, and not the fact that Rift converted
    /// it. Both answer under one tag: which gate held it back is the
    /// caller's question to have answered, not an attacker's. A sensitive
    /// path names its own gate, because the caller can act on it: the
    /// audited verb that asks for a reason is the way in.
    fn doc_gate(&self, path: &str) -> Option<OmitReason> {
        if self.policy.is_fail_closed() || self.policy.is_denied_path(path) {
            return Some(OmitReason::PolicyDeniedExact);
        }
        if self.policy.exact_source_required(path) {
            return Some(OmitReason::SensitivePathRequiresReason);
        }
        None
    }

    /// The sidecar through the chokepoint, in the state the sealed
    /// constructor names, with the declared baseline claimed only for an
    /// answer that carried the Markdown.
    ///
    /// Provenance decides, never the caller: a document in the checkout is
    /// a code fact, and anything whose address does not parse as one fails
    /// closed to untrusted — which cannot become a `TrustedPayload` at all.
    /// A caller sent to read the sidecar itself has been saved something
    /// too, and this verb does not know how much, so it books nothing.
    fn serve_sidecar(
        handle: &str,
        path: &str,
        doc: &Document,
        sidecar: &Sidecar,
        budget: &mut TurnBudget,
    ) -> ToolResponse {
        let trust = provenance_trust(handle);
        let body = match TrustedPayload::new(trust, sidecar.markdown.clone().into_bytes()) {
            Ok(payload) => payload.into_raw_body(),
            Err(_) => {
                return Self::refuse_doc(handle, path, doc, OmitReason::PolicyDeniedExact, budget)
            }
        };
        let state = Self::doc_state(path, trust, handle, doc, &body, budget);
        let inlined = state == "exact_included";
        let (baseline, reason) = if inlined {
            (doc.declared_baseline, TAG_DECLARED_BASELINE.to_owned())
        } else {
            (0, omit_tag(OmitReason::BudgetExhausted).to_owned())
        };
        let content = if inlined { Some(body) } else { None };
        let payload = json!({
            "tool": "doc_read",
            "path": path,
            "state": state,
            "basis_sha256": doc.source_sha256,
            "sidecar": sidecar.path,
            "content": content,
            "reason": reason,
        });
        let audited = reason.clone();
        let handle = handle.to_owned();
        Self::respond(
            ToolName::DocRead,
            payload,
            baseline,
            budget,
            move |returned, avoided| MemdEvent::DocServed {
                handle,
                reason: audited,
                estimated_tokens_returned: returned,
                estimated_tokens_avoided: avoided,
            },
        )
    }

    /// The four-state word for one sidecar, from the sealed constructor.
    ///
    /// The ceiling handed to it is the **document's own declared cost**, and
    /// nothing else where there is one. Markdown that costs more than the
    /// container it stands in for has saved nobody anything, and the honest
    /// answer there is the path rather than the text — however much of the turn
    /// is left to spend on it. The allowance does not get a veto in the other
    /// direction either, because the alternative to serving the Markdown is not
    /// silence: it is the caller reading the document itself, at the declared
    /// figure. A document that declares nothing has no such cost to be weighed
    /// against, and there the turn's own allowance is the only ceiling there
    /// is. The answer claims nothing for itself either way, since what is
    /// booked is the declaration net of what it cost. No summary is bound to a
    /// sidecar — its validity is its key — so the constructor decides between
    /// the bytes and a handle on the budget alone.
    fn doc_state(
        path: &str,
        trust: Trust,
        handle: &str,
        doc: &Document,
        body: &str,
        budget: &TurnBudget,
    ) -> String {
        use rift_recall_validity::{envelope_for, MemoryDigestStore, SummaryStore};

        let validity = rift_recall_validity::resolve(
            &SummaryStore::new(),
            &MemoryDigestStore::default(),
            &rift_recall_validity::WorktreeSnapshot::new(),
            &format!("summary:{path}"),
        );
        let mut vbudget = Budget::new(if doc.declared_baseline == 0 {
            budget.tokens_remaining()
        } else {
            doc.declared_baseline
        });
        let envelope = envelope_for(
            path,
            trust,
            handle,
            &doc.source_sha256,
            SeenStatus::NotSeenOrChanged,
            &validity,
            Some(body),
            true,
            &mut vbudget,
        );
        envelope_view(&envelope)
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("available_by_handle")
            .to_owned()
    }

    /// The answer when Rift holds no Markdown for a document: what it is about
    /// to hold (`sidecar_owed`), or what it has decided it never will
    /// (`sidecar_declined`). Neither is a refusal and neither is silence, and
    /// both are audited like any other serve — a deferral that booked no row
    /// would be a serve nobody could count.
    fn doc_without_a_sidecar(
        handle: &str,
        path: &str,
        doc: &Document,
        tag: &'static str,
        budget: &mut TurnBudget,
    ) -> ToolResponse {
        let payload = json!({
            "tool": "doc_read",
            "path": path,
            "state": "pending",
            "basis_sha256": doc.source_sha256,
            "sidecar": Value::Null,
            "content": Value::Null,
            "reason": tag,
        });
        let handle = handle.to_owned();
        // What the caller received, not what it was worth: an answer that
        // hands over no Markdown still costs the tokens it is written in, and
        // a row booking those away would take the cost of asking out of every
        // honesty figure this plane publishes.
        Self::respond(ToolName::DocRead, payload, 0, budget, move |returned, _| {
            MemdEvent::DocDeferred {
                handle,
                reason: tag.to_owned(),
                estimated_tokens_returned: returned,
            }
        })
    }

    /// A refusal on the document verb, audited and measured like every other
    /// serve. It names the path the CALLER named and nothing about the
    /// document: a refusal that quoted the Markdown it withheld would have
    /// withheld nothing.
    fn refuse_doc(
        handle: &str,
        path: &str,
        doc: &Document,
        reason: OmitReason,
        budget: &mut TurnBudget,
    ) -> ToolResponse {
        let payload = json!({
            "tool": "doc_read",
            "path": path,
            "state": "refused",
            "basis_sha256": doc.source_sha256,
            "sidecar": Value::Null,
            "content": Value::Null,
            "reason": omit_tag(reason),
        });
        let target = handle.to_owned();
        let tag = omit_tag(reason).to_owned();
        Self::respond(ToolName::DocRead, payload, 0, budget, move |returned, _| {
            MemdEvent::Refused {
                target,
                reason: tag,
                estimated_tokens_returned: returned,
            }
        })
    }
}

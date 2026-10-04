//! Audited exact retrieval: by handle, or by the ordinal a pack published —
//! the one place raw bytes leave this crate, and the gate in front of it.

use rift_recall_events::MemdEvent;
use rift_recall_ledger::candidates::load_candidates;
use rift_recall_policy::{AuditDecision, TurnBudget};
use rift_recall_substrate::SubstrateReader;
use rift_recall_types::{Handle, OmitReason, Trust};
use rift_recall_validity::trust::{
    serialize_untrusted, IntoNeutralDigest, IntoRawBody, TrustedPayload, UntrustedPayload,
};
use rift_store::CanonicalStore;
use serde_json::{json, Value};

use super::credit::Current;
use super::pack::{citation_pass, pack_inputs};
use crate::envelope::{exact_sha, handle_display_path, omit_tag, provenance_trust};
use crate::{RecallContext, ServeDispatcher, ToolError, ToolName, ToolRequest, ToolResponse};

/// A granted exact retrieval, ready to serve. A struct rather than seven
/// arguments, and every field is a decision the two routes make differently.
struct ExactGrant {
    /// What the audit row records this retrieval as. For a citation this is
    /// the pack key, the ordinal AND the resolved address (the four
    /// facts, in the one neutral column the sink has for them).
    audit_name: String,
    /// The identity the agent-facing **exact** payload states: the path for a
    /// handle the caller already held, the ordinal for a citation — whose whole
    /// point is that the agent never learns the path.
    exact_id: (&'static str, String),
    /// The identity the over-budget downgrade payload states: the name the
    /// agent would retry with.
    handle_id: (&'static str, String),
    /// The path the policy gate saw (the downgrade re-checks it).
    policy_path: String,
    /// The content address whose bytes are served.
    sha: String,
    /// Provenance-derived trust, never caller-declared.
    trust: Trust,
    /// The caller's mandatory reason, as the audit records it.
    reason: String,
    /// The file the bytes are a revision of, when they are a file's: the
    /// answer says whether they are still what the checkout holds.
    file: Option<String>,
}

impl ServeDispatcher<'_> {
    pub(crate) async fn retrieve_exact<S: CanonicalStore, R: SubstrateReader>(
        &self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let target = req.target.clone().unwrap_or_default();
        // `pack:<key>/evidence:<n>` is a *citation*, not a handle —
        // the agent no longer holds a path to send, so the server resolves the
        // ordinal itself and gates the path it resolves to.
        if let Some(citation) = rift_recall_pack::CitationTarget::parse(&target) {
            return self.retrieve_by_citation(&citation, req, ctx, budget).await;
        }
        // Trust is derived from provenance, never from the caller: a
        // file/summary handle is a code fact; a log handle is untrusted tool
        // output; anything unparseable fails closed to untrusted.
        let trust = provenance_trust(&target);
        // Policy matches the artifact's repo path, not the raw handle string.
        let policy_path = handle_display_path(&target);
        // **BOTH HALVES OF THE ADDRESS, OR NOTHING.** `Handle::File`'s own
        // contract says a resolver must match path AND content address, and
        // this handler matched NEITHER against the record: it gated policy on
        // the caller's path and fetched the caller's hash. Measured, not
        // reasoned — `red_team_a_fabricated_path_with_a_captured_hash_must_not
        // _resolve` served `AKIA-DEADBEEF-PROD` out of a `secrets/**` file
        // under the path `src/a.rs` on the tree this commit edits. That is the
        // hash-only resolution the citation protocol refuses, live in the one
        // handler that hands over raw bytes.
        if !Self::address_is_recorded(ctx, &target).await? {
            // The tag is the ordinary denial tag, deliberately: a distinct
            // "not in the record" tag would answer the attacker's real
            // question — which half of the address was wrong — and turn the
            // refusal into an oracle for probing paths against hashes.
            return Ok(Self::refuse_exact(
                &target,
                trust,
                OmitReason::PolicyDeniedExact,
                budget,
            ));
        }
        match self
            .policy
            .decide_exact(&policy_path, trust, req.reason.as_deref())
        {
            // Trusted classes only reach here (policy denies untrusted).
            AuditDecision::Allow { reason, .. } => self.exact_response(
                ctx,
                budget,
                ExactGrant {
                    audit_name: target.clone(),
                    exact_id: ("path", policy_path.clone()),
                    handle_id: ("handle", target),
                    policy_path,
                    sha: exact_sha_owned(req.target.as_deref().unwrap_or_default()),
                    trust,
                    reason,
                    file: file_of(req.target.as_deref().unwrap_or_default()),
                },
            ),
            AuditDecision::Deny(reason) => Ok(Self::refuse_exact(&target, trust, reason, budget)),
        }
    }

    /// Resolve `pack:<key>/evidence:<n>` and serve what it names.
    ///
    /// # The pack key is VERIFIED, never trusted
    ///
    /// There is no stored table to look a key up in, and deliberately so: a
    /// server-side map keyed by a caller-supplied string is a lookup an
    /// attacker can drive by guessing. Instead the table is **rebuilt** from
    /// Rift's own facts — the ranked set as the store holds it now, the policy
    /// version in force, the budget the caller asked for — the pack key is
    /// recomputed from those same inputs, and the ordinal resolves only if the
    /// recomputed key equals the presented one. A key nobody issued and a key
    /// whose world has moved on are therefore the SAME answer, which is what
    /// the protocol asks for (unknown or expired is a REFUSAL, never a path
    /// lookup an attacker can enumerate) and is also the honest one: an
    /// ordinal means something only relative to the pack that published it.
    ///
    /// [`citation_pass`] is the single builder, shared with
    /// [`Self::context_pack`], so the table a pack carries and the table this
    /// resolves against cannot become two tables.
    async fn retrieve_by_citation<S: CanonicalStore, R: SubstrateReader>(
        &self,
        citation: &rift_recall_pack::CitationTarget,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let (ranked, _in_scope) = Self::ranked_candidates(req, ctx).await?;
        let recomputed =
            rift_recall_pack::pack_key(&pack_inputs(req, &ranked, self.policy.version()));
        let table = citation_pass(&self.policy, &ranked);
        // Ordering matters: the key is checked first, so an ordinal is never
        // looked up against a table the caller has not proven they hold.
        let resolved = (recomputed == citation.pack_key)
            .then(|| table.get(citation.ordinal))
            .flatten();
        let Some(cited) = resolved else {
            return Ok(Self::refuse_citation(
                citation,
                None,
                OmitReason::PolicyDeniedExact,
                budget,
            ));
        };
        // The caller may restate the address it read off the pack line. The
        // TABLE is the authority; a restatement that disagrees is refused
        // rather than reconciled, because reconciling it would let the caller
        // choose which bytes an ordinal names — the substitution half of the
        // hash-only attack.
        if citation
            .content_sha256
            .as_deref()
            .is_some_and(|h| h != cited.content_sha256())
        {
            return Ok(Self::refuse_citation(
                citation,
                Some(cited),
                OmitReason::PolicyDeniedExact,
                budget,
            ));
        }
        // Both halves came out of one `n_file_revision` row, so the address is
        // bound by construction here — there is no caller-supplied pair to
        // check. Trust still derives from provenance, through the same single
        // authority the handle route uses.
        let handle = format!("file:{}@sha256:{}", cited.path(), cited.content_sha256());
        let trust = provenance_trust(&handle);
        // **THE EXISTING PATH-BASED POLICY GATE, ON THE RESOLVED PATH, BEFORE
        // ANY BYTES ARE READ.** This is the whole reason an ordinal resolves to
        // an address instead of the caller naming one: the design took the path
        // away from the agent without taking it away from the gate.
        match self
            .policy
            .decide_exact(cited.path(), trust, req.reason.as_deref())
        {
            AuditDecision::Deny(reason) => {
                Ok(Self::refuse_citation(citation, Some(cited), reason, budget))
            }
            AuditDecision::Allow { reason, .. } => {
                self.exact_response(ctx, budget, citation_grant(citation, cited, trust, reason))
            }
        }
    }

    /// Refuse one citation, audited with the pack key, the ordinal, the
    /// resolved path when the table answered, and the verdict.
    ///
    /// What the AGENT sees carries none of it — only the neutral tag — because
    /// a refusal that names the path it withheld has withheld nothing. Every
    /// arm refuses under the same tag on purpose: distinguishing "no such key",
    /// "no such ordinal" and "that ordinal was dropped" would answer the
    /// attacker's real question.
    fn refuse_citation(
        citation: &rift_recall_pack::CitationTarget,
        resolved: Option<&rift_recall_pack::Citation>,
        reason: OmitReason,
        budget: &mut TurnBudget,
    ) -> ToolResponse {
        Self::refuse_exact(
            &citation_audit_name(citation, resolved),
            Trust::CodeFact,
            reason,
            budget,
        )
    }

    /// Serve granted exact bytes — **the one place raw content leaves this
    /// server**, shared by the handle route and the citation route so a second
    /// inline of exact bytes cannot grow up beside the first.
    fn exact_response<S: CanonicalStore, R: SubstrateReader>(
        &self,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
        grant: ExactGrant,
    ) -> Result<ToolResponse, ToolError> {
        let bytes = ctx
            .reader
            .blob(&grant.sha)
            .map_err(|e| ToolError::Internal(e.to_string()))?
            .unwrap_or_default();
        // Chokepoint: the ONLY inline of exact bytes routes through
        // a `TrustedPayload`, so untrusted provenance can never be inlined raw
        // — a type guarantee, not a per-handler convention.
        let body = match TrustedPayload::new(grant.trust, bytes) {
            Ok(t) => t.into_raw_body(),
            // provenance disagrees with the grant → fail closed.
            Err(_) => {
                return Ok(Self::refuse_exact(
                    &grant.audit_name,
                    grant.trust,
                    OmitReason::PolicyDeniedExact,
                    budget,
                ))
            }
        };
        let cost = rift_tokens::count(&body);
        // Downgrade: a sensitive exact that will not fit the remaining
        // budget is served as a handle, never a partial secret. The trial is on
        // the INNER body count (whole-or-nothing) and never charges; the seam
        // charges the serialized response. Still audited.
        if self
            .policy
            .downgrade_for(&grant.policy_path, budget.tokens_remaining() >= cost)
            .is_some()
        {
            let payload = json!({
                "tool": "retrieve_exact",
                "result": { "state": "available_by_handle", grant.handle_id.0: grant.handle_id.1, "reason": "sensitive_over_budget" },
            });
            let target = grant.audit_name;
            return Ok(Self::respond(
                ToolName::RetrieveExact,
                payload,
                0,
                budget,
                move |returned, _| MemdEvent::Refused {
                    target,
                    reason: "sensitive_over_budget".to_string(),
                    estimated_tokens_returned: returned,
                },
            ));
        }
        let mut result =
            json!({ "state": "exact", grant.exact_id.0: grant.exact_id.1, "body": body });
        if is_historical(ctx.reader, &grant) {
            if let Some(obj) = result.as_object_mut() {
                obj.insert("historical".to_owned(), json!(true));
            }
        }
        let payload = json!({ "tool": "retrieve_exact", "result": result });
        let handle = grant.audit_name;
        let reason = grant.reason;
        Ok(Self::respond(
            ToolName::RetrieveExact,
            payload,
            0,
            budget,
            move |returned, _| MemdEvent::ExactRetrieved {
                handle,
                reason,
                estimated_tokens_returned: returned,
            },
        ))
    }

    /// Whether the append-only record holds the address `target` names — for a
    /// file revision, a row whose **path and `content_sha256` both** match
    /// (`Handle::File`'s written contract, and what `bins/rift`'s
    /// `investigate::resolve_handle` has always done). Non-file handles carry
    /// no repo address to bind, so they pass here and are decided by policy on
    /// provenance alone (untrusted is denied outright).
    ///
    /// Bound against the **scope-loaded** candidate set rather than a bare
    /// `n_file_revision` read, so the containment that governs every other
    /// handler governs this one too: an out-of-scope revision is not a fact
    /// this server may confirm, let alone serve.
    async fn address_is_recorded<S: CanonicalStore, R: SubstrateReader>(
        ctx: &RecallContext<'_, S, R>,
        target: &str,
    ) -> Result<bool, ToolError> {
        let Ok(Handle::File { path, sha256, .. }) = target.parse::<Handle>() else {
            return Ok(true);
        };
        let loaded = load_candidates(ctx.store, &ctx.scope, ctx.now, ctx.ancestry)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        Ok(loaded
            .iter()
            .any(|l| l.candidate.path == path && l.content_sha256 == sha256))
    }

    /// Build the audited refusal for an exact request. Untrusted content is
    /// surfaced only as a neutral digest + handle; trusted-but-denied content
    /// (missing reason, denied/sensitive path) is refused with a neutral reason
    /// tag. Either way a single terminal `Refused` event is emitted (the serve↔event
    /// bijection), and the refusal payload is measured and charged like any
    /// serve — no response class is exempt at the seam.
    fn refuse_exact(
        target: &str,
        trust: Trust,
        reason: OmitReason,
        budget: &mut TurnBudget,
    ) -> ToolResponse {
        let payload = if trust.is_untrusted() {
            let digest = UntrustedPayload::from_trust(trust, Vec::new()).map_or_else(
                || serialize_untrusted("tool_output", b"", target),
                |p| p.into_neutral_digest(&format!("log:{target}")),
            );
            json!({ "tool": "retrieve_exact", "result": { "state": "refused", "neutral_digest": serde_json::to_value(&digest).unwrap_or(Value::Null) } })
        } else {
            json!({ "tool": "retrieve_exact", "result": { "state": "refused", "reason": omit_tag(reason) } })
        };
        let target = target.to_string();
        let tag = omit_tag(reason).to_string();
        Self::respond(
            ToolName::RetrieveExact,
            payload,
            0,
            budget,
            move |returned, _| MemdEvent::Refused {
                target,
                reason: tag,
                estimated_tokens_returned: returned,
            },
        )
    }
}

/// Whether served bytes are a revision the checkout no longer holds — or one
/// it cannot be shown to hold. A hash-bound retrieval serves the revision it
/// names, which may be history; the answer says so rather than letting the
/// bytes pass for the file as it is now. A reader bound to no checkout can
/// say nothing either way, and the answer carries no label.
fn is_historical<R: SubstrateReader>(reader: &R, grant: &ExactGrant) -> bool {
    let Some(path) = grant.file.as_deref() else {
        return false;
    };
    if reader.worktree_root().is_none() {
        return false;
    }
    Current::of(reader, path) != Current::Verified(grant.sha.clone())
}

/// The path a `file:` handle names, when the target is one.
fn file_of(target: &str) -> Option<String> {
    match target.parse::<Handle>() {
        Ok(Handle::File { path, .. }) => Some(path),
        _ => None,
    }
}

/// [`exact_sha`] as an owned string (the grant outlives the borrow).
fn exact_sha_owned(target: &str) -> String {
    exact_sha(target).to_string()
}

/// The grant a resolved citation serves under.
///
/// Audited with all four facts the citation protocol requires: pack key,
/// ordinal, resolved path, and the verdict (the arm that builds it). What the
/// AGENT is told is the ordinal, never the path — a payload that echoed the
/// resolved name would hand back exactly what the ordinal exists to withhold.
fn citation_grant(
    citation: &rift_recall_pack::CitationTarget,
    cited: &rift_recall_pack::Citation,
    trust: Trust,
    reason: String,
) -> ExactGrant {
    let evidence = format!("{}{}", rift_recall_pack::EVIDENCE_TAG, citation.ordinal);
    ExactGrant {
        audit_name: citation_audit_name(citation, Some(cited)),
        exact_id: ("evidence", evidence.clone()),
        handle_id: ("evidence", evidence),
        policy_path: cited.path().to_string(),
        sha: cited.content_sha256().to_string(),
        trust,
        reason,
        file: Some(cited.path().to_string()),
    }
}

/// The audit identity of one citation resolution: `pack:<key>/evidence:<n>`,
/// and — once the table has answered — the address it resolved to.
///
/// It is a composed string rather than new columns because the serve audit sink
/// is an append-only relation whose shape is owned elsewhere; widening it to
/// two more columns is a schema change owned elsewhere, not one this module makes.
fn citation_audit_name(
    citation: &rift_recall_pack::CitationTarget,
    resolved: Option<&rift_recall_pack::Citation>,
) -> String {
    let base = format!(
        "{}{}/{}{}",
        rift_recall_pack::PACK_TAG,
        citation.pack_key,
        rift_recall_pack::EVIDENCE_TAG,
        citation.ordinal
    );
    match resolved {
        Some(c) => format!("{base}=file:{}@sha256:{}", c.path(), c.content_sha256()),
        None => base,
    }
}

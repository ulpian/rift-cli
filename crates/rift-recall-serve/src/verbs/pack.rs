//! The context pack — an ordered, budgeted, position-aware evidence pack —
//! the citation table it publishes, and the cost-awareness verb beside it.
//!
//! Every section entry is built from a sealed-constructor output:
//! `envelope_for` (with `resolve` supplying the validity verdict) for trusted
//! artifacts, the trust chokepoint for untrusted provenance, parse-validated
//! handles for recommendations, and the closed critical/cost vocabularies. No
//! inline strings enter a pack.

use std::collections::BTreeSet;

use rift_recall_events::MemdEvent;
use rift_recall_ledger::candidates::{load_candidates, LoadedCandidate};
use rift_recall_ledger::hunks::{serve_hunks, Hunk};
use rift_recall_pack::{
    compile, pack_key, Citations, CostNote, CriticalFact, EvidenceRef, PackError, PackRequest,
    SectionEntry, Sensitivity,
};
use rift_recall_policy::{PolicyEngine, TurnBudget};
use rift_recall_substrate::SubstrateReader;
use rift_recall_types::{Handle, Trust};
use rift_recall_validity::trust::{serialize_untrusted, IntoRawBody, TrustedPayload};
use rift_recall_validity::{
    envelope_for, resolve, Budget, MemoryDigestStore, SeenStatus, SummaryStore, WorktreeSnapshot,
};
use rift_store::{CanonicalStore, Row};
use serde_json::{json, Value};

use super::credit::{candidate_costs, Recorded};
use super::{internal, HUNK_RANGES_SQL};
use crate::envelope::{manifest_event, provenance_trust};
use crate::{
    RecallContext, ServeDispatcher, ToolError, ToolName, ToolRequest, ToolResponse, COST_NOTE,
};

/// How many ranked candidates a context pack considers.
///
/// It is part of the **citation protocol**, not a local tuning knob: the
/// ordinal a pack publishes is this pass's index, so an ordinal past this bound
/// names nothing and `retrieve_exact` refuses it.
pub const PACK_CANDIDATES: usize = 8;

/// A pack under assembly: the request the compiler will take, the citation
/// table that is also its admission list, and the sealed constructor's
/// inputs.
///
/// The summary store is empty until ingest persists digest bindings (a
/// deliberate deferral): `resolve` then verdicts `Degraded`, so the envelope
/// honestly serves known-unchanged / exact-hunks / handles and never
/// fabricates a summary.
struct PackAssembly<'a> {
    request: PackRequest,
    /// **THE EVIDENCE ORDINAL PASS + THE CITATION TABLE** — one function
    /// ([`citation_pass`]), shared with `retrieve_by_citation`, so the numbers
    /// this pack publishes and the numbers that server resolves are the same
    /// numbers by construction rather than by two matching loops.
    table: Citations,
    known: BTreeSet<&'a str>,
    summaries: SummaryStore,
    digests: MemoryDigestStore,
    snapshot: WorktreeSnapshot,
    env_budget: Budget,
}

impl ServeDispatcher<'_> {
    /// Assemble the pack: admit each cited candidate in rank order, compile
    /// under the composed budget, and seal the figures.
    ///
    /// Pack identity is input-keyed: computed from the immutable inputs BEFORE
    /// any rendering, never from the output bytes. The agent sends it back
    /// beside an ordinal, and `retrieve_by_citation` recomputes it from these
    /// same inputs rather than looking it up.
    pub(crate) async fn context_pack<S: CanonicalStore, R: SubstrateReader>(
        &self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let (ranked, _in_scope) = Self::ranked_candidates(req, ctx).await?;
        let key = pack_key(&pack_inputs(req, &ranked, self.policy.version()));
        let mut assembly = PackAssembly::new(req, citation_pass(&self.policy, &ranked))?;
        for (rank, lc) in ranked.iter().take(PACK_CANDIDATES).enumerate() {
            let ordinal = u32::try_from(rank).unwrap_or(u32::MAX).saturating_add(1);
            // The table IS the admission list: an artifact the pass refused to
            // cite (policy-denied, or a path no record should hold) is one this
            // pack cannot name, so it is not rendered either. One screen, one
            // place — the two used to be written out twice and could drift.
            if assembly.table.get(ordinal).is_none() {
                continue;
            }
            self.admit(ctx, &mut assembly, ordinal, lc).await?;
        }
        // The pack budget composes with the remaining TURN budget: the
        // effective cap is min(pack target, hard ceiling, remaining).
        // `compile` is a pure trial — the single charge happens at the seam.
        let pack = compile(&assembly.request, budget.tokens_remaining()).map_err(
            |PackError::BudgetTooSmall { needed, cap }| ToolError::PackBudgetTooSmall {
                needed,
                cap,
            },
        )?;
        // Only the ranked files the caller held, unchanged, and recorded as
        // read are re-reads this pack spared; the rest are a list.
        let recorded = Recorded::of(ctx, &assembly.known).await;
        let baseline = recorded.baseline(&assembly.known, candidate_costs(&ranked));
        let sections = u32::try_from(pack.sections.len()).unwrap_or(u32::MAX);
        let total_tokens = pack.total_tokens;
        let payload = json!({
            "tool": "context_pack",
            "sections": pack.sections.iter().map(|s| json!({ "kind": format!("{:?}", s.kind), "lines": s.lines })).collect::<Vec<_>>(),
            "total_tokens": total_tokens,
            "pack_key": key,
        });
        Ok(Self::respond(
            ToolName::ContextPack,
            payload,
            baseline,
            budget,
            move |returned, avoided| MemdEvent::PackBuilt {
                sections,
                total_tokens,
                estimated_tokens_returned: returned,
                estimated_tokens_avoided: avoided,
            },
        ))
    }

    /// Admit one cited candidate: its recommendation, then its section entry
    /// — through the chokepoint for untrusted provenance, through the sealed
    /// envelope otherwise.
    ///
    /// ADDRESSED, not bare: this pack has no human surface and no
    /// ordinal->path map, so the content address is the only thing that makes
    /// `evidence:3` resolvable by anyone at all. The hash is right here in the
    /// ranked row; leaving it out made a spilled entry identityless the moment
    /// `Recommended` degraded away, which `degrade_order` does FIRST.
    ///
    /// The screens the table applied, and why they are not repeated here:
    /// [`citation_pass`] drops a policy-denied path (never served, not even as
    /// a handle) and an anomalous one (absolute, `..`, or control chars —
    /// never a legitimate repo artifact, and a newline-bearing path could
    /// forge a directive line at any placement). Both leave a gap in the
    /// numbering, and the caller's `continue` is that gap reaching the
    /// renderer.
    ///
    /// The recommendation names the artifact by its ordinal. The handle still
    /// has to PARSE — a recommendation must stand for a real address or the
    /// number names nothing — but its text stops here; only the ordinal and
    /// the hash half of the address render.
    async fn admit<S: CanonicalStore, R: SubstrateReader>(
        &self,
        ctx: &RecallContext<'_, S, R>,
        assembly: &mut PackAssembly<'_>,
        ordinal: u32,
        lc: &LoadedCandidate,
    ) -> Result<(), ToolError> {
        let evidence = EvidenceRef::addressed(ordinal, &lc.content_sha256);
        let handle_str = format!("file:{}@sha256:{}", lc.candidate.path, lc.content_sha256);
        // Trust derives from provenance, never the caller.
        let trust = provenance_trust(&handle_str);
        if let Ok(h) = handle_str.parse::<Handle>() {
            let _ = assembly
                .request
                .push_recommended(SectionEntry::from_handle(&h, ordinal));
        }
        if trust.is_untrusted() {
            // Chokepoint: untrusted provenance enters as a neutral digest
            // only — counts + citation, never payload text. The digest
            // is handed `handle_str` for its own accounting; what the pack
            // renders is `evidence`, so the second of the two
            // retrieval names is an ordinal here too.
            let digest = serialize_untrusted("tool_output", b"", &handle_str);
            return assembly
                .request
                .push_changed(SectionEntry::from_neutral_cited(&digest, &evidence))
                .map_err(internal);
        }
        self.admit_trusted(ctx, assembly, lc, &handle_str, trust, &evidence)
            .await
    }

    /// A trusted artifact's entry: its four-state envelope — carrying the
    /// changed hunks' text where the caller does not hold the revision —
    /// pushed to the section its state belongs in, and the reverted-provenance
    /// fact beside it when the projection records one.
    ///
    /// The four-state envelope is the SOLE per-artifact constructor. Exact
    /// inlining is allowed only for non-sensitive paths — a sensitive path is
    /// exact-gated (the audited retrieve_exact) or a handle here, never
    /// pack-inlined or a lossy summary.
    async fn admit_trusted<S: CanonicalStore, R: SubstrateReader>(
        &self,
        ctx: &RecallContext<'_, S, R>,
        assembly: &mut PackAssembly<'_>,
        lc: &LoadedCandidate,
        handle_str: &str,
        trust: Trust,
        evidence: &EvidenceRef,
    ) -> Result<(), ToolError> {
        let c = &lc.candidate;
        let seen = if assembly.known.contains(lc.content_sha256.as_str()) {
            SeenStatus::SeenSameHash
        } else {
            SeenStatus::NotSeenOrChanged
        };
        let validity = resolve(
            &assembly.summaries,
            &assembly.digests,
            &assembly.snapshot,
            &format!("summary:{}", c.path),
        );
        let hunk_rows = ctx
            .store
            .query(HUNK_RANGES_SQL, &[rift_store::Value::text(c.id.as_str())])
            .await
            .map_err(internal)?;
        let is_changed = !hunk_rows.is_empty() && seen == SeenStatus::NotSeenOrChanged;
        let exact_text = if is_changed {
            changed_hunk_text(ctx, lc, trust, &hunk_rows, assembly.env_budget.remaining())?
        } else {
            None
        };
        let sensitivity = if self.policy.exact_source_required(&c.path) {
            Sensitivity::Sensitive
        } else {
            Sensitivity::Normal
        };
        let env = envelope_for(
            &c.path,
            trust,
            handle_str,
            &lc.content_sha256,
            seen,
            &validity,
            exact_text.as_deref(),
            matches!(sensitivity, Sensitivity::Normal),
            &mut assembly.env_budget,
        );
        let entry = SectionEntry::from_envelope(&env, sensitivity, evidence);
        if is_changed {
            assembly.request.push_changed(entry)
        } else {
            assembly.request.push_unchanged(entry)
        }
        .map_err(internal)?;
        // Reverted provenance is decisive context — edge-placed,
        // from the closed critical-fact vocabulary.
        if reverted_provenance(ctx, &c.id).await? {
            assembly
                .request
                .push_critical(SectionEntry::critical(&CriticalFact::RevertedProvenance {
                    evidence: evidence.clone(),
                }))
                .map_err(internal)?;
        }
        Ok(())
    }

    pub(crate) async fn cost_hotspots<S: CanonicalStore, R: SubstrateReader>(
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let loaded = load_candidates(ctx.store, &ctx.scope, ctx.now, ctx.ancestry)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let total: u64 = loaded
            .iter()
            .map(|l| u64::from(l.candidate.est_tokens))
            .sum();
        let mut by_tokens: Vec<&LoadedCandidate> = loaded.iter().collect();
        by_tokens.sort_by(|a, b| {
            b.candidate
                .est_tokens
                .cmp(&a.candidate.est_tokens)
                .then(a.candidate.path.cmp(&b.candidate.path))
        });
        let top_n = req.top_n.unwrap_or(5) as usize;
        let hotspots: Vec<Value> = by_tokens
            .into_iter()
            .take(top_n)
            .map(|l| {
                let t = u64::from(l.candidate.est_tokens);
                let share = if total == 0 {
                    0.0
                } else {
                    t as f64 / total as f64
                };
                json!({ "area": l.candidate.path, "est_tokens": t, "share": share })
            })
            .collect();
        // Descriptive only — the payload carries no directive.
        let payload = json!({ "tool": "cost_hotspots", "hotspots": hotspots, "note": COST_NOTE });
        Ok(Self::respond(
            ToolName::CostHotspots,
            payload,
            0,
            budget,
            manifest_event,
        ))
    }
}

impl<'a> PackAssembly<'a> {
    fn new(req: &'a ToolRequest, table: Citations) -> Result<Self, ToolError> {
        let pack_tokens = pack_tokens(req);
        let mut request = PackRequest::new(pack_tokens);
        request
            .set_cost_note(SectionEntry::cost_note(CostNote::RepeatedReadAreas))
            .map_err(internal)?;
        request.set_citations(table.clone());
        Ok(Self {
            request,
            table,
            known: req.known_hashes.iter().map(String::as_str).collect(),
            summaries: SummaryStore::new(),
            digests: MemoryDigestStore::default(),
            snapshot: WorktreeSnapshot::new(),
            env_budget: Budget::new(pack_tokens),
        })
    }
}

/// **THE EVIDENCE ORDINAL PASS and the citation table**, in one function.
///
/// Called by `context_pack` to build the table its pack carries and by
/// `retrieve_by_citation` to rebuild the same table, so the two cannot be two
/// tables. The ordinal is the index in the ranked fact set's own order, never a
/// position in the finished pack, which `compile` reorders and shortens.
///
/// The skips leave a **GAP**: a policy-denied path is never served — not even
/// as a handle, and now not as an ordinal either — and an anomalous path
/// (absolute, `..`, control chars) is never a legitimate repo artifact. Neither
/// renumbers what follows, so a dropped artifact cannot silently rename the
/// next one, and an ordinal in a gap resolves to a refusal.
pub(crate) fn citation_pass(policy: &PolicyEngine<'_>, ranked: &[LoadedCandidate]) -> Citations {
    let mut table = Citations::new();
    for (rank, lc) in ranked.iter().take(PACK_CANDIDATES).enumerate() {
        let ordinal = u32::try_from(rank).unwrap_or(u32::MAX).saturating_add(1);
        if policy.is_denied_path(&lc.candidate.path) {
            continue;
        }
        // `cite` itself refuses an unsafe path and half an address, so the
        // anomalous-path screen lives in one place rather than two.
        let _ = table.cite(ordinal, &lc.candidate.path, &lc.content_sha256);
    }
    table
}

/// The immutable inputs a pack's identity is computed from.
pub(super) fn pack_inputs(
    req: &ToolRequest,
    ranked: &[LoadedCandidate],
    policy_version: u32,
) -> rift_recall_pack::PackKeyInputs {
    rift_recall_pack::PackKeyInputs {
        query: pack_query(req),
        ranked: ranked
            .iter()
            .map(|l| (l.candidate.id.clone(), l.content_sha256.clone()))
            .collect(),
        policy_version,
        token_budget: pack_tokens(req),
    }
}

/// The query string a pack's identity is keyed on, spelled once so
/// the compile side and the retrieval side cannot drift into two spellings of
/// "the same pack".
fn pack_query(req: &ToolRequest) -> String {
    format!(
        "task:{}|focus:{}|errors:{}",
        req.task.as_deref().unwrap_or(""),
        req.files_in_focus.join(","),
        req.error_signatures.join(","),
    )
}

/// The pack budget a request asks for (pre-clamp — the contract input the pack
/// key is computed over).
fn pack_tokens(req: &ToolRequest) -> u32 {
    if req.token_budget == 0 {
        rift_recall_pack::DEFAULT_PACK_TOKENS
    } else {
        req.token_budget
    }
}

/// Changed-hunk TEXT: the persisted line ranges slice the revision's bytes,
/// and the ONLY raw inline goes through the chokepoint's `TrustedPayload` —
/// untrusted bytes cannot ride. `None` when there are no bytes, when the
/// trust class refuses them, or when no hunk fits the pack's allowance.
fn changed_hunk_text<S: CanonicalStore, R: SubstrateReader>(
    ctx: &RecallContext<'_, S, R>,
    lc: &LoadedCandidate,
    trust: Trust,
    hunk_rows: &[Row],
    remaining: u32,
) -> Result<Option<String>, ToolError> {
    let bytes = ctx
        .reader
        .blob(&lc.content_sha256)
        .map_err(internal)?
        .unwrap_or_default();
    if bytes.is_empty() {
        return Ok(None);
    }
    let Ok(payload) = TrustedPayload::new(trust, bytes) else {
        return Ok(None);
    };
    let body = payload.into_raw_body();
    let hunks: Vec<Hunk> = hunk_rows.iter().map(recorded_hunk).collect();
    let page = serve_hunks(
        body.as_bytes(),
        &hunks,
        &lc.candidate.path,
        &lc.content_sha256,
        remaining,
    );
    if page.hunks.is_empty() {
        return Ok(None);
    }
    let blocks: Vec<String> = page
        .hunks
        .iter()
        .map(|h| format!("L{}-L{}:\n{}", h.range.lo, h.range.hi, h.text))
        .collect();
    Ok(Some(blocks.join("")))
}

/// One persisted range as the hunk the slicer takes: the new side only, since
/// the old side is not recorded and the slicer does not read it.
fn recorded_hunk(r: &Row) -> Hunk {
    Hunk {
        old_start: 0,
        old_len: 0,
        new_start: u32::try_from(r.get_i64(0).unwrap_or(0).max(0)).unwrap_or(0),
        new_len: u32::try_from(r.get_i64(1).unwrap_or(0).max(0)).unwrap_or(0),
        binary: false,
    }
}

/// Whether a checkpoint that read this revision was labelled `reverted`.
async fn reverted_provenance<S: CanonicalStore, R: SubstrateReader>(
    ctx: &RecallContext<'_, S, R>,
    revision_id: &str,
) -> Result<bool, ToolError> {
    let reverted = ctx
        .store
        .query(
            "SELECT ol.label_id FROM outcome_label ol \
             JOIN n_file_read r ON r.checkpoint_id = ol.subject_id \
             WHERE r.file_revision = ?1 AND ol.kind = 'reverted' LIMIT 1",
            &[rift_store::Value::text(revision_id)],
        )
        .await
        .map_err(internal)?;
    Ok(!reverted.is_empty())
}

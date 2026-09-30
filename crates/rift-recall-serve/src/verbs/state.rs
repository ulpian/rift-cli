//! The verbs that answer "what do you already hold, and what moved": the
//! manifest, the delta list, one file's verdict, and a hash-bound summary.
//! All four answer from the projection alone; none reaches for bytes.

use std::collections::{BTreeMap, BTreeSet};

use rift_recall_events::MemdEvent;
use rift_recall_ledger::candidates::{
    load_candidates, load_candidates_with_scope, LoadedCandidate,
};
use rift_recall_policy::TurnBudget;
use rift_recall_substrate::SubstrateReader;
use rift_recall_types::{Envelope, Trust};
use rift_recall_validity::{manifest_envelope, Budget};
use rift_store::CanonicalStore;
use serde_json::{json, Value};

use super::{fit_with_provenance, manifest_cap, sighting, Story};
use crate::envelope::{envelope_view, manifest_event};
use crate::{RecallContext, ServeDispatcher, ToolError, ToolName, ToolRequest, ToolResponse};

impl ServeDispatcher<'_> {
    pub(crate) async fn known_state<S: CanonicalStore, R: SubstrateReader>(
        &self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let (ranked, in_scope) = Self::ranked_candidates(req, ctx).await?;
        let containment = Self::containment_view(&in_scope, ctx);
        let known: BTreeSet<&str> = req.known_hashes.iter().map(String::as_str).collect();
        let (envelopes, baseline) = manifest_entries(&ranked, &known, &req.seen_turns, budget);
        // The story behind each entry that moved. Anchors come from the rows
        // already loaded — the loader picked the checkpoint to decide recency —
        // so this costs one query for the commit mirror and nothing else.
        let story = Story::over(ctx, &ranked, &known).await;
        let (telling, plain) = manifest_views(ctx, &envelopes, &ranked, &known, &story);
        // The manifest cap: known_state anchored before the trial,
        // so the trial sees the fresh allowance. Entries are already
        // in rank order — truncation keeps the highest-ranked prefix and says
        // so honestly (`truncated`, `omitted`).
        let cap = manifest_cap(req, budget);
        let n = telling.len();
        let build = |kept: usize, truncated: bool, detail: bool| {
            let views = if detail { &telling } else { &plain };
            Self::with_containment(
                json!({
                    "tool": "known_state",
                    "envelopes": views.get(..kept).unwrap_or(&[]),
                    "truncated": truncated,
                    "omitted": n.saturating_sub(kept),
                }),
                containment.as_ref(),
            )
        };
        let (payload, _kept) = fit_with_provenance(cap, n, &build);
        Ok(Self::respond(
            ToolName::KnownState,
            payload,
            baseline,
            budget,
            manifest_event,
        ))
    }

    pub(crate) async fn diff_since<S: CanonicalStore, R: SubstrateReader>(
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let (loaded, in_scope) =
            load_candidates_with_scope(ctx.store, &ctx.scope, ctx.now, ctx.ancestry)
                .await
                .map_err(|e| ToolError::Internal(e.to_string()))?;
        let containment = Self::containment_view(&in_scope, ctx);
        let known: BTreeSet<&str> = req.known_hashes.iter().map(String::as_str).collect();
        let story = Story::over(ctx, &loaded, &known).await;
        let mut changed = Vec::new();
        let mut changed_plain = Vec::new();
        let mut unchanged = Vec::new();
        let mut baseline = 0u32;
        for lc in &loaded {
            baseline = baseline.saturating_add(lc.candidate.est_tokens);
            if known.contains(lc.content_sha256.as_str()) {
                unchanged.push(json!(lc.candidate.path));
            } else {
                let mut entry =
                    json!({ "path": lc.candidate.path, "new_sha256": lc.content_sha256 });
                let mut bare = entry.clone();
                story.attach(ctx.provenance, &mut entry, &loaded, lc, &known, None);
                Story::attach_handle(&mut bare, &lc.candidate.path, None);
                changed.push(entry);
                changed_plain.push(bare);
            }
        }
        // The manifest cap bounds the delta list too. Droppable
        // order is the concatenation [changed…, unchanged…] with drops from
        // the tail: unchanged paths (cheap, least informative) go before
        // changed entries (the tool's point). Entry order inside each list is
        // the deterministic id order the candidates load in.
        let cap = manifest_cap(req, budget);
        let n = changed.len().saturating_add(unchanged.len());
        let build = |kept: usize, truncated: bool, detail: bool| {
            let changed_kept = kept.min(changed.len());
            let unchanged_kept = kept.saturating_sub(changed.len());
            let rows = if detail { &changed } else { &changed_plain };
            Self::with_containment(
                json!({
                    "tool": "diff_since",
                    "changed": rows.get(..changed_kept).unwrap_or(&[]),
                    "unchanged": unchanged.get(..unchanged_kept).unwrap_or(&[]),
                    "truncated": truncated,
                    "omitted": n.saturating_sub(kept),
                }),
                containment.as_ref(),
            )
        };
        let (payload, _kept) = fit_with_provenance(cap, n, &build);
        Ok(Self::respond(
            ToolName::DiffSince,
            payload,
            baseline,
            budget,
            manifest_event,
        ))
    }

    pub(crate) async fn file_if_changed<S: CanonicalStore, R: SubstrateReader>(
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let path = req.target.clone().unwrap_or_default();
        let loaded = load_candidates(ctx.store, &ctx.scope, ctx.now, ctx.ancestry)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let known: BTreeSet<&str> = req.known_hashes.iter().map(String::as_str).collect();
        let latest = loaded.iter().find(|l| l.candidate.path == path);
        let (state, baseline) = match latest {
            None => (
                json!({ "state": "invalid", "path": path, "reason": "path_not_found" }),
                0,
            ),
            Some(lc) if known.contains(lc.content_sha256.as_str()) => (
                json!({ "state": "unchanged", "path": path, "known_hash": lc.content_sha256 }),
                lc.candidate.est_tokens,
            ),
            Some(lc) => (
                json!({ "state": "changed", "path": path, "new_hash": lc.content_sha256 }),
                lc.candidate.est_tokens,
            ),
        };
        let payload = json!({ "tool": "file_if_changed", "result": state });
        Ok(Self::respond(
            ToolName::FileIfChanged,
            payload,
            baseline,
            budget,
            manifest_event,
        ))
    }

    pub(crate) async fn read_summary<S: CanonicalStore, R: SubstrateReader>(
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        // The summary is hash-bound; serving re-checks validity. We look up
        // the binding for the requested path and compare its bound hash to the
        // current revision: a moved source returns `stale`, never the bytes.
        let path = req.target.clone().unwrap_or_default();
        let loaded = load_candidates(ctx.store, &ctx.scope, ctx.now, ctx.ancestry)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let current = loaded.iter().find(|l| l.candidate.path == path);
        let rows = ctx
            .store
            .query(
                "SELECT source_sha256 FROM summary_binding WHERE path = ?1 ORDER BY created_at DESC LIMIT 1",
                &[rift_store::Value::text(path.as_str())],
            )
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let bound = rows.first().and_then(|r| r.get_text(0).ok());
        // Each arm yields the result view, the gross baseline the response
        // stands in for, and its event builder; the seam supplies the figures.
        type EventBuilder = Box<dyn FnOnce(u32, u32) -> MemdEvent + Send>;
        let (result, baseline, event): (Value, u32, EventBuilder) = match (bound, current) {
            (Some(bound), Some(lc)) if bound == lc.content_sha256 => {
                // valid: the bound source is still current.
                let summary = format!("summary bound to {bound}");
                let handle = format!("summary:{path}");
                (
                    json!({ "state": "summary", "path": path, "source_hash": bound, "summary": summary }),
                    lc.candidate.est_tokens,
                    Box::new(move |returned, avoided| MemdEvent::SummaryServed {
                        handle,
                        estimated_tokens_returned: returned,
                        estimated_tokens_avoided: avoided,
                    }),
                )
            }
            (Some(_), Some(lc)) => {
                let handle = format!("summary:{path}");
                (
                    json!({ "state": "stale", "path": path, "current_hash": lc.content_sha256 }),
                    0,
                    Box::new(move |returned, _| MemdEvent::Invalidated {
                        handle,
                        estimated_tokens_returned: returned,
                    }),
                )
            }
            _ => (
                json!({ "state": "unavailable", "path": path, "reason": "not_bound" }),
                0,
                Box::new(manifest_event),
            ),
        };
        let payload = json!({ "tool": "read_summary", "result": result });
        Ok(Self::respond(
            ToolName::ReadSummary,
            payload,
            baseline,
            budget,
            event,
        ))
    }
}

/// The sealed manifest entry for every ranked revision, and the gross
/// baseline they stand in for.
///
/// Every per-artifact result is built by the sealed `manifest_envelope`
/// constructor — no `ArtifactState` is constructed in this handler. Inline
/// content (summaries) will draw on this trial allowance; manifest-first,
/// most entries carry no bytes. The trial never touches the turn budget — the
/// single charge happens at the respond seam. Provenance: a tracked file blob
/// is a code fact. Real summaries are resolved here once ingest populates
/// them; until then a live revision with no bound summary is
/// `available_by_handle`, never "deleted".
fn manifest_entries(
    ranked: &[LoadedCandidate],
    known: &BTreeSet<&str>,
    seen_turns: &BTreeMap<(String, String), u64>,
    budget: &TurnBudget,
) -> (Vec<Envelope>, u32) {
    let mut envelopes: Vec<Envelope> = Vec::new();
    let mut baseline = 0u32;
    let mut vbudget = Budget::new(budget.tokens_remaining());
    for lc in ranked {
        let c = &lc.candidate;
        let handle = format!("file:{}@sha256:{}", c.path, lc.content_sha256);
        let seen = sighting(known, seen_turns, &c.path, &lc.content_sha256);
        baseline = baseline.saturating_add(c.est_tokens);
        envelopes.push(manifest_envelope(
            &c.path,
            Trust::CodeFact,
            &handle,
            &lc.content_sha256,
            seen,
            None,
            &mut vbudget,
        ));
    }
    (envelopes, baseline)
}

/// Every entry's view twice over: telling its story, and with the story
/// downgraded to the tag and the command — the two the fitter chooses
/// between before it drops an entry.
fn manifest_views<S: CanonicalStore, R: SubstrateReader>(
    ctx: &RecallContext<'_, S, R>,
    envelopes: &[Envelope],
    ranked: &[LoadedCandidate],
    known: &BTreeSet<&str>,
    story: &Story,
) -> (Vec<Value>, Vec<Value>) {
    let telling: Vec<Value> = envelopes
        .iter()
        .zip(ranked)
        .map(|(e, lc)| {
            let mut view = envelope_view(e);
            if !known.contains(lc.content_sha256.as_str()) {
                story.attach(ctx.provenance, &mut view, ranked, lc, known, None);
            }
            view
        })
        .collect();
    let plain: Vec<Value> = envelopes
        .iter()
        .zip(ranked)
        .map(|(e, lc)| {
            let mut view = envelope_view(e);
            if !known.contains(lc.content_sha256.as_str()) {
                Story::attach_handle(&mut view, &lc.candidate.path, None);
            }
            view
        })
        .collect();
    (telling, plain)
}

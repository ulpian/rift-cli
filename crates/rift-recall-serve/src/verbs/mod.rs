//! The store-backed handlers, one module per verb, and what they share: the
//! ranking every enumerating verb runs, the containment facts it reports, the
//! manifest cap, the two fitters that decide what a capped payload drops
//! first, the story a delta carries, and the accounting seam every response
//! passes through exactly once.
//!
//! The seam is why this is one module tree rather than ten free functions:
//! [`ServeDispatcher::respond`] is the single place a payload is serialized,
//! counted and charged, and every verb reaches it as a private method of the
//! same `impl`, so a second charge site cannot appear without appearing here.

pub(crate) mod credit;
mod doc;
mod exact;
mod hunks;
pub(crate) mod pack;
mod search;
mod state;

use std::collections::{BTreeMap, BTreeSet};

use rift_recall_events::MemdEvent;
use rift_recall_ledger::candidates::{load_candidates_with_scope, LoadedCandidate};
use rift_recall_ledger::scope_query::ScopeSet;
use rift_recall_policy::TurnBudget;
use rift_recall_rank::{rank, RankQuery, SignalWeights};
use rift_recall_substrate::SubstrateReader;
use rift_recall_types::ProvenanceOracle;
use rift_store::CanonicalStore;
use serde_json::{json, Value};

use crate::envelope::charge;
use crate::{
    provenance, RecallContext, ServeDispatcher, ToolError, ToolName, ToolRequest, ToolResponse,
};

pub use self::hunks::MAX_CHANGED_HUNKS;
pub use self::pack::PACK_CANDIDATES;

/// The default manifest token cap when the request carries no budget
/// (pack parity: mirrors `DEFAULT_PACK_TOKENS`, deliberately
/// below the turn ceiling so an unbudgeted anchor call leaves room for
/// same-turn content serves).
pub const DEFAULT_MANIFEST_TOKENS: u32 = 1200;

/// The hard ceiling an explicit manifest `token_budget` is clamped to
/// (pack parity: mirrors `MAX_PACK_TOKENS`).
pub const MAX_MANIFEST_TOKENS: u32 = 4000;

/// The recorded line ranges of one revision, in position order — read by the
/// ranked hunk verb and by the pack over one spelling, so the two cannot
/// drift into two queries.
const HUNK_RANGES_SQL: &str =
    "SELECT new_start, new_len FROM n_changed_hunk WHERE file_revision = ?1 ORDER BY new_start ASC";

/// The checkpoint each entry of a manifest belongs to, and the commits that
/// anchored them.
///
/// Resolved once per serve rather than per entry. The anchors are already on
/// the rows the handler loaded — the candidate loader picks each revision's
/// most-recent in-scope checkpoint to decide recency, and now carries it — so
/// the only query this costs is the one for the commit mirror, which is asked
/// for the whole set at once.
struct Story {
    /// Checkpoint id → the commit it anchored, where one is mirrored.
    commits: BTreeMap<String, String>,
}

impl ServeDispatcher<'_> {
    fn rank_query(req: &ToolRequest) -> Result<RankQuery, ToolError> {
        RankQuery::new(
            req.task.clone().unwrap_or_default(),
            req.files_in_focus.clone(),
            req.error_signatures.clone(),
        )
        .map_err(|_| ToolError::QueryParamsRequired)
    }

    pub(crate) async fn ranked_candidates<S: CanonicalStore, R: SubstrateReader>(
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
    ) -> Result<(Vec<LoadedCandidate>, ScopeSet), ToolError> {
        let q = Self::rank_query(req)?;
        let (loaded, in_scope) =
            load_candidates_with_scope(ctx.store, &ctx.scope, ctx.now, ctx.ancestry)
                .await
                .map_err(|e| ToolError::Internal(e.to_string()))?;
        let cands: Vec<_> = loaded.iter().map(|l| l.candidate.clone()).collect();
        let order = rank(&q, &cands, ctx.now, &SignalWeights::default());
        // Re-order `loaded` to match the ranking (by candidate id).
        let mut by_id: std::collections::BTreeMap<String, LoadedCandidate> = loaded
            .into_iter()
            .map(|l| (l.candidate.id.clone(), l))
            .collect();
        Ok((
            order
                .into_iter()
                .filter_map(|s| by_id.remove(&s.candidate.id))
                .collect(),
            in_scope,
        ))
    }

    /// The containment facts an **enumerating** response carries — when it has
    /// any to carry.
    ///
    /// The omission line grows one cause for containment: a
    /// manifest that quietly held back the family's other line would be a
    /// manifest an agent could not reason about. `out_of_branch` counts what
    /// the branch axis excluded (before the response's own cap, which
    /// `omitted` counts separately); `unknown_head` says the axis could not be
    /// decided *at all*, which is a different sentence from "there was
    /// nothing"; `cross_branch_grant` says a grant widened this serve.
    ///
    /// **Absent when there is nothing to say**, and that is a deliberate
    /// decision rather than a shortcut: the member is worth real tokens out of
    /// the caller's budget on every manifest, the accounting seam charges the
    /// caller for the bytes it ships, and a block reporting three zeroes buys
    /// the agent nothing while pushing a real entry off the end of a capped
    /// manifest. Nothing held back, nothing said.
    fn containment_view<S: CanonicalStore, R: SubstrateReader>(
        set: &ScopeSet,
        ctx: &RecallContext<'_, S, R>,
    ) -> Option<Value> {
        let out_of_branch = set.omitted_out_of_branch();
        let unknown_head = set.unknown_head();
        let granted = ctx.scope.cross.branches();
        (out_of_branch > 0 || unknown_head || granted).then(|| {
            json!({
                "out_of_branch": out_of_branch,
                "unknown_head": unknown_head,
                "cross_branch_grant": granted,
            })
        })
    }

    /// Attach [`Self::containment_view`]'s block to a response document.
    fn with_containment(mut doc: Value, containment: Option<&Value>) -> Value {
        if let (Some(block), Some(obj)) = (containment, doc.as_object_mut()) {
            obj.insert("containment".to_owned(), block.clone());
        }
        doc
    }

    /// The accounting seam: serialize the payload once (the same
    /// bytes the transport ships — serde_json default field order is
    /// load-bearing), count it with the real tokenizer, perform the single
    /// clamped turn-budget charge, and seal the event from those same figures.
    /// `baseline` is the gross ingest-time estimate this response stands in
    /// for; the emitted `avoided` is net (`baseline − returned`, saturating).
    /// Handlers never mutate the budget — every shaping decision upstream is a
    /// non-charging trial against `tokens_remaining()`.
    fn respond(
        tool: ToolName,
        payload: Value,
        baseline: u32,
        budget: &mut TurnBudget,
        event: impl FnOnce(u32, u32) -> MemdEvent,
    ) -> ToolResponse {
        let returned = rift_tokens::count(&payload.to_string());
        let _ = charge(budget, returned);
        let avoided = baseline.saturating_sub(returned);
        let event = event(returned, avoided);
        ToolResponse {
            tool,
            estimated_tokens_returned: returned,
            estimated_tokens_avoided: avoided,
            tokenizer: rift_tokens::provenance(),
            payload,
            event,
        }
    }
}

impl Story {
    /// Resolve the story for a loaded candidate set.
    ///
    /// Never fails the serve: a projection that cannot answer the commit
    /// mirror yields an empty map, and every delta then carries its checkpoints
    /// without a commit. An unavailable fact is absent, not fatal.
    async fn over<S: CanonicalStore, R: SubstrateReader>(
        ctx: &RecallContext<'_, S, R>,
        loaded: &[LoadedCandidate],
        known: &BTreeSet<&str>,
    ) -> Self {
        let ids: Vec<String> = loaded
            .iter()
            .filter(|lc| !known.contains(lc.content_sha256.as_str()))
            .map(|lc| lc.checkpoint.clone())
            .collect();
        Self {
            commits: rift_recall_ledger::provenance::commit_oids(ctx.store, &ids)
                .await
                .unwrap_or_default(),
        }
    }

    /// The checkpoint that recorded the revision of `path` the caller says it
    /// holds, when this family recorded one.
    ///
    /// **Path-bound.** A hash the caller holds for some *other* file is not a
    /// basis for this one, and pairing the two would attribute a change to a
    /// checkpoint that never touched the path — the same failure the pull
    /// verb's own resolver exists to prevent.
    fn held_in<'c>(
        loaded: &'c [LoadedCandidate],
        known: &BTreeSet<&str>,
        path: &str,
    ) -> Option<&'c str> {
        loaded
            .iter()
            .find(|l| l.candidate.path == path && known.contains(l.content_sha256.as_str()))
            .map(|l| l.checkpoint.as_str())
    }

    /// Attach the full object to one entry, or nothing when the delta is not
    /// anchored to a checkpoint.
    fn attach(
        &self,
        oracle: &dyn ProvenanceOracle,
        entry: &mut Value,
        loaded: &[LoadedCandidate],
        lc: &LoadedCandidate,
        known: &BTreeSet<&str>,
        line: Option<u32>,
    ) {
        let bracket = provenance::Bracket {
            from: Self::held_in(loaded, known, &lc.candidate.path),
            to: Some(lc.checkpoint.as_str()),
            commit: self.commits.get(&lc.checkpoint).map(String::as_str),
        };
        let (Some(value), Some(obj)) = (
            provenance::object(oracle, &bracket, &lc.candidate.path, line),
            entry.as_object_mut(),
        ) else {
            return;
        };
        obj.insert("provenance".to_owned(), value);
    }

    /// Attach the downgraded form: the tag, and the command that fetches the
    /// rest. Unconditional, because a caller that saw the full object on a
    /// roomier turn must not read its absence here as "nothing is recorded".
    fn attach_handle(entry: &mut Value, path: &str, line: Option<u32>) {
        let Some(obj) = entry.as_object_mut() else {
            return;
        };
        let (tag, why) = provenance::downgraded(path, line);
        obj.insert("provenance".to_owned(), tag);
        obj.insert("why".to_owned(), why);
    }
}

/// Whether this call is a **pull against a basis**: a path and the revision the
/// caller holds, both named.
///
/// It is the one question that decides the shape of a `changed-hunks` call —
/// which handler answers it, and whether ranking (and so a task and a focus
/// set) is required at all — so it is asked in one place and both callers read
/// the same answer.
pub(crate) fn targeted_pull(tool: ToolName, req: &ToolRequest) -> bool {
    matches!(tool, ToolName::ChangedHunks)
        && req.target.as_deref().is_some_and(|t| !t.trim().is_empty())
        && !req.known_hashes.is_empty()
}

/// The effective manifest cap: the pack-parity default/max
/// split composed with the remaining turn budget. A non-charging trial —
/// the single charge stays at the respond seam.
pub(crate) fn manifest_cap(req: &ToolRequest, budget: &TurnBudget) -> u32 {
    if req.token_budget == 0 {
        DEFAULT_MANIFEST_TOKENS
    } else {
        req.token_budget.clamp(1, MAX_MANIFEST_TOKENS)
    }
    .min(budget.tokens_remaining())
}

/// The largest droppable-entry prefix whose serialized payload fits `cap`.
/// `build(kept, truncated)` rebuilds the full payload for a
/// prefix of `kept` entries; entries must be ordered so dropping from the
/// tail is the right sacrifice (rank order for the manifest, unchanged-last
/// for the delta list). Deterministic: a fast-path full fit, a binary search
/// for the largest fitting prefix, then a defensive linear walk-down — BPE
/// token counts are not formally monotone under concatenation, so the
/// postcondition (`fits(kept) || kept == 0`) is guaranteed, not assumed.
/// With nothing droppable (`n == 0`) or nothing fitting, the skeleton still
/// ships (the anchor is never stranded); the seam's clamped charge absorbs a
/// skeleton that exceeds a tiny cap.
pub(crate) fn fit_to_cap(
    cap: u32,
    n: usize,
    build: &dyn Fn(usize, bool) -> Value,
) -> (Value, usize) {
    let full = build(n, false);
    if n == 0 || rift_tokens::count(&full.to_string()) <= cap {
        return (full, n);
    }
    let fits = |k: usize| rift_tokens::count(&build(k, true).to_string()) <= cap;
    // Binary search over [0, n): find the first prefix length that does NOT
    // fit (assuming monotone prefix costs), then step back one.
    let (mut lo, mut hi) = (0usize, n);
    while lo < hi {
        let mid = lo.saturating_add(hi.saturating_sub(lo) / 2);
        if fits(mid) {
            lo = mid.saturating_add(1);
        } else {
            hi = mid;
        }
    }
    let mut kept = lo.saturating_sub(1);
    // Walk down to certainty (non-monotone counts cannot break the bound).
    while kept > 0 && !fits(kept) {
        kept = kept.saturating_sub(1);
    }
    (build(kept, true), kept)
}

/// The largest fitting payload, sacrificing the deltas' **story** before any
/// delta.
///
/// Three attempts in one fixed order, and the order is the whole content of
/// this function: the payload with provenance; the payload with every
/// provenance object downgraded to its tag and its command; and only then
/// [`fit_to_cap`], which starts dropping entries. A delta the caller cannot see
/// is a re-read it has to do; a delta whose story it cannot see is one command
/// away. That is the same sacrifice the sensitive-path rule makes — serve the
/// handle rather than half the thing — applied to the rider instead of the
/// body, and it is decided on the **whole** serialized payload rather than
/// per entry, because the cap is a fact about the payload.
///
/// A non-charging trial like every other shaping decision: the single charge
/// stays at the respond seam.
fn fit_with_provenance(
    cap: u32,
    n: usize,
    build: &dyn Fn(usize, bool, bool) -> Value,
) -> (Value, usize) {
    let told = build(n, false, true);
    if n == 0 || rift_tokens::count(&told.to_string()) <= cap {
        return (told, n);
    }
    let plain = build(n, false, false);
    if rift_tokens::count(&plain.to_string()) <= cap {
        return (plain, n);
    }
    fit_to_cap(cap, n, &|kept, truncated| build(kept, truncated, false))
}

/// A read or build failure as the agent sees it: the reason, never content.
fn internal(err: impl std::fmt::Display) -> ToolError {
    ToolError::Internal(err.to_string())
}

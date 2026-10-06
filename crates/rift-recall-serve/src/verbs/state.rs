//! The verbs that answer "what do you already hold, and what moved": the
//! manifest, the delta list, one file's verdict, and a hash-bound summary.
//! All four answer from the projection. When the reader is bound to a
//! checkout, every *unchanged* they say is the stated hash matching the SHA-256
//! of the file as the checkout holds it ([`super::credit`]); a reader bound to
//! no checkout — the push tier's — gets the manifest and the delta list as the
//! projection's view, where a stated hash alone is *unchanged* and nothing is
//! credited, while one file's verdict and a summary answer `unavailable`.

use std::collections::{BTreeMap, BTreeSet};

use rift_recall_events::MemdEvent;
use rift_recall_ledger::candidates::{
    load_candidates, load_candidates_with_scope, LoadedCandidate,
};
use rift_recall_ledger::validity_store::{current_summary, CurrentSummary};
use rift_recall_policy::TurnBudget;
use rift_recall_substrate::worktree::CurrentRefusal;
use rift_recall_substrate::SubstrateReader;
use rift_recall_types::{Envelope, Trust};
use rift_recall_validity::{manifest_envelope, Budget, ValidSummary, Validity};
use rift_store::{CanonicalStore, PlainCipher};
use serde_json::{json, Value};

use super::credit::{Current, Evidence, STATED_UNRECORDED};
use super::summaries::{checked_summaries, served_versions, versions_view};
use super::{fit_with_provenance, internal, manifest_cap, Story};
use crate::envelope::{envelope_view, manifest_event};
use crate::{RecallContext, ServeDispatcher, ToolError, ToolName, ToolRequest, ToolResponse};

/// The event a summary answer is sealed with, once the seam has its figures.
type EventBuilder = Box<dyn FnOnce(u32, u32) -> MemdEvent + Send>;

/// One `read_summary` call, as its answer is decided: the path asked about,
/// the hashes the caller stated, what the checkout and the record say, and
/// the projection's revisions.
struct SummaryCall<'a> {
    path: &'a str,
    known: &'a BTreeSet<&'a str>,
    evidence: &'a Evidence,
    loaded: &'a [LoadedCandidate],
}

/// The delta list's rows: the changed entries with their story and with it
/// downgraded — the two the fitter chooses between — and the unchanged ones.
struct Deltas {
    changed: Vec<Value>,
    changed_plain: Vec<Value>,
    unchanged: Vec<Value>,
}

/// What a manifest entry is decided from besides its revision: the hashes
/// the caller stated, the turns its window read them, what the checkout and
/// the record say, and the valid summaries of the revisions the checkout holds.
struct ManifestSources<'a> {
    known: &'a BTreeSet<&'a str>,
    seen_turns: &'a BTreeMap<(String, String), u64>,
    evidence: &'a Evidence,
    summaries: &'a BTreeMap<String, Validity>,
}

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
        let evidence = Evidence::over(ctx, &known, &ranked).await;
        let summaries = checked_summaries(ctx, &evidence, &ranked).await;
        let sources = ManifestSources {
            known: &known,
            seen_turns: &req.seen_turns,
            evidence: &evidence,
            summaries: &summaries,
        };
        let envelopes = manifest_entries(&ranked, &sources, budget);
        let baseline = evidence.list_baseline(&known, &ranked);
        // The story behind each entry that moved. Anchors come from the rows
        // already loaded — the loader picked the checkpoint to decide recency —
        // so this costs one query for the commit mirror and nothing else.
        let story = Story::over(ctx, &ranked, &known).await;
        let (telling, plain) = manifest_views(ctx, &envelopes, &ranked, &known, &story, &evidence);
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
        let evidence = Evidence::over(ctx, &known, &loaded).await;
        let baseline = evidence.list_baseline(&known, &loaded);
        let Deltas {
            changed,
            changed_plain,
            unchanged,
        } = deltas(ctx, &loaded, &known, &story, &evidence);
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
        let evidence = Evidence::at_path(ctx, &known, &path).await;
        let (state, baseline) = file_verdict(&evidence, &loaded, &known, &path);
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
        // the bindings for the requested path against the file as the
        // checkout holds it: a moved source returns `stale`, never the
        // summary, and a file that cannot be read returns `unavailable`.
        let path = req.target.clone().unwrap_or_default();
        let loaded = load_candidates(ctx.store, &ctx.scope, ctx.now, ctx.ancestry)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let known: BTreeSet<&str> = req.known_hashes.iter().map(String::as_str).collect();
        let evidence = Evidence::at_path(ctx, &known, &path).await;
        let now = match evidence.current(&path) {
            Some(Current::Verified(now)) => Some(now.as_str()),
            Some(Current::Unverified(_)) | None => None,
        };
        let bound = current_summary(ctx.store, &PlainCipher, &path, now, served_versions())
            .await
            .map_err(internal)?;
        let summary = SummaryCall {
            path: &path,
            known: &known,
            evidence: &evidence,
            loaded: &loaded,
        };
        let (result, baseline, event) = summary.answer(bound);
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

impl SummaryCall<'_> {
    /// The result view, the baseline it stands in for, and its event builder;
    /// the seam supplies the figures.
    ///
    /// A binding is valid only when its source is the file as the checkout
    /// holds it now, and a valid one is credited with the file's cost only
    /// when the caller stated that revision and this family recorded reading
    /// it: a summary served to a caller who never held the file spared no
    /// re-read.
    fn answer(&self, bound: CurrentSummary) -> (Value, u32, EventBuilder) {
        let path = self.path;
        if matches!(bound, CurrentSummary::NotBound) {
            return unavailable_summary(path, "not_bound");
        }
        if !self.loaded.iter().any(|l| l.candidate.path == path) {
            return unavailable_summary(path, "not_bound");
        }
        let now = match self.evidence.current(path) {
            Some(Current::Verified(now)) => now.clone(),
            Some(Current::Unverified(why)) => return unavailable_summary(path, why.tag()),
            None => return unavailable_summary(path, CurrentRefusal::NoWorktree.tag()),
        };
        let summary = match bound {
            CurrentSummary::Bound(Validity::Valid(summary)) => summary,
            CurrentSummary::Bound(Validity::Degraded { .. }) => {
                return unavailable_summary(path, "digest_unavailable")
            }
            CurrentSummary::Stale | CurrentSummary::Bound(Validity::Invalid { .. }) => {
                return stale_summary(path, now)
            }
            CurrentSummary::NotBound | CurrentSummary::Unchecked => {
                return unavailable_summary(path, "not_bound")
            }
        };
        let mut result = served_summary(path, &now, &summary);
        let row = self
            .loaded
            .iter()
            .find(|l| l.candidate.path == path && l.content_sha256 == now);
        let baseline = row.map_or(0, |lc| self.evidence.unchanged_credit(self.known, lc));
        if let Some(lc) = row {
            self.evidence.tag(self.known, lc, &mut result);
        }
        let handle = format!("summary:{path}");
        (
            result,
            baseline,
            Box::new(move |returned, avoided| MemdEvent::SummaryServed {
                handle,
                estimated_tokens_returned: returned,
                estimated_tokens_avoided: avoided,
                pull_claim: None,
            }),
        )
    }
}

/// A valid summary's answer: the stored outline, with the key it is bound
/// under, the versions in that key and the revision it describes.
fn served_summary(path: &str, now: &str, summary: &ValidSummary) -> Value {
    json!({
        "state": "summary",
        "path": path,
        "source_hash": now,
        "summary": String::from_utf8_lossy(summary.digest()),
        "summary_key": summary.key(),
        "versions": versions_view(served_versions()),
        "handle": format!("file:{path}@sha256:{now}"),
    })
}

/// A summary whose source moved: the new hash, never the summary.
fn stale_summary(path: &str, now: String) -> (Value, u32, EventBuilder) {
    let handle = format!("summary:{path}");
    (
        json!({ "state": "stale", "path": path, "current_hash": now }),
        0,
        Box::new(move |returned, _| MemdEvent::Invalidated {
            handle,
            estimated_tokens_returned: returned,
        }),
    )
}

/// A summary that cannot be served, and why.
fn unavailable_summary(path: &str, reason: &str) -> (Value, u32, EventBuilder) {
    (
        json!({ "state": "unavailable", "path": path, "reason": reason }),
        0,
        Box::new(manifest_event),
    )
}

/// One file's verdict against the bytes on disk, and what it books.
///
/// A path the projection never recorded a read of is `invalid` with
/// `path_not_found`: the caller reads the file itself. A file the checkout
/// cannot read is `unavailable` with the reason, and the caller reads it
/// itself too. Otherwise the file is `unchanged` exactly when the caller
/// stated the hash of the bytes on disk, and `changed` with that hash when it
/// did not; only an unchanged file this family recorded reading is credited.
fn file_verdict(
    evidence: &Evidence,
    loaded: &[LoadedCandidate],
    known: &BTreeSet<&str>,
    path: &str,
) -> (Value, u32) {
    if !loaded.iter().any(|l| l.candidate.path == path) {
        return (
            json!({ "state": "invalid", "path": path, "reason": "path_not_found" }),
            0,
        );
    }
    let now = match evidence.current(path) {
        Some(Current::Verified(now)) => now.as_str(),
        Some(Current::Unverified(why)) => {
            return (
                json!({ "state": "unavailable", "path": path, "reason": why.tag() }),
                0,
            )
        }
        None => {
            return (
                json!({ "state": "unavailable", "path": path, "reason": CurrentRefusal::NoWorktree.tag() }),
                0,
            )
        }
    };
    if !known.contains(now) {
        return (
            json!({ "state": "changed", "path": path, "new_hash": now }),
            0,
        );
    }
    let mut state = json!({ "state": "unchanged", "path": path, "known_hash": now });
    if !evidence.is_recorded(path, now) {
        if let Some(obj) = state.as_object_mut() {
            obj.insert("avoided_basis".to_owned(), json!(STATED_UNRECORDED));
        }
    }
    let row = loaded
        .iter()
        .find(|l| l.candidate.path == path && l.content_sha256 == now);
    let baseline = row.map_or(0, |lc| evidence.unchanged_credit(known, lc));
    (state, baseline)
}

/// The delta list's rows, in the order the candidates loaded in: a held file
/// by path — or, when the caller stated it but this family never recorded
/// reading it, by path and the tag that says why it earned nothing — and every
/// other file as a move with its story. A file the caller stated that the
/// checkout no longer holds, or could not read, is a move too, tagged with
/// what the checkout found.
fn deltas<S: CanonicalStore, R: SubstrateReader>(
    ctx: &RecallContext<'_, S, R>,
    loaded: &[LoadedCandidate],
    known: &BTreeSet<&str>,
    story: &Story,
    evidence: &Evidence,
) -> Deltas {
    let mut rows = Deltas {
        changed: Vec::new(),
        changed_plain: Vec::new(),
        unchanged: Vec::new(),
    };
    for lc in loaded {
        if evidence.holds(known, lc) {
            let mut entry = json!({ "path": lc.candidate.path });
            evidence.tag(known, lc, &mut entry);
            let tagged = entry.as_object().is_some_and(|o| o.len() > 1);
            rows.unchanged.push(if tagged {
                entry
            } else {
                json!(lc.candidate.path)
            });
            continue;
        }
        let mut entry = json!({ "path": lc.candidate.path, "new_sha256": lc.content_sha256 });
        evidence.tag(known, lc, &mut entry);
        let mut bare = entry.clone();
        story.attach(ctx.provenance, &mut entry, loaded, lc, known, None);
        Story::attach_handle(&mut bare, &lc.candidate.path, None);
        rows.changed.push(entry);
        rows.changed_plain.push(bare);
    }
    rows
}

/// The sealed manifest entry for every ranked revision.
///
/// Every per-artifact result is built by the sealed `manifest_envelope`
/// constructor — no `ArtifactState` is constructed in this handler. Inline
/// content (summaries) will draw on this trial allowance; manifest-first,
/// most entries carry no bytes. The trial never touches the turn budget — the
/// single charge happens at the respond seam. Provenance: a tracked file blob
/// is a code fact. A revision with a valid summary of the bytes the checkout
/// holds carries it when it fits; one without is `available_by_handle`, never
/// "deleted".
fn manifest_entries(
    ranked: &[LoadedCandidate],
    sources: &ManifestSources<'_>,
    budget: &TurnBudget,
) -> Vec<Envelope> {
    let mut envelopes: Vec<Envelope> = Vec::new();
    let mut vbudget = Budget::new(budget.tokens_remaining());
    for lc in ranked {
        let c = &lc.candidate;
        let handle = format!("file:{}@sha256:{}", c.path, lc.content_sha256);
        let seen = sources
            .evidence
            .sighting(sources.known, sources.seen_turns, lc);
        let summary = match sources.summaries.get(&c.path) {
            Some(Validity::Valid(summary)) if summary.bound_oid() == lc.content_sha256 => {
                Some(summary)
            }
            _ => None,
        };
        envelopes.push(manifest_envelope(
            &c.path,
            Trust::CodeFact,
            &handle,
            &lc.content_sha256,
            seen,
            summary,
            &mut vbudget,
        ));
    }
    envelopes
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
    evidence: &Evidence,
) -> (Vec<Value>, Vec<Value>) {
    let telling: Vec<Value> = envelopes
        .iter()
        .zip(ranked)
        .map(|(e, lc)| {
            let mut view = envelope_view(e);
            evidence.tag(known, lc, &mut view);
            if !evidence.holds(known, lc) {
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
            evidence.tag(known, lc, &mut view);
            if !evidence.holds(known, lc) {
                Story::attach_handle(&mut view, &lc.candidate.path, None);
            }
            view
        })
        .collect();
    (telling, plain)
}

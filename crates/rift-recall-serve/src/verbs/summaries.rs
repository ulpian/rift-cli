//! The serve-time summary loader: the outlines a pull may carry, read from
//! the durable store and kept only where the checkout still holds the
//! revision each one is bound to.
//!
//! One module because three verbs ask the same question — the manifest, the
//! context pack and the one-file summary — and an answer that says a summary
//! is current must be decided by one rule, the same rule everywhere.

use std::collections::BTreeMap;

use rift_recall_ledger::candidates::LoadedCandidate;
use rift_recall_ledger::validity_store::{current_summaries, current_summary, CurrentSummary};
use rift_recall_policy::SERVE_POLICY_VERSION;
use rift_recall_substrate::SubstrateReader;
use rift_recall_validity::{
    outline_shape, outline_versions, OutlineShape, SummaryVersions, Validity,
};
use rift_store::{CanonicalStore, PlainCipher};
use serde_json::{json, Value};

use super::credit::{Current, Evidence};
use crate::RecallContext;

/// The versions a summary is served under: the outline's own and the serve
/// policy's, the same figures the writer binds with.
pub(crate) fn served_versions() -> SummaryVersions {
    outline_versions(SERVE_POLICY_VERSION)
}

/// The versions a served summary carries, as its answer spells them.
pub(crate) fn versions_view(versions: SummaryVersions) -> Value {
    json!({
        "extractor": versions.extractor_version,
        "policy": versions.policy_version,
        "redaction": versions.redaction_version,
    })
}

/// The valid summaries of the ranked revisions the checkout still holds, by
/// path.
///
/// The store is asked once for every ranked revision; the checkout is then
/// asked only about the paths that came back with a valid summary, and a
/// summary survives only where the bytes on disk hash to its bound revision.
/// A store that cannot answer serves no summary — a summary is never required
/// for an answer, so its absence is the honest degradation. The digest is read
/// through the plain frame: no recall key custody exists, and an encrypted row
/// fails closed to no summary.
pub(crate) async fn checked_summaries<S: CanonicalStore, R: SubstrateReader>(
    ctx: &RecallContext<'_, S, R>,
    evidence: &Evidence,
    ranked: &[LoadedCandidate],
) -> BTreeMap<String, Validity> {
    let wanted: Vec<(&str, &str)> = ranked
        .iter()
        .map(|lc| (lc.candidate.path.as_str(), lc.content_sha256.as_str()))
        .collect();
    let Ok(found) = current_summaries(ctx.store, &PlainCipher, &wanted, served_versions()).await
    else {
        return BTreeMap::new();
    };
    found
        .into_iter()
        .filter(|(path, validity)| match validity {
            Validity::Valid(summary) => holds_now(ctx, evidence, path, summary.bound_oid()),
            Validity::Invalid { .. } | Validity::Degraded { .. } => false,
        })
        .collect()
}

/// The shape of the outline bound to `path` at `current`, the revision the
/// caller has just hashed off the checkout, or `None` when no valid outline
/// of that revision is bound.
///
/// For the recovery map, which already holds the store open at a turn
/// boundary and has already hashed the file: one read, no tokenizer.
pub async fn current_outline_shape(
    store: &impl CanonicalStore,
    path: &str,
    current: &str,
) -> Option<OutlineShape> {
    let bound = current_summary(store, &PlainCipher, path, Some(current), served_versions())
        .await
        .ok()?;
    match bound {
        CurrentSummary::Bound(Validity::Valid(summary)) => outline_shape(&summary),
        CurrentSummary::Bound(_)
        | CurrentSummary::NotBound
        | CurrentSummary::Unchecked
        | CurrentSummary::Stale => None,
    }
}

/// Whether the checkout's bytes at `path` hash to `revision`, asking the
/// evidence first and the checkout only when the evidence never asked.
fn holds_now<S: CanonicalStore, R: SubstrateReader>(
    ctx: &RecallContext<'_, S, R>,
    evidence: &Evidence,
    path: &str,
    revision: &str,
) -> bool {
    let Some(current) = evidence.current(path) else {
        let asked = Current::of(ctx.reader, path);
        return matches!(asked, Current::Verified(now) if now == revision);
    };
    matches!(current, Current::Verified(now) if now == revision)
}

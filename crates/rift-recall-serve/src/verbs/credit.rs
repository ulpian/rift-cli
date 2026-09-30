//! The evidence a pull's avoided figure rests on, asked once per call.
//!
//! A pull is credited with a spared re-read only for a file the caller said it
//! held, that has not moved, and that this family is recorded as having read.
//! The first two facts are in the request and the candidate row; the third is
//! a query, and it is a separate module because every crediting handler asks
//! it and none may ask it twice.
//!
//! The query sits here rather than inside the accounting seam
//! ([`ServeDispatcher::respond`](crate::ServeDispatcher)) because the seam is
//! synchronous and takes a finished baseline: the baseline has to be known
//! before the payload is priced, so the evidence is gathered first, beside the
//! handler's own reads, and the seam still measures, charges and seals once.
//! It runs only on the pull path, in the process that already opened the
//! store; no hook decision reaches it.

use std::collections::BTreeSet;

use rift_recall_ledger::candidates::{recorded_reads, LoadedCandidate};
use rift_recall_substrate::SubstrateReader;
use rift_store::CanonicalStore;
use serde_json::{json, Value};

use crate::RecallContext;

/// The payload tag on an entry the caller stated and that matched, but that
/// this family never recorded reading: the answer still says the file is
/// unchanged, because its bytes did not move, and only the credit is withheld.
pub(crate) const STATED_UNRECORDED: &str = "stated_unrecorded";

/// The stated hashes this family is recorded as having read.
pub(crate) struct Recorded {
    hashes: BTreeSet<String>,
}

impl Recorded {
    /// Ask the projection which of `stated` a read landed on — one query, and
    /// none at all when nothing was stated.
    ///
    /// A store that cannot answer credits nothing rather than failing the
    /// serve: the answer the caller asked for does not depend on the figure,
    /// and a figure Rift cannot ground is one it does not publish.
    pub(crate) async fn of<S: CanonicalStore, R: SubstrateReader>(
        ctx: &RecallContext<'_, S, R>,
        stated: &BTreeSet<&str>,
    ) -> Self {
        if stated.is_empty() {
            return Self::none();
        }
        Self {
            hashes: recorded_reads(ctx.store, stated).await.unwrap_or_default(),
        }
    }

    /// Nothing recorded — the evidence of a call that stated nothing.
    pub(crate) fn none() -> Self {
        Self {
            hashes: BTreeSet::new(),
        }
    }

    /// Whether a read of `hash` is on record.
    pub(crate) fn holds(&self, hash: &str) -> bool {
        self.hashes.contains(hash)
    }

    /// What one candidate books: its cost when the caller stated its current
    /// hash and a read of it is on record, zero otherwise.
    pub(crate) fn credit(&self, known: &BTreeSet<&str>, hash: &str, est_tokens: u32) -> u32 {
        if known.contains(hash) && self.holds(hash) {
            return est_tokens;
        }
        0
    }

    /// Whether the caller stated `hash`, it matched, and no read of it is on
    /// record — the one case the payload tags.
    pub(crate) fn unrecorded(&self, known: &BTreeSet<&str>, hash: &str) -> bool {
        known.contains(hash) && !self.holds(hash)
    }

    /// The baseline of a candidate set: the sum of what each entry books.
    pub(crate) fn baseline<'c>(
        &self,
        known: &BTreeSet<&str>,
        candidates: impl IntoIterator<Item = (&'c str, u32)>,
    ) -> u32 {
        candidates
            .into_iter()
            .map(|(hash, est)| self.credit(known, hash, est))
            .fold(0u32, u32::saturating_add)
    }

    /// Tag an entry object as stated but unrecorded, when it is.
    pub(crate) fn tag(&self, known: &BTreeSet<&str>, hash: &str, entry: &mut Value) {
        if !self.unrecorded(known, hash) {
            return;
        }
        let Some(obj) = entry.as_object_mut() else {
            return;
        };
        obj.insert("avoided_basis".to_owned(), json!(STATED_UNRECORDED));
    }
}

/// What each candidate would cost to re-read, keyed by the hash it is
/// credited under — the shape [`Recorded::baseline`] sums.
pub(crate) fn candidate_costs(loaded: &[LoadedCandidate]) -> impl Iterator<Item = (&str, u32)> {
    loaded
        .iter()
        .map(|lc| (lc.content_sha256.as_str(), lc.candidate.est_tokens))
}

#[cfg(test)]
impl Recorded {
    /// A recorded set built by hand, for the property over arbitrary evidence.
    pub(crate) fn from_hashes(hashes: BTreeSet<String>) -> Self {
        Self { hashes }
    }
}

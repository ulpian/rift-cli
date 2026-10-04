//! The evidence a pull's avoided figure and its *unchanged* answers rest on,
//! gathered once per call.
//!
//! Two facts decide both, and neither is the caller's word. The first is the
//! checkout: a projection records the revisions a checkpoint carried, so its
//! newest revision of a file is a statement about the past, and only the bytes
//! on disk say what the file is now. The second is the record: a caller that
//! states a hash may have found it anywhere, while a read the capture recorded
//! is a fact Rift saw. A file is answered unchanged only where the checkout
//! agrees with the caller, and a spared re-read is credited only where the
//! record agrees too.
//!
//! The evidence is gathered here rather than inside the accounting seam
//! ([`ServeDispatcher::respond`](crate::ServeDispatcher)) because the seam is
//! synchronous and takes a finished baseline: the baseline has to be known
//! before the payload is priced, so the evidence comes first, beside the
//! handler's own reads, and the seam still measures, charges and seals once.
//! It runs only on the pull path, in the process that already opened the
//! store; no hook decision reaches it.
//!
//! A reader bound to no checkout answers the projection's view, exactly as it
//! always did, and credits nothing: that answer was never checked against the
//! file, so a figure resting on it would rest on a guess.

use std::collections::{BTreeMap, BTreeSet};

use rift_recall_ledger::candidates::{recorded_reads, LoadedCandidate};
use rift_recall_substrate::parse::MAX_ARTIFACT_BYTES;
use rift_recall_substrate::worktree::{CurrentFile, CurrentRefusal};
use rift_recall_substrate::{current_file_of, SubstrateReader};
use rift_recall_validity::SeenStatus;
use rift_store::CanonicalStore;
use serde_json::{json, Value};

use super::sighting;
use crate::RecallContext;

/// The payload tag on an entry the caller stated and that is current, but
/// that this family never recorded reading: the answer still says the file is
/// unchanged, because its bytes did not move, and only the credit is withheld.
pub(crate) const STATED_UNRECORDED: &str = "stated_unrecorded";

/// The largest file an answer hashes to learn what it is now. The answer needs
/// only the hash, and the capture already refuses artifacts past this size.
const CURRENT_MAX_BYTES: u64 = MAX_ARTIFACT_BYTES as u64;

/// What the checkout says one path is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Current {
    /// The plain SHA-256 of the bytes on disk.
    Verified(String),
    /// The bytes on disk could not be read, and why.
    Unverified(CurrentRefusal),
}

/// The record and the checkout, for the paths one call makes claims about.
pub(crate) struct Evidence {
    /// `(path, content_sha256)` of every stated revision this family read.
    recorded: BTreeSet<(String, String)>,
    /// What the checkout holds at each path a claim could be made about.
    current: BTreeMap<String, Current>,
    /// Whether the reader is bound to a checkout at all.
    checkout: bool,
}

impl Current {
    /// Ask the reader's checkout what `path` is now.
    pub(crate) fn of<R: SubstrateReader>(reader: &R, path: &str) -> Self {
        match current_file_of(reader, path, CURRENT_MAX_BYTES) {
            CurrentFile::Present { content_sha256, .. } => Self::Verified(content_sha256),
            CurrentFile::Refused(why) => Self::Unverified(why),
        }
    }
}

impl Evidence {
    /// The evidence for a candidate set: one recorded-read query for the
    /// stated hashes, and one read of each path a claim could be made about —
    /// a candidate the caller stated, or one of a file whose older revision
    /// the caller stated. Nothing is asked when nothing was stated or no
    /// checkout is bound.
    ///
    /// A store that cannot answer credits nothing rather than failing the
    /// serve: the answer the caller asked for does not depend on the figure,
    /// and a figure Rift cannot ground is one it does not publish.
    pub(crate) async fn over<S: CanonicalStore, R: SubstrateReader>(
        ctx: &RecallContext<'_, S, R>,
        known: &BTreeSet<&str>,
        loaded: &[LoadedCandidate],
    ) -> Self {
        let checkout = ctx.reader.worktree_root().is_some();
        if known.is_empty() || !checkout {
            return Self::empty(checkout);
        }
        let recorded = recorded_reads(ctx.store, known).await.unwrap_or_default();
        let claimed: BTreeSet<&str> = loaded
            .iter()
            .filter(|lc| {
                known.contains(lc.content_sha256.as_str())
                    || recorded.iter().any(|(path, _)| *path == lc.candidate.path)
            })
            .map(|lc| lc.candidate.path.as_str())
            .collect();
        let current = claimed
            .into_iter()
            .map(|path| (path.to_owned(), Current::of(ctx.reader, path)))
            .collect();
        Self {
            recorded,
            current,
            checkout,
        }
    }

    /// The evidence for one named path: what the checkout holds there, read
    /// whether or not anything was stated, and the stated revisions read.
    pub(crate) async fn at_path<S: CanonicalStore, R: SubstrateReader>(
        ctx: &RecallContext<'_, S, R>,
        known: &BTreeSet<&str>,
        path: &str,
    ) -> Self {
        let checkout = ctx.reader.worktree_root().is_some();
        let recorded = if known.is_empty() {
            BTreeSet::new()
        } else {
            recorded_reads(ctx.store, known).await.unwrap_or_default()
        };
        let now = Current::of(ctx.reader, path);
        Self {
            recorded,
            current: BTreeMap::from([(path.to_owned(), now)]),
            checkout,
        }
    }

    fn empty(checkout: bool) -> Self {
        Self {
            recorded: BTreeSet::new(),
            current: BTreeMap::new(),
            checkout,
        }
    }

    /// What the checkout holds at `path`, when it was asked.
    pub(crate) fn current(&self, path: &str) -> Option<&Current> {
        self.current.get(path)
    }

    /// Whether this family recorded reading revision `hash` of `path`.
    pub(crate) fn is_recorded(&self, path: &str, hash: &str) -> bool {
        self.recorded
            .iter()
            .any(|(p, h)| p.as_str() == path && h.as_str() == hash)
    }

    /// Whether the answer says the caller holds this candidate unchanged: it
    /// was stated, and the checkout holds those very bytes. With no checkout
    /// the answer is the projection's view, which is the statement alone.
    pub(crate) fn holds(&self, known: &BTreeSet<&str>, lc: &LoadedCandidate) -> bool {
        let hash = lc.content_sha256.as_str();
        if !known.contains(hash) {
            return false;
        }
        if !self.checkout {
            return true;
        }
        self.is_current(lc)
    }

    /// The sealed constructors' input: held only where [`Self::holds`] says.
    pub(crate) fn sighting(
        &self,
        known: &BTreeSet<&str>,
        seen_turns: &BTreeMap<(String, String), u64>,
        lc: &LoadedCandidate,
    ) -> SeenStatus {
        if !self.holds(known, lc) {
            return SeenStatus::NotSeenOrChanged;
        }
        sighting(known, seen_turns, &lc.candidate.path, &lc.content_sha256)
    }

    /// The baseline of an answer that returns no lines: the cost of every
    /// candidate the caller stated, that the checkout holds unchanged, and
    /// that this family recorded reading. A changed file books nothing here:
    /// the answer only says it moved, the caller still has to read it, and the
    /// pull that serves its changed lines is the answer that spares the read.
    pub(crate) fn list_baseline(&self, known: &BTreeSet<&str>, loaded: &[LoadedCandidate]) -> u32 {
        loaded
            .iter()
            .map(|lc| self.unchanged_credit(known, lc))
            .fold(0u32, u32::saturating_add)
    }

    /// The baseline of a pack, which carries changed lines: the list's
    /// baseline, plus the current cost of every changed file whose older
    /// revision the caller stated and this family recorded reading. The
    /// changed lines the pack carries are in its payload, so the seam's one
    /// subtraction takes them off.
    pub(crate) fn pack_baseline(&self, known: &BTreeSet<&str>, loaded: &[LoadedCandidate]) -> u32 {
        loaded
            .iter()
            .map(|lc| {
                self.unchanged_credit(known, lc)
                    .saturating_add(self.changed_credit(known, lc))
            })
            .fold(0u32, u32::saturating_add)
    }

    /// One candidate's credit as unchanged: stated, current, recorded.
    pub(crate) fn unchanged_credit(&self, known: &BTreeSet<&str>, lc: &LoadedCandidate) -> u32 {
        let credited = self.checkout
            && self.holds(known, lc)
            && self.is_recorded(&lc.candidate.path, &lc.content_sha256);
        if credited {
            return lc.candidate.est_tokens;
        }
        0
    }

    /// One candidate's credit as a changed file: the checkout holds it, the
    /// caller did not state it, and the caller stated an older revision of the
    /// same file that this family recorded reading.
    fn changed_credit(&self, known: &BTreeSet<&str>, lc: &LoadedCandidate) -> u32 {
        let path = lc.candidate.path.as_str();
        let held_older = self.recorded.iter().any(|(p, h)| {
            p.as_str() == path && *h != lc.content_sha256 && known.contains(h.as_str())
        });
        let credited = self.checkout
            && !known.contains(lc.content_sha256.as_str())
            && self.is_current(lc)
            && held_older;
        if credited {
            return lc.candidate.est_tokens;
        }
        0
    }

    /// Say on a stated entry what the checkout and the record found, when it
    /// is something the caller would otherwise misread: a current file never
    /// recorded as read, a file that moved on disk past every recorded
    /// revision, or a file whose bytes could not be read. Nothing is added to
    /// an entry the caller did not state, or when no checkout is bound.
    pub(crate) fn tag(&self, known: &BTreeSet<&str>, lc: &LoadedCandidate, entry: &mut Value) {
        let hash = lc.content_sha256.as_str();
        if !self.checkout || !known.contains(hash) {
            return;
        }
        let (key, value) = match self.current(&lc.candidate.path) {
            Some(Current::Verified(now)) if now == hash => {
                if self.is_recorded(&lc.candidate.path, hash) {
                    return;
                }
                ("avoided_basis", json!(STATED_UNRECORDED))
            }
            Some(Current::Verified(now)) => ("worktree_sha256", json!(now)),
            Some(Current::Unverified(why)) => ("unverified", json!(why.tag())),
            None => return,
        };
        let Some(obj) = entry.as_object_mut() else {
            return;
        };
        obj.insert(key.to_owned(), value);
    }

    fn is_current(&self, lc: &LoadedCandidate) -> bool {
        matches!(
            self.current(&lc.candidate.path),
            Some(Current::Verified(now)) if *now == lc.content_sha256
        )
    }
}

/// Whether this family recorded reading revision `hash` of `path` — the one
/// question a pull against a held revision turns on, asked once. A store that
/// cannot answer is a read nobody can show, so it credits nothing.
pub(crate) async fn recorded_read<S: CanonicalStore, R: SubstrateReader>(
    ctx: &RecallContext<'_, S, R>,
    path: &str,
    hash: &str,
) -> bool {
    let stated = BTreeSet::from([hash]);
    let Ok(recorded) = recorded_reads(ctx.store, &stated).await else {
        return false;
    };
    recorded.contains(&(path.to_owned(), hash.to_owned()))
}

#[cfg(test)]
impl Evidence {
    /// Evidence built by hand, for the property over arbitrary evidence.
    pub(crate) fn from_parts(
        recorded: BTreeSet<(String, String)>,
        current: BTreeMap<String, Current>,
    ) -> Self {
        Self {
            recorded,
            current,
            checkout: true,
        }
    }
}

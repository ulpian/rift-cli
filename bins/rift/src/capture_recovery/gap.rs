//! The published Cut as one checked core gap batch: every pinned entry's safe
//! facts through the core ticket constructor, the Cut file's digest as the
//! evidence, and one private record that fixes the batch's recorded time the
//! first time and hands it back on every retry.
//!
//! The batch asserts missing observations, never historical authorship: no
//! raw ticket byte, source name or inode enters it. Nothing here picks a
//! carrier, assembles an artifact, archives, retires or releases a hold; an
//! intent with no historical member has no batch at all, never an empty one.
#[cfg(test)]
mod tests;

use rift_core::boundary_gap::{
    BoundaryEdge, ContextDigest, EvidenceDigest, GapBatch, GapId, GapTicket, GapTicketFields,
    MemberKey, SelectionDigest, SessionKey, SourceKey, TicketDigest,
};
use rift_core::RepoId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::cut::{CutManifest, CutOutcome, Pinned, PinnedEntry, CUT_NAME, MAX_CUT_BYTES};
use super::hold::{OwnHold, ReadyQueue};
use super::record::{self, Publication, RecordError};
use super::selection::intent::{encode_part, hex64};
use super::{plan, WireError, WireResult};

pub(crate) const GAP_NAME: &str = "gap.json";
/// One fixed-shape record of digests and two integers.
pub(crate) const MAX_GAP_RECORD_BYTES: usize = 4096;

/// Why no batch is published, as a static class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GapOwed {
    HoldUnavailable,
    QueueUnavailable,
    /// The Cut on disk is absent, unreadable or not a Cut.
    CutUnavailable,
    /// The Cut is not the published digest or not this operation's.
    CutMismatch,
    /// An entry's facts are refused by the core ticket constructor.
    TicketInvalid,
    /// The core batch constructor refused the membership or the time.
    BatchInvalid,
    /// The record there is not a record, or its identities do not verify.
    RecordInvalid,
    /// The record there is bound to another Plan, intent or Cut.
    RecordConflict,
    PublishFailed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GapOutcome {
    /// This attempt chose the recorded time and wrote the record.
    Published(GapBatch),
    /// The record was already there; the batch is rebuilt with its time.
    Resumed(GapBatch),
    /// The intent had no historical member: there is no batch.
    NoHistoricalCut,
}

/// The private record that binds a Cut to the one batch derived from it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GapRecord {
    pub(crate) schema_version: u32,
    pub(crate) operation_id: String,
    pub(crate) plan_sha256: String,
    pub(crate) intent_sha256: String,
    pub(crate) cut_sha256: String,
    pub(crate) recorded_at: i64,
    pub(crate) gap_id: String,
    pub(crate) selection_sha256: String,
    pub(crate) ticket_count: u32,
}

impl GapRecord {
    fn of(manifest: &CutManifest, cut_sha256: &str, batch: &GapBatch) -> Self {
        Self {
            schema_version: 1,
            operation_id: manifest.operation_id.clone(),
            plan_sha256: manifest.plan_sha256.clone(),
            intent_sha256: manifest.intent_sha256.clone(),
            cut_sha256: cut_sha256.to_owned(),
            recorded_at: batch.recorded_at(),
            gap_id: batch.gap_id().to_hex(),
            selection_sha256: batch.selection_digest().to_hex(),
            ticket_count: batch.ticket_count(),
        }
    }

    pub(crate) fn encode(&self) -> WireResult<Vec<u8>> {
        self.check()?;
        let bytes = encode_part(self)?;
        if bytes.len() > MAX_GAP_RECORD_BYTES {
            return Err(WireError);
        }
        Ok(bytes)
    }

    pub(crate) fn decode(bytes: &[u8]) -> WireResult<Self> {
        if bytes.len() > MAX_GAP_RECORD_BYTES {
            return Err(WireError);
        }
        let value: Value =
            crate::capture_queue::delivery::decode_unique_value(bytes).map_err(|_| WireError)?;
        let record: Self = serde_json::from_value(value).map_err(|_| WireError)?;
        record.check()?;
        Ok(record)
    }

    fn check(&self) -> WireResult<()> {
        if self.schema_version != 1 || self.recorded_at < 0 {
            return Err(WireError);
        }
        plan::operation(&self.operation_id)?;
        for digest in [
            &self.plan_sha256,
            &self.intent_sha256,
            &self.cut_sha256,
            &self.gap_id,
            &self.selection_sha256,
        ] {
            hex64(digest)?;
        }
        Ok(())
    }

    /// The same operation, Plan, intent and Cut as the manifest in hand.
    fn binds(&self, manifest: &CutManifest, cut_sha256: &str) -> bool {
        self.operation_id == manifest.operation_id
            && self.plan_sha256 == manifest.plan_sha256
            && self.intent_sha256 == manifest.intent_sha256
            && self.cut_sha256 == cut_sha256
    }

    /// The stored identities and count are the batch rebuilt from the Cut.
    fn verify(&self, batch: &GapBatch) -> Result<(), GapOwed> {
        let gap_id = GapId::parse_hex(&self.gap_id).map_err(|_| GapOwed::RecordInvalid)?;
        let selection = SelectionDigest::parse_hex(&self.selection_sha256)
            .map_err(|_| GapOwed::RecordInvalid)?;
        batch
            .verify_stored(gap_id, selection, self.ticket_count)
            .map_err(|_| GapOwed::RecordInvalid)
    }
}

/// One Cut entry's safe facts through the single checked ticket constructor.
fn ticket_of(entry: &PinnedEntry) -> Result<GapTicket, GapOwed> {
    let known = &entry.known_boundary;
    let invalid = |_| GapOwed::TicketInvalid;
    GapTicket::new(GapTicketFields {
        source_key: SourceKey::parse_hex(&entry.source_key).map_err(invalid)?,
        context_digest: ContextDigest::parse_hex(&entry.context_sha256).map_err(invalid)?,
        ticket_digest: TicketDigest::parse_hex(&entry.ticket_sha256).map_err(invalid)?,
        session_key: SessionKey::parse_hex(&known.session_key).map_err(invalid)?,
        turn_ordinal: known.turn_ordinal,
        boundary: BoundaryEdge::parse(&known.boundary).map_err(invalid)?,
        head_oid: known.head_oid.clone(),
        captured_at: known.captured_at,
        queued_at: known.queued_at,
        attempts: entry.attempts,
    })
    .map_err(invalid)
}

/// The core batch of a Cut: its repository and member, its file digest as
/// the evidence, every entry a checked ticket, at the given recorded time.
pub(crate) fn batch_of(
    manifest: &CutManifest,
    cut_sha256: &str,
    recorded_at: i64,
) -> Result<GapBatch, GapOwed> {
    let evidence = EvidenceDigest::parse_hex(cut_sha256).map_err(|_| GapOwed::CutMismatch)?;
    let repo_id = RepoId::new(manifest.repo_id.clone()).map_err(|_| GapOwed::CutMismatch)?;
    let member = MemberKey::parse_hex(&manifest.member_key).map_err(|_| GapOwed::CutMismatch)?;
    let tickets = manifest
        .entries
        .iter()
        .map(ticket_of)
        .collect::<Result<Vec<_>, _>>()?;
    let batch = GapBatch::new(repo_id, member, evidence, recorded_at, tickets)
        .map_err(|_| GapOwed::BatchInvalid)?;
    if batch.ticket_count() != manifest.selected_count {
        return Err(GapOwed::BatchInvalid);
    }
    Ok(batch)
}

/// The Cut on disk, read whole from the private directory, hashing to the
/// published digest and bound to this hold's Plan and the pinned intent.
fn read_cut(
    queue: &ReadyQueue<'_>,
    pinned: &Pinned,
    cut_sha256: &str,
) -> Result<CutManifest, GapOwed> {
    let private = queue.private().map_err(|_| GapOwed::HoldUnavailable)?;
    let bytes = record::read_private_record(private, CUT_NAME, MAX_CUT_BYTES)
        .map_err(|_| GapOwed::CutUnavailable)?;
    if format!("{:x}", Sha256::digest(&bytes)) != cut_sha256 {
        return Err(GapOwed::CutMismatch);
    }
    let manifest = CutManifest::decode(&bytes).map_err(|_| GapOwed::CutUnavailable)?;
    let plan = queue.plan();
    let plan_sha256 = plan.sha256().map_err(|_| GapOwed::HoldUnavailable)?;
    if manifest.operation_id != plan.operation()
        || manifest.plan_sha256 != plan_sha256.as_str()
        || manifest.member_key != plan.member().to_hex()
        || manifest.repo_id != plan.repo_id().as_str()
        || manifest.intent_sha256 != pinned.intent_sha256
    {
        return Err(GapOwed::CutMismatch);
    }
    Ok(manifest)
}

/// The record already there, if any: bound to this Cut and verifying
/// against the batch rebuilt with its own recorded time.
fn resume(
    queue: &ReadyQueue<'_>,
    manifest: &CutManifest,
    cut_sha256: &str,
) -> Result<Option<GapBatch>, GapOwed> {
    let private = queue.private().map_err(|_| GapOwed::HoldUnavailable)?;
    let bytes = match record::read_private_record(private, GAP_NAME, MAX_GAP_RECORD_BYTES) {
        Ok(bytes) => bytes,
        Err(RecordError::Absent) => return Ok(None),
        Err(RecordError::Unavailable) => return Err(GapOwed::HoldUnavailable),
        Err(_) => return Err(GapOwed::RecordInvalid),
    };
    let stored = GapRecord::decode(&bytes).map_err(|_| GapOwed::RecordInvalid)?;
    if !stored.binds(manifest, cut_sha256) {
        return Err(GapOwed::RecordConflict);
    }
    let batch = batch_of(manifest, cut_sha256, stored.recorded_at)?;
    stored.verify(&batch)?;
    Ok(Some(batch))
}

/// Derive the one core batch of the published Cut and bind it privately.
/// The clock is read only when no record exists yet; a retry hands back the
/// batch with the first recorded time, and a record bound to anything else
/// refuses and is never overwritten.
pub(crate) fn publish_gap(
    hold: &OwnHold<'_>,
    pinned: &Pinned,
    now: impl FnOnce() -> i64,
) -> Result<GapOutcome, GapOwed> {
    let cut_sha256 = match &pinned.cut {
        CutOutcome::NoHistoricalCut => return Ok(GapOutcome::NoHistoricalCut),
        CutOutcome::Published { cut_sha256 } | CutOutcome::Resumed { cut_sha256 } => cut_sha256,
    };
    hold.check_current().map_err(|_| GapOwed::HoldUnavailable)?;
    let queue = hold
        .agent_queue_namespace()
        .map_err(|_| GapOwed::QueueUnavailable)?;
    let manifest = read_cut(&queue, pinned, cut_sha256)?;
    if let Some(batch) = resume(&queue, &manifest, cut_sha256)? {
        return Ok(GapOutcome::Resumed(batch));
    }
    let batch = batch_of(&manifest, cut_sha256, now())?;
    let bytes = GapRecord::of(&manifest, cut_sha256, &batch)
        .encode()
        .map_err(|_| GapOwed::BatchInvalid)?;
    let private = queue.private().map_err(|_| GapOwed::HoldUnavailable)?;
    let publication = record::publish_private_record(
        private,
        GAP_NAME,
        MAX_GAP_RECORD_BYTES,
        &bytes,
        &mut |_| Ok(()),
    )
    .map_err(|error| match error {
        RecordError::Unavailable => GapOwed::HoldUnavailable,
        RecordError::Conflict => GapOwed::RecordConflict,
        RecordError::Failed | RecordError::Resource | RecordError::Absent => GapOwed::PublishFailed,
    })?;
    Ok(match publication {
        Publication::Fresh => GapOutcome::Published(batch),
        Publication::Resumed => GapOutcome::Resumed(batch),
    })
}

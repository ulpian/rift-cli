//! Intent resume and the publication receipt. After a crash an intent on disk
//! is the only pointer to what was delegated; the resume authenticates the
//! native tree it names and records the publication once, immutably.
//!
//! The resume takes a reader, never a backend, so it cannot delegate a second
//! archive: an intent whose checkpoint is absent stays owed, typed, for the
//! later retirement stage to decide. Nothing here retires the intent, writes
//! progress or a marker, or consumes the carrier's ticket.
#[cfg(test)]
pub(super) mod tests;

use rift_recall_substrate::{Dialect, SubstrateReader};
use rift_shadow::backend::native_read::load_native_boundary_gaps;
use rift_shadow::backend::BOUNDARY_GAPS_NAME;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::{ArchiveIntent, INTENT_NAME, MAX_INTENT_BYTES};
use crate::capture_recovery::files::Namespace;
use crate::capture_recovery::hold::{OwnHold, ReadyQueue};
use crate::capture_recovery::record::{self, RecordError};
use crate::capture_recovery::selection::intent::{encode_part, hex64};
use crate::capture_recovery::{plan, WireError, WireResult};

pub(crate) const RECEIPT_NAME: &str = "publication-receipt.json";
/// Digests, one gap id and a path: the intent's shape, never larger.
pub(crate) const MAX_RECEIPT_BYTES: usize = 4096;

/// Why the resume did not complete, as a static class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResumeOwed {
    HoldUnavailable,
    /// No intent is on disk: nothing was ever delegated.
    NoIntent,
    IntentInvalid,
    /// The intent was written under another plan.
    PlanMismatch,
    NativeUnavailable,
    /// The native tree holds no checkpoint under the intent's id.
    NotArchived,
    /// The archived gap artifact is not the intent's frozen one.
    GapMismatch,
    /// The checkpoint's root, binding or origin does not authenticate.
    NativeInvalid,
    /// Another receipt is already there; it is never overwritten.
    ReceiptConflict,
    PublishFailed,
}

/// The immutable proof that the intent's archive is in the native tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicationReceipt {
    pub(crate) schema_version: u32,
    pub(crate) operation_id: String,
    pub(crate) plan_sha256: String,
    pub(crate) intent_sha256: String,
    pub(crate) checkpoint_id: String,
    pub(crate) shard_path: String,
    pub(crate) gap_ids: Vec<String>,
    pub(crate) gap_artifact_sha256: String,
    pub(crate) carrier_session_key: String,
}

impl PublicationReceipt {
    fn of(intent: &ArchiveIntent, intent_bytes: &[u8]) -> Self {
        Self {
            schema_version: 1,
            operation_id: intent.operation_id.clone(),
            plan_sha256: intent.plan_sha256.clone(),
            intent_sha256: format!("{:x}", Sha256::digest(intent_bytes)),
            checkpoint_id: intent.checkpoint_id.clone(),
            shard_path: intent.shard_path.clone(),
            gap_ids: intent.gap_ids.clone(),
            gap_artifact_sha256: intent.gap_artifact_sha256.clone(),
            carrier_session_key: intent.carrier_session_key.clone(),
        }
    }

    pub(crate) fn encode(&self) -> WireResult<Vec<u8>> {
        self.check()?;
        let bytes = encode_part(self)?;
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(WireError);
        }
        Ok(bytes)
    }

    pub(crate) fn decode(bytes: &[u8]) -> WireResult<Self> {
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(WireError);
        }
        let value: Value =
            crate::capture_queue::delivery::decode_unique_value(bytes).map_err(|_| WireError)?;
        let receipt: Self = serde_json::from_value(value).map_err(|_| WireError)?;
        receipt.check()?;
        Ok(receipt)
    }

    fn check(&self) -> WireResult<()> {
        if self.schema_version != 1 || self.gap_ids.len() != 1 {
            return Err(WireError);
        }
        plan::operation(&self.operation_id)?;
        let digests = [
            &self.plan_sha256,
            &self.intent_sha256,
            &self.checkpoint_id,
            &self.gap_artifact_sha256,
            &self.carrier_session_key,
        ];
        for digest in digests.into_iter().chain(&self.gap_ids) {
            hex64(digest)?;
        }
        let (Some(hi), Some(rest)) = (self.checkpoint_id.get(..2), self.checkpoint_id.get(2..))
        else {
            return Err(WireError);
        };
        if self.shard_path != format!("{hi}/{rest}") {
            return Err(WireError);
        }
        Ok(())
    }
}

/// Authenticate the native tree the durable intent names, then publish the
/// receipt no-clobber; an identical receipt already there completes again.
pub(crate) fn resume_intent(
    hold: &OwnHold<'_>,
    reader: &impl SubstrateReader,
) -> Result<PublicationReceipt, ResumeOwed> {
    hold.check_current()
        .map_err(|_| ResumeOwed::HoldUnavailable)?;
    let queue = hold
        .agent_queue_namespace()
        .map_err(|_| ResumeOwed::HoldUnavailable)?;
    let private = queue.private().map_err(|_| ResumeOwed::HoldUnavailable)?;
    let bytes = match record::read_private_record(private, INTENT_NAME, MAX_INTENT_BYTES) {
        Ok(bytes) => bytes,
        Err(RecordError::Absent) => return Err(ResumeOwed::NoIntent),
        Err(RecordError::Unavailable) => return Err(ResumeOwed::HoldUnavailable),
        Err(_) => return Err(ResumeOwed::IntentInvalid),
    };
    let intent = ArchiveIntent::decode(&bytes).map_err(|_| ResumeOwed::IntentInvalid)?;
    check_plan(&queue, &intent)?;
    authenticate(reader, &intent)?;
    let receipt = PublicationReceipt::of(&intent, &bytes);
    publish_receipt(private, &receipt)?;
    Ok(receipt)
}

/// The receipt on disk, if any; later stages key on it.
pub(crate) fn read_receipt(hold: &OwnHold<'_>) -> Result<Option<PublicationReceipt>, ResumeOwed> {
    let queue = hold
        .agent_queue_namespace()
        .map_err(|_| ResumeOwed::HoldUnavailable)?;
    let private = queue.private().map_err(|_| ResumeOwed::HoldUnavailable)?;
    match record::read_private_record(private, RECEIPT_NAME, MAX_RECEIPT_BYTES) {
        Ok(bytes) => PublicationReceipt::decode(&bytes)
            .map(Some)
            .map_err(|_| ResumeOwed::ReceiptConflict),
        Err(RecordError::Absent) => Ok(None),
        Err(RecordError::Unavailable) => Err(ResumeOwed::HoldUnavailable),
        Err(_) => Err(ResumeOwed::ReceiptConflict),
    }
}

fn check_plan(queue: &ReadyQueue<'_>, intent: &ArchiveIntent) -> Result<(), ResumeOwed> {
    let plan = queue.plan();
    let digest = plan.sha256().map_err(|_| ResumeOwed::HoldUnavailable)?;
    if plan.operation() != intent.operation_id || digest.as_str() != intent.plan_sha256 {
        return Err(ResumeOwed::PlanMismatch);
    }
    Ok(())
}

/// The checkpoint exists natively, carries exactly the intent's frozen gap
/// artifact (by digest and by its one envelope's ids) and authenticates by
/// root and origin through the one native loader.
fn authenticate(reader: &impl SubstrateReader, intent: &ArchiveIntent) -> Result<(), ResumeOwed> {
    let shards = reader
        .checkpoints()
        .map_err(|_| ResumeOwed::NativeUnavailable)?;
    let Some(shard) = shards.into_iter().find(|shard| {
        shard.dialect == Dialect::Rift && shard.checkpoint_id == intent.checkpoint_id
    }) else {
        return Err(ResumeOwed::NotArchived);
    };
    let gap = reader
        .read_native_artifact(&shard.checkpoint_id, BOUNDARY_GAPS_NAME)
        .map_err(|_| ResumeOwed::NativeUnavailable)?
        .ok_or(ResumeOwed::GapMismatch)?;
    if format!("{:x}", Sha256::digest(&gap)) != intent.gap_artifact_sha256 {
        return Err(ResumeOwed::GapMismatch);
    }
    let envelopes =
        rift_core::boundary_gap::decode_checked(&gap).map_err(|_| ResumeOwed::GapMismatch)?;
    let [envelope] = envelopes.as_slice() else {
        return Err(ResumeOwed::GapMismatch);
    };
    let batch = envelope.batch();
    if envelope.carrier_session_key().to_hex() != intent.carrier_session_key
        || [batch.gap_id().to_hex()].as_slice() != intent.gap_ids.as_slice()
        || batch.evidence_digest().to_hex() != intent.cut_sha256
    {
        return Err(ResumeOwed::GapMismatch);
    }
    load_native_boundary_gaps(reader, &shard, &gap).map_err(|_| ResumeOwed::NativeInvalid)?;
    Ok(())
}

fn publish_receipt(private: &Namespace, receipt: &PublicationReceipt) -> Result<(), ResumeOwed> {
    let bytes = receipt.encode().map_err(|_| ResumeOwed::IntentInvalid)?;
    record::publish_private_record(
        private,
        RECEIPT_NAME,
        MAX_RECEIPT_BYTES,
        &bytes,
        &mut |_| Ok(()),
    )
    .map_err(|error| match error {
        RecordError::Unavailable => ResumeOwed::HoldUnavailable,
        RecordError::Conflict => ResumeOwed::ReceiptConflict,
        RecordError::Failed | RecordError::Resource | RecordError::Absent => {
            ResumeOwed::PublishFailed
        }
    })?;
    Ok(())
}

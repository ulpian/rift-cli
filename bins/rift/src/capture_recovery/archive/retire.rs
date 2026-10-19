//! The retirement intent and the per-slot retirement report. Once a
//! publication receipt proves the recovery archived, the queue entries it
//! discharges are named durably, no-clobber, before any queue file is
//! touched, so a crash mid-retirement resumes from the same names instead of
//! re-deriving them from a queue that has already changed.
//!
//! The slots come from the Cut the published gap was derived from, bound
//! through the gap record to the receipt's one gap id; nothing is inferred
//! from what the queue holds now. The report only reads: it reopens each pin
//! and classifies the original under its name. Nothing here unlinks, moves or
//! releases anything.
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::resume::{PublicationReceipt, MAX_RECEIPT_BYTES, RECEIPT_NAME};
use crate::capture_recovery::cut::{CutManifest, PinnedEntry, CUT_NAME, MAX_CUT_BYTES};
use crate::capture_recovery::files::Namespace;
use crate::capture_recovery::gap::{GapRecord, GAP_NAME, MAX_GAP_RECORD_BYTES};
use crate::capture_recovery::hold::{OwnHold, ReadyQueue};
use crate::capture_recovery::pins::{self, DeliveryMatch, SharedScope};
use crate::capture_recovery::record::{self, Publication, RecordError};
use crate::capture_recovery::selection::intent::{encode_part, hex64};
use crate::capture_recovery::{codec, plan, WireError, WireResult};

pub(crate) const RETIRE_NAME: &str = "retire-intent.json";
/// A slot is a number, two basenames and a digest, so the Cut's own ceiling
/// bounds the whole intent.
const MAX_RETIRE_BYTES: usize = MAX_CUT_BYTES;

/// Why no retirement intent or report was produced, as a static class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetireOwed {
    HoldUnavailable,
    /// No publication receipt: nothing is proven archived, so nothing is discharged.
    NoReceipt,
    ReceiptInvalid,
    /// A record was written under another plan.
    PlanMismatch,
    /// The gap record is absent, malformed or not the receipt's gap.
    GapMismatch,
    /// The Cut is absent, malformed or not the one the gap was derived from.
    CutMismatch,
    /// No retirement intent is on disk.
    NoIntent,
    IntentInvalid,
    /// Another retirement intent is already there; it is never overwritten.
    IntentConflict,
    PublishFailed,
    ScopeUnavailable,
}

/// The closed record of the queue entries the published recovery discharges.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetireIntent {
    pub(crate) schema_version: u32,
    pub(crate) operation_id: String,
    pub(crate) plan_sha256: String,
    pub(crate) receipt_sha256: String,
    pub(crate) gap_id: String,
    pub(crate) cut_sha256: String,
    pub(crate) slots: Vec<RetireSlot>,
}

/// One pinned original: its slot, both names and the whole-ticket digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetireSlot {
    pub(crate) pin_slot: u32,
    pub(crate) source_name: String,
    pub(crate) pin_name: String,
    pub(crate) ticket_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RetireOutcome {
    Published(RetireIntent),
    /// The identical intent was already durable; nothing was rewritten.
    Resumed(RetireIntent),
}

/// Why a slot is still owed retirement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kept {
    /// The original is still queued under its name.
    OriginalQueued,
    /// Another file sits under the original's name; it is never removed.
    NameReused,
    /// The private pin is absent, so nothing proves what the slot held.
    PinMissing,
    /// The pin is present but no longer verifies against the Cut.
    PinUnverified,
    /// The queue could not be read cleanly.
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SlotState {
    /// The original is gone from the queue and its pin still proves its bytes.
    Retired,
    Kept(Kept),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SlotReport {
    pub(crate) pin_slot: u32,
    pub(crate) state: SlotState,
}

impl RetireSlot {
    fn of(entry: &PinnedEntry) -> Self {
        Self {
            pin_slot: entry.pin_slot,
            source_name: entry.source_name.clone(),
            pin_name: entry.pin_name.clone(),
            ticket_sha256: entry.ticket_sha256.clone(),
        }
    }
}

impl RetireIntent {
    pub(crate) fn encode(&self) -> WireResult<Vec<u8>> {
        self.check()?;
        let bytes = encode_part(self)?;
        if bytes.len() > MAX_RETIRE_BYTES {
            return Err(WireError);
        }
        Ok(bytes)
    }

    pub(crate) fn decode(bytes: &[u8]) -> WireResult<Self> {
        if bytes.len() > MAX_RETIRE_BYTES {
            return Err(WireError);
        }
        let value: Value =
            crate::capture_queue::delivery::decode_unique_value(bytes).map_err(|_| WireError)?;
        let intent: Self = serde_json::from_value(value).map_err(|_| WireError)?;
        intent.check()?;
        Ok(intent)
    }

    /// Closed invariants: digests are digests, names are bare basenames, and
    /// no slot or name is named twice.
    fn check(&self) -> WireResult<()> {
        if self.schema_version != 1 || self.slots.is_empty() {
            return Err(WireError);
        }
        plan::operation(&self.operation_id)?;
        for digest in [
            &self.plan_sha256,
            &self.receipt_sha256,
            &self.gap_id,
            &self.cut_sha256,
        ] {
            hex64(digest)?;
        }
        let mut slots = BTreeSet::new();
        let mut names = BTreeSet::new();
        for slot in &self.slots {
            hex64(&slot.ticket_sha256)?;
            for name in [&slot.source_name, &slot.pin_name] {
                codec::basename(OsStr::new(name)).map_err(|_| WireError)?;
                if !names.insert(name.as_str()) {
                    return Err(WireError);
                }
            }
            if !slots.insert(slot.pin_slot) {
                return Err(WireError);
            }
        }
        Ok(())
    }
}

/// Name the slots the receipt's recovery discharges, durably and no-clobber;
/// an identical intent already there resumes, another refuses.
pub(crate) fn write_retire_intent(hold: &OwnHold<'_>) -> Result<RetireOutcome, RetireOwed> {
    hold.check_current()
        .map_err(|_| RetireOwed::HoldUnavailable)?;
    let queue = hold
        .agent_queue_namespace()
        .map_err(|_| RetireOwed::HoldUnavailable)?;
    let private = queue.private().map_err(|_| RetireOwed::HoldUnavailable)?;
    let receipt_bytes = read(
        private,
        RECEIPT_NAME,
        MAX_RECEIPT_BYTES,
        RetireOwed::NoReceipt,
        RetireOwed::ReceiptInvalid,
    )?;
    let receipt =
        PublicationReceipt::decode(&receipt_bytes).map_err(|_| RetireOwed::ReceiptInvalid)?;
    bound(&queue, &receipt.operation_id, &receipt.plan_sha256)?;
    let (gap, cut) = discharged(&queue, private, &receipt)?;
    let intent = RetireIntent {
        schema_version: 1,
        operation_id: receipt.operation_id,
        plan_sha256: receipt.plan_sha256,
        receipt_sha256: format!("{:x}", Sha256::digest(&receipt_bytes)),
        gap_id: gap.gap_id,
        cut_sha256: gap.cut_sha256,
        slots: cut.entries.iter().map(RetireSlot::of).collect(),
    };
    publish(private, intent)
}

/// Each slot of the durable intent, read-only: retired when its original is
/// gone and its pin still verifies, otherwise kept with the typed reason.
pub(crate) fn retirement_report(hold: &OwnHold<'_>) -> Result<Vec<SlotReport>, RetireOwed> {
    hold.check_current()
        .map_err(|_| RetireOwed::HoldUnavailable)?;
    let queue = hold
        .agent_queue_namespace()
        .map_err(|_| RetireOwed::HoldUnavailable)?;
    let private = queue.private().map_err(|_| RetireOwed::HoldUnavailable)?;
    let bytes = read(
        private,
        RETIRE_NAME,
        MAX_RETIRE_BYTES,
        RetireOwed::NoIntent,
        RetireOwed::IntentInvalid,
    )?;
    let intent = RetireIntent::decode(&bytes).map_err(|_| RetireOwed::IntentInvalid)?;
    bound(&queue, &intent.operation_id, &intent.plan_sha256)?;
    let cut = read_cut(&queue, private, &intent.cut_sha256)?;
    if cut
        .entries
        .iter()
        .map(RetireSlot::of)
        .ne(intent.slots.iter().cloned())
    {
        return Err(RetireOwed::IntentInvalid);
    }
    let queue_dir = &queue.queue().path;
    let scope = pins::shared_scope(hold.owner(), queue_dir, &private.path)
        .map_err(|_| RetireOwed::ScopeUnavailable)?;
    let report = cut.entries.iter().map(|entry| SlotReport {
        pin_slot: entry.pin_slot,
        state: slot_state(hold, &scope, queue_dir, entry),
    });
    Ok(report.collect())
}

fn slot_state(
    hold: &OwnHold<'_>,
    scope: &SharedScope,
    queue_dir: &Path,
    entry: &PinnedEntry,
) -> SlotState {
    match scope.pin_present(&entry.pin_name) {
        Ok(true) => {}
        Ok(false) => return SlotState::Kept(Kept::PinMissing),
        Err(_) => return SlotState::Kept(Kept::Unavailable),
    }
    let Ok(live) = pins::reopen_expected(hold.owner(), scope, &entry.as_expectation()) else {
        return SlotState::Kept(Kept::PinUnverified);
    };
    match pins::classify(hold.owner(), &queue_dir.join(&entry.source_name), &live) {
        DeliveryMatch::Missing => SlotState::Retired,
        DeliveryMatch::OriginalPresent => SlotState::Kept(Kept::OriginalQueued),
        DeliveryMatch::Different => SlotState::Kept(Kept::NameReused),
        DeliveryMatch::Unavailable => SlotState::Kept(Kept::Unavailable),
    }
}

/// The gap record bound to the receipt's one gap id, and the Cut it was
/// derived from, both under this plan.
fn discharged(
    queue: &ReadyQueue<'_>,
    private: &Namespace,
    receipt: &PublicationReceipt,
) -> Result<(GapRecord, CutManifest), RetireOwed> {
    let bytes = read(
        private,
        GAP_NAME,
        MAX_GAP_RECORD_BYTES,
        RetireOwed::GapMismatch,
        RetireOwed::GapMismatch,
    )?;
    let gap = GapRecord::decode(&bytes).map_err(|_| RetireOwed::GapMismatch)?;
    bound(queue, &gap.operation_id, &gap.plan_sha256)?;
    if receipt.gap_ids.as_slice() != std::slice::from_ref(&gap.gap_id) {
        return Err(RetireOwed::GapMismatch);
    }
    let cut = read_cut(queue, private, &gap.cut_sha256)?;
    let count = usize::try_from(gap.ticket_count).map_err(|_| RetireOwed::GapMismatch)?;
    if cut.intent_sha256 != gap.intent_sha256 || cut.entries.len() != count {
        return Err(RetireOwed::GapMismatch);
    }
    Ok((gap, cut))
}

fn read_cut(
    queue: &ReadyQueue<'_>,
    private: &Namespace,
    cut_sha256: &str,
) -> Result<CutManifest, RetireOwed> {
    let bytes = read(
        private,
        CUT_NAME,
        MAX_CUT_BYTES,
        RetireOwed::CutMismatch,
        RetireOwed::CutMismatch,
    )?;
    if format!("{:x}", Sha256::digest(&bytes)) != cut_sha256 {
        return Err(RetireOwed::CutMismatch);
    }
    let cut = CutManifest::decode(&bytes).map_err(|_| RetireOwed::CutMismatch)?;
    bound(queue, &cut.operation_id, &cut.plan_sha256)?;
    Ok(cut)
}

fn bound(queue: &ReadyQueue<'_>, operation_id: &str, plan_sha256: &str) -> Result<(), RetireOwed> {
    let plan = queue.plan();
    let digest = plan.sha256().map_err(|_| RetireOwed::HoldUnavailable)?;
    if plan.operation() != operation_id || digest.as_str() != plan_sha256 {
        return Err(RetireOwed::PlanMismatch);
    }
    Ok(())
}

/// A private record's bytes: an absent name and anything else there that
/// cannot be read whole are the caller's two classes.
fn read(
    private: &Namespace,
    name: &str,
    max: usize,
    absent: RetireOwed,
    invalid: RetireOwed,
) -> Result<Vec<u8>, RetireOwed> {
    record::read_private_record(private, name, max).map_err(|error| match error {
        RecordError::Unavailable => RetireOwed::HoldUnavailable,
        RecordError::Absent => absent,
        RecordError::Conflict | RecordError::Failed | RecordError::Resource => invalid,
    })
}

fn publish(private: &Namespace, intent: RetireIntent) -> Result<RetireOutcome, RetireOwed> {
    let bytes = intent.encode().map_err(|_| RetireOwed::IntentInvalid)?;
    let publication =
        record::publish_private_record(private, RETIRE_NAME, MAX_RETIRE_BYTES, &bytes, &mut |_| {
            Ok(())
        })
        .map_err(|error| match error {
            RecordError::Unavailable => RetireOwed::HoldUnavailable,
            RecordError::Conflict => RetireOwed::IntentConflict,
            RecordError::Failed | RecordError::Resource | RecordError::Absent => {
                RetireOwed::PublishFailed
            }
        })?;
    Ok(match publication {
        Publication::Fresh => RetireOutcome::Published(intent),
        Publication::Resumed => RetireOutcome::Resumed(intent),
    })
}

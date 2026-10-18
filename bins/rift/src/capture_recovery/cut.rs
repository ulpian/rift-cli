//! One slot at a time from the durable intent to the immutable Cut: each
//! retained original is reread through its descriptor, pinned under its
//! expected name, validated and released before the next; a crash resumes by
//! reopening the pins that exist through the intent alone — never by
//! reselecting, never by re-pinning a missing one from stored numbers; and
//! only when every pin validates is the CutManifest published, no-clobber.
//!
//! Nothing here builds a core GapBatch, archives, retires, unlinks, releases a
//! hold or writes a cache: the result is the Cut (or the tagged
//! NoHistoricalCut) bound to the intent's digest, and nothing else.
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use rift_core::RepoId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::hold::{OwnHold, ReadyQueue};
use super::pins::{self, SharedScope};
use super::record::{self, Publication, RecordError};
use super::selection::intent::{encode_part, hex64, Measured, MAX_HEADER_BYTES, MAX_INTENT_BYTES};
use super::selection::{
    self, Expectation, KnownBoundary, RetainedOriginal, Selected, SelectionIntent,
};
use super::{plan, WireError, WireResult};

pub(crate) const CUT_NAME: &str = "cut.json";
/// The whole manifest, including the small fixed framing.
pub(crate) const MAX_CUT_BYTES: usize = 8 * 1024 * 1024;
/// One canonical pinned entry: an intent entry plus the two fixed literals
/// and the longer identity names.
pub(crate) const MAX_PINNED_ENTRY_BYTES: usize = 1024;
/// The core selection ceiling the Cut must stay under.
const MAX_SELECTED: usize = 8192;

/// Why the pins or the Cut are owed rather than made, as a static identifier
/// with the slot it stopped at where one applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Owed {
    HoldUnavailable,
    QueueUnavailable,
    /// The shared scope could not be opened.
    ScopeUnavailable,
    /// The shared scope is not the one the intent froze.
    ScopeMismatch,
    /// The durable intent record is absent or not these bytes.
    IntentUnavailable,
    /// The selection in hand disagrees with the intent or the Plan.
    IntentMismatch,
    /// The retained descriptor no longer proves the expectation.
    RetainedMismatch(u32),
    /// The queue no longer names the source; a link needs the name.
    SourceMissing(u32),
    /// The link or a post-link check refused.
    PinRefused(u32),
    /// An expected pin is absent after a restart; it is never recreated.
    PinMissing(u32),
    /// An expected pin is present but is not the expectation.
    PinMismatch(u32),
    CutInvalid,
    CutOversize,
    CutConflict,
    PublishFailed,
    Resource,
}

/// The seam a test observes, in the order the loop reaches it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    BeforeLink(u32),
    AfterLink(u32),
    BeforeCut,
}

/// One pin made or reopened: the expectation it validated against. The
/// handles are gone; the private hard link is what remains.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PinReceipt {
    pub(crate) expectation: Expectation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CutOutcome {
    Published { cut_sha256: String },
    Resumed { cut_sha256: String },
    NoHistoricalCut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pinned {
    pub(crate) intent_sha256: String,
    pub(crate) pins: Vec<PinReceipt>,
    pub(crate) reserved_pin: Option<PinReceipt>,
    pub(crate) cut: CutOutcome,
}

/// The closed Cut: the Plan's and the intent's digests, the member, and
/// every historical pin's facts sorted by source key. The reserved
/// observation is committed by the intent and is not an entry here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CutManifest {
    pub(crate) schema_version: u32,
    pub(crate) operation_id: String,
    pub(crate) plan_sha256: String,
    pub(crate) intent_sha256: String,
    pub(crate) repo_id: String,
    pub(crate) member_key: String,
    pub(crate) selected_count: u32,
    pub(crate) entries: Vec<PinnedEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PinnedEntry {
    pub(crate) pin_slot: u32,
    pub(crate) source_key: String,
    pub(crate) queue_root: String,
    pub(crate) source_name: String,
    pub(crate) pin_name: String,
    pub(crate) source_dev: u64,
    pub(crate) source_ino: u64,
    pub(crate) file_type: String,
    pub(crate) mode: u32,
    pub(crate) raw_length: u64,
    pub(crate) ticket_sha256: String,
    pub(crate) context_sha256: String,
    pub(crate) attempts: u32,
    pub(crate) known_boundary: KnownBoundary,
}

impl PinnedEntry {
    fn of(expectation: &Expectation) -> Self {
        Self {
            pin_slot: expectation.pin_slot,
            source_key: expectation.source_key.clone(),
            queue_root: "agent".to_owned(),
            source_name: expectation.source_name.clone(),
            pin_name: expectation.pin_name.clone(),
            source_dev: expectation.dev,
            source_ino: expectation.ino,
            file_type: "regular".to_owned(),
            mode: expectation.mode,
            raw_length: expectation.raw_length,
            ticket_sha256: expectation.ticket_sha256.clone(),
            context_sha256: expectation.context_sha256.clone(),
            attempts: expectation.attempts,
            known_boundary: expectation.known_boundary.clone(),
        }
    }

    /// The entry as the expectation it records, for the shared per-field checks.
    fn as_expectation(&self) -> Expectation {
        Expectation {
            pin_slot: self.pin_slot,
            source_key: self.source_key.clone(),
            source_name: self.source_name.clone(),
            pin_name: self.pin_name.clone(),
            dev: self.source_dev,
            ino: self.source_ino,
            mode: self.mode,
            raw_length: self.raw_length,
            ticket_sha256: self.ticket_sha256.clone(),
            context_sha256: self.context_sha256.clone(),
            attempts: self.attempts,
            known_boundary: self.known_boundary.clone(),
        }
    }
}

impl CutManifest {
    /// The Cut of a published intent: its historical expectations sorted by
    /// source key, the reservation excluded.
    pub(crate) fn from_intent(intent: &SelectionIntent, intent_sha256: &str) -> Self {
        let mut entries: Vec<PinnedEntry> = intent.historical.iter().map(PinnedEntry::of).collect();
        entries.sort_by(|a, b| a.source_key.cmp(&b.source_key));
        Self {
            schema_version: 1,
            operation_id: intent.operation_id.clone(),
            plan_sha256: intent.plan_sha256.clone(),
            intent_sha256: intent_sha256.to_owned(),
            repo_id: intent.scope.repo_id.clone(),
            member_key: intent.scope.member_key.clone(),
            selected_count: u32::try_from(entries.len()).unwrap_or(u32::MAX),
            entries,
        }
    }

    /// Measure the parts under their caps and the whole under its cap, check
    /// every closed invariant, then serialize once; a whole that disagrees
    /// with its measured parts refuses.
    pub(crate) fn encode(&self) -> WireResult<Vec<u8>> {
        let sized = self.measure()?;
        let predicted = sized.predicted_len().ok_or(WireError)?;
        if predicted > MAX_CUT_BYTES {
            return Err(WireError);
        }
        self.check()?;
        let bytes = encode_part(self)?;
        if bytes.len() != predicted {
            return Err(WireError);
        }
        Ok(bytes)
    }

    pub(super) fn measure(&self) -> WireResult<Measured> {
        let bare = Self {
            entries: Vec::new(),
            ..self.clone()
        };
        let header = encode_part(&bare)?;
        if header.len() > MAX_HEADER_BYTES {
            return Err(WireError);
        }
        let mut entries = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            let bytes = encode_part(entry)?;
            if bytes.len() > MAX_PINNED_ENTRY_BYTES {
                return Err(WireError);
            }
            entries.push(bytes);
        }
        Ok(Measured {
            header,
            entries,
            reserved: None,
        })
    }

    /// Decode a manifest read whole under the cap, then check every closed
    /// invariant.
    pub(crate) fn decode(bytes: &[u8]) -> WireResult<Self> {
        if bytes.len() > MAX_CUT_BYTES {
            return Err(WireError);
        }
        let value: Value =
            crate::capture_queue::delivery::decode_unique_value(bytes).map_err(|_| WireError)?;
        let manifest: Self = serde_json::from_value(value).map_err(|_| WireError)?;
        manifest.check()?;
        Ok(manifest)
    }

    /// Schema, digests, member and count; entries strictly ascending by
    /// source key with unique names, slots, keys and inode aliases, each the
    /// closed literals and a valid expectation.
    fn check(&self) -> WireResult<()> {
        if self.schema_version != 1 {
            return Err(WireError);
        }
        plan::operation(&self.operation_id)?;
        hex64(&self.plan_sha256)?;
        hex64(&self.intent_sha256)?;
        hex64(&self.member_key)?;
        RepoId::new(self.repo_id.clone()).map_err(|_| WireError)?;
        if self.entries.len() > MAX_SELECTED
            || usize::try_from(self.selected_count).map_err(|_| WireError)? != self.entries.len()
        {
            return Err(WireError);
        }
        let mut names = BTreeSet::new();
        let mut slots = BTreeSet::new();
        let mut inodes = BTreeSet::new();
        let mut previous: Option<&str> = None;
        for entry in &self.entries {
            if entry.queue_root != "agent" || entry.file_type != "regular" {
                return Err(WireError);
            }
            entry.as_expectation().check()?;
            if previous.is_some_and(|key| key >= entry.source_key.as_str())
                || !names.insert(&entry.source_name)
                || !names.insert(&entry.pin_name)
                || !slots.insert(entry.pin_slot)
                || !inodes.insert((entry.source_dev, entry.source_ino))
            {
                return Err(WireError);
            }
            previous = Some(&entry.source_key);
        }
        Ok(())
    }
}

/// The admitted operation the loop runs under: the hold, its queue capability
/// and the shared scopes opened once, all checked against the intent.
struct Operation<'h> {
    hold: &'h OwnHold<'h>,
    queue: ReadyQueue<'h>,
    scope: SharedScope,
}

impl<'h> Operation<'h> {
    /// The hold current, the intent the Plan's own, the durable record exactly
    /// these bytes, the scopes opened once and equal to the frozen scope.
    fn admit(
        hold: &'h OwnHold<'h>,
        intent: &SelectionIntent,
        intent_sha256: &str,
    ) -> Result<Self, Owed> {
        hold.check_current().map_err(|_| Owed::HoldUnavailable)?;
        let queue = hold
            .agent_queue_namespace()
            .map_err(|_| Owed::QueueUnavailable)?;
        let plan = queue.plan();
        let plan_sha256 = plan.sha256().map_err(|_| Owed::HoldUnavailable)?;
        if intent.operation_id != plan.operation()
            || intent.plan_sha256 != plan_sha256.as_str()
            || intent.scope.member_key != plan.member().to_hex()
            || intent.scope.repo_id != plan.repo_id().as_str()
            || intent.policy != selection::POLICY
        {
            return Err(Owed::IntentMismatch);
        }
        let private = queue.private().map_err(|_| Owed::HoldUnavailable)?;
        let bytes = record::read_private_record(private, selection::INTENT_NAME, MAX_INTENT_BYTES)
            .map_err(|_| Owed::IntentUnavailable)?;
        let encoded = intent.encode().map_err(|_| Owed::IntentMismatch)?;
        if bytes != encoded || format!("{:x}", Sha256::digest(&bytes)) != intent_sha256 {
            return Err(Owed::IntentUnavailable);
        }
        let scope = pins::shared_scope(hold.owner(), &queue.queue().path, &private.path)
            .map_err(|_| Owed::ScopeUnavailable)?;
        scope
            .matches_scope(&intent.scope)
            .map_err(|_| Owed::ScopeMismatch)?;
        Ok(Self { hold, queue, scope })
    }

    fn recheck(&self) -> Result<(), Owed> {
        self.hold
            .check_current()
            .map_err(|_| Owed::HoldUnavailable)?;
        self.queue.recheck().map_err(|_| Owed::QueueUnavailable)?;
        self.scope
            .recheck(self.hold.owner())
            .map_err(|_| Owed::ScopeUnavailable)
    }

    /// One slot: the retained descriptor inspected, the name still present,
    /// the link made and validated, then every handle of this slot released
    /// so only the private hard link remains.
    fn pin_one(
        &self,
        original: RetainedOriginal,
        expected: &Expectation,
        observe: &mut impl FnMut(Phase) -> Result<(), ()>,
    ) -> Result<PinReceipt, Owed> {
        let slot = expected.pin_slot;
        if original.slot != slot || original.expectation != *expected {
            return Err(Owed::IntentMismatch);
        }
        self.recheck()?;
        observe(Phase::BeforeLink(slot)).map_err(|()| Owed::Resource)?;
        let owner = self.hold.owner();
        let frozen = pins::inspect_retained(owner, &self.scope, &original)
            .map_err(|_| Owed::RetainedMismatch(slot))?;
        if !self
            .scope
            .source_present(&expected.source_name)
            .map_err(|_| Owed::QueueUnavailable)?
        {
            return Err(Owed::SourceMissing(slot));
        }
        let live = pins::preserve_selected(owner, &self.scope, original, frozen)
            .map_err(|_| Owed::PinRefused(slot))?;
        observe(Phase::AfterLink(slot)).map_err(|()| Owed::Resource)?;
        pins::verify_pin(owner, &live).map_err(|_| Owed::PinRefused(slot))?;
        drop(live);
        Ok(PinReceipt {
            expectation: expected.clone(),
        })
    }

    /// One expected pin after a restart: present and exactly the expectation,
    /// or owed — a missing pin is never recreated from the stored numbers.
    fn reopen_one(&self, expected: &Expectation) -> Result<PinReceipt, Owed> {
        let slot = expected.pin_slot;
        self.recheck()?;
        if !self
            .scope
            .pin_present(&expected.pin_name)
            .map_err(|_| Owed::HoldUnavailable)?
        {
            return Err(Owed::PinMissing(slot));
        }
        let live = pins::reopen_expected(self.hold.owner(), &self.scope, expected)
            .map_err(|_| Owed::PinMismatch(slot))?;
        drop(live);
        Ok(PinReceipt {
            expectation: expected.clone(),
        })
    }

    /// The Cut of the intent, measured, checked and published no-clobber; an
    /// equal file already there is the resume answer. An intent with no
    /// historical member writes nothing and is the tagged NoHistoricalCut.
    fn publish_cut(
        &self,
        intent: &SelectionIntent,
        intent_sha256: &str,
        observe: &mut impl FnMut(Phase) -> Result<(), ()>,
    ) -> Result<CutOutcome, Owed> {
        if intent.historical.is_empty() {
            return Ok(CutOutcome::NoHistoricalCut);
        }
        let manifest = CutManifest::from_intent(intent, intent_sha256);
        let bytes = manifest.encode().map_err(|_| Owed::CutInvalid)?;
        let budget = self.queue.plan().budgets().max_cut_bytes;
        if u64::try_from(bytes.len()).map_err(|_| Owed::CutOversize)? > budget {
            return Err(Owed::CutOversize);
        }
        self.recheck()?;
        observe(Phase::BeforeCut).map_err(|()| Owed::Resource)?;
        let private = self.queue.private().map_err(|_| Owed::HoldUnavailable)?;
        let publication =
            record::publish_private_record(private, CUT_NAME, MAX_CUT_BYTES, &bytes, &mut |_| {
                Ok(())
            })
            .map_err(|error| match error {
                RecordError::Unavailable => Owed::HoldUnavailable,
                RecordError::Conflict => Owed::CutConflict,
                RecordError::Failed => Owed::PublishFailed,
                RecordError::Resource => Owed::Resource,
            })?;
        let cut_sha256 = format!("{:x}", Sha256::digest(&bytes));
        Ok(match publication {
            Publication::Fresh => CutOutcome::Published { cut_sha256 },
            Publication::Resumed => CutOutcome::Resumed { cut_sha256 },
        })
    }
}

/// Pin every selected original one slot at a time, historical in slot order
/// then the reserved one, and publish the Cut. `Err` names the slot the loop
/// stopped at; every pin already made stays, nothing is unlinked or retried.
pub(crate) fn pin_all(hold: &OwnHold<'_>, selected: Selected) -> Result<Pinned, Owed> {
    pin_all_with(hold, selected, &mut |_| Ok(()))
}

pub(crate) fn pin_all_with(
    hold: &OwnHold<'_>,
    selected: Selected,
    observe: &mut impl FnMut(Phase) -> Result<(), ()>,
) -> Result<Pinned, Owed> {
    let operation = Operation::admit(hold, &selected.intent, &selected.intent_sha256)?;
    let Selected {
        intent,
        intent_sha256,
        retained,
        reserved,
        ..
    } = selected;
    if retained.len() != intent.historical.len() {
        return Err(Owed::IntentMismatch);
    }
    let mut pins = Vec::with_capacity(retained.len());
    for (original, expected) in retained.into_iter().zip(&intent.historical) {
        pins.push(operation.pin_one(original, expected, observe)?);
    }
    let reserved_pin = match (reserved, &intent.reserved_observation) {
        (None, None) => None,
        (Some(reserved), Some(observation)) => {
            Some(operation.pin_one(reserved.original, &observation.expectation, observe)?)
        }
        _ => return Err(Owed::IntentMismatch),
    };
    let cut = operation.publish_cut(&intent, &intent_sha256, observe)?;
    Ok(Pinned {
        intent_sha256,
        pins,
        reserved_pin,
        cut,
    })
}

/// Resume from the durable intent alone: every expected pin reopened through
/// its expectation, historical in slot order then the reserved one, and the
/// Cut published only when all of them validate.
pub(crate) fn resume_pins(hold: &OwnHold<'_>, intent: &SelectionIntent) -> Result<Pinned, Owed> {
    resume_pins_with(hold, intent, &mut |_| Ok(()))
}

pub(crate) fn resume_pins_with(
    hold: &OwnHold<'_>,
    intent: &SelectionIntent,
    observe: &mut impl FnMut(Phase) -> Result<(), ()>,
) -> Result<Pinned, Owed> {
    let intent_sha256 = format!(
        "{:x}",
        Sha256::digest(intent.encode().map_err(|_| Owed::IntentMismatch)?)
    );
    let operation = Operation::admit(hold, intent, &intent_sha256)?;
    let mut pins = Vec::with_capacity(intent.historical.len());
    for expected in &intent.historical {
        pins.push(operation.reopen_one(expected)?);
    }
    let reserved_pin = match &intent.reserved_observation {
        None => None,
        Some(observation) => Some(operation.reopen_one(&observation.expectation)?),
    };
    let cut = operation.publish_cut(intent, &intent_sha256, observe)?;
    Ok(Pinned {
        intent_sha256,
        pins,
        reserved_pin,
        cut,
    })
}

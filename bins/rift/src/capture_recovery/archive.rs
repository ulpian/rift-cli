//! The recovery-only archive wrapper: one frozen gap artifact carried by one
//! admitted carrier session, and an immutable archive intent made durable
//! before the native backend is called with the same archive.
//!
//! The intent is a crash pointer, not publication proof: it names the actual
//! assembled id, shard path and artifact digest so a later resume can
//! authenticate the native tree instead of archiving again. Nothing here
//! admits the carrier, assembles the session, settles usage, writes progress,
//! a marker or a cache, or consumes the carrier's ticket.
#[cfg(test)]
mod tests;

use std::cell::RefCell;

use rift_core::boundary_gap::{ContextDigest, GapBatch, GapEnvelope, SessionKey, TicketDigest};
use rift_core::{Result as RiftResult, RiftError};
use rift_shadow::backend::{CheckpointArchive, FrozenBoundaryGaps, ShadowBackend};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::hold::{OwnHold, ReadyQueue};
use super::record::{self, Publication, RecordError};
use super::selection::intent::{encode_part, hex64};
use super::{plan, WireError, WireResult};

pub(crate) const INTENT_NAME: &str = "archive-intent.json";
/// A fixed-shape record of digests, one id list of one batch and a path.
const MAX_INTENT_BYTES: usize = 4096;
/// The one attempt this slice makes; a second attempt's policy is unselected.
const ATTEMPT: u32 = 1;
const MAX_AGENT_BYTES: usize = 64;
const MAX_SESSION_BYTES: usize = 256;

/// Why the archive was not delegated, as a static class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArchiveOwed {
    HoldUnavailable,
    QueueUnavailable,
    /// The core codec refused the one-envelope artifact.
    ArtifactInvalid,
    /// The archive does not carry exactly the frozen gap bytes.
    GapMismatch,
    /// The archive does not name exactly the carrier session and agent.
    CarrierMismatch,
    /// The archive's id is not the one its own root derives.
    IdMismatch,
    /// This exact intent is already durable: the resume authenticates the
    /// native tree first and never delegates a second time.
    IntentExists,
    /// Another intent is already there; it is never overwritten.
    IntentConflict,
    PublishFailed,
}

/// What the one call did, kept for the caller behind the backend seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Delegation {
    Delegated { intent_sha256: String },
    Refused(ArchiveOwed),
}

/// The carrier's admitted facts. The session key is derived here, once,
/// from the agent and native session, so it cannot disagree with them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Carrier {
    agent: String,
    native_session: String,
    session_key: SessionKey,
    context_sha256: String,
    ticket_sha256: String,
}

/// The immutable pointer written before the one delegation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArchiveIntent {
    pub(crate) schema_version: u32,
    pub(crate) operation_id: String,
    pub(crate) plan_sha256: String,
    pub(crate) cut_sha256: String,
    pub(crate) attempt: u32,
    pub(crate) gap_ids: Vec<String>,
    pub(crate) carrier_session_key: String,
    pub(crate) carrier_context_sha256: String,
    pub(crate) carrier_ticket_sha256: String,
    pub(crate) checkpoint_id: String,
    pub(crate) gap_artifact_sha256: String,
    pub(crate) shard_path: String,
}

/// The native backend behind one recovery attempt's checks and intent.
pub(crate) struct RecoveryBackend<'a, B> {
    inner: &'a B,
    hold: &'a OwnHold<'a>,
    carrier: Carrier,
    batch: GapBatch,
    artifact: FrozenBoundaryGaps,
    delegation: RefCell<Option<Delegation>>,
}

impl Carrier {
    pub(super) fn new(
        agent: &str,
        native_session: &str,
        context_sha256: &str,
        ticket_sha256: &str,
    ) -> WireResult<Self> {
        let agent_ok = !agent.is_empty()
            && agent.len() <= MAX_AGENT_BYTES
            && agent
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        let session_ok = !native_session.is_empty()
            && native_session.len() <= MAX_SESSION_BYTES
            && !native_session.contains('\0');
        if !agent_ok || !session_ok {
            return Err(WireError);
        }
        ContextDigest::parse_hex(context_sha256).map_err(|_| WireError)?;
        TicketDigest::parse_hex(ticket_sha256).map_err(|_| WireError)?;
        let key = rift_core::capture_session_row_id(agent, native_session);
        Ok(Self {
            agent: agent.to_owned(),
            native_session: native_session.to_owned(),
            session_key: SessionKey::parse_hex(&key).map_err(|_| WireError)?,
            context_sha256: context_sha256.to_owned(),
            ticket_sha256: ticket_sha256.to_owned(),
        })
    }

    /// Exactly this one session, captured from this agent.
    fn carries(&self, archive: &CheckpointArchive) -> bool {
        archive.sessions == [self.native_session.as_str()]
            && archive.session_agents.get(&self.native_session) == Some(&self.agent)
    }
}

impl ArchiveIntent {
    pub(crate) fn encode(&self) -> WireResult<Vec<u8>> {
        self.check()?;
        let bytes = encode_part(self)?;
        if bytes.len() > MAX_INTENT_BYTES {
            return Err(WireError);
        }
        Ok(bytes)
    }

    pub(crate) fn decode(bytes: &[u8]) -> WireResult<Self> {
        if bytes.len() > MAX_INTENT_BYTES {
            return Err(WireError);
        }
        let value: Value =
            crate::capture_queue::delivery::decode_unique_value(bytes).map_err(|_| WireError)?;
        let intent: Self = serde_json::from_value(value).map_err(|_| WireError)?;
        intent.check()?;
        Ok(intent)
    }

    fn check(&self) -> WireResult<()> {
        if self.schema_version != 1 || self.attempt != ATTEMPT || self.gap_ids.len() != 1 {
            return Err(WireError);
        }
        plan::operation(&self.operation_id)?;
        let digests = [
            &self.plan_sha256,
            &self.cut_sha256,
            &self.carrier_session_key,
            &self.carrier_context_sha256,
            &self.carrier_ticket_sha256,
            &self.checkpoint_id,
            &self.gap_artifact_sha256,
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

impl<'a, B: ShadowBackend> RecoveryBackend<'a, B> {
    /// Freeze the one artifact this attempt may carry: the batch under the
    /// carrier's session key, through the single core canonical codec.
    pub(crate) fn new(
        inner: &'a B,
        hold: &'a OwnHold<'a>,
        carrier: Carrier,
        batch: GapBatch,
    ) -> Result<Self, ArchiveOwed> {
        let envelope = GapEnvelope::new(carrier.session_key, batch.clone());
        let artifact =
            FrozenBoundaryGaps::new(vec![envelope]).map_err(|_| ArchiveOwed::ArtifactInvalid)?;
        Ok(Self {
            inner,
            hold,
            carrier,
            batch,
            artifact,
            delegation: RefCell::new(None),
        })
    }

    /// The bytes the assembly injects before the checkpoint id is derived.
    pub(crate) fn artifact(&self) -> &FrozenBoundaryGaps {
        &self.artifact
    }

    /// What the last call did; `None` before any call.
    pub(crate) fn delegation(&self) -> Option<Delegation> {
        self.delegation.borrow().clone()
    }

    /// The archive is exactly the frozen carrier, by bytes, session and id.
    fn check_archive(&self, archive: &CheckpointArchive) -> Result<(), ArchiveOwed> {
        let Some(gaps) = &archive.boundary_gaps else {
            return Err(ArchiveOwed::GapMismatch);
        };
        if gaps.as_bytes() != self.artifact.as_bytes() {
            return Err(ArchiveOwed::GapMismatch);
        }
        if !self.carrier.carries(archive) {
            return Err(ArchiveOwed::CarrierMismatch);
        }
        if archive.derived_checkpoint_id().as_deref() != Some(archive.checkpoint_id.as_str()) {
            return Err(ArchiveOwed::IdMismatch);
        }
        Ok(())
    }

    fn intent_of(
        &self,
        queue: &ReadyQueue<'_>,
        archive: &CheckpointArchive,
    ) -> Result<ArchiveIntent, ArchiveOwed> {
        let plan = queue.plan();
        let plan_sha256 = plan.sha256().map_err(|_| ArchiveOwed::HoldUnavailable)?;
        Ok(ArchiveIntent {
            schema_version: 1,
            operation_id: plan.operation().to_owned(),
            plan_sha256: plan_sha256.as_str().to_owned(),
            cut_sha256: self.batch.evidence_digest().to_hex(),
            attempt: ATTEMPT,
            gap_ids: vec![self.batch.gap_id().to_hex()],
            carrier_session_key: self.carrier.session_key.to_hex(),
            carrier_context_sha256: self.carrier.context_sha256.clone(),
            carrier_ticket_sha256: self.carrier.ticket_sha256.clone(),
            checkpoint_id: archive.checkpoint_id.clone(),
            gap_artifact_sha256: format!("{:x}", Sha256::digest(self.artifact.as_bytes())),
            shard_path: archive.shard_path(),
        })
    }

    /// Check, then make the intent durable; its digest on a fresh write.
    fn persist_intent(&self, archive: &CheckpointArchive) -> Result<String, ArchiveOwed> {
        self.hold
            .check_current()
            .map_err(|_| ArchiveOwed::HoldUnavailable)?;
        self.check_archive(archive)?;
        let queue = self
            .hold
            .agent_queue_namespace()
            .map_err(|_| ArchiveOwed::QueueUnavailable)?;
        // Every field is a checked digest or derived from one; a refusal
        // here can only be an id the shard layout does not accept.
        let bytes = self
            .intent_of(&queue, archive)?
            .encode()
            .map_err(|_| ArchiveOwed::IdMismatch)?;
        let private = queue.private().map_err(|_| ArchiveOwed::HoldUnavailable)?;
        let publication = record::publish_private_record(
            private,
            INTENT_NAME,
            MAX_INTENT_BYTES,
            &bytes,
            &mut |_| Ok(()),
        )
        .map_err(|error| match error {
            RecordError::Unavailable => ArchiveOwed::HoldUnavailable,
            RecordError::Conflict => ArchiveOwed::IntentConflict,
            RecordError::Failed | RecordError::Resource | RecordError::Absent => {
                ArchiveOwed::PublishFailed
            }
        })?;
        match publication {
            Publication::Fresh => Ok(format!("{:x}", Sha256::digest(&bytes))),
            Publication::Resumed => Err(ArchiveOwed::IntentExists),
        }
    }
}

/// The one road from a recovery attempt to the native writer: every refusal
/// and every intent write failure leaves the writer uncalled.
impl<B: ShadowBackend> ShadowBackend for RecoveryBackend<'_, B> {
    fn archive_checkpoint(&self, archive: &CheckpointArchive) -> RiftResult<()> {
        match self.persist_intent(archive) {
            Ok(intent_sha256) => {
                self.delegation
                    .replace(Some(Delegation::Delegated { intent_sha256 }));
                self.inner.archive_checkpoint(archive)
            }
            Err(owed) => {
                self.delegation.replace(Some(Delegation::Refused(owed)));
                Err(RiftError::validity(
                    "recovery archive refused before delegation",
                ))
            }
        }
    }
}

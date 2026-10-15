//! Frozen structural recovery-audit bytes and explicit native carrier checks.

use std::collections::{BTreeMap, BTreeSet};

use rift_core::boundary_gap::{encode_canonical, GapEnvelope, MAX_TICKETS};
use rift_core::{Result, RiftError};

pub(crate) use rift_recall_substrate::parse::boundary_gaps::parse_gap_session_agents as strict_session_agents;

/// The optional whole-file audit leaf's canonical shard path.
pub const BOUNDARY_GAPS_NAME: &str = "turn_boundary_gaps.jsonl";

/// Checked canonical bytes frozen once and reused by the writer and root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrozenBoundaryGaps {
    bytes: Vec<u8>,
    envelopes: Vec<GapEnvelope>,
}

impl FrozenBoundaryGaps {
    /// Freeze checked envelopes with the single canonical codec.
    ///
    /// # Errors
    /// Refuses empty, overlapping or over-bound membership.
    pub fn new(envelopes: Vec<GapEnvelope>) -> Result<Self> {
        let bytes = encode_canonical(&envelopes)?;
        // Keep the codec's canonical order alongside its exact bytes.
        let envelopes = rift_recall_substrate::parse::boundary_gaps::parse_boundary_gaps(&bytes)?;
        Ok(Self { bytes, envelopes })
    }

    /// The exact structural bytes written and hashed.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Checked facts; subjects carry no native session authority.
    #[must_use]
    pub fn envelopes(&self) -> &[GapEnvelope] {
        &self.envelopes
    }
}

impl super::CheckpointArchive {
    pub(super) fn insert_boundary_gap_artifact(&self, out: &mut BTreeMap<String, Vec<u8>>) {
        if let Some(gaps) = &self.boundary_gaps {
            out.insert(BOUNDARY_GAPS_NAME.to_owned(), gaps.as_bytes().to_vec());
        }
    }
}

pub(crate) fn invalid() -> RiftError {
    RiftError::integrity("boundary gap: unsupported or unauthenticated artifact")
}

/// Validate all real archive carriers before any Git object write.
pub(super) fn validate_archive(archive: &super::CheckpointArchive) -> Result<()> {
    let Some(gaps) = &archive.boundary_gaps else {
        return Ok(());
    };
    validate_carriers(&archive.sessions, &archive.session_agents, gaps.envelopes())?;
    if archive.derived_checkpoint_id().as_deref() != Some(archive.checkpoint_id.as_str()) {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) fn validate_carriers(
    sessions: &[String],
    agents: &BTreeMap<String, String>,
    envelopes: &[GapEnvelope],
) -> Result<()> {
    let unique: BTreeSet<_> = sessions.iter().collect();
    if sessions.is_empty()
        || sessions.len() > MAX_TICKETS
        || unique.len() != sessions.len()
        || agents.len() != sessions.len()
        || sessions
            .iter()
            .any(|s| s.is_empty() || s.contains('\0') || !agents.contains_key(s))
        || agents.values().any(|agent| {
            !rift_core::AgentId::FIRST_PARTY
                .iter()
                .any(|id| id.as_str() == agent)
        })
    {
        return Err(invalid());
    }
    let carriers: BTreeSet<_> = agents
        .iter()
        .map(|(native, agent)| rift_core::capture_session_row_id(agent, native))
        .collect();
    if envelopes
        .iter()
        .any(|e| !carriers.contains(&e.carrier_session_key().to_hex()))
    {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests;

//! The recovery-audit stream has one bounded checked codec, shared with writers.

use rift_core::boundary_gap::GapEnvelope;
use rift_core::Result;

/// Required gap-binding fields. Other ordinary metadata members stay compatible.
#[derive(serde::Deserialize)]
pub struct GapBindingMetadata {
    /// Exact numeric canonicalization epoch; the admission layer checks support.
    pub canon_version: u32,
    /// Recorded complete artifact root.
    pub merkle_root: String,
    /// Actual native header membership, without normalization.
    pub sessions: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionAgent {
    agent: String,
    schema_version: u32,
    session: String,
}

/// Decode an entire present artifact; no invalid line is skipped.
///
/// # Errors
/// Refuses unsupported, malformed, over-bound or inconsistent input.
pub fn parse_boundary_gaps(bytes: &[u8]) -> Result<Vec<GapEnvelope>> {
    rift_core::boundary_gap::decode_checked(bytes)
}

fn invalid() -> rift_core::RiftError {
    rift_core::RiftError::validity("boundary gap: malformed binding fields")
}

/// Parse required binding fields with duplicate refusal and no raw error chain.
///
/// # Errors
/// Refuses missing, malformed, repeated or over-bound required fields.
pub fn parse_gap_binding_metadata(bytes: &[u8]) -> Result<GapBindingMetadata> {
    if bytes.len() > rift_core::boundary_gap::MAX_ARTIFACT_BYTES {
        return Err(invalid());
    }
    serde_json::from_slice(bytes).map_err(|_| invalid())
}

/// Parse every explicit mapping, rejecting repeated sessions and invalid lines.
///
/// # Errors
/// Refuses unsupported, duplicate, unknown, malformed or over-bound input.
pub fn parse_gap_session_agents(
    bytes: &[u8],
) -> Result<std::collections::BTreeMap<String, String>> {
    use rift_core::boundary_gap::{MAX_ARTIFACT_BYTES, MAX_TICKETS};
    if bytes.is_empty() || bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(invalid());
    }
    let mut agents = std::collections::BTreeMap::new();
    let body = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    for line in body.split(|b| *b == b'\n') {
        if agents.len() >= MAX_TICKETS {
            return Err(invalid());
        }
        let row: SessionAgent = serde_json::from_slice(line).map_err(|_| invalid())?;
        if row.schema_version != 1 || agents.insert(row.session, row.agent).is_some() {
            return Err(invalid());
        }
    }
    Ok(agents)
}

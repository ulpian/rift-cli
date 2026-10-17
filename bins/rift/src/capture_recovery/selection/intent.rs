//! The closed SelectionIntent record: one shared scope header, the inventory
//! counts, the historical expectations in slot order and the one nullable
//! reserved observation. Identifiers, digests, counts and paths only — never a
//! raw value, raw bytes or a payload. Encoded through the queue's canonical
//! codec (sorted keys, no LF); its file SHA is its commitment.
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::POLICY;
use crate::capture_recovery::codec;
use crate::capture_recovery::plan::{self, PlanDigest};
use crate::capture_recovery::{WireError, WireResult};

/// The whole record, including the small fixed framing.
pub(crate) const MAX_INTENT_BYTES: usize = 8 * 1024 * 1024;
/// The header: everything but the entries.
pub(crate) const MAX_HEADER_BYTES: usize = 128 * 1024;
/// One canonical entry.
pub(crate) const MAX_ENTRY_BYTES: usize = 960;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SelectionIntent {
    pub(crate) schema_version: u32,
    pub(crate) operation_id: String,
    pub(crate) plan_sha256: String,
    pub(crate) policy: String,
    pub(crate) scope: Scope,
    pub(crate) inventory: InventoryCounts,
    pub(crate) historical: Vec<Expectation>,
    pub(crate) reserved_observation: Option<ReservedObservation>,
}

/// The shared owner/source/private scope, once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Scope {
    pub(crate) member_key: String,
    pub(crate) repo_id: String,
    pub(crate) canonical_git_dir: String,
    pub(crate) canonical_work_tree: String,
    pub(crate) source_queue_dir: String,
    pub(crate) private_dir: String,
    pub(crate) source_dir_identity: Identity,
    pub(crate) private_dir_identity: Identity,
    pub(crate) source_ancestry: Vec<Identity>,
    pub(crate) private_ancestry: Vec<Identity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    pub(crate) dev: u64,
    pub(crate) ino: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InventoryCounts {
    pub(crate) entries_counted: u32,
    pub(crate) tickets: u32,
    pub(crate) understood_other: u32,
    pub(crate) ancillary: u32,
    pub(crate) refused_by_reason: BTreeMap<String, u32>,
}

/// One frozen source: its slot, identities, digests and known boundary facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Expectation {
    pub(crate) pin_slot: u32,
    pub(crate) source_key: String,
    pub(crate) source_name: String,
    pub(crate) pin_name: String,
    pub(crate) dev: u64,
    pub(crate) ino: u64,
    pub(crate) mode: u32,
    pub(crate) raw_length: u64,
    pub(crate) ticket_sha256: String,
    pub(crate) context_sha256: String,
    pub(crate) attempts: u32,
    pub(crate) known_boundary: KnownBoundary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct KnownBoundary {
    pub(crate) agent: String,
    pub(crate) native_session: String,
    pub(crate) session_key: String,
    pub(crate) turn_ordinal: u32,
    pub(crate) boundary: String,
    pub(crate) head_oid: Option<String>,
    pub(crate) captured_at: i64,
    pub(crate) queued_at: i64,
}

/// The reserved current boundary: its expectation plus the event, receipt and
/// cache evidence the eligibility pass read it on. Nested rather than
/// flattened so the record stays closed (`deny_unknown_fields`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReservedObservation {
    pub(crate) expectation: Expectation,
    pub(crate) event_key: String,
    pub(crate) receipt_id: String,
    pub(crate) cache_evidence_sha256: String,
}

/// The exact canonical size of the whole record, computed from the bounded
/// parts before any whole serialization: the header with an empty entry list
/// and a null reservation, each entry on its own, the commas between them and
/// the null the reservation replaces.
pub(crate) struct Measured {
    pub(crate) header: Vec<u8>,
    pub(crate) entries: Vec<Vec<u8>>,
    pub(crate) reserved: Option<Vec<u8>>,
}

impl Measured {
    pub(crate) fn predicted_len(&self) -> Option<usize> {
        let entries: usize = self.entries.iter().map(Vec::len).sum();
        let commas = self.entries.len().saturating_sub(1);
        let reserved = self
            .reserved
            .as_ref()
            .map_or(0, |bytes| bytes.len().saturating_sub(4));
        self.header
            .len()
            .checked_add(entries)?
            .checked_add(commas)?
            .checked_add(reserved)
    }
}

pub(crate) fn hex64(value: &str) -> WireResult<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(WireError);
    }
    Ok(())
}

fn encode_part<T: Serialize>(value: &T) -> WireResult<Vec<u8>> {
    let value = serde_json::to_value(value).map_err(|_| WireError)?;
    crate::capture_queue::delivery::encode(&value).map_err(|_| WireError)
}

impl SelectionIntent {
    /// Measure the parts under their caps and the whole under its cap, then
    /// serialize once; a whole that disagrees with its measured parts refuses.
    pub(crate) fn encode(&self) -> WireResult<Vec<u8>> {
        let sized = self.measure()?;
        let predicted = sized.predicted_len().ok_or(WireError)?;
        if predicted > MAX_INTENT_BYTES {
            return Err(WireError);
        }
        let bytes = encode_part(self)?;
        if bytes.len() != predicted {
            return Err(WireError);
        }
        Ok(bytes)
    }

    pub(super) fn measure(&self) -> WireResult<Measured> {
        let bare = Self {
            historical: Vec::new(),
            reserved_observation: None,
            ..self.clone()
        };
        let header = encode_part(&bare)?;
        if header.len() > MAX_HEADER_BYTES {
            return Err(WireError);
        }
        let mut entries = Vec::with_capacity(self.historical.len());
        for entry in &self.historical {
            let bytes = encode_part(entry)?;
            if bytes.len() > MAX_ENTRY_BYTES {
                return Err(WireError);
            }
            entries.push(bytes);
        }
        let reserved = match &self.reserved_observation {
            None => None,
            Some(observation) => {
                let bytes = encode_part(observation)?;
                if bytes.len() > MAX_ENTRY_BYTES + 3 * 64 + 96 {
                    return Err(WireError);
                }
                Some(bytes)
            }
        };
        Ok(Measured {
            header,
            entries,
            reserved,
        })
    }

    /// Decode a record read whole under the cap, then check every closed
    /// invariant: schema, policy, digests, slots in order, unique names, keys
    /// and inodes, lexical paths and counts that agree with the entries.
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
        if self.schema_version != 1 || self.policy != POLICY {
            return Err(WireError);
        }
        plan::operation(&self.operation_id)?;
        PlanDigest::parse(&self.plan_sha256)?;
        hex64(&self.scope.member_key)?;
        for path in [
            &self.scope.canonical_git_dir,
            &self.scope.canonical_work_tree,
            &self.scope.source_queue_dir,
            &self.scope.private_dir,
        ] {
            plan::lexical_path(path)?;
        }
        let mut names = BTreeSet::new();
        let mut keys = BTreeSet::new();
        let mut inodes = BTreeSet::new();
        let reserved = self.reserved_observation.as_ref().map(|r| &r.expectation);
        for (index, entry) in self.historical.iter().chain(reserved).enumerate() {
            entry.check()?;
            if usize::try_from(entry.pin_slot).map_err(|_| WireError)? != index
                || !names.insert(&entry.source_name)
                || !keys.insert(&entry.source_key)
                || !inodes.insert((entry.dev, entry.ino))
            {
                return Err(WireError);
            }
        }
        if let Some(observation) = &self.reserved_observation {
            hex64(&observation.event_key)?;
            hex64(&observation.receipt_id)?;
            hex64(&observation.cache_evidence_sha256)?;
        }
        let selected = self.historical.len() + usize::from(self.reserved_observation.is_some());
        if usize::try_from(self.inventory.tickets).map_err(|_| WireError)? != selected {
            return Err(WireError);
        }
        Ok(())
    }
}

impl Expectation {
    fn check(&self) -> WireResult<()> {
        codec::basename(OsStr::new(&self.source_name)).map_err(|_| WireError)?;
        codec::basename(OsStr::new(&self.pin_name)).map_err(|_| WireError)?;
        hex64(&self.source_key)?;
        hex64(&self.ticket_sha256)?;
        hex64(&self.context_sha256)?;
        hex64(&self.known_boundary.session_key)?;
        if self.mode > 0o7777
            || !matches!(self.known_boundary.boundary.as_str(), "start" | "end")
            || self.known_boundary.captured_at < 0
            || self.known_boundary.queued_at < 0
            || !rift_shadow::hooks::is_safe_session_id(&self.known_boundary.native_session)
            || !self
                .known_boundary
                .head_oid
                .as_deref()
                .is_none_or(codec::is_git_oid)
        {
            return Err(WireError);
        }
        Ok(())
    }
}

//! Closed recovery policy records; parsing proves shape, never local authority.
use std::path::Path;

use rift_core::boundary_gap::MemberKey;
use rift_core::RepoId;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{codec, WireError, WireResult};

pub(super) const MAX_PLAN_BYTES: usize = 64 * 1024;

pub(crate) struct Plan {
    wire: PlanWire,
    repo_id: RepoId,
    member: MemberKey,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PlanWire {
    schema_version: u32,
    operation_id: String,
    repo_id: String,
    member_key: String,
    canonical_git_dir: String,
    canonical_work_tree: String,
    evidence_dir: String,
    selector: Selector,
    budgets: Budgets,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Selector {
    kind: String,
    schema_version: u32,
    unknown_fields: String,
    inventory: String,
    reservation: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_field_names,
    reason = "the max_* names are the Plan record's serde wire fields (deny_unknown_fields); renaming changes the wire"
)]
pub(super) struct Budgets {
    pub(super) max_inventory_entries: u32,
    pub(super) max_selected_tickets: u32,
    pub(super) max_ticket_bytes: u64,
    pub(super) max_total_ticket_bytes: u64,
    pub(super) max_cut_bytes: u64,
    pub(super) max_artifact_bytes: u64,
}

/// Raw canonical Plan-file SHA, distinct from internal context handles.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct PlanDigest(String);

impl PlanDigest {
    pub(super) fn parse(value: &str) -> WireResult<Self> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(WireError);
        }
        Ok(Self(value.to_owned()))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Plan {
    pub(crate) fn decode(bytes: &[u8]) -> WireResult<Self> {
        let wire: PlanWire = codec::wire_decode(bytes, MAX_PLAN_BYTES)?;
        if wire.schema_version != 1 {
            return Err(WireError);
        }
        operation(&wire.operation_id)?;
        for path in [
            &wire.canonical_git_dir,
            &wire.canonical_work_tree,
            &wire.evidence_dir,
        ] {
            lexical_path(path)?;
        }
        wire.selector.check()?;
        wire.budgets.check()?;
        let repo_id = RepoId::new(wire.repo_id.clone()).map_err(|_| WireError)?;
        let member = MemberKey::parse_hex(&wire.member_key).map_err(|_| WireError)?;
        if member_key(&repo_id, &wire.canonical_git_dir)? != member {
            return Err(WireError);
        }
        Ok(Self {
            wire,
            repo_id,
            member,
        })
    }

    pub(crate) fn encode(&self) -> WireResult<Vec<u8>> {
        codec::wire_encode(&self.wire, MAX_PLAN_BYTES)
    }

    pub(super) fn sha256(&self) -> WireResult<PlanDigest> {
        PlanDigest::parse(&format!("{:x}", Sha256::digest(self.encode()?)))
    }

    pub(super) fn operation(&self) -> &str {
        &self.wire.operation_id
    }
    pub(super) fn repo_id(&self) -> &RepoId {
        &self.repo_id
    }
    pub(super) fn member(&self) -> MemberKey {
        self.member
    }
    pub(super) fn git_dir(&self) -> &Path {
        Path::new(&self.wire.canonical_git_dir)
    }
    pub(super) fn work_tree(&self) -> &Path {
        Path::new(&self.wire.canonical_work_tree)
    }
    pub(super) fn evidence_dir(&self) -> &Path {
        Path::new(&self.wire.evidence_dir)
    }
    pub(super) fn budgets(&self) -> &Budgets {
        &self.wire.budgets
    }
    /// The closed reservation policy the selector names.
    pub(super) fn reservation(&self) -> &str {
        &self.wire.selector.reservation
    }
}

impl Selector {
    fn check(&self) -> WireResult<()> {
        if self.kind != "boundary-snapshots-only"
            || self.schema_version != 1
            || self.unknown_fields != "refuse"
            || self.inventory != "complete-bounded"
            || self.reservation != "one-eligible-explicit-current-boundary@1"
        {
            return Err(WireError);
        }
        Ok(())
    }
}

impl Budgets {
    fn check(&self) -> WireResult<()> {
        for (actual, limit) in [
            (u64::from(self.max_inventory_entries), 8192),
            (u64::from(self.max_selected_tickets), 8192),
            (self.max_ticket_bytes, 65536),
            (self.max_total_ticket_bytes, 64 * 1024 * 1024),
            (self.max_cut_bytes, 8 * 1024 * 1024),
            (self.max_artifact_bytes, 8 * 1024 * 1024),
        ] {
            if actual == 0 || actual > limit {
                return Err(WireError);
            }
        }
        if self.max_selected_tickets > self.max_inventory_entries
            || self.max_ticket_bytes > self.max_total_ticket_bytes
        {
            return Err(WireError);
        }
        self.max_inventory_entries.checked_add(1).ok_or(WireError)?;
        u64::from(self.max_selected_tickets)
            .checked_mul(self.max_ticket_bytes)
            .ok_or(WireError)?;
        Ok(())
    }
}

pub(super) fn operation(value: &str) -> WireResult<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(WireError);
    }
    Ok(())
}

pub(super) fn lexical_path(value: &str) -> WireResult<()> {
    if value.is_empty()
        || value.len() > 4096
        || value.as_bytes().contains(&0)
        || !value.starts_with('/')
    {
        return Err(WireError);
    }
    if value != "/"
        && value
            .get(1..)
            .ok_or(WireError)?
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(WireError);
    }
    Ok(())
}

pub(super) fn member_key(repo: &RepoId, git_dir: &str) -> WireResult<MemberKey> {
    lexical_path(git_dir)?;
    let bytes = codec::wire_encode(
        &serde_json::json!({"canonical_git_dir":git_dir,"repo_id":repo.as_str()}),
        MAX_PLAN_BYTES,
    )?;
    let mut hash = Sha256::new();
    hash.update(b"rift-capture-recovery-member@1\0");
    hash.update(bytes);
    MemberKey::parse_hex(&format!("{:x}", hash.finalize())).map_err(|_| WireError)
}

#[cfg(test)]
mod tests;

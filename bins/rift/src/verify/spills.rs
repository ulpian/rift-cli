//! The sealed spilled tool outputs of a shard, read back and re-checked.
//!
//! A spill's body is a Merkle leaf, so an edited one already fails the root and
//! the id. This pass is what names it: the `spills` manifest on the metadata
//! document beside each transcript records the digest and size of every body
//! it sealed, and a body that no longer matches is reported as its own file
//! rather than only as a checkpoint whose id no longer addresses it. The same
//! manifest is the reader's enumeration — the substrate has no tree listing —
//! so a body hidden from it is a leaf missing from the re-derivation.

use rift_core::Result;
use rift_recall_substrate::SubstrateReader;
use rift_shadow::backend::{SpillLeafBytes, SPILLS_DIR, SPILL_SUFFIX};

use super::verdict::Verdict;

/// One manifest entry that claims a sealed body.
#[derive(Clone)]
pub(super) struct SpillClaim {
    pub(super) tool_use_id: String,
    pub(super) sha256: Option<String>,
    pub(super) bytes: Option<u64>,
}

/// One claimed spill as read back out of the shard.
pub(super) struct SpillBytes {
    /// The task subtree it sits in, `None` for the session's own transcript.
    pub(super) task: Option<String>,
    pub(super) claim: SpillClaim,
    /// The stored body; `None` when the claimed file is not in the shard.
    pub(super) body: Option<Vec<u8>>,
}

impl SpillBytes {
    /// The shard-relative path the body is sealed at.
    fn path(&self) -> String {
        let dir = self
            .task
            .as_ref()
            .map_or_else(String::new, |task| format!("tasks/{task}/"));
        format!("{dir}{SPILLS_DIR}/{}{SPILL_SUFFIX}", self.claim.tool_use_id)
    }
}

/// The entries of a metadata document's `spills` manifest that claim a
/// sealed body — those carrying a digest or a size. A lost spill claims none
/// and owes nothing here.
pub(super) fn claims(doc: Option<&serde_json::Value>) -> Vec<SpillClaim> {
    doc.and_then(|d| d.get("spills"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let sha256 = entry.get("sha256").and_then(serde_json::Value::as_str);
            let bytes = entry.get("bytes").and_then(serde_json::Value::as_u64);
            if sha256.is_none() && bytes.is_none() {
                return None;
            }
            Some(SpillClaim {
                tool_use_id: entry.get("tool_use_id")?.as_str()?.to_owned(),
                sha256: sha256.map(str::to_owned),
                bytes,
            })
        })
        .collect()
}

/// Read every claimed body beside one transcript.
pub(super) fn read(
    reader: &impl SubstrateReader,
    id: &str,
    task: Option<&str>,
    claims: Vec<SpillClaim>,
) -> Result<Vec<SpillBytes>> {
    let mut out = Vec::with_capacity(claims.len());
    for claim in claims {
        let mut spill = SpillBytes {
            task: task.map(str::to_owned),
            claim,
            body: None,
        };
        spill.body = reader.read_artifact(id, &spill.path())?;
        out.push(spill);
    }
    Ok(out)
}

/// The leaf views of the bodies read back. A claimed body that is not in the
/// shard reads as empty bytes, which cannot reproduce the writer's leaf.
pub(super) fn leaves(spills: &[SpillBytes]) -> Vec<SpillLeafBytes<'_>> {
    spills
        .iter()
        .map(|s| SpillLeafBytes {
            task: s.task.as_deref(),
            tool_use_id: &s.claim.tool_use_id,
            body: s.body.as_deref().unwrap_or_default(),
        })
        .collect()
}

/// Each claimed body must be in the shard and match the digest and size its
/// manifest records; one that does not is named by its path.
pub(super) fn spill_digest_pass(id: &str, spills: &[SpillBytes]) -> Vec<Verdict> {
    spills
        .iter()
        .filter(|s| {
            let Some(body) = &s.body else {
                return true;
            };
            let digest = rift_shadow::sha256_hex(&[body]);
            s.claim.sha256.as_deref() != Some(digest.as_str())
                || s.claim.bytes != u64::try_from(body.len()).ok()
        })
        .map(|s| {
            Verdict::Tamper(format!(
                "{id}: {} does not match its recorded sha256",
                s.path()
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests;

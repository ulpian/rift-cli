//! Native spill and original manifests and their exact stored body views.

use crate::backend::rewrite::LineRange;
use crate::backend::{
    OriginalLeafBytes, SpillLeafBytes, ORIGINALS_DIR, ORIGINAL_SUFFIX, SPILLS_DIR, SPILL_SUFFIX,
};
use rift_core::Result;
use rift_recall_substrate::SubstrateReader;

/// One manifest entry that claims a sealed body.
#[derive(Clone)]
pub struct SpillClaim {
    /// Stored tool use id value, unchanged from its artifact.
    pub tool_use_id: String,
    /// Stored sha256 value, unchanged from its artifact.
    pub sha256: Option<String>,
    /// Stored bytes value, unchanged from its artifact.
    pub bytes: Option<u64>,
}

/// One claimed spill as read back out of the shard.
pub struct SpillBytes {
    /// The task subtree it sits in, `None` for the session's own transcript.
    pub task: Option<String>,
    /// Stored claim value, unchanged from its artifact.
    pub claim: SpillClaim,
    /// The stored body; `None` when the claimed file is not in the shard.
    pub body: Option<Vec<u8>>,
}

impl SpillBytes {
    /// The shard-relative path the body is sealed at.
    pub fn path(&self) -> String {
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
pub fn spill_claims(doc: Option<&serde_json::Value>) -> Vec<SpillClaim> {
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
///
/// # Errors
/// Propagates claimed body artifact read errors.
pub fn read_spills(
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
/// shard reads as empty bytes and may preserve a sealed-empty legacy hash.
/// Present-gap admission separately refuses the missing body.
#[must_use]
pub fn spill_leaves(spills: &[SpillBytes]) -> Vec<SpillLeafBytes<'_>> {
    spills
        .iter()
        .map(|s| SpillLeafBytes {
            task: s.task.as_deref(),
            tool_use_id: &s.claim.tool_use_id,
            body: s.body.as_deref().unwrap_or_default(),
        })
        .collect()
}

/// One manifest entry.
#[derive(Clone)]
pub struct OriginalClaim {
    /// Stored tool use id value, unchanged from its artifact.
    pub tool_use_id: String,
    /// Stored sha256 value, unchanged from its artifact.
    pub sha256: Option<String>,
    /// Stored bytes value, unchanged from its artifact.
    pub bytes: Option<u64>,
    /// Stored form value, unchanged from its artifact.
    pub form: Option<String>,
    /// Stored replaced value, unchanged from its artifact.
    pub replaced: Option<Vec<LineRange>>,
    /// Stored line value, unchanged from its artifact.
    pub line: Option<usize>,
    /// Stored rewritten value, unchanged from its artifact.
    pub rewritten: Option<String>,
    /// Stored state value, unchanged from its artifact.
    pub state: Option<String>,
}

impl OriginalClaim {
    /// Whether the entry claims a sealed body.
    #[must_use]
    pub fn claims_body(&self) -> bool {
        self.sha256.is_some() || self.bytes.is_some()
    }
}

/// One manifest entry with its body as read back out of the shard.
pub struct OriginalBytes {
    /// The task subtree it sits in, `None` for the session's own transcript.
    pub task: Option<String>,
    /// Stored claim value, unchanged from its artifact.
    pub claim: OriginalClaim,
    /// The stored body; `None` when none is claimed or the file is absent.
    pub body: Option<Vec<u8>>,
}

impl OriginalBytes {
    /// The shard-relative path the body is sealed at.
    #[must_use]
    pub fn path(&self) -> String {
        format!(
            "{}{ORIGINALS_DIR}/{}{ORIGINAL_SUFFIX}",
            task_dir(self.task.as_deref()),
            self.claim.tool_use_id
        )
    }
}

/// `tasks/<task>/` for a subagent's transcript, nothing for the session's.
pub fn task_dir(task: Option<&str>) -> String {
    task.map_or_else(String::new, |t| format!("tasks/{t}/"))
}

/// Every entry of a metadata document's `originals` manifest that names a
/// call.
pub fn original_claims(doc: Option<&serde_json::Value>) -> Vec<OriginalClaim> {
    doc.and_then(|d| d.get("originals"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let text = |k: &str| {
                entry
                    .get(k)
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            };
            let replaced = entry
                .get("replaced")
                .and_then(serde_json::Value::as_array)
                .map(|rs| {
                    rs.iter()
                        .map(LineRange::from_json)
                        .collect::<Option<Vec<_>>>()
                });
            Some(OriginalClaim {
                tool_use_id: text("tool_use_id")?,
                sha256: text("sha256"),
                bytes: entry.get("bytes").and_then(serde_json::Value::as_u64),
                form: text("form"),
                replaced: replaced.flatten(),
                line: entry
                    .get("line")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|l| usize::try_from(l).ok()),
                rewritten: text("rewritten"),
                state: text("original"),
            })
        })
        .collect()
}

/// Read every claimed body beside one transcript.
///
/// # Errors
/// Propagates claimed body artifact read errors.
pub fn read_originals(
    reader: &impl SubstrateReader,
    id: &str,
    task: Option<&str>,
    claims: Vec<OriginalClaim>,
) -> Result<Vec<OriginalBytes>> {
    let mut out = Vec::with_capacity(claims.len());
    for claim in claims {
        let mut original = OriginalBytes {
            task: task.map(str::to_owned),
            claim,
            body: None,
        };
        if original.claim.claims_body() {
            original.body = reader.read_artifact(id, &original.path())?;
        }
        out.push(original);
    }
    Ok(out)
}

/// The leaf views of the bodies claimed. A claimed body that is not in the
/// shard reads as empty bytes and may preserve a sealed-empty legacy hash.
/// Present-gap admission separately refuses the missing body.
#[must_use]
pub fn original_leaves(originals: &[OriginalBytes]) -> Vec<OriginalLeafBytes<'_>> {
    originals
        .iter()
        .filter(|o| o.claim.claims_body())
        .map(|o| OriginalLeafBytes {
            task: o.task.as_deref(),
            tool_use_id: &o.claim.tool_use_id,
            body: o.body.as_deref().unwrap_or_default(),
        })
        .collect()
}

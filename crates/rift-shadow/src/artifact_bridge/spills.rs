//! Counting a shard's spilled tool outputs on a rebuild.
//!
//! The projection holds no row for a spill: the shard is its record, and the
//! transcript line that names it is already in the sealed transcript. A
//! rebuild therefore stores nothing new and only proves the spills still
//! read — each body a manifest claims is read back and its recorded digest
//! recomputed, so two rebuilds from a wiped projection count the same spills.

use rift_core::Result;
use rift_recall_substrate::SubstrateReader;

use super::BridgeReport;
use crate::backend::{SPILLS_DIR, SPILL_SUFFIX};

/// Count the spills one metadata document's `spills` manifest lists — the
/// session transcript's when `task` is `None`, else that task's. A claimed
/// body that is absent or no longer matches its digest is a parse issue, not
/// a spill.
pub(super) fn count_spills(
    sub: &impl SubstrateReader,
    report: &mut BridgeReport,
    id: &str,
    task: Option<&str>,
    metadata: &[u8],
) -> Result<()> {
    let Ok(doc) = serde_json::from_slice::<serde_json::Value>(metadata) else {
        return Ok(());
    };
    let entries = doc.get("spills").and_then(serde_json::Value::as_array);
    let dir = task.map_or_else(String::new, |t| format!("tasks/{t}/"));
    for entry in entries.into_iter().flatten() {
        let text = |k: &str| entry.get(k).and_then(serde_json::Value::as_str);
        let (Some(tool_use_id), Some(recorded)) = (text("tool_use_id"), text("sha256")) else {
            report.spills_lost += usize::from(text("spill").is_some());
            continue;
        };
        let path = format!("{dir}{SPILLS_DIR}/{tool_use_id}{SPILL_SUFFIX}");
        let reads = sub
            .read_artifact(id, &path)?
            .is_some_and(|body| crate::sha256_hex(&[&body]) == recorded);
        if reads {
            report.spills_sealed += 1;
        } else {
            report.parse_issues += 1;
        }
    }
    Ok(())
}

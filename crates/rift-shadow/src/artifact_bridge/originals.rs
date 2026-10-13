//! Counting a shard's sealed originals of rewritten results on a rebuild.
//!
//! The projection holds no row for an original, as for a spill: the shard is
//! its record. A rebuild stores nothing and only proves each body a manifest
//! claims still reads with its recorded digest, so two rebuilds from a wiped
//! projection count the same originals.

use rift_core::Result;
use rift_recall_substrate::SubstrateReader;

use super::BridgeReport;
use crate::backend::{ORIGINALS_DIR, ORIGINAL_SUFFIX};

/// Count the originals one metadata document's `originals` manifest lists —
/// the session transcript's when `task` is `None`, else that task's. A
/// claimed body that is absent or no longer matches its digest is a parse
/// issue, not an original.
pub(super) fn count_originals(
    sub: &impl SubstrateReader,
    report: &mut BridgeReport,
    id: &str,
    task: Option<&str>,
    metadata: &[u8],
) -> Result<()> {
    let Ok(doc) = serde_json::from_slice::<serde_json::Value>(metadata) else {
        return Ok(());
    };
    let entries = doc.get("originals").and_then(serde_json::Value::as_array);
    let dir = task.map_or_else(String::new, |t| format!("tasks/{t}/"));
    for entry in entries.into_iter().flatten() {
        let text = |k: &str| entry.get(k).and_then(serde_json::Value::as_str);
        let (Some(tool_use_id), Some(recorded)) = (text("tool_use_id"), text("sha256")) else {
            report.originals_unsealed += usize::from(text("original").is_some());
            continue;
        };
        let path = format!("{dir}{ORIGINALS_DIR}/{tool_use_id}{ORIGINAL_SUFFIX}");
        let reads = sub
            .read_artifact(id, &path)?
            .is_some_and(|body| crate::sha256_hex(&[&body]) == recorded);
        if reads {
            report.originals_sealed += 1;
        } else {
            report.parse_issues += 1;
        }
    }
    Ok(())
}

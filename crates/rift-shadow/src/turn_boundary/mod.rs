//! Bounded local working-tree collections attached to turn-boundary events.
//!
//! Event timestamps record when hooks journaled boundaries. The separate
//! projection-only collection audit records a later, non-atomic attempt over
//! current bytes; it cannot reconstruct historical event-time content.
//! Descriptor-relative reads refuse links in source ancestors, retain link
//! payloads without following targets, and address only captured buffers.
//!
//! Current observations lack immutable event-time bytes and causal linkage
//! before a target commit. Capture writes no actor-labelled closed hunks;
//! enrichment preserves commit-diff attribution unchanged and preview returns
//! no percentages. Historical canonical rows remain immutable and may be wrong.
//! The pure attribution replay engine still accepts explicitly supplied edits.

mod capture;
#[cfg(test)]
mod capture_tests;
mod enrich;
mod observation;
mod preview;

use std::collections::BTreeMap;

use rift_core::Result;
use rift_store::{
    latest_turn_boundary_snapshot, turn_boundary_files_for_snapshot, CanonicalStore,
    TurnBoundarySnapshotRow,
};

pub use capture::{boundary_collection_time, capture_boundary, capture_boundary_with_clock};
pub use enrich::enrich_attribution;
pub use preview::attribution_preview;

/// Historical turn-replay method identifier, retained for compatibility.
pub const METHOD_VERSION: &str = "turn-replay@1";

/// Which boundary is being captured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    /// A journaled turn-start event.
    Start,
    /// A journaled turn-end event.
    End,
}

impl Boundary {
    /// The stored boundary token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::End => "end",
        }
    }

    /// Conventional event actor token; this is not evidence of a file writer.
    #[must_use]
    pub fn actor(self) -> &'static str {
        match self {
            Self::Start => "human",
            Self::End => "agent",
        }
    }
}

/// The latest logical event and its associated observation set, selected once.
/// Event ordering does not establish when the stored bytes existed. The
/// projection-only collection companion supplies separate audit timing.
async fn last_boundary(
    store: &impl CanonicalStore,
    session_row: &str,
) -> Result<Option<(TurnBoundarySnapshotRow, BTreeMap<String, String>)>> {
    let Some(snapshot) = latest_turn_boundary_snapshot(store, session_row).await? else {
        return Ok(None);
    };
    let files = turn_boundary_files_for_snapshot(store, &snapshot.snapshot_id).await?;
    Ok(Some((
        snapshot,
        files.into_iter().map(|r| (r.path, r.blob_oid)).collect(),
    )))
}

/// The previous boundary's content map (`path → blob_oid`), or empty if this is
/// the session's first boundary. The capture side needs the bytes only.
#[cfg(test)]
async fn previous_state(
    store: &impl CanonicalStore,
    session_row: &str,
) -> Result<BTreeMap<String, String>> {
    Ok(last_boundary(store, session_row)
        .await?
        .map(|(_, files)| files)
        .unwrap_or_default())
}

//! The catalogue-row seeder the state, catalogue and draw tests share.

use super::model::{ViewerCheckpointRow, ViewerTimestampProvenance};

/// A bare row — unknown time, no origins, unprojected — so each test sets
/// only the facts it asserts on.
pub(super) fn seeded_row(id: &str, search: &str) -> ViewerCheckpointRow {
    ViewerCheckpointRow {
        checkpoint_id: id.to_owned(),
        effective_at_ms: None,
        timestamp_provenance: ViewerTimestampProvenance::Unknown,
        compact_utc: None,
        excerpt: None,
        excerpt_loading: false,
        anchor_commit: None,
        origins: Vec::new(),
        chain_position: None,
        local_branches: Vec::new(),
        strategy: None,
        projection: "unprojected".into(),
        issues: Vec::new(),
        metadata_search: search.to_owned(),
        worktree: None,
    }
}

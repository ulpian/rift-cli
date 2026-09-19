//! The lifecycle card: the condensation decision the checkpoint metadata
//! records, and the verification witnesses the projection links to it.
//! Absence of a witness is reported, never read as a pass.

use std::path::Path;

use rift_canopy::{ViewerLine, ViewerLineKind, ViewerTimelineItem, ViewerTimelineSegment};
use rift_recall_substrate::{CheckpointCatalogueEntry, TimestampProvenance};
use rift_store::{CanonicalStore, Row, SqliteStore, Value};

use super::catalogue::map_timestamp_provenance;
use super::open_existing_read_only;
use super::text::{compact_utc, fact_line, short, warning_item};

use crate::store::runtime;

pub(super) fn lifecycle_segment(
    projection_path: &Path,
    entry: &CheckpointCatalogueEntry,
) -> ViewerTimelineSegment {
    let decision = decision_item(entry);
    let Some(store) = open_existing_read_only(projection_path) else {
        return unavailable_segment(
            decision,
            "lifecycle-unavailable",
            "capture projection is absent or unreadable",
        );
    };
    let Ok(rt) = runtime() else {
        return unavailable_segment(
            decision,
            "lifecycle-worker",
            "data-worker runtime unavailable",
        );
    };
    let mut items = vec![decision];
    items.extend(witness_items(&rt, &store, &entry.checkpoint_id));
    if items.len() == 1 {
        items.push(warning_item(
            "lifecycle-no-witness",
            "no verification witness",
            "no durable verification run is linked to this condensation; absence is not a pass",
        ));
    }
    items.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| a.id.cmp(&b.id)));
    ViewerTimelineSegment {
        title: "Lifecycle / soak".to_owned(),
        boundary: Some(
            "verification witnesses are sorted only by checked_at; no order is inferred across other segments"
                .to_owned(),
        ),
        ordered_by_time: true,
        items,
    }
}

/// Only a recorded checkpoint timestamp can truthfully timestamp the
/// condensation/archive decision. Session-derived and anchor-commit times
/// remain useful catalogue fallbacks, but are not decision chronology.
fn decision_item(entry: &CheckpointCatalogueEntry) -> ViewerTimelineItem {
    let decision_time = entry.recorded_at_ms.filter(|value| *value >= 0);
    let provenance = if decision_time.is_some() {
        TimestampProvenance::Recorded
    } else {
        TimestampProvenance::Unknown
    };
    ViewerTimelineItem {
        id: "condensation-decision".to_owned(),
        title: "condensation / archive decision".to_owned(),
        timestamp: decision_time.and_then(compact_utc),
        summary: vec![
            fact_line(
                "strategy",
                entry.metadata.strategy.as_deref().unwrap_or("unavailable"),
            ),
            fact_line(
                "timestamp provenance",
                map_timestamp_provenance(provenance).label(),
            ),
            fact_line(
                "decision evidence",
                "checkpoint metadata; no separate decision row is inferred",
            ),
        ],
        body: Vec::new(),
        expanded_body: None,
        collapsed: false,
        full_session_only: false,
    }
}

/// The decision still stands when the witnesses cannot be read; the card
/// says which read failed rather than showing an empty list.
fn unavailable_segment(
    decision: ViewerTimelineItem,
    id: &str,
    reason: &str,
) -> ViewerTimelineSegment {
    ViewerTimelineSegment {
        title: "Lifecycle / soak".to_owned(),
        boundary: Some("ordered only by each verification run's recorded timestamp".to_owned()),
        ordered_by_time: true,
        items: vec![decision, warning_item(id, "lifecycle unavailable", reason)],
    }
}

fn witness_items(
    rt: &tokio::runtime::Runtime,
    store: &SqliteStore,
    checkpoint_id: &str,
) -> Vec<ViewerTimelineItem> {
    let Ok(rows) = rt.block_on(store.query(
        "SELECT vr.run_id, vr.branch, vr.exit_code, vr.checked_at, vr.outcome_sha256, \
                (SELECT COUNT(*) FROM verification_divergence vd WHERE vd.run_id = vr.run_id) \
         FROM verification_witness vw JOIN verification_run vr ON vr.run_id = vw.run_id \
         WHERE vw.checkpoint_id = ?1 \
         ORDER BY vr.checked_at DESC, vr.run_id DESC",
        &[Value::text(checkpoint_id.to_owned())],
    )) else {
        return Vec::new();
    };
    rows.iter().filter_map(witness_item).collect()
}

fn witness_item(row: &Row) -> Option<ViewerTimelineItem> {
    let (Ok(run_id), Ok(branch), Ok(exit_code), Ok(checked_at), Ok(outcome), Ok(divergences)) = (
        row.get_text(0),
        row.get_text(1),
        row.get_i64(2),
        row.get_i64(3),
        row.get_text(4),
        row.get_i64(5),
    ) else {
        return None;
    };
    let mut summary = vec![
        fact_line("branch", &branch),
        fact_line("exit code", &exit_code.to_string()),
        fact_line("outcome digest", &outcome),
        fact_line("divergences", &divergences.to_string()),
    ];
    if divergences > 0 {
        summary.push(ViewerLine::plain(
            ViewerLineKind::Warning,
            "verification recorded one or more divergences",
        ));
    }
    Some(ViewerTimelineItem {
        id: format!("verify:{run_id}"),
        title: format!("verification run {}", short(&run_id)),
        timestamp: compact_utc(checked_at),
        summary,
        body: Vec::new(),
        expanded_body: None,
        collapsed: false,
        full_session_only: false,
    })
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{entry, rich_entry};
    use super::*;

    #[test]
    fn an_absent_projection_keeps_the_decision_and_names_the_gap() {
        let rich = rich_entry(&"a".repeat(40));
        let project = tempfile::tempdir().unwrap();
        let projection = project.path().join("missing-projection.db");
        let lifecycle = lifecycle_segment(&projection, &rich);
        assert!(lifecycle
            .items
            .iter()
            .any(|item| item.id == "lifecycle-unavailable"));
    }

    #[test]
    fn a_projection_without_witnesses_reports_their_absence_never_a_pass() {
        let rich = entry(&"e".repeat(64), &"d".repeat(40));
        let repo = tempfile::tempdir().unwrap();
        let projection = repo.path().join("projection.db");
        std::fs::write(&projection, []).unwrap();
        let lifecycle = lifecycle_segment(&projection, &rich);
        assert!(lifecycle
            .items
            .iter()
            .any(|item| item.id == "lifecycle-no-witness" || item.id == "lifecycle-unavailable"));
    }
}

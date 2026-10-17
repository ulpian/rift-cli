//! Gap audit projection specs; neither table alone authorizes ticket retirement.
use super::{CanonicalTable, Column, NO_UNIQUE};

pub(crate) const BATCH: CanonicalTable = CanonicalTable {
    name: "turn_boundary_gap_batch",
    columns: &[
        Column::text("gap_id"),
        Column::text("repo_id"),
        Column::text("member_key"),
        Column::text("selection_sha256"),
        Column::text("evidence_sha256"),
        Column::int("ticket_count"),
        Column::int("recorded_at"),
        Column::text_one_of("reason", &["uncaptured-backlog"]),
        Column::int("schema_version"),
    ],
    primary_key: &["gap_id"],
    unique: NO_UNIQUE,
};

pub(crate) const TICKET: CanonicalTable = CanonicalTable {
    name: "turn_boundary_gap_ticket",
    columns: &[
        Column::text("gap_id"),
        Column::text("source_key"),
        Column::text("context_sha256"),
        Column::text("ticket_sha256"),
        Column::text("session_key"),
        Column::int("turn_ordinal"),
        Column::text_one_of("boundary", &["start", "end"]),
        Column::text_null("head_oid"),
        Column::int("captured_at"),
        Column::int("queued_at"),
        Column::int("attempts"),
        Column::int("schema_version"),
    ],
    primary_key: &["gap_id", "source_key"],
    unique: NO_UNIQUE,
};

pub(crate) const BOUNDARY_GAP_TABLES: [CanonicalTable; 2] = [BATCH, TICKET];
pub(crate) const BOUNDARY_GAP_CONTEXT_INDEX_SQL: &str =
    "CREATE INDEX IF NOT EXISTS idx_turn_boundary_gap_context \
     ON turn_boundary_gap_ticket(context_sha256, gap_id);\n";

//! Typed, append-only writers for the capture-projection relations.
//!
//! These mirror the DDL in [`crate::schema`] and use [`CanonicalStore::insert_or_ignore`]
//! so re-ingesting the same content-addressed fact is a **no-op** (idempotent
//! set-union, db-design §4.4/§4.7). They never UPDATE/DELETE — corrections are new
//! superseding rows handled by the event bridge.
//!
//! Covers the full normalized capture model: the diff-capture relations,
//! the session/turn/model-call/tool-call decomposition, token usage,
//! the turn-boundary relations, the ingest watermark, and the hook and
//! retired-wrapper audit relations. Ids are content-addressed by the caller
//! (`rift-shadow`'s shared id-derivation module), never here.
//!
//! One file per relation family; every writer and reader keeps its path at
//! `rift_store::capture::…` through the re-exports below.
//!
//! [`CanonicalStore::insert_or_ignore`]: crate::CanonicalStore::insert_or_ignore

mod attribution;
mod cost;
mod enablement;
mod files;
#[cfg(test)]
mod fixtures;
mod observations;
mod sessions;
mod tools;
mod turns;
mod usage_windows;
mod verification;
mod watermarks;
mod worktrees;

pub use attribution::{
    confidence_to_milli, record_attribution, record_attribution_hunk, AttributionHunkRow,
    AttributionRow,
};
pub use cost::{
    checkpoint_window_tokens, effective_prices, quote_usage_costs, record_price,
    record_token_usage, session_usage_facts, CostQuoteScope, EffectivePrices, PriceTableRow,
    SessionUsageFact, TokenUsageRow, WindowTokens,
};
pub use enablement::{
    latest_archive_ms, record_agent_enablement, record_agent_hook_event, record_archive_decision,
    record_captured_event, record_substrate_block_event, AgentEnablementRow, AgentHookEventRow,
    ArchiveDecisionRow, CapturedEventRow, SubstrateBlockEventRow,
};
pub use files::{record_changed_hunk, record_file_revision, ChangedHunkRow, FileRevisionRow};
pub use observations::{
    record_main_context_observation, record_model_observation, MainContextObservationRow,
    ModelObservationRow,
};
pub use sessions::{
    record_capture_session_identity, record_session, record_session_event,
    record_session_in_checkpoint, record_session_transition, record_subagent_session,
    CaptureSessionIdentityRow, SessionEventRow, SessionInCheckpointRow, SessionRow,
    SessionTransitionRow, SubagentSessionRow,
};
pub use tools::{
    content_blob_oid_map, record_file_read_tokens, record_tool_call, record_tool_edits_file,
    record_tool_reads_file, record_tool_result_tokens, session_read_facts, SessionReadFact,
    ToolCallRow,
};
pub use turns::{
    latest_turn_boundary_files, latest_turn_boundary_snapshot, record_model_call, record_turn,
    record_turn_boundary_file, record_turn_boundary_snapshot, record_turn_diff_hunk,
    turn_boundary_counts, turn_boundary_files_for_session_path, turn_boundary_files_for_snapshot,
    turn_diff_hunks_for_session, ModelCallRow, TurnBoundaryFileRow, TurnBoundarySnapshotRow,
    TurnDiffHunkRow, TurnRow,
};
pub use usage_windows::{checkpoint_usage_windows, UsageWindow, WindowScope};
pub use verification::{
    record_verification_divergence, record_verification_run, record_verification_witness,
    VerificationDivergenceRow, VerificationRunRow, VerificationWitnessRow,
};
pub use watermarks::{
    latest_ingest_watermark, latest_ingest_watermark_with_position, record_ingest_watermark,
    record_ingest_watermark_position, IngestWatermarkRow,
};
pub use worktrees::{
    record_checkpoint_worktree_if_new, record_session_worktree, SessionWorktreeRow,
};

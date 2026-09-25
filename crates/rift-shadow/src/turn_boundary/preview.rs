//! The pre-commit preview: LIFO tallies over the session's turn hunks alone.

use rift_core::Result;
use rift_shadow_attribution::replay::{replay, TurnEdit};
use rift_shadow_attribution::Actor;
use rift_shadow_hooks::trailers::AttributionSummary;
use rift_store::{turn_boundary_counts, turn_diff_hunks_for_session, CanonicalStore};

use super::stack::edit_stacks;
use crate::ids;

/// The pre-commit attribution preview the `prepare-commit-msg` trailer renders:
/// LIFO tallies over the session's turn hunks alone (no committed diff yet).
/// `None` when the session has produced no turn edits.
///
/// # Errors
/// [`rift_core::RiftError`] on a storage failure.
pub async fn attribution_preview(
    store: &impl CanonicalStore,
    agent: &str,
    native_session: &str,
) -> Result<Option<AttributionSummary>> {
    let session_row = ids::session_row_id(agent, native_session);
    let rows = turn_diff_hunks_for_session(store, &session_row).await?;
    if rows.is_empty() {
        return Ok(None);
    }
    let (starts, ends) = turn_boundary_counts(store, &session_row).await?;

    let mut agent_lines: u64 = 0;
    let mut human_lines: u64 = 0;
    for (_path, keyed) in edit_stacks(&rows) {
        // The preview is pre-commit: there is no parent to rebase on yet, so
        // the whole stack replays and the keys are not needed.
        let edits: Vec<TurnEdit> = keyed.into_iter().map(|(_, e)| e).collect();
        // Replay over a generous base so session-relative coordinates fit; only
        // session-written lines (non-empty histories) are tallied.
        let max_extent = edits
            .iter()
            .map(|e| e.hunk.old_start.saturating_add(e.hunk.old_len))
            .max()
            .unwrap_or(0);
        let replayed = replay(max_extent, &edits);
        for h in &replayed.histories {
            match h.last() {
                None => {}
                Some(Actor::Agent) => agent_lines += 1,
                Some(Actor::Human) => human_lines += 1,
            }
        }
    }
    let total = agent_lines + human_lines;
    if total == 0 {
        return Ok(None);
    }
    let pct = |n: u64| u8::try_from(n * 100 / total).unwrap_or(100);
    // Confidence: the whole-turn granularity factor, scaled by boundary
    // coverage (every started turn also observed its end).
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let confidence_pct = if starts == 0 {
        0u8
    } else {
        ((ends as f64 / starts as f64).clamp(0.0, 1.0) * 80.0).round() as u8
    };
    Ok(Some(AttributionSummary {
        agent_pct: pct(agent_lines),
        human_pct: pct(human_lines),
        confidence_pct,
    }))
}

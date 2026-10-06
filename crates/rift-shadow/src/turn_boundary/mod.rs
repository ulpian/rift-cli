//! Turn-boundary working-tree snapshots and the H_i/A_i attribution
//! enrichment.
//!
//! Two halves:
//!
//! * [`capture_boundary`] runs at a turn boundary (`start` closes the human's
//!   H_i window, `end` closes the agent's A_i window): it observes the working
//!   set (every tracked path differing from HEAD plus everything the previous
//!   boundary observed), diffs each file against the previous boundary's
//!   content, and persists append-only `turn_boundary_snapshot` /
//!   `turn_boundary_file` / `turn_diff_hunk` rows. File **bytes** go only into
//!   the local git object store (`hash-object -w`, the same trust class as the
//!   shadow branch); the projection gets hashes and hunk coordinates.
//!
//! * [`enrich_attribution`] runs at commit time: it replays the session's
//!   ordered `[H_0, A_0, H_1, A_1, …]` hunk stack (plus the synthesized final
//!   human window `last boundary → committed`) through the LIFO engine and
//!   replaces the commit capture's all-`unknown` attribution with real
//!   agent/human counts, per-hunk category rows, and a confidence that rises
//!   with capture completeness. [`attribution_preview`] is the pre-commit
//!   flavor the `prepare-commit-msg` trailer renders.
//!
//! Each half is its own module — `capture` for the write side, `enrich` for
//! the commit side and `preview` for its pre-commit flavor — and `stack`
//! holds the per-file edit stacks both replays read, so a change to how rows
//! are keyed or rebased has one home.
//!
//! # Why a line still answers `unknown` — the causes and their status (2026-08-29)
//!
//! Measured on real repositories (blame unknown on 96 of 96 lines), five causes
//! kept the replay from naming a writer. Each is either closed in this tree or
//! deliberately left, and this list is the register:
//!
//! * **A — trailer/anchor divergence.** A transcript-carrying session's
//!   predicted trailer never lands; the reader resolves through the union of
//!   the commit's claims and the shard's recorded anchor. CLOSED, and the
//!   resolution is permanent.
//! * **B — the `-1` created-at sentinel** ranking as resolved metadata. CLOSED.
//! * **C — End-only adapters never advanced their turn ordinal** (Antigravity:
//!   `Stop` closes a turn nothing opened), so every Stop after the first was
//!   dropped by the insert-or-ignore snapshot key and `open_window_actor`
//!   never resolved. CLOSED: adapters declare `rift_agent::TurnSignals` and
//!   the bridge synthesizes the missing Start (`rift-shadow::bridge`).
//! * **D — a file committed mid-session** replayed its already-consumed hunks
//!   again, over-lengthened the model, and degraded to `unknown`. CLOSED:
//!   the boundary that observed the commit's parent blob is the watermark
//!   (`consumed_through` / `rebase_on_parent` in `stack`).
//! * **E — boundary-snapshot failures were swallowed** on the hook path, so a
//!   session could lose its boundaries without a word. CLOSED: the failure is
//!   said once through the capture queue's note channel and `rift doctor`
//!   reports it (`bins/rift/src/hooks.rs::snapshot_boundaries`).
//!
//! Left by construction: **imports carry no boundary** — `rift import` lands
//! a read-only transcript with no working-tree observation, so its lines stay
//! `unknown` and the checkpoint says `import`. Left for a later pin: boundary
//! anchor blobs are unreachable objects and **gc-able** (`absent anchor →
//! unreadable → not an anchor` in `enrich`); a `refs/rift/anchors/` pin would keep
//! them, and is not built. Installer drift is already pinned elsewhere.

mod capture;
#[cfg(test)]
mod capture_tests;
mod enrich;
mod preview;
mod stack;

use std::collections::BTreeMap;

use rift_core::Result;
use rift_store::{
    latest_turn_boundary_snapshot, turn_boundary_files_for_snapshot, CanonicalStore,
    TurnBoundarySnapshotRow,
};

pub use capture::capture_boundary;
pub use enrich::enrich_attribution;
pub use preview::attribution_preview;

/// The attribution method version stamped on turn-replay rows (provenance).
pub const METHOD_VERSION: &str = "turn-replay@1";

/// Which boundary is being captured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    /// Turn start — closes the human's between-turns window (H_i).
    Start,
    /// Turn end — closes the agent's within-turn window (A_i).
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

    /// Whose edit window this boundary closes.
    #[must_use]
    pub fn actor(self) -> &'static str {
        match self {
            Self::Start => "human",
            Self::End => "agent",
        }
    }

    /// The `turn_diff_hunk.edit_kind` this boundary's diffs carry.
    #[must_use]
    fn edit_kind(self) -> &'static str {
        self.actor()
    }
}

/// The session's last boundary — **the row and the bytes it anchors, from ONE
/// ordered read.**
///
/// `None` when the session has no boundary at all. The content map is
/// `path → blob_oid` for exactly the returned snapshot, so a caller that needs
/// both the window's actor (the row's `boundary` token) and the anchor bytes
/// takes them from the same snapshot **by construction** rather than by running
/// the selection twice.
///
/// The selection itself lives in `rift-store`
/// ([`latest_turn_boundary_snapshot`]) and is not restated here. It used to be:
/// this function's `ORDER BY` and the store's differed only by a table alias,
/// so "actor and anchor come from the same snapshot" rested on two clauses in
/// two crates happening to match. Ties are impossible today, so nothing would
/// have caught a drift.
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
async fn previous_state(
    store: &impl CanonicalStore,
    session_row: &str,
) -> Result<BTreeMap<String, String>> {
    Ok(last_boundary(store, session_row)
        .await?
        .map(|(_, files)| files)
        .unwrap_or_default())
}

//! P4 S2 (pulled into M1) — turn-boundary working-tree snapshots and the
//! H_i/A_i attribution enrichment.
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

use std::collections::BTreeMap;
use std::path::Path;

use rift_core::Result;
use rift_git::{BlobReader, RepoPaths};
use rift_shadow_attribution::replay::{committed_writers, replay, TurnEdit};
use rift_shadow_attribution::{
    attribute, confidence, diff_attributable, factors_from_signals, Actor, CommitCapture,
    HunkAttribution,
};
use rift_shadow_hooks::trailers::AttributionSummary;
use rift_store::{
    latest_turn_boundary_files, record_turn_boundary_file, record_turn_boundary_snapshot,
    record_turn_diff_hunk, turn_boundary_counts, turn_diff_hunks_for_session, CanonicalStore,
    TurnBoundaryFileRow, TurnBoundarySnapshotRow, TurnDiffHunkRow,
};

use crate::ids;

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

/// The previous boundary's content map (`path → blob_oid`), or empty if this is
/// the session's first boundary.
async fn previous_state(
    store: &impl CanonicalStore,
    session_row: &str,
) -> Result<BTreeMap<String, String>> {
    Ok(latest_turn_boundary_files(store, session_row)
        .await?
        .into_iter()
        .map(|r| (r.path, r.blob_oid))
        .collect())
}

/// Read one worktree file (empty when deleted/absent). Only regular files are
/// observed; anything unreadable is treated as absent.
fn worktree_bytes(work_tree: &Path, rel: &str) -> Vec<u8> {
    std::fs::read(work_tree.join(rel)).unwrap_or_default()
}

fn sha256_hex_of(bytes: &[u8]) -> String {
    crate::sha256_hex(&[bytes])
}

/// Capture one turn boundary: snapshot the working set and persist the closed
/// window's H_i/A_i hunks. Idempotent per `(session, ordinal, boundary)`.
///
/// # Errors
/// [`rift_core::RiftError`] on a git or storage failure (callers at the hook
/// boundary swallow it — capture never blocks the agent).
#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // one linear observe→diff→persist pass
pub async fn capture_boundary(
    store: &impl CanonicalStore,
    repo: &RepoPaths,
    reader: &impl BlobReader,
    head_commit: Option<&str>,
    agent: &str,
    native_session: &str,
    turn_ordinal: u32,
    boundary: Boundary,
    now_ms: i64,
) -> Result<()> {
    let session_row = ids::session_row_id(agent, native_session);
    let prev = previous_state(store, &session_row).await?;

    // The observation set: everything currently differing from HEAD, plus every
    // path the previous boundary observed (a file reverted since then still
    // needs its closing diff).
    let mut paths: Vec<String> = rift_git::changed_worktree_paths(repo)?;
    paths.extend(prev.keys().cloned());
    paths.sort();
    paths.dedup();

    // HEAD tree for baseline reads (first boundary of a path).
    let head_tree: BTreeMap<String, String> = match head_commit {
        Some(head) => reader
            .tree_entries_at(head)?
            .into_iter()
            .map(|e| (e.path, e.blob_oid))
            .collect(),
        None => BTreeMap::new(),
    };

    let snapshot_id = ids::turn_boundary_snapshot_id(&session_row, turn_ordinal, boundary.as_str());
    let mut observed: Vec<(String, String, String)> = Vec::new(); // (path, blob_oid, sha256)

    for path in &paths {
        let cur = worktree_bytes(&repo.work_tree, path);
        let prev_bytes = match prev.get(path) {
            Some(oid) => reader.read_blob(oid)?.unwrap_or_default(),
            None => match head_tree.get(path) {
                Some(oid) => reader.read_blob(oid)?.unwrap_or_default(),
                None => Vec::new(),
            },
        };
        if cur == prev_bytes {
            // Unchanged within this window; still part of the boundary state if
            // it differs from HEAD (so the next window diffs against it).
            if head_tree.get(path).map(String::as_str)
                != Some(prev.get(path).map_or("", String::as_str))
                && prev.contains_key(path)
            {
                observed.push((
                    path.clone(),
                    prev.get(path).cloned().unwrap_or_default(),
                    sha256_hex_of(&cur),
                ));
            }
            continue;
        }

        // The closed window's hunks for this file (binary/oversize → no hunks,
        // the file still joins the boundary state).
        if let Some(hunks) = diff_attributable(&prev_bytes, &cur) {
            for h in hunks {
                record_turn_diff_hunk(
                    store,
                    &TurnDiffHunkRow {
                        hunk_id: ids::turn_diff_hunk_id(
                            &session_row,
                            turn_ordinal,
                            boundary.edit_kind(),
                            path,
                            h.old_start,
                            h.old_len,
                            h.new_start,
                            h.new_len,
                        ),
                        session_id: session_row.clone(),
                        turn_ordinal: i64::from(turn_ordinal),
                        edit_kind: boundary.edit_kind().to_owned(),
                        path: path.clone(),
                        old_start: i64::from(h.old_start),
                        old_len: i64::from(h.old_len),
                        new_start: i64::from(h.new_start),
                        new_len: i64::from(h.new_len),
                        schema_version: 1,
                    },
                )
                .await?;
            }
        }

        // The file's bytes at this boundary go into the local object store; the
        // projection gets only the addresses.
        let blob_oid = rift_git::hash_object_write(repo, &cur)?;
        observed.push((path.clone(), blob_oid, sha256_hex_of(&cur)));
    }

    // The whole-boundary content hash: over the sorted (path, sha256) set.
    let mut acc: Vec<u8> = Vec::new();
    for (path, _oid, sha) in &observed {
        acc.extend_from_slice(path.as_bytes());
        acc.push(0);
        acc.extend_from_slice(sha.as_bytes());
        acc.push(b'\n');
    }
    let worktree_hash = sha256_hex_of(&acc);

    record_turn_boundary_snapshot(
        store,
        &TurnBoundarySnapshotRow {
            snapshot_id: snapshot_id.clone(),
            session_id: session_row.clone(),
            turn_ordinal: i64::from(turn_ordinal),
            boundary: boundary.as_str().to_owned(),
            actor: boundary.actor().to_owned(),
            worktree_hash,
            captured_at: now_ms,
            schema_version: 1,
        },
    )
    .await?;
    for (path, blob_oid, content_sha256) in observed {
        record_turn_boundary_file(
            store,
            &TurnBoundaryFileRow {
                snapshot_id: snapshot_id.clone(),
                path,
                blob_oid,
                content_sha256,
                schema_version: 1,
            },
        )
        .await?;
    }
    Ok(())
}

/// Count the lines of a byte buffer the way the diff engine does.
#[allow(clippy::naive_bytecount)] // tiny inputs; a bytecount dep is not warranted
fn line_count(bytes: &[u8]) -> u32 {
    if bytes.is_empty() {
        return 0;
    }
    let n = bytes.iter().filter(|b| **b == b'\n').count();
    let tail = usize::from(!bytes.ends_with(b"\n"));
    u32::try_from(n + tail).unwrap_or(u32::MAX)
}

/// Convert stored rows into the replay's per-file edit stacks (rows arrive in
/// replay order from the store query).
fn edit_stacks(rows: &[TurnDiffHunkRow]) -> BTreeMap<String, Vec<TurnEdit>> {
    let mut map: BTreeMap<String, Vec<TurnEdit>> = BTreeMap::new();
    for r in rows {
        let actor = if r.edit_kind == "agent" {
            Actor::Agent
        } else {
            Actor::Human
        };
        let clamp = |v: i64| u32::try_from(v).unwrap_or(0);
        map.entry(r.path.clone()).or_default().push(TurnEdit {
            actor,
            hunk: rift_shadow_attribution::Hunk {
                old_start: clamp(r.old_start),
                old_len: clamp(r.old_len),
                new_start: clamp(r.new_start),
                new_len: clamp(r.new_len),
            },
        });
    }
    map
}

/// Enrich a commit capture with turn-boundary attribution: replay each file's
/// `[H_0, A_0, …]` stack plus the synthesized final human window (last boundary
/// → committed content), score it through the LIFO engine, and replace the
/// all-`unknown` commit-boundary attribution. Files without any session edits
/// keep their honest `unknown` counts.
///
/// # Errors
/// [`rift_core::RiftError`] on a storage or blob-read failure.
pub async fn enrich_attribution(
    store: &impl CanonicalStore,
    reader: &impl BlobReader,
    agent: &str,
    native_session: &str,
    capture: &mut CommitCapture,
) -> Result<()> {
    let session_row = ids::session_row_id(agent, native_session);
    let rows = turn_diff_hunks_for_session(store, &session_row).await?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut stacks = edit_stacks(&rows);
    let boundary_state = previous_state(store, &session_row).await?;
    let (starts, ends) = turn_boundary_counts(store, &session_row).await?;
    // Coverage: a fully-bracketed session has an `end` for every `start`.
    let coverage = if starts == 0 {
        0.0
    } else {
        #[allow(clippy::cast_precision_loss)]
        let c = ends as f64 / starts as f64;
        c.clamp(0.0, 1.0)
    };

    for f in &mut capture.files {
        if !f.attributable {
            continue;
        }
        let Some(mut edits) = stacks.remove(&f.path) else {
            continue; // untouched by the session → stays unknown
        };
        let committed = reader.read_blob(&f.head_blob_oid)?.unwrap_or_default();
        let committed_len = line_count(&committed);

        // Synthesize the final human window: last observed boundary state →
        // the committed content (the human's staging/tweaks before commit).
        let last_bytes = match boundary_state.get(&f.path) {
            Some(oid) => reader.read_blob(oid)?.unwrap_or_default(),
            None => Vec::new(),
        };
        if boundary_state.contains_key(&f.path) && last_bytes != committed {
            if let Some(hunks) = diff_attributable(&last_bytes, &committed) {
                edits.extend(hunks.into_iter().map(|hunk| TurnEdit {
                    actor: Actor::Human,
                    hunk,
                }));
            }
        }

        // The replay base is the commit's parent-side line count (exact from
        // diff conservation: parent = committed − net insertions).
        let net: i64 = f
            .hunks
            .iter()
            .map(|h| i64::from(h.new_len) - i64::from(h.old_len))
            .sum();
        let base_len = u32::try_from(i64::from(committed_len) - net).unwrap_or(0);

        let replayed = replay(base_len, &edits);
        let writers = committed_writers(&replayed, &f.hunks, committed_len);
        let attribution = attribute(&writers, replayed.agent_removed_by_human);

        let unknown_ratio = if attribution.total_committed() == 0 {
            1.0
        } else {
            f64::from(attribution.unknown_lines) / f64::from(attribution.total_committed())
        };
        let factors = factors_from_signals(unknown_ratio, false, false, false, true);
        let file_confidence = (confidence(&factors) * coverage.max(0.5)).clamp(0.0, 1.0);

        // Per-hunk category rows (the irreducible facts the rollup sums).
        let mut hunk_categories = Vec::new();
        let mut cursor = 0usize;
        for (ordinal, h) in f.hunks.iter().enumerate() {
            let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
            for w in writers.iter().skip(cursor).take(h.new_len as usize) {
                let cat = match w.last() {
                    None => "unknown",
                    Some(Actor::Agent) => "agent_added",
                    Some(Actor::Human) => {
                        if w.contains(&Actor::Agent) {
                            "human_modified_agent"
                        } else {
                            "human_added"
                        }
                    }
                };
                *counts.entry(cat).or_default() += 1;
            }
            cursor += h.new_len as usize;
            for (category, line_count) in counts {
                hunk_categories.push(HunkAttribution {
                    hunk_ordinal: u32::try_from(ordinal).unwrap_or(u32::MAX),
                    new_start: h.new_start,
                    new_len: h.new_len,
                    category: category.to_owned(),
                    line_count,
                });
            }
        }

        f.attribution = attribution;
        f.confidence = file_confidence;
        f.hunk_categories = hunk_categories;
        METHOD_VERSION.clone_into(&mut f.method_version);
    }
    Ok(())
}

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
    for (_path, edits) in edit_stacks(&rows) {
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

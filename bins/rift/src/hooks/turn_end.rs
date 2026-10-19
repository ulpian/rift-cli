//! The turn-end checkpoint (L-5 SealTurn ruling, 2026-10-18): a `Stop`
//! journals a [`SealTurn`] and the worker mints one checkpoint per turn end
//! through the interval-archive road, under its own reason `turn-end`.
//!
//! The full transcript is sealed by [`seal_turn`] over the session's previous
//! shard, read back from git with each part's blob oid, so only the turn's new
//! lines pass the gate. The commit anchor is absent when HEAD is unborn or is
//! the commit the previous checkpoint already anchors to (Q10). No turn end is
//! ever synthesised (Q11).

pub(crate) mod frontier;

use std::path::Path;

use rift_core::{Result, RiftError, SecretBytes};
use rift_recall_substrate::parse::segments::{
    compact_segment_name, declared_layout, segment_name, Declared, COMPACT_SEGMENTS_KEY,
    SEGMENT_PACKS_KEY,
};
use rift_shadow::backend::ArchivedTranscript;
use rift_shadow::hooks::strategy::{ArchiveDecision, ArchiveReason, StrategyKind};
use rift_shadow::turn_seal::{
    seal_turn, seal_turn_compact, CompactCut, PriorChain, PriorPart, TurnSeal,
};
use rift_store::CanonicalStore as _;

use crate::agent_queue::{AgentJob, SealTurn};
use frontier::SealRead;

/// The transcript's length now, or `None` with no transcript on record: the
/// hook's frontier, read by one stat and no store.
pub(super) fn transcript_frontier(git_dir: &Path, session: &str) -> Option<u64> {
    let path = transcript_path(git_dir, session)?;
    std::fs::metadata(path).ok().map(|m| m.len())
}

/// Mint the checkpoint one turn end owes. A failure names the session, so a
/// kept job's note says whose turn it was; a busy or deferred cause passes
/// through typed.
pub(super) fn turn_end_job(
    t: &crate::hooktime::HookTimer,
    job: &AgentJob,
    seal: &SealTurn,
) -> Result<bool> {
    mint_turn_end(t, job, seal).map_err(|e| {
        if matches!(e, RiftError::ProjectionBusy { .. }) || crate::capture_queue::is_deferred(&e) {
            return e;
        }
        RiftError::Other(format!(
            "turn-end checkpoint of session {}: {e}",
            seal.session_id
        ))
    })
}

/// Replace `pair.full` with the turn's segmented seal — and, when the turn
/// appended a part, `pair.compact` with the compact form cut at the same
/// line — and say what it read, for [`frontier::record`] once the shard lands.
/// Whatever cannot be read leaves the whole seal the archive already holds
/// and says nothing; a refused chain keeps the whole compact form too.
pub(super) fn seal_turn_full(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    session: &str,
    packs: &rift_shadow::redact::RulePackSet,
    pair: &mut ArchivedTranscript,
) -> Option<SealRead> {
    let raw = transcript_path(&repo.git_dir, session).and_then(|p| std::fs::read(p).ok())?;
    let read = raw.len();
    let raw = SecretBytes::new(raw);
    let last = rt.block_on(last_checkpoint(store, session));
    let prior = last
        .as_deref()
        .and_then(|id| prior_chain(&repo.work_tree, id));
    let before = prior.as_ref().map_or(0, |c| c.raw_lines.iter().sum());
    let seal = seal_turn(&raw, prior, packs).ok()?;
    let said = SealRead::of(read, &seal);
    match seal {
        TurnSeal::Appended {
            body,
            document_lines,
            ..
        } => {
            let cut = CompactCut {
                before,
                now: document_lines,
            };
            let earlier = last
                .as_deref()
                .and_then(|id| prior_compact(&repo.work_tree, id));
            if let Ok(compact) = seal_turn_compact(&raw, cut, earlier, packs) {
                // The compact form now ends where the full parts do, so the
                // slice start can be no later than its last record: rounding
                // down keeps the slice's rule of erring toward inclusion.
                let records = compact.as_slice().split_inclusive(|b| *b == b'\n').count();
                pair.compact_start = pair.compact_start.min(records);
                pair.compact = compact;
            }
            pair.full = body;
        }
        TurnSeal::Whole { body, .. } => pair.full = body,
        TurnSeal::Unchanged => {}
    }
    Some(said)
}

/// [`turn_end_job`]'s work: the anchor, then the archive the interval road runs.
fn mint_turn_end(t: &crate::hooktime::HookTimer, job: &AgentJob, seal: &SealTurn) -> Result<bool> {
    let repo = rift_git::discover(&job.work_tree)?;
    let reader = rift_git::GixBlobReader::open(&repo.work_tree)?;
    let head = reader.head_hex()?;
    let store = rift_store::open_capture_projection(super::projection_db_at(&repo.work_tree)?)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(RiftError::from)?;
    let anchored = match rt.block_on(last_checkpoint(&store, &seal.session_id)) {
        Some(id) => rt.block_on(anchored_commit(&store, &id)),
        None => None,
    };
    let anchor = head.filter(|h| anchored.as_deref() != Some(h.as_str()));
    let decision = ArchiveDecision {
        archive: true,
        reason: ArchiveReason::TurnEnd,
        strategy: StrategyKind::Scheduled,
    };
    let checkpoint = t.phase("archive", || {
        super::archive::archive_now(
            &rt,
            &store,
            &repo.work_tree,
            anchor.as_deref(),
            &seal.session_id,
            &seal.agent,
            decision,
            job.queued_at_ms,
        )
    })?;
    Ok(checkpoint.is_some())
}

/// The transcript address the session cache keeps.
fn transcript_path(git_dir: &Path, session: &str) -> Option<String> {
    let dir = crate::family::run_state::sessions_dir(git_dir);
    rift_shadow::hooks::read_cache(&dir, session)
        .ok()
        .flatten()?
        .transcript_path
}

/// The session's newest archived checkpoint.
async fn last_checkpoint(store: &rift_store::SqliteStore, session: &str) -> Option<String> {
    let rows = store
        .query(
            "SELECT checkpoint_id FROM archive_decision \
             WHERE session_id = ?1 AND archived = 1 AND checkpoint_id IS NOT NULL \
             ORDER BY decided_at DESC, decision_id DESC LIMIT 1",
            &[rift_store::Value::text(session.to_owned())],
        )
        .await
        .ok()?;
    rows.first()?.get_text(0).ok()
}

/// The commit a checkpoint anchors to, if any.
async fn anchored_commit(store: &rift_store::SqliteStore, checkpoint: &str) -> Option<String> {
    let rows = store
        .query(
            "SELECT commit_oid FROM checkpoint_commit WHERE checkpoint_id = ?1 LIMIT 1",
            &[rift_store::Value::text(checkpoint.to_owned())],
        )
        .await
        .ok()?;
    rows.first()?.get_text(0).ok()
}

/// What the previous shard recorded of its segmented transcript, each part
/// with the oid its tree entry names; `None` for a whole-blob shard. A part
/// the tree lacks reads as empty, which `seal_turn` refuses by line count.
fn prior_chain(work_tree: &Path, checkpoint: &str) -> Option<PriorChain> {
    let walker = rift_recall_substrate::ShardWalker::open(work_tree).ok()?;
    let bytes = walker
        .read_artifact_any(checkpoint, "metadata.json")
        .ok()??;
    let meta: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let Declared::Segments(raw_lines) = declared_layout(&meta) else {
        return None;
    };
    let parts: Vec<PriorPart> = (0..raw_lines.len())
        .map(|i| {
            let (oid, bytes) = walker
                .read_native_entry(checkpoint, &segment_name(i))
                .ok()
                .flatten()
                .unwrap_or_default();
            PriorPart { oid, bytes }
        })
        .collect();
    let joined: Vec<u8> = parts.iter().flat_map(|p| p.bytes.iter().copied()).collect();
    Some(PriorChain {
        leaf: rift_hashchain::leaf_hash(&joined),
        packs: meta
            .get(SEGMENT_PACKS_KEY)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        fallbacks: rift_shadow::backend::segment_fallbacks(&meta),
        raw_lines,
        parts,
    })
}

/// The previous shard's compact parts, each with the oid its tree entry
/// names; `None` for a whole compact form. A part the tree lacks reads as
/// empty, which the compact seal refuses by its record count.
fn prior_compact(work_tree: &Path, checkpoint: &str) -> Option<Vec<PriorPart>> {
    let walker = rift_recall_substrate::ShardWalker::open(work_tree).ok()?;
    let bytes = walker
        .read_artifact_any(checkpoint, "metadata.json")
        .ok()??;
    let meta: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let count = usize::try_from(meta.get(COMPACT_SEGMENTS_KEY)?.as_u64()?).ok()?;
    let parts = (0..count).map(|i| {
        let (oid, bytes) = walker
            .read_native_entry(checkpoint, &compact_segment_name(i))
            .ok()
            .flatten()
            .unwrap_or_default();
        PriorPart { oid, bytes }
    });
    Some(parts.collect())
}

#[cfg(test)]
mod tests;

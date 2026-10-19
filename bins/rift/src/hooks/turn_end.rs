//! The turn-end checkpoint (L-5 SealTurn ruling, 2026-10-18): a `Stop`
//! journals a [`SealTurn`] and the worker mints one checkpoint per turn end
//! through the interval-archive road, under its own reason `turn-end`.
//!
//! The full transcript is sealed by [`rift_shadow::turn_seal::seal_turn`] over the session's previous
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
use rift_shadow::backend::{ArchivedTranscript, RedactionFallback};
use rift_shadow::hooks::strategy::{ArchiveDecision, ArchiveReason, StrategyKind};
use rift_shadow::turn_seal::{
    seal_turn_compact, seal_turn_through, CompactCut, PriorChain, PriorPart, TurnSeal,
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

/// What a turn-end seal reads from beyond the transcript bytes.
pub(super) struct TurnInputs<'a> {
    pub(super) rt: &'a tokio::runtime::Runtime,
    pub(super) store: &'a rift_store::SqliteStore,
    pub(super) repo: &'a rift_git::RepoPaths,
    pub(super) session: &'a str,
    pub(super) packs: &'a rift_shadow::redact::RulePackSet,
}

/// A turn-end archive's transcript pair, built from the turn's segmented seal
/// alone: the main transcript's earlier lines are carried from the previous
/// shard and never pass the gate again (ruling "L-5 turn-end assembly must not
/// re-redact"). Returns the pair, the raw-line count and what the seal read,
/// for [`frontier::record`] once the shard lands. `None` when the seal fails,
/// and the caller then seals the whole file as every other road does.
pub(super) fn seal_turn_pair(
    inputs: &TurnInputs<'_>,
    raw: &SecretBytes,
    start_line: usize,
) -> Option<(ArchivedTranscript, u64, SealRead)> {
    let mut gate = |bytes: &SecretBytes, first_line: usize| {
        rift_shadow::redact::redact_with_at(bytes, inputs.packs, first_line)
    };
    seal_turn_pair_through(inputs, raw, start_line, &mut gate)
}

/// [`seal_turn_pair`] over an injected gate for the main transcript's full
/// form, so a test can count the bytes that enter it.
fn seal_turn_pair_through(
    inputs: &TurnInputs<'_>,
    raw: &SecretBytes,
    start_line: usize,
    gate: &mut rift_shadow::turn_seal::Gate<'_>,
) -> Option<(ArchivedTranscript, u64, SealRead)> {
    let last = inputs
        .rt
        .block_on(last_checkpoint(inputs.store, inputs.session));
    let prior = last
        .as_deref()
        .and_then(|id| prior_chain(&inputs.repo.work_tree, id));
    let before = prior.as_ref().map_or(0, |c| c.raw_lines.iter().sum());
    let carried = prior
        .as_ref()
        .map(|c| summed(&c.fallbacks))
        .unwrap_or_default();
    let seal = seal_turn_through(raw, prior, inputs.packs, gate).ok()?;
    let mut said = SealRead::of(raw.as_slice().len(), &seal);
    let (full, fallback, cut) = match seal {
        TurnSeal::Appended {
            body,
            document_lines,
            fallback,
        } => (body, summed(&[carried, fallback]), Some(document_lines)),
        TurnSeal::Whole { body, fallback, .. } => (body, fallback, None),
        // Nothing new past the frontier: the whole file is sealed as before,
        // and seal_turn put nothing through the gate.
        TurnSeal::Unchanged => {
            let (pair, lines) =
                rift_shadow::archived_transcript(raw, start_line, inputs.packs).ok()?;
            return Some((pair, u64::try_from(lines).unwrap_or(u64::MAX), said));
        }
    };
    let segmented = cut.and_then(|now| {
        let cut = CompactCut { before, now };
        turn_compact(inputs, raw, cut, last.as_deref())
    });
    let (compact, compact_start, raw_lines, redaction_fallback) =
        if let Some((compact, carry)) = segmented {
            said.compact = Some(carry);
            // The carried compactor already built the whole compact form, so
            // the file is not compacted again here; only the slice past
            // `start_line` is. The compact form ends where the full parts do,
            // so the slice start can be no later than its last record:
            // rounding down keeps the slice's rule of erring toward inclusion.
            let records = compact.as_slice().split_inclusive(|b| *b == b'\n').count();
            let (raw_lines, start) = rift_shadow::transcript::slice_start(raw, start_line, records);
            (compact, start.min(records), raw_lines, fallback)
        } else {
            let compacted = rift_shadow::transcript::compact_jsonl(raw, start_line);
            let whole = rift_shadow::redact::redact_with(compacted.compact(), inputs.packs).ok()?;
            let counts = summed(&[fallback, RedactionFallback::of(&[&whole.report])]);
            let start = compacted.compact_transcript_start;
            (whole.redacted.into(), start, compacted.raw_lines, counts)
        };
    let raw_lines = u64::try_from(raw_lines).unwrap_or(u64::MAX);
    let pair = ArchivedTranscript {
        full,
        compact,
        compact_start,
        spills: Vec::new(),
        originals: Vec::new(),
        redaction_fallback,
    };
    Some((pair, raw_lines, said))
}

/// The compact form cut where the full parts end, the earlier compact parts
/// carried by oid; `None` when the compact chain is refused.
fn turn_compact(
    inputs: &TurnInputs<'_>,
    raw: &SecretBytes,
    cut: CompactCut,
    last: Option<&str>,
) -> Option<(
    rift_shadow::backend::TranscriptBody,
    rift_shadow::turn_seal::CompactCarry,
)> {
    let earlier = last.and_then(|id| prior_compact(&inputs.repo.work_tree, id));
    let carry = last.and_then(|id| frontier::carry_after(&inputs.repo.git_dir, inputs.session, id));
    let sealed = seal_turn_compact(raw, cut, earlier, carry.as_ref(), inputs.packs).ok()?;
    Some((sealed.body, sealed.carry))
}

/// Fallback counts added up. The carried parts' counts were recorded when
/// they were sealed; a segmented compact form's new part reports none here.
fn summed(counts: &[RedactionFallback]) -> RedactionFallback {
    counts
        .iter()
        .fold(RedactionFallback::default(), |acc, c| RedactionFallback {
            lines: acc.lines.saturating_add(c.lines),
            placeholders: acc.placeholders.saturating_add(c.placeholders),
        })
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

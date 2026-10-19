//! The compact transcript's per-turn part: at a turn end the compact form is
//! segmented at the same document line as the full form, so the two parts of
//! one turn describe the same lines.
//!
//! A compact part cannot simply be the compaction of the turn's new lines: a
//! streamed message whose first fragment an earlier turn holds grows when a
//! later fragment arrives. So a part holds every record the turn added plus
//! each earlier record the turn grew, re-emitted whole, and the reader joins
//! the parts by keeping a repeated record's last bytes at its first position
//! (`join_compact_segments`). Compacting a prefix keeps each record where its
//! first fragment put it, so position alone says which records a turn changed.
//!
//! Earlier parts are carried by their checked blob oid and never pass the gate
//! again. The records they cover are recounted from the raw prefix — a
//! compaction, with no redaction — and a part off its oid, or a join whose
//! record count is not that prefix's, reseals the compact form whole as one
//! part. Recompacting the prefix each turn is O(prefix) parsing; carrying the
//! still-open records forward would make it O(turn) at the cost of a sidecar
//! the parts could disagree with.

use rift_core::{Result, SecretBytes};
use rift_recall_substrate::parse::segments::join_compact_segments;
use rift_shadow_redact::RulePackSet;

use super::PriorPart;
use crate::backend::{CarriedPart, Segments, TranscriptBody};
use crate::transcript::compact_records;

/// Where one turn's compact part begins and ends, in document lines: the
/// lines the earlier full parts cover, and the lines they cover now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactCut {
    /// Document lines the earlier parts cover.
    pub before: u64,
    /// Document lines this turn's parts cover.
    pub now: u64,
}

/// Seal the compact form of `raw`'s first `cut.now` lines for one turn:
/// `prior`'s parts carried and one new part, or the whole form as one part
/// when the earlier parts cannot be trusted.
///
/// # Errors
/// [`rift_core::RiftError`] if the gate returns one, which it does for no input.
pub fn seal_turn_compact(
    raw: &SecretBytes,
    cut: CompactCut,
    prior: Option<Vec<PriorPart>>,
    packs: &RulePackSet,
) -> Result<TranscriptBody> {
    let raw = raw.as_slice();
    let earlier =
        prior.and_then(|parts| carry(parts, compact_records(prefix(raw, cut.before)).len()));
    let (earlier, new) = match earlier {
        Some(earlier) => (earlier, compact_part(raw, cut.before, cut.now)),
        None => (Vec::new(), compact_part(raw, 0, cut.now)),
    };
    let sealed = rift_shadow_redact::redact_with(&SecretBytes::new(new), packs)?.redacted;
    if earlier.is_empty() && sealed.as_slice().is_empty() {
        return Ok(sealed.into());
    }
    let new = (!sealed.as_slice().is_empty()).then_some(sealed);
    Ok(TranscriptBody::Segments(Segments::compact(earlier, new)?))
}

/// Admit every earlier part by its oid, and the set only if its join holds
/// exactly the records the raw prefix compacts to.
fn carry(parts: Vec<PriorPart>, records: usize) -> Option<Vec<CarriedPart>> {
    if parts.is_empty() {
        return None;
    }
    let pieces: Vec<Vec<u8>> = parts.iter().map(|p| p.bytes.clone()).collect();
    let joined = join_compact_segments(&pieces);
    if joined.split_inclusive(|b| *b == b'\n').count() != records {
        return None;
    }
    parts
        .into_iter()
        .map(|p| CarriedPart::verified(p.oid, p.bytes))
        .collect()
}

/// The raw part between two cuts: every record the lines `[before, now)`
/// added, and every earlier record they grew, each on its own line.
pub(super) fn compact_part(raw: &[u8], before: u64, now: u64) -> Vec<u8> {
    let earlier = compact_records(prefix(raw, before));
    let mut out = Vec::new();
    for (at, record) in compact_records(prefix(raw, now)).iter().enumerate() {
        if earlier.get(at) != Some(record) {
            out.extend_from_slice(record);
            out.push(b'\n');
        }
    }
    out
}

/// The bytes of `raw`'s first `lines` `\n`-terminated lines; a count past
/// the end stops at the last complete line, never at a partial record.
pub(super) fn prefix(raw: &[u8], lines: u64) -> &[u8] {
    let Some(last) = usize::try_from(lines).ok().and_then(|n| n.checked_sub(1)) else {
        return &[];
    };
    let mut ends = raw
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .map(|(at, _)| at + 1);
    let end = ends
        .clone()
        .nth(last)
        .or_else(|| ends.next_back())
        .unwrap_or(0);
    raw.get(..end).unwrap_or_default()
}

#[cfg(test)]
mod tests;

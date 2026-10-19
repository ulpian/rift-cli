//! The per-turn transcript writer (L-5 slice 3a): at a turn end it seals only
//! the lines past the last admitted cut, as one new segment appended to the
//! parts the previous checkpoint recorded.
//!
//! Two guards keep the parts equal to the whole document's seal. The cut is
//! admitted by `rift_shadow_redact::cut_is_safe` (rule R1) and needs a line
//! after it, so a turn's last record waits for the next turn or the session-end
//! reconciliation rather than ending a segment no one can prove safe. And the
//! earlier parts are read back and checked before anything is appended (design
//! §2.6): their line counts against `segment_raw_lines`, their concatenation
//! against the previous checkpoint's full-transcript leaf, and each part's
//! bytes against the blob oid its tree records. Any miss refuses the segment
//! path for this turn — a whole reseal plus a `segment-chain-broken` finding —
//! and never re-bases onto whatever the tree holds.
//!
//! A checked part is carried forward by that oid, never passed through the
//! gate again (L-5 ruling, 2026-10-18): re-redacting the prefix each turn
//! would make a turn's cost O(transcript), which per-turn segments exist to
//! remove. The chain also records the pack set's fingerprint, and a turn
//! under a different one reseals whole rather than carry bytes other rules
//! produced. A carried part is not [`rift_core::RedactedBytes`] — the gate stays
//! the one constructor — and no blob is written for it.

use rift_core::{Result, SecretBytes, Sha256Id};
use rift_shadow_redact::{RedactionOutput, RulePackSet};

use crate::backend::{CarriedPart, RedactionFallback, Segments, TranscriptBody};

mod compact;
pub use compact::{seal_turn_compact, CompactCarry, CompactCut, CompactSeal};

/// The finding a refused chain records, naming the checkpoint it read back.
pub const SEGMENT_CHAIN_BROKEN: &str = "segment-chain-broken";

/// What the previous checkpoint recorded of its full transcript, read back
/// from its shard tree.
pub struct PriorChain {
    /// `transcript.segments/NNNNNN`, in order, as the tree holds them.
    pub parts: Vec<PriorPart>,
    /// Its `segment_raw_lines` metadata member.
    pub raw_lines: Vec<u64>,
    /// Its recorded full-transcript leaf (`leaf_hash` over the concatenation).
    pub leaf: Sha256Id,
    /// Its `segment_packs` member: the fingerprint of the packs its parts
    /// were sealed under, `None` where it records none.
    pub packs: Option<String>,
    /// Its `segment_redaction_fallback` member, one entry per part; empty
    /// where it records none.
    pub fallbacks: Vec<RedactionFallback>,
}

/// One earlier part: the blob oid its tree entry names, and the bytes read
/// from that blob.
pub struct PriorPart {
    /// The tree entry's blob oid.
    pub oid: String,
    /// The blob's bytes.
    pub bytes: Vec<u8>,
}

/// Why the earlier parts could not be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainBreak {
    /// A part's line count differs from `segment_raw_lines`, or one is empty
    /// or does not end at a line boundary.
    LineCount,
    /// The concatenation does not hash to the recorded leaf.
    LeafMismatch,
    /// The transcript now holds fewer lines than the parts cover.
    Truncated,
    /// A part's bytes are not the blob its tree entry names.
    OidMismatch,
    /// The parts were sealed under another pack set, or name none: carrying
    /// them would keep bytes the current rules never produced.
    PacksChanged,
}

impl ChainBreak {
    /// The doctor finding's name; the reason is the variant, never bytes.
    #[must_use]
    pub fn finding(self) -> &'static str {
        SEGMENT_CHAIN_BROKEN
    }
}

/// The outcome of one turn's seal.
pub enum TurnSeal {
    /// No admitted cut lies past the frontier: nothing new to record.
    Unchanged,
    /// One new part appended after every earlier one.
    Appended {
        /// Every earlier part plus the new one.
        body: TranscriptBody,
        /// Document lines the parts now cover — the next turn's frontier.
        document_lines: u64,
        /// The new part's fallback counts (earlier parts were counted then).
        fallback: RedactionFallback,
    },
    /// The chain was refused and the whole file resealed.
    Whole {
        /// The whole file, sealed once.
        body: TranscriptBody,
        /// Why the earlier parts were refused.
        broken: ChainBreak,
        /// The whole seal's fallback counts.
        fallback: RedactionFallback,
    },
}

/// Seal one turn of `raw` (the whole transcript file as it stands now).
///
/// # Errors
/// [`rift_core::RiftError`] if the gate returns one, which it does for no input.
pub fn seal_turn(
    raw: &SecretBytes,
    prior: Option<PriorChain>,
    packs: &RulePackSet,
) -> Result<TurnSeal> {
    let mut gate = |bytes: &SecretBytes, first_line: usize| {
        rift_shadow_redact::redact_with_at(bytes, packs, first_line)
    };
    seal_turn_through(raw, prior, packs, &mut gate)
}

/// The gate as [`seal_turn_through`] calls it: bytes and their first line.
pub type Gate<'a> = dyn FnMut(&SecretBytes, usize) -> Result<RedactionOutput> + 'a;

/// [`seal_turn`] over an injected gate, so a test — here or in a caller's
/// crate — can count what enters it. Production passes the real gate through
/// [`seal_turn`]; nothing else should call this.
///
/// # Errors
/// [`rift_core::RiftError`] if the gate returns one.
pub fn seal_turn_through(
    raw: &SecretBytes,
    prior: Option<PriorChain>,
    packs: &RulePackSet,
    gate: &mut Gate<'_>,
) -> Result<TurnSeal> {
    let lines = complete_lines(raw.as_slice());
    let fingerprint = packs.fingerprint();
    let carried_counts = prior
        .as_ref()
        .map(|c| c.fallbacks.clone())
        .unwrap_or_default();
    let earlier = match prior.map(|chain| read_back(chain, lines.len(), &fingerprint)) {
        None => Vec::new(),
        Some(Ok(parts)) => parts,
        Some(Err(broken)) => return whole_reseal(raw, broken, gate),
    };
    let frontier = covered(&earlier);
    let Some(cut) = last_admitted_cut(&lines, frontier, packs) else {
        return Ok(TurnSeal::Unchanged);
    };
    let delta: Vec<u8> = lines.get(frontier..cut).unwrap_or_default().concat();
    let out = gate(&SecretBytes::new(delta), frontier)?;
    let fallback = RedactionFallback::of(&[&out.report]);
    let counts = per_part_counts(carried_counts, earlier.len(), fallback);
    let segments =
        Segments::carrying(earlier, out.redacted, fingerprint)?.with_fallbacks(counts)?;
    Ok(TurnSeal::Appended {
        body: TranscriptBody::Segments(segments),
        document_lines: u64::try_from(cut).unwrap_or(u64::MAX),
        fallback,
    })
}

/// The earlier parts' recorded counts, then this turn's. A previous shard
/// that recorded none, or not one per part, counts each carried part as zero:
/// the parts are still sound, and the whole pair's own member keeps the total.
fn per_part_counts(
    mut carried: Vec<RedactionFallback>,
    earlier: usize,
    new: RedactionFallback,
) -> Vec<RedactionFallback> {
    if carried.len() != earlier {
        carried = vec![RedactionFallback::default(); earlier];
    }
    carried.push(new);
    carried
}

/// Check the earlier parts and admit each by its recorded blob oid.
fn read_back(
    chain: PriorChain,
    available: usize,
    fingerprint: &str,
) -> std::result::Result<Vec<CarriedPart>, ChainBreak> {
    // First, because no other check can make a part sealed under other rules
    // safe to carry (00-decisions L-5 follow-up, 2026-10-18).
    if chain.packs.as_deref() != Some(fingerprint) {
        return Err(ChainBreak::PacksChanged);
    }
    check_layout(&chain, available)?;
    chain
        .parts
        .into_iter()
        .map(|part| CarriedPart::verified(part.oid, part.bytes).ok_or(ChainBreak::OidMismatch))
        .collect()
}

/// The cheap checks, before any part is admitted.
fn check_layout(chain: &PriorChain, available: usize) -> std::result::Result<(), ChainBreak> {
    let counts_match = chain.parts.len() == chain.raw_lines.len()
        && chain
            .parts
            .iter()
            .zip(&chain.raw_lines)
            .all(|(part, declared)| {
                part.bytes.last() == Some(&b'\n')
                    && u64::try_from(line_count(&part.bytes)).is_ok_and(|n| n == *declared)
            });
    if !counts_match || chain.parts.is_empty() {
        return Err(ChainBreak::LineCount);
    }
    let joined: Vec<u8> = chain
        .parts
        .iter()
        .flat_map(|p| p.bytes.iter().copied())
        .collect();
    if rift_hashchain::leaf_hash(&joined) != chain.leaf {
        return Err(ChainBreak::LeafMismatch);
    }
    let sealed: usize = chain.parts.iter().map(|p| line_count(&p.bytes)).sum();
    if sealed > available {
        return Err(ChainBreak::Truncated);
    }
    Ok(())
}

/// The refused path: the whole file through the gate once, as before L-5.
fn whole_reseal(raw: &SecretBytes, broken: ChainBreak, gate: &mut Gate<'_>) -> Result<TurnSeal> {
    let out = gate(raw, 0)?;
    Ok(TurnSeal::Whole {
        fallback: RedactionFallback::of(&[&out.report]),
        body: out.redacted.into(),
        broken,
    })
}

/// The largest `k` past `frontier` whose cut R1 admits, with a line after it.
fn last_admitted_cut(lines: &[&[u8]], frontier: usize, packs: &RulePackSet) -> Option<usize> {
    let text = |i: usize| {
        let line = lines.get(i)?;
        std::str::from_utf8(line.strip_suffix(b"\n").unwrap_or(line)).ok()
    };
    (frontier + 1..lines.len()).rev().find(|&k| {
        let (Some(prev), Some(next)) = (text(k - 1), text(k)) else {
            return false;
        };
        rift_shadow_redact::cut_is_safe(prev, next, packs)
    })
}

/// Every `\n`-terminated line; a partial last record is not yet a line.
fn complete_lines(raw: &[u8]) -> Vec<&[u8]> {
    raw.split_inclusive(|b| *b == b'\n')
        .filter(|line| line.last() == Some(&b'\n'))
        .collect()
}

fn line_count(part: &[u8]) -> usize {
    complete_lines(part).len()
}

fn covered(parts: &[CarriedPart]) -> usize {
    parts.iter().map(|p| line_count(p.as_slice())).sum()
}

#[cfg(test)]
mod tests;

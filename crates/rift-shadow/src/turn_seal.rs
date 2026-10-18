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
//! against the previous checkpoint's full-transcript leaf, and each against a
//! second pass through the gate with the current packs. Any miss refuses the
//! segment path for this turn — a whole reseal plus a `segment-chain-broken`
//! finding — and never re-bases onto whatever the tree holds.
//!
//! The second pass is also how a read-back part becomes [`RedactedBytes`]
//! again: the gate is the only constructor, so a part from an earlier shard
//! re-enters the record through it, exactly as a spooled original does.

use rift_core::{RedactedBytes, Result, SecretBytes, Sha256Id};
use rift_shadow_redact::RulePackSet;

use crate::backend::{RedactionFallback, Segments, TranscriptBody};

/// The finding a refused chain records, naming the checkpoint it read back.
pub const SEGMENT_CHAIN_BROKEN: &str = "segment-chain-broken";

/// What the previous checkpoint recorded of its full transcript, read back
/// from its shard tree.
pub struct PriorChain {
    /// `transcript.segments/NNNNNN`, in order, as the tree holds them.
    pub parts: Vec<Vec<u8>>,
    /// Its `segment_raw_lines` metadata member.
    pub raw_lines: Vec<u64>,
    /// Its recorded full-transcript leaf (`leaf_hash` over the concatenation).
    pub leaf: Sha256Id,
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
    /// The current packs seal a part differently than it was sealed.
    Regated,
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
    let lines = complete_lines(raw.as_slice());
    let earlier = match prior {
        None => Vec::new(),
        Some(chain) => match read_back(chain, lines.len(), packs)? {
            Ok(parts) => parts,
            Err(broken) => return whole_reseal(raw, broken, packs),
        },
    };
    let frontier = covered(&earlier);
    let Some(cut) = last_admitted_cut(&lines, frontier, packs) else {
        return Ok(TurnSeal::Unchanged);
    };
    let delta: Vec<u8> = lines.get(frontier..cut).unwrap_or_default().concat();
    let out = rift_shadow_redact::redact_with_at(&SecretBytes::new(delta), packs, frontier)?;
    let fallback = RedactionFallback::of(&[&out.report]);
    let mut parts = earlier;
    parts.push(out.redacted);
    Ok(TurnSeal::Appended {
        body: TranscriptBody::Segments(Segments::new(parts)?),
        document_lines: u64::try_from(cut).unwrap_or(u64::MAX),
        fallback,
    })
}

/// Check the earlier parts and pass each through the gate again; the outer
/// error is the gate's, the inner one the reason the chain is refused.
fn read_back(
    chain: PriorChain,
    available: usize,
    packs: &RulePackSet,
) -> Result<std::result::Result<Vec<RedactedBytes>, ChainBreak>> {
    if let Err(broken) = check_layout(&chain, available) {
        return Ok(Err(broken));
    }
    let mut first_line = 0;
    let mut parts = Vec::with_capacity(chain.parts.len());
    for part in chain.parts {
        let lines = line_count(&part);
        let out =
            rift_shadow_redact::redact_with_at(&SecretBytes::new(part.clone()), packs, first_line)?;
        if out.redacted.as_slice() != part.as_slice() {
            return Ok(Err(ChainBreak::Regated));
        }
        first_line += lines;
        parts.push(out.redacted);
    }
    Ok(Ok(parts))
}

/// The cheap checks, before any part re-enters the gate.
fn check_layout(chain: &PriorChain, available: usize) -> std::result::Result<(), ChainBreak> {
    let counts_match = chain.parts.len() == chain.raw_lines.len()
        && chain
            .parts
            .iter()
            .zip(&chain.raw_lines)
            .all(|(part, declared)| {
                part.last() == Some(&b'\n')
                    && u64::try_from(line_count(part)).is_ok_and(|n| n == *declared)
            });
    if !counts_match || chain.parts.is_empty() {
        return Err(ChainBreak::LineCount);
    }
    if rift_hashchain::leaf_hash(&chain.parts.concat()) != chain.leaf {
        return Err(ChainBreak::LeafMismatch);
    }
    let sealed: usize = chain.parts.iter().map(|p| line_count(p)).sum();
    if sealed > available {
        return Err(ChainBreak::Truncated);
    }
    Ok(())
}

/// The refused path: the whole file through the gate once, as before L-5.
fn whole_reseal(raw: &SecretBytes, broken: ChainBreak, packs: &RulePackSet) -> Result<TurnSeal> {
    let out = rift_shadow_redact::redact_with(raw, packs)?;
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

fn covered(parts: &[RedactedBytes]) -> usize {
    parts.iter().map(|p| line_count(p.as_slice())).sum()
}

#[cfg(test)]
mod tests;

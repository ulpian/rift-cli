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
//! again. The compactor's own state is carried too ([`CompactCarry`], kept
//! beside the turn-seal frontier), so a turn compacts only its new lines; a
//! carry that is missing or does not fit the earlier cut is rebuilt from the
//! raw prefix — a compaction, with no redaction. A part off its oid, or a join
//! whose record count is not the carry's, reseals the compact form whole as
//! one part.

mod carry;

use rift_core::{Result, RiftError, SecretBytes};
use rift_recall_substrate::parse::segments::join_compact_segments;
use rift_recall_substrate::worktree::git_blob_oid;
use rift_shadow_redact::RulePackSet;

pub use carry::CompactCarry;

use super::PriorPart;
use crate::backend::{CarriedPart, SegmentPart, Segments, TranscriptBody};

/// Where one turn's compact part begins and ends, in document lines: the
/// lines the earlier full parts cover, and the lines they cover now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactCut {
    /// Document lines the earlier parts cover.
    pub before: u64,
    /// Document lines this turn's parts cover.
    pub now: u64,
}

/// One turn's compact seal and the compactor state the next turn starts from.
pub struct CompactSeal {
    /// The compact form: carried parts and this turn's, or one whole part.
    pub body: TranscriptBody,
    /// The state the next turn compacts from.
    pub carry: CompactCarry,
}

/// Seal the compact form of `raw`'s first `cut.now` lines for one turn:
/// `prior`'s parts carried and one new part, or the whole form as one part
/// when the earlier parts cannot be trusted. `carry` is the state the
/// previous turn left; one that does not fit `cut.before` is rebuilt.
///
/// # Errors
/// [`rift_core::RiftError`] if the gate returns one, which it does for no
/// input, or if a record cannot be rebuilt from its fragments.
pub fn seal_turn_compact(
    raw: &SecretBytes,
    cut: CompactCut,
    prior: Option<Vec<PriorPart>>,
    carry: Option<&CompactCarry>,
    packs: &RulePackSet,
) -> Result<CompactSeal> {
    let raw = raw.as_slice();
    let fitted = carry.filter(|c| c.fits(raw, cut.before));
    let base = fitted
        .cloned()
        .unwrap_or_else(|| CompactCarry::rebuilt(raw, cut.before));
    let earlier = prior.and_then(|parts| {
        let vouched = fitted.is_some_and(|c| c.vouches_for(&parts));
        carry_parts(parts, base.records(), vouched)
    });
    let base = if earlier.is_some() {
        base
    } else {
        CompactCarry::default()
    };
    // A line that grows a record the carry closed starts the turn over from
    // the whole prefix, where every record is open.
    let advanced = base
        .advance(raw, cut.now)
        .or_else(|| CompactCarry::rebuilt(raw, cut.before).advance(raw, cut.now));
    let Some((new, carry)) = advanced else {
        return Err(RiftError::Other(
            "a compact record could not be rebuilt from its fragments".into(),
        ));
    };
    let earlier = earlier.unwrap_or_default();
    let sealed = rift_shadow_redact::redact_with(&SecretBytes::new(new), packs)?.redacted;
    if earlier.is_empty() && sealed.as_slice().is_empty() {
        return Ok(CompactSeal {
            body: sealed.into(),
            carry,
        });
    }
    let new = (!sealed.as_slice().is_empty()).then_some(sealed);
    let segments = Segments::compact(earlier, new)?;
    let oids = segments.parts().iter().map(part_oid).collect();
    Ok(CompactSeal {
        body: TranscriptBody::Segments(segments),
        carry: carry.with_parts(oids),
    })
}

/// Admit every earlier part by its oid, and the set only if its join holds
/// exactly the records the earlier lines compact to. Parts the carry vouches
/// for by oid skip the recount, which parses the whole compact form.
fn carry_parts(parts: Vec<PriorPart>, records: usize, vouched: bool) -> Option<Vec<CarriedPart>> {
    if parts.is_empty() || !(vouched || joins_to(&parts, records)) {
        return None;
    }
    parts
        .into_iter()
        .map(|p| CarriedPart::verified(p.oid, p.bytes))
        .collect()
}

/// Whether the parts' join holds exactly `records` records.
fn joins_to(parts: &[PriorPart], records: usize) -> bool {
    let pieces: Vec<Vec<u8>> = parts.iter().map(|p| p.bytes.clone()).collect();
    let joined = join_compact_segments(&pieces);
    joined.split_inclusive(|b| *b == b'\n').count() == records
}

/// The blob oid a part is, or will be, stored under.
fn part_oid(part: &SegmentPart) -> String {
    match part {
        SegmentPart::Carried(c) => c.oid().to_owned(),
        SegmentPart::Sealed(r) => git_blob_oid(r.as_slice()),
    }
}

#[cfg(test)]
mod tests;

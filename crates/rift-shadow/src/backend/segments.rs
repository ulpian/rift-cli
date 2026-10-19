//! The full transcript's body: one sealed blob, or the line-aligned parts a
//! per-turn seal writes (L-5, round 30 Q1). Either way the bytes every leaf,
//! `content_hash.txt` input and id is computed over are the whole document —
//! [`TranscriptBody::as_slice`] is the concatenation — so a segmented shard
//! needs no new id rule and no canon epoch. The layout differs only in the
//! tree (`transcript.segments/NNNNNN` instead of `transcript.jsonl`) and in
//! two metadata members the substrate's one resolver reads back.

use std::collections::BTreeMap;

use rift_core::{RedactedBytes, Result, RiftError};
use rift_recall_substrate::parse::segments::{
    compact_segment_name, join_compact_segments, segment_name, COMPACT_SEGMENTS_DIR,
    COMPACT_SEGMENTS_KEY, SEGMENT_PACKS_KEY, SEGMENT_RAW_LINES_KEY, TRANSCRIPT_SEGMENTS_DIR,
    TRANSCRIPT_SEGMENTS_KEY,
};
use rift_recall_substrate::worktree::git_blob_oid;

/// The redacted full transcript as the shard stores it.
///
/// No `Debug`: every arm holds [`RedactedBytes`], which withholds it.
#[derive(Clone)]
pub enum TranscriptBody {
    /// One `transcript.jsonl` blob — every shard written before L-5.
    Whole(RedactedBytes),
    /// The parts of a transcript sealed turn by turn.
    Segments(Segments),
}

/// Line-aligned parts, each already through the redaction gate. The fields
/// are private so a part list that does not end on line boundaries cannot be
/// built: the cut rule (`rift_shadow_redact::cut_is_safe`) and the verify
/// line-count check both assume every part ends with `\n`.
#[derive(Clone)]
pub struct Segments {
    parts: Vec<SegmentPart>,
    /// The concatenation, kept so the body reads as one slice. It is a copy
    /// of gate output, never a write path: each part takes the durable door.
    joined: Vec<u8>,
    /// The fingerprint of the packs the sealed part ran under; `None` for a
    /// body built from parts alone, which a later turn never carries.
    packs: Option<String>,
    /// Which transcript the parts make up; it names the tree and the members.
    layout: Layout,
}

/// The two transcripts a shard can segment. They differ in how parts make
/// the document: the full form concatenates, while a compact part may
/// re-emit a record an earlier part holds, so the compact form joins.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Full,
    Compact,
}

/// One part of a segmented body.
///
/// No `Debug`, for the reason [`TranscriptBody`] has none.
#[derive(Clone)]
pub enum SegmentPart {
    /// Sealed this turn by the gate; it takes the durable door.
    Sealed(RedactedBytes),
    /// An earlier turn's part, referenced by the blob the previous shard
    /// already holds (L-5 ruling: earlier segments are reused, never
    /// re-redacted). No bytes are written for it.
    Carried(CarriedPart),
}

/// A part read back from the previous shard whose bytes hash to the blob oid
/// its tree records. The bytes are already-redacted output Rift wrote; they
/// are kept only so the body still reads as the whole document, and never
/// reach a blob write — which is why this is not a second way to make
/// [`RedactedBytes`].
#[derive(Clone)]
pub struct CarriedPart {
    oid: String,
    bytes: Vec<u8>,
}

impl CarriedPart {
    /// Admit a read-back part if its bytes are the blob `oid` names.
    ///
    /// The check is git's default (SHA-1) blob id, so in a SHA-256 object
    /// format every part misses and each turn takes the whole reseal: slower,
    /// never wrong.
    #[must_use]
    pub fn verified(oid: String, bytes: Vec<u8>) -> Option<Self> {
        (git_blob_oid(&bytes) == oid).then_some(Self { oid, bytes })
    }

    /// The blob oid the previous shard recorded, reused in the new tree.
    #[must_use]
    pub fn oid(&self) -> &str {
        &self.oid
    }

    /// The part's bytes, as the previous shard holds them.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
}

impl SegmentPart {
    /// The part's bytes, sealed now or carried.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Sealed(sealed) => sealed.as_slice(),
            Self::Carried(carried) => carried.as_slice(),
        }
    }
}

impl Segments {
    /// Gather sealed parts in document order.
    ///
    /// # Errors
    /// [`RiftError::Validity`] for no parts, an empty part, or a part that does
    /// not end at a line boundary.
    pub fn new(parts: Vec<RedactedBytes>) -> Result<Self> {
        Self::of(parts.into_iter().map(SegmentPart::Sealed).collect())
    }

    /// The earlier parts carried by oid, then this turn's sealed one, all
    /// under the pack set `packs` fingerprints (`RulePackSet::fingerprint`).
    ///
    /// # Errors
    /// As [`Segments::new`].
    pub fn carrying(earlier: Vec<CarriedPart>, new: RedactedBytes, packs: String) -> Result<Self> {
        let mut parts: Vec<SegmentPart> = earlier.into_iter().map(SegmentPart::Carried).collect();
        parts.push(SegmentPart::Sealed(new));
        Ok(Self {
            packs: Some(packs),
            ..Self::of(parts)?
        })
    }

    /// The compact transcript's parts: the earlier ones carried by oid, then
    /// this turn's sealed part when the turn changed any record. The body
    /// reads as their join, which is what every leaf is computed over.
    ///
    /// # Errors
    /// As [`Segments::new`].
    pub fn compact(earlier: Vec<CarriedPart>, new: Option<RedactedBytes>) -> Result<Self> {
        let mut parts: Vec<SegmentPart> = earlier.into_iter().map(SegmentPart::Carried).collect();
        parts.extend(new.map(SegmentPart::Sealed));
        let of = Self::of(parts)?;
        let pieces: Vec<Vec<u8>> = of.parts.iter().map(|p| p.as_slice().to_vec()).collect();
        Ok(Self {
            joined: join_compact_segments(&pieces),
            layout: Layout::Compact,
            ..of
        })
    }

    fn of(parts: Vec<SegmentPart>) -> Result<Self> {
        if parts.is_empty() {
            return Err(RiftError::validity("a segmented transcript has no parts"));
        }
        if parts.iter().any(|p| p.as_slice().last() != Some(&b'\n')) {
            return Err(RiftError::validity(
                "a transcript segment must end at a line boundary",
            ));
        }
        let joined = parts
            .iter()
            .flat_map(SegmentPart::as_slice)
            .copied()
            .collect();
        Ok(Self {
            parts,
            joined,
            packs: None,
            layout: Layout::Full,
        })
    }

    /// The fingerprint of the packs the parts were sealed under, if recorded.
    #[must_use]
    pub fn packs(&self) -> Option<&str> {
        self.packs.as_deref()
    }

    /// The parts, in document order.
    #[must_use]
    pub fn parts(&self) -> &[SegmentPart] {
        &self.parts
    }

    /// The tree the parts are written under.
    #[must_use]
    pub fn dir(&self) -> &'static str {
        match self.layout {
            Layout::Full => TRANSCRIPT_SEGMENTS_DIR,
            Layout::Compact => COMPACT_SEGMENTS_DIR,
        }
    }

    fn part_name(&self, index: usize) -> String {
        match self.layout {
            Layout::Full => segment_name(index),
            Layout::Compact => compact_segment_name(index),
        }
    }

    /// Each part's line count, which `segment_raw_lines` records: the gate is
    /// line-preserving, so the raw and the sealed counts agree.
    #[must_use]
    pub fn raw_lines(&self) -> Vec<u64> {
        // Every part ends with `\n` (the constructor's rule), so its pieces
        // split after each newline are exactly its lines.
        let lines = |p: &SegmentPart| p.as_slice().split_inclusive(|b| *b == b'\n').count();
        self.parts
            .iter()
            .map(|p| u64::try_from(lines(p)).unwrap_or(u64::MAX))
            .collect()
    }
}

impl TranscriptBody {
    /// The whole document's bytes: the blob, or the parts' concatenation.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Whole(whole) => whole.as_slice(),
            Self::Segments(segments) => &segments.joined,
        }
    }
}

impl From<RedactedBytes> for TranscriptBody {
    fn from(whole: RedactedBytes) -> Self {
        Self::Whole(whole)
    }
}

/// Record a segmented body's layout on its metadata document; a whole body
/// writes nothing, so every shard written before L-5 keeps its bytes.
pub(super) fn insert_segment_members(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    body: &TranscriptBody,
) {
    let TranscriptBody::Segments(segments) = body else {
        return;
    };
    if segments.layout == Layout::Compact {
        // A compact part may re-emit a record, so its line count says nothing
        // a reader can check; the count of parts is the whole layout.
        obj.insert(COMPACT_SEGMENTS_KEY.into(), segments.parts.len().into());
        return;
    }
    obj.insert(TRANSCRIPT_SEGMENTS_KEY.into(), segments.parts.len().into());
    obj.insert(SEGMENT_RAW_LINES_KEY.into(), segments.raw_lines().into());
    if let Some(packs) = &segments.packs {
        obj.insert(SEGMENT_PACKS_KEY.into(), packs.clone().into());
    }
}

/// The artifact-map entries a body occupies under `dir` (`""` or
/// `tasks/<id>/`), named exactly as `write_shard` names them; `whole` is the
/// blob's name when the body is not segmented.
pub(super) fn insert_body_artifacts(
    out: &mut BTreeMap<String, Vec<u8>>,
    dir: &str,
    whole: &str,
    body: &TranscriptBody,
) {
    match body {
        TranscriptBody::Whole(blob) => {
            out.insert(format!("{dir}{whole}"), blob.as_slice().to_vec());
        }
        TranscriptBody::Segments(segments) => {
            for (index, part) in segments.parts.iter().enumerate() {
                out.insert(
                    format!("{dir}{}", segments.part_name(index)),
                    part.as_slice().to_vec(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;

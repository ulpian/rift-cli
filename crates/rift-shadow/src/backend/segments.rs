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
    segment_name, SEGMENT_RAW_LINES_KEY, TRANSCRIPT_SEGMENTS_KEY,
};

use super::TRANSCRIPT_FULL_NAME;

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
    parts: Vec<RedactedBytes>,
    /// The concatenation, kept so the body reads as one slice. It is a copy
    /// of gate output, never a write path: each part takes the durable door.
    joined: Vec<u8>,
}

impl Segments {
    /// Gather sealed parts in document order.
    ///
    /// # Errors
    /// [`RiftError::Validity`] for no parts, an empty part, or a part that does
    /// not end at a line boundary.
    pub fn new(parts: Vec<RedactedBytes>) -> Result<Self> {
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
            .flat_map(RedactedBytes::as_slice)
            .copied()
            .collect();
        Ok(Self { parts, joined })
    }

    /// The parts, in document order.
    #[must_use]
    pub fn parts(&self) -> &[RedactedBytes] {
        &self.parts
    }

    /// Each part's line count, which `segment_raw_lines` records: the gate is
    /// line-preserving, so the raw and the sealed counts agree.
    #[must_use]
    pub fn raw_lines(&self) -> Vec<u64> {
        // Every part ends with `\n` (the constructor's rule), so its pieces
        // split after each newline are exactly its lines.
        let lines = |p: &RedactedBytes| p.as_slice().split_inclusive(|b| *b == b'\n').count();
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
    obj.insert(TRANSCRIPT_SEGMENTS_KEY.into(), segments.parts.len().into());
    obj.insert(SEGMENT_RAW_LINES_KEY.into(), segments.raw_lines().into());
}

/// The artifact-map entries a body occupies under `dir` (`""` or
/// `tasks/<id>/`), named exactly as `write_shard` names them.
pub(super) fn insert_body_artifacts(
    out: &mut BTreeMap<String, Vec<u8>>,
    dir: &str,
    body: &TranscriptBody,
) {
    match body {
        TranscriptBody::Whole(whole) => {
            out.insert(
                format!("{dir}{TRANSCRIPT_FULL_NAME}"),
                whole.as_slice().to_vec(),
            );
        }
        TranscriptBody::Segments(segments) => {
            for (index, part) in segments.parts.iter().enumerate() {
                out.insert(
                    format!("{dir}{}", segment_name(index)),
                    part.as_slice().to_vec(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;

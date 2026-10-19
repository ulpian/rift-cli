//! A transcript sealed turn by turn is stored as line-aligned parts under
//! `transcript.segments/NNNNNN` instead of one `transcript.jsonl` blob (L-5,
//! round 30 Q1). Every reader still asks for `transcript.jsonl`; this module
//! is the one place that answers that name for a segmented shard, so no reader
//! grows a second rule.
//!
//! The answer is the concatenation of the parts, or absence. It is never a
//! partial transcript: a missing part, a malformed declaration, or a whole
//! blob beside declared parts (two layouts, so neither is the record) all
//! read as absent, which every leaf and id check then refuses by name.

use rift_core::Result;
use serde_json::Value;

/// The tree a segmented transcript's parts live in, beside its metadata.
pub const TRANSCRIPT_SEGMENTS_DIR: &str = "transcript.segments";

/// The metadata member counting the parts.
pub const TRANSCRIPT_SEGMENTS_KEY: &str = "transcript_segments";

/// The metadata member listing each part's line count, in order.
pub const SEGMENT_RAW_LINES_KEY: &str = "segment_raw_lines";

/// The metadata member naming the redaction pack set the parts were sealed
/// under (`RulePackSet::fingerprint`); a later turn carries them only under
/// the same one.
pub const SEGMENT_PACKS_KEY: &str = "segment_packs";

/// The artifact name the resolver answers for.
const TRANSCRIPT_NAME: &str = "transcript.jsonl";

/// What one metadata document declares about its transcript's layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Declared {
    /// Neither member: the transcript, if any, is one blob.
    Whole,
    /// Both members, consistent: one line count per part, at least one part.
    Segments(Vec<u64>),
    /// One member without the other, a wrong type, or a count the list
    /// disagrees with.
    Malformed,
}

/// The part's name inside its transcript's directory, 0-based and six digits
/// wide so the tree's bytewise order is the parts' order.
#[must_use]
pub fn segment_name(index: usize) -> String {
    format!("{TRANSCRIPT_SEGMENTS_DIR}/{index:06}")
}

/// Read the layout a metadata document declares. A document that is not an
/// object declares nothing, which keeps every pre-L-5 reading unchanged.
#[must_use]
pub fn declared_layout(metadata: &Value) -> Declared {
    let Some(obj) = metadata.as_object() else {
        return Declared::Whole;
    };
    match (
        obj.get(TRANSCRIPT_SEGMENTS_KEY),
        obj.get(SEGMENT_RAW_LINES_KEY),
    ) {
        (None, None) => Declared::Whole,
        (Some(count), Some(Value::Array(lines))) => {
            let lines: Option<Vec<u64>> = lines.iter().map(Value::as_u64).collect();
            match (count.as_u64(), lines) {
                (Some(n), Some(lines)) if n > 0 && usize::try_from(n) == Ok(lines.len()) => {
                    Declared::Segments(lines)
                }
                _ => Declared::Malformed,
            }
        }
        _ => Declared::Malformed,
    }
}

/// Answer `rel` through `read`, resolving a `transcript.jsonl` (the parent's
/// or a task's) to its parts' concatenation when its metadata declares them.
/// Every other name passes straight through.
///
/// # Errors
/// Whatever `read` returns.
pub fn resolve_transcript(
    rel: &str,
    read: impl Fn(&str) -> Result<Option<Vec<u8>>>,
) -> Result<Option<Vec<u8>>> {
    let Some(dir) = rel.strip_suffix(TRANSCRIPT_NAME) else {
        return read(rel);
    };
    if !(dir.is_empty() || dir.ends_with('/')) {
        return read(rel);
    }
    let whole = read(rel)?;
    let declared = read(&format!("{dir}metadata.json"))?
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .map_or(Declared::Whole, |doc| declared_layout(&doc));
    match (declared, whole) {
        (Declared::Whole, whole) => Ok(whole),
        (Declared::Segments(lines), None) => concatenate(dir, lines.len(), &read),
        (Declared::Segments(_) | Declared::Malformed, _) => Ok(None),
    }
}

/// The parts in order, or absence the moment one is missing.
fn concatenate(
    dir: &str,
    count: usize,
    read: &impl Fn(&str) -> Result<Option<Vec<u8>>>,
) -> Result<Option<Vec<u8>>> {
    let mut joined = Vec::new();
    for index in 0..count {
        let Some(part) = read(&format!("{dir}{}", segment_name(index)))? else {
            return Ok(None);
        };
        joined.extend_from_slice(&part);
    }
    Ok(Some(joined))
}

#[cfg(test)]
mod tests;

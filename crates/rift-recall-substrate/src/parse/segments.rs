//! A transcript sealed turn by turn is stored as line-aligned parts under
//! `transcript.segments/NNNNNN` instead of one `transcript.jsonl` blob (L-5,
//! round 30 Q1). Every reader still asks for `transcript.jsonl`; this module
//! is the one place that answers that name for a segmented shard, so no reader
//! grows a second rule.
//!
//! The compact form is segmented the same way, under
//! `transcript.compact.segments/NNNNNN`, and answers `transcript.compact.jsonl`.
//! Its parts are joined rather than concatenated: a message whose fragments
//! straddle a turn end is re-emitted, merged, by the later part, so the later
//! occurrence of an `id` replaces the earlier one at the earlier position.
//! That keeps a turn's compact seal proportional to its own lines while the
//! join still equals compacting the whole file.
//!
//! The answer is the concatenation (or join) of the parts, or absence. It is never a
//! partial transcript: a missing part, a malformed declaration, or a whole
//! blob beside declared parts (two layouts, so neither is the record) all
//! read as absent, which every leaf and id check then refuses by name.

use std::collections::HashMap;

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

/// The tree a segmented compact transcript's parts live in.
pub const COMPACT_SEGMENTS_DIR: &str = "transcript.compact.segments";

/// The metadata member counting the compact transcript's parts. No line
/// counts beside it: a re-emitted record makes a compact part's lines
/// overlap the previous part's, so per-part counts would not sum.
pub const COMPACT_SEGMENTS_KEY: &str = "transcript_compact_segments";

/// The artifact names the resolver answers for.
const TRANSCRIPT_NAME: &str = "transcript.jsonl";
const COMPACT_NAME: &str = "transcript.compact.jsonl";

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

/// A compact part's name, numbered as [`segment_name`] numbers full parts.
#[must_use]
pub fn compact_segment_name(index: usize) -> String {
    format!("{COMPACT_SEGMENTS_DIR}/{index:06}")
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
    if let Some(dir) = artifact_dir(rel, TRANSCRIPT_NAME) {
        let declared = read_metadata(dir, &read)?.map_or(Declared::Whole, |d| declared_layout(&d));
        let whole = read(rel)?;
        return match (declared, whole) {
            (Declared::Whole, whole) => Ok(whole),
            (Declared::Segments(lines), None) => {
                Ok(read_parts(dir, lines.len(), segment_name, &read)?.map(|p| p.concat()))
            }
            (Declared::Segments(_) | Declared::Malformed, _) => Ok(None),
        };
    }
    if let Some(dir) = artifact_dir(rel, COMPACT_NAME) {
        let declared = read_metadata(dir, &read)?.map_or(Ok(None), |d| declared_compact(&d));
        let whole = read(rel)?;
        return match (declared, whole) {
            (Ok(None), whole) => Ok(whole),
            (Ok(Some(count)), None) => Ok(read_parts(dir, count, compact_segment_name, &read)?
                .map(|parts| join_compact_segments(&parts))),
            (Ok(Some(_)) | Err(()), _) => Ok(None),
        };
    }
    read(rel)
}

/// Join compact parts: concatenate, then for every record `id` seen more
/// than once keep the last occurrence's bytes at the first occurrence's
/// position. Compacting a whole file never emits an `id` twice, so this is
/// the one rule that makes the parts equal the whole.
#[must_use]
pub fn join_compact_segments(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut lines: Vec<&[u8]> = Vec::new();
    let mut first_at: HashMap<String, usize> = HashMap::new();
    for line in parts
        .iter()
        .flat_map(|p| p.split_inclusive(|b| *b == b'\n'))
    {
        let Some(id) = record_id(line) else {
            lines.push(line);
            continue;
        };
        if let Some(slot) = first_at.get(&id).and_then(|at| lines.get_mut(*at)) {
            *slot = line;
            continue;
        }
        first_at.insert(id, lines.len());
        lines.push(line);
    }
    lines.concat()
}

/// The directory `rel` names `name` in: the parent's (`""`) or a task's.
fn artifact_dir<'a>(rel: &'a str, name: &str) -> Option<&'a str> {
    rel.strip_suffix(name)
        .filter(|dir| dir.is_empty() || dir.ends_with('/'))
}

fn read_metadata(
    dir: &str,
    read: &impl Fn(&str) -> Result<Option<Vec<u8>>>,
) -> Result<Option<Value>> {
    Ok(read(&format!("{dir}metadata.json"))?.and_then(|b| serde_json::from_slice(&b).ok()))
}

/// The compact part count a metadata document declares: `Ok(None)` for a
/// whole compact, `Err` for a member that is not a positive count.
fn declared_compact(metadata: &Value) -> std::result::Result<Option<usize>, ()> {
    let Some(count) = metadata.get(COMPACT_SEGMENTS_KEY) else {
        return Ok(None);
    };
    match count.as_u64().map(usize::try_from) {
        Some(Ok(n)) if n > 0 => Ok(Some(n)),
        _ => Err(()),
    }
}

/// A compact line's top-level `id`, the key the compactor merges by.
fn record_id(line: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(line.trim_ascii_end()).ok()?;
    value.get("id")?.as_str().map(str::to_owned)
}

/// The parts in order, or absence the moment one is missing.
fn read_parts(
    dir: &str,
    count: usize,
    name: fn(usize) -> String,
    read: &impl Fn(&str) -> Result<Option<Vec<u8>>>,
) -> Result<Option<Vec<Vec<u8>>>> {
    let mut parts = Vec::with_capacity(count);
    for index in 0..count {
        let Some(part) = read(&format!("{dir}{}", name(index)))? else {
            return Ok(None);
        };
        parts.push(part);
    }
    Ok(Some(parts))
}

#[cfg(test)]
mod tests;

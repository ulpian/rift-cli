//! What a segmented transcript owes (L-5, round 30 Q1). The leaf needs no
//! check of its own: the substrate's one resolver hands the strict-Merkle and
//! id passes the parts' concatenation, so an edited part already moves the
//! root. These checks name the layout faults the root alone would only call
//! tamper: a part missing, a part off a line boundary or holding other than the
//! lines its metadata records, a part past the declared count, a whole blob
//! beside the parts, and parts no metadata declares.

use rift_core::Result;
use rift_recall_substrate::parse::segments::{declared_layout, segment_name, Declared};
use serde_json::Value;

use super::verdict::Verdict;

/// One checkpoint's artifact read, by shard-relative name. A closure rather
/// than a reader because the gap path must read the native origin it
/// authenticated, and every other path reads any origin.
pub(super) type ReadArtifact<'a> = &'a dyn Fn(&str) -> Result<Option<Vec<u8>>>;

/// The parent's transcript and each task its metadata names, each judged by
/// its own metadata document.
///
/// # Errors
/// A substrate read error.
pub(super) fn segments_pass(read: ReadArtifact<'_>, id: &str) -> Result<Vec<Verdict>> {
    let tasks = parse(read("metadata.json")?).map_or_else(Vec::new, |doc| task_ids(&doc));
    let dirs = std::iter::once(String::new()).chain(tasks.iter().map(|t| format!("tasks/{t}/")));
    let mut found = Vec::new();
    for dir in dirs {
        if let Some(fault) = layout_fault(&|name| read(&format!("{dir}{name}")))? {
            found.push(Verdict::Structure(format!("{id}: {dir}{fault}")));
        }
    }
    Ok(found)
}

fn parse(bytes: Option<Vec<u8>>) -> Option<Value> {
    bytes.and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// The task ids a parent metadata document lists; the task-subtree pass owns
/// the malformed cases, so anything else reads as none here.
fn task_ids(doc: &Value) -> Vec<String> {
    let ids = doc.get("tasks").and_then(Value::as_array);
    ids.map_or_else(Vec::new, |ids| {
        ids.iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    })
}

/// The first layout fault of the transcript `read` is rooted at, if any.
fn layout_fault(read: ReadArtifact<'_>) -> Result<Option<String>> {
    let declared =
        parse(read("metadata.json")?).map_or(Declared::Whole, |doc| declared_layout(&doc));
    let lines = match declared {
        Declared::Segments(lines) => lines,
        Declared::Malformed => return Ok(Some("metadata.json declares malformed segments".into())),
        Declared::Whole if read(&segment_name(0))?.is_some() => {
            return Ok(Some("transcript.segments is present but undeclared".into()));
        }
        Declared::Whole => return Ok(None),
    };
    for (index, recorded) in lines.iter().enumerate() {
        let name = segment_name(index);
        let Some(part) = read(&name)? else {
            return Ok(Some(format!("{name} is missing")));
        };
        if part.last() != Some(&b'\n') {
            return Ok(Some(format!("{name} does not end at a line boundary")));
        }
        // It ends with `\n`, so its pieces split after each newline are its lines.
        let held = part.split_inclusive(|b| *b == b'\n').count();
        if u64::try_from(held).ok() != Some(*recorded) {
            return Ok(Some(format!(
                "{name} holds {held} lines, metadata records {recorded}"
            )));
        }
    }
    if read(&segment_name(lines.len()))?.is_some() {
        let count = lines.len();
        return Ok(Some(format!("holds a segment past the {count} declared")));
    }
    // Every declared part is present, so the resolver's only reason left to
    // answer nothing is a whole blob stored beside the parts.
    if read("transcript.jsonl")?.is_none() {
        return Ok(Some(
            "transcript.jsonl is stored beside its declared segments".into(),
        ));
    }
    Ok(None)
}

#[cfg(test)]
mod tests;

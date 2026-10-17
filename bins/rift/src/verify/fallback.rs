//! Transcript fallback facts: the hash-bound parent report is authoritative;
//! parent metadata mirrors it, while each task's metadata is itself a leaf.
//! Typed paired counts are checked before recounting a lower bound: literal
//! canonical placeholder records can already occur in the input. Doctor reads
//! these bound artifacts but does not independently verify their root or id.

use rift_core::Result;
use rift_recall_substrate::SubstrateReader;

use super::artifacts::NativeArtifacts;
use super::verdict::Verdict;

/// Preserve absence separately from malformed or non-object parent metadata.
pub(super) fn header_counts(header: &super::artifacts::ShardHeader) -> Checked {
    if header.metadata_bytes.is_none() {
        Ok(Recorded::default())
    } else {
        parent_counts(header.doc.as_ref())
    }
}

/// What one metadata document records about its transcript pair's fallback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Recorded {
    /// Lines the fallback sealed, flat or as a record.
    pub(crate) lines: u64,
    /// The subset replaced by a placeholder record.
    pub(crate) placeholders: u64,
}

impl Recorded {
    /// Missing both fields means zero; every present pair follows writer grammar.
    pub(crate) fn of(doc: Option<&serde_json::Value>) -> Checked {
        let lines = doc.and_then(|d| d.get("redaction_flat_fallback"));
        let placeholders = doc.and_then(|d| d.get("redaction_fallback_placeholders"));
        match (lines, placeholders) {
            (None, None) => Ok(Self::default()),
            (Some(lines), Some(placeholders)) => {
                let number = |value: &serde_json::Value| {
                    value
                        .as_u64()
                        .filter(|n| u32::try_from(*n).is_ok())
                        .ok_or("fallback count is not a u32")
                };
                let result = Self {
                    lines: number(lines)?,
                    placeholders: number(placeholders)?,
                };
                if result.lines == 0 || result.placeholders > result.lines {
                    return Err("fallback count pair is outside the writer grammar");
                }
                Ok(result)
            }
            _ => Err("fallback count pair is incomplete"),
        }
    }

    fn of_bytes(bytes: &[u8]) -> Checked {
        let doc = serde_json::from_slice::<serde_json::Value>(bytes)
            .map_err(|_| "fallback count document is malformed")?;
        if !doc.is_object() {
            return Err("fallback count document is not an object");
        }
        Self::of(Some(&doc))
    }

    fn plus(self, other: Self) -> Self {
        Self {
            lines: self.lines.saturating_add(other.lines),
            placeholders: self.placeholders.saturating_add(other.placeholders),
        }
    }

    /// Whether the fallback sealed anything at all.
    pub(crate) fn any(self) -> bool {
        self.lines > 0
    }
}

/// The checkpoint's fallback, summed over the session transcript and every
/// task's: what the doctor names a checkpoint by.
///
/// # Errors
/// A substrate read error or unreadable/inconsistent relevant count facts.
pub(crate) fn on_checkpoint(reader: &impl SubstrateReader, id: &str) -> Result<Recorded> {
    let Some(bytes) = reader.read_artifact(id, "metadata.json")? else {
        return Err(count_error(id, "metadata.json"));
    };
    let doc = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|_| count_error(id, "metadata.json"))?;
    if !doc.is_object() {
        return Err(count_error(id, "metadata.json"));
    }
    let report = reader
        .read_artifact(id, "redaction_report.json")?
        .ok_or_else(|| count_error(id, "redaction_report.json"))?;
    let own = Recorded::of_bytes(&report);
    let mut total = own.map_err(|_| count_error(id, "redaction_report.json"))?;
    if parent_counts(Some(&doc)) != Ok(total) {
        return Err(count_error(id, "metadata.json"));
    }
    let tasks = task_ids(&doc).map_err(|_| count_error(id, "metadata.json"))?;
    for tid in tasks {
        let path = format!("tasks/{tid}/metadata.json");
        let meta = reader
            .read_artifact(id, &path)?
            .ok_or_else(|| count_error(id, &path))?;
        total = total.plus(Recorded::of_bytes(&meta).map_err(|_| count_error(id, &path))?);
    }
    Ok(total)
}

/// Only the relevant parent fields are strict; unrelated metadata stays lenient.
pub(super) fn parent_counts(doc: Option<&serde_json::Value>) -> Checked {
    let doc = doc
        .filter(|doc| doc.is_object())
        .ok_or("parent metadata is not an object")?;
    task_ids(doc)?;
    Recorded::of(Some(doc))
}

fn task_ids(doc: &serde_json::Value) -> std::result::Result<Vec<&str>, &'static str> {
    let Some(tasks) = doc.get("tasks") else {
        return Ok(Vec::new());
    };
    let mut seen = std::collections::BTreeSet::new();
    tasks
        .as_array()
        .ok_or("task list is not an array")?
        .iter()
        .map(|value| {
            let id = value
                .as_str()
                .filter(|id| crate::hooks::subagent_claims::is_safe_id(id))
                .ok_or("task id is invalid")?;
            if !seen.insert(id) {
                return Err("task id is duplicated");
            }
            Ok(id)
        })
        .collect()
}

fn count_error(id: &str, path: &str) -> rift_core::RiftError {
    rift_core::RiftError::Other(format!(
        "{id}: {path} has unreadable or inconsistent fallback counts"
    ))
}

pub(super) type Checked = std::result::Result<Recorded, &'static str>;

/// Each transcript pair must hold at least the placeholder records its
/// metadata counts, and a count of placeholders past the count of lines is a
/// document that does not describe any pair the writer could produce.
pub(super) fn fallback_pass(
    id: &str,
    mirror: Checked,
    native: &NativeArtifacts,
    report: Option<&[u8]>,
) -> Vec<Verdict> {
    let own = report.map_or(Ok(Recorded::default()), Recorded::of_bytes);
    let mut findings = Vec::new();
    if mirror.is_err() || own.is_err() || mirror != own {
        findings.push(Verdict::Structure(format!(
            "{id}: parent fallback counts are malformed or inconsistent with redaction_report.json"
        )));
    }
    let mut pairs = vec![(
        String::new(),
        own,
        native.transcript_full.as_deref(),
        native.transcript_compact.as_deref(),
    )];
    pairs.extend(native.tasks.iter().map(|t| {
        (
            format!("tasks/{}/", t.tool_use_id),
            Recorded::of_bytes(&t.metadata),
            t.transcript_full.as_deref(),
            t.transcript_compact.as_deref(),
        )
    }));
    findings.extend(pairs.into_iter().filter_map(
        |(dir, recorded, full, compact)| match recorded {
            Ok(recorded) => judge(id, &dir, recorded, held(full).saturating_add(held(compact))),
            Err(_) if !dir.is_empty() => Some(Verdict::Structure(format!(
                "{id}: {dir}metadata.json has malformed fallback counts"
            ))),
            Err(_) => None,
        },
    ));
    findings
}

/// The placeholder records one sealed artifact holds.
fn held(bytes: Option<&[u8]>) -> u64 {
    bytes.map_or(0, |b| {
        u64::from(rift_shadow::redact::count_fallback_placeholders(b))
    })
}

fn judge(id: &str, dir: &str, recorded: Recorded, held: u64) -> Option<Verdict> {
    if recorded.placeholders > recorded.lines {
        return Some(Verdict::Structure(format!(
            "{id}: {dir}metadata.json records more redaction placeholders than fallback lines"
        )));
    }
    if held < recorded.placeholders {
        return Some(Verdict::Redaction(format!(
            "{id}: {dir}metadata.json records {} redaction placeholder(s) and its transcript \
             holds {held}",
            recorded.placeholders
        )));
    }
    None
}

#[cfg(test)]
mod tests;

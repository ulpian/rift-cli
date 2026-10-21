//! Collecting and sealing the tool outputs a transcript says were spilled to a
//! file of their own.
//!
//! Claude Code keeps a tool result over its inline threshold (~30 K
//! characters) in `<session dir>/tool-results/<name>.txt` and records only a
//! ~2 KB preview and the file's address in the transcript. Without the file the
//! sealed transcript holds the preview, so each named file is read at archive,
//! redacted by the same gate and packs as the transcript that names it, and
//! sealed beside that transcript as `tool-results/<tool-use-id>.txt`
//! ([`rift_shadow::backend::ArchivedSpill`]). A spill of a subagent's result
//! lives in the parent session's directory, so both transcripts share one
//! containment root.
//!
//! **Where the address comes from.** Only addresses the transcript names are
//! read; the `tool-results` directory is never listed. The client's own field,
//! `toolUseResult.persistedOutputPath`, outranks everything. A subagent's file
//! records no `toolUseResult`, so there the address is read from the envelope
//! the client wraps the preview in — and only from its front, so a result that
//! merely prints the words cannot name a file. Either way the address is
//! untrusted text, which is why containment is checked before any read.
//!
//! **Containment.** An address is read only when it is, lexically, an absolute
//! path whose parent is exactly `<session dir>/tool-results` and whose name is a
//! safe stem with the client's `.txt` suffix: no `..`, no `.`, no other
//! directory. Then the session directory, its `tool-results` directory and the
//! file are each inspected without following links — a symbolic link anywhere
//! there is refused — and the file opened is checked to be the one inspected.
//!
//! **The bound.** A file is read up to [`MAX_SPILL_BYTES`] (4 MiB), and a
//! transcript's spills together up to [`MAX_SPILL_BUDGET`] (32 MiB). The
//! largest spill on the development machine measured 0.5 MB and the median
//! 60 KB, so the per-file bound keeps eight times the largest observed whole
//! while capping what one runaway command can make the archive read and redact
//! on every commit. A file cut at the bound is cut at its last line break inside
//! it, so a token is never split in half before the redactor sees it, and the
//! cut is recorded (`spill_truncated` with the file's size); a spill past the
//! budget is recorded `spill_over_budget` unread.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use rift_shadow::backend::{ArchivedSpill, SpillState, SPILLS_DIR, SPILL_SUFFIX};
use rift_shadow::redact::RulePackSet;

use super::super::subagent_claims::is_safe_id;
use super::tasks::same_file;

/// The most of one spill file an archive reads.
pub(super) const MAX_SPILL_BYTES: u64 = 4 * 1024 * 1024;

/// The most of all one transcript's spill files an archive reads.
pub(super) const MAX_SPILL_BUDGET: u64 = 32 * 1024 * 1024;

/// The client's own field naming a spilled result's file.
const PERSISTED_OUTPUT_PATH: &str = "persistedOutputPath";

/// The first line of the envelope the client wraps a spilled result's preview
/// in.
const ENVELOPE_OPEN: &str = "<persisted-output>";

/// The words before the address on the envelope's second line.
const ENVELOPE_SAVED: &str = "Full output saved to: ";

/// The longest address read; anything longer is not one the client wrote.
const MAX_ADDRESS_BYTES: usize = 4096;

/// How much an archive reads.
#[derive(Clone, Copy)]
pub(super) struct SpillLimits {
    pub(super) per_file: u64,
    pub(super) per_transcript: u64,
}

/// What one transcript's spills are read under.
pub(super) struct SpillScope<'a> {
    /// `<dir of the session transcript>/<session id>`: the only directory
    /// whose `tool-results` a spill may be read from.
    pub(super) session_dir: PathBuf,
    /// The packs the transcript is sealed with.
    pub(super) packs: &'a RulePackSet,
    pub(super) limits: SpillLimits,
}

/// One spill file's bytes as read.
struct Read {
    bytes: Vec<u8>,
    /// The file's size, recorded when the bytes were cut.
    cut_from: Option<u64>,
}

impl<'a> SpillScope<'a> {
    /// The scope of a session whose transcript is `main`, at the archive's
    /// bounds. `None` when the address has no directory.
    pub(super) fn of(main: &Path, session_id: &str, packs: &'a RulePackSet) -> Option<Self> {
        Some(Self {
            session_dir: main.parent()?.join(session_id),
            packs,
            limits: SpillLimits {
                per_file: MAX_SPILL_BYTES,
                per_transcript: MAX_SPILL_BUDGET,
            },
        })
    }
}

/// The spills of one transcript of the session whose transcript is `main`:
/// the session's own, or one of its subagents'.
pub(super) fn of_transcript(
    main: &Path,
    session_id: &str,
    packs: &RulePackSet,
    transcript: &[u8],
) -> Vec<ArchivedSpill> {
    SpillScope::of(main, session_id, packs)
        .map(|scope| transcript_spills(&scope, transcript))
        .unwrap_or_default()
}

/// Every spill `transcript` names, each sealed or accounted for, in
/// tool-use-id order.
pub(super) fn transcript_spills(scope: &SpillScope<'_>, transcript: &[u8]) -> Vec<ArchivedSpill> {
    let mut budget = scope.limits.per_transcript;
    named_spills(transcript)
        .into_iter()
        .map(|(id, address)| spill(scope, id, &address, &mut budget))
        .collect()
}

/// Tool-use id → the address its result names, the first naming winning.
/// Only lines that can carry an address are parsed.
fn named_spills(transcript: &[u8]) -> BTreeMap<String, String> {
    let mut named = BTreeMap::new();
    for line in transcript.split(|b| *b == b'\n') {
        if !mentions(line, PERSISTED_OUTPUT_PATH) && !mentions(line, ENVELOPE_OPEN) {
            continue;
        }
        let Ok(serde_json::Value::Object(record)) = serde_json::from_slice(line) else {
            continue;
        };
        for (id, address) in record_spills(&record) {
            if !is_safe_id(&id) {
                tracing::warn!("a spilled result's tool-use id cannot name a file; not archived");
                continue;
            }
            named.entry(id).or_insert(address);
        }
    }
    named
}

/// Whether `line` holds `needle`. Every turn end runs this over each line of
/// the whole transcript, so a UTF-8 line takes the standard library's
/// substring search rather than a byte-window comparison per offset; a line
/// that is not UTF-8 keeps the window scan, with the same answer.
pub(super) fn mentions(line: &[u8], needle: &str) -> bool {
    match std::str::from_utf8(line) {
        Ok(text) => text.contains(needle),
        Err(_) => line.windows(needle.len()).any(|w| w == needle.as_bytes()),
    }
}

/// The spills one record names. The structured field speaks for the record's
/// result only when the record holds exactly one, so it is never attributed
/// to the wrong call; otherwise each result's envelope speaks for itself.
fn record_spills(record: &serde_json::Map<String, serde_json::Value>) -> Vec<(String, String)> {
    let structured = record
        .get("toolUseResult")
        .and_then(|r| r.get(PERSISTED_OUTPUT_PATH))
        .and_then(serde_json::Value::as_str);
    let results: Vec<&serde_json::Value> = record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|b| b.get("type").and_then(serde_json::Value::as_str) == Some("tool_result"))
        .collect();
    let single = results.len() == 1;
    results
        .into_iter()
        .filter_map(|block| {
            let id = block
                .get("tool_use_id")
                .and_then(serde_json::Value::as_str)?;
            let address = structured
                .filter(|_| single)
                .map(str::to_owned)
                .or_else(|| envelope_address(block.get("content")))?;
            Some((id.to_owned(), address))
        })
        .collect()
}

/// The address the client's envelope names, read only from its front: the
/// result's text must open with the envelope, and the address is the rest of
/// its second line.
fn envelope_address(content: Option<&serde_json::Value>) -> Option<String> {
    let text = match content? {
        serde_json::Value::String(s) => s.as_str(),
        serde_json::Value::Array(blocks) => blocks.first()?.get("text")?.as_str()?,
        _ => return None,
    };
    let mut lines = text
        .strip_prefix(ENVELOPE_OPEN)?
        .strip_prefix('\n')?
        .lines();
    let (_, address) = lines.next()?.split_once(ENVELOPE_SAVED)?;
    Some(address.to_owned())
}

/// Read, redact and seal one named spill, or record why not.
fn spill(scope: &SpillScope<'_>, id: String, address: &str, budget: &mut u64) -> ArchivedSpill {
    let lost = |tool_use_id: String, state| ArchivedSpill {
        tool_use_id,
        state,
        body: None,
        source_bytes: None,
    };
    let Some(path) = contained(&scope.session_dir, address) else {
        return lost(id, SpillState::Refused);
    };
    if *budget == 0 {
        return lost(id, SpillState::OverBudget);
    }
    let read = match read_bounded(
        &scope.session_dir,
        &path,
        scope.limits.per_file.min(*budget),
    ) {
        Ok(read) => read,
        Err(state) => return lost(id, state),
    };
    *budget = budget.saturating_sub(u64::try_from(read.bytes.len()).unwrap_or(u64::MAX));
    let raw = rift_core::SecretBytes::new(read.bytes);
    match rift_shadow::sealed_spill(&raw, scope.packs) {
        Ok(body) => ArchivedSpill {
            tool_use_id: id,
            state: read
                .cut_from
                .map_or(SpillState::Sealed, |_| SpillState::Truncated),
            body: Some(body),
            source_bytes: read.cut_from,
        },
        // `unsealed` is unreachable for any body: the gate seals a line it
        // cannot settle flat or as a placeholder record instead of refusing
        // the body. Kept for the fallible signature, never as a silent drop.
        Err(e) => {
            tracing::warn!(error = %e, tool_use_id = %id, "spilled tool output not sealed");
            lost(id, SpillState::Unsealed)
        }
    }
}

/// The address as a path, when it names a file directly inside
/// `<session dir>/tool-results` by a safe stem and the client's suffix, spelled
/// canonically — the check is on the spelling, before anything is opened.
pub(crate) fn contained(session_dir: &Path, address: &str) -> Option<PathBuf> {
    if address.len() > MAX_ADDRESS_BYTES || address.contains('\0') {
        return None;
    }
    let path = Path::new(address);
    // `components` silently drops an interior `.` and a doubled separator,
    // so the spelling must also be the one the components rebuild: an
    // address is read only as the client writes one.
    let canonical = path.components().collect::<PathBuf>().as_os_str() == path.as_os_str();
    let plain = canonical
        && path.components().all(|c| {
            matches!(
                c,
                Component::Normal(_) | Component::RootDir | Component::Prefix(_)
            )
        });
    if !path.is_absolute() || !plain || path.parent()? != session_dir.join(SPILLS_DIR) {
        return None;
    }
    let stem = path.file_name()?.to_str()?.strip_suffix(SPILL_SUFFIX)?;
    is_safe_id(stem).then(|| path.to_path_buf())
}

/// A contained spill's bytes, at most `bound` of them, or the closed state
/// for why there are none. No link is followed: the session directory, its
/// `tool-results` directory and the file are each inspected as they are, and
/// the file opened must be the one inspected.
fn read_bounded(session_dir: &Path, path: &Path, bound: u64) -> Result<Read, SpillState> {
    for dir in [session_dir.to_path_buf(), session_dir.join(SPILLS_DIR)] {
        let meta = std::fs::symlink_metadata(&dir).map_err(|e| absent_or_unreadable(&e))?;
        if meta.file_type().is_symlink() {
            return Err(SpillState::Refused);
        }
    }
    let inspected = std::fs::symlink_metadata(path).map_err(|e| absent_or_unreadable(&e))?;
    if inspected.file_type().is_symlink() {
        return Err(SpillState::Refused);
    }
    if !inspected.file_type().is_file() {
        return Err(SpillState::Unreadable);
    }
    let file = std::fs::File::open(path).map_err(|_| SpillState::Unreadable)?;
    let opened = file.metadata().map_err(|_| SpillState::Unreadable)?;
    if !same_file(&inspected, &opened) {
        return Err(SpillState::Unreadable);
    }
    let mut bytes = Vec::new();
    file.take(bound.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| SpillState::Unreadable)?;
    let limit = usize::try_from(bound).unwrap_or(usize::MAX);
    if bytes.len() <= limit {
        return Ok(Read {
            bytes,
            cut_from: None,
        });
    }
    let keep = bytes
        .get(..limit)
        .and_then(|head| head.iter().rposition(|b| *b == b'\n'))
        .map_or(limit, |at| at + 1);
    bytes.truncate(keep);
    Ok(Read {
        bytes,
        cut_from: Some(opened.len().max(bound.saturating_add(1))),
    })
}

/// `spill_missing` for an address that names nothing, `spill_unreadable` for
/// any other failure to inspect it.
fn absent_or_unreadable(e: &std::io::Error) -> SpillState {
    match e.kind() {
        std::io::ErrorKind::NotFound => SpillState::Missing,
        _ => SpillState::Unreadable,
    }
}

#[cfg(test)]
mod tests;

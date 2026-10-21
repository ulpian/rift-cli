//! Sealing the spooled originals of rewritten results beside the transcripts
//! that record their calls.
//!
//! Every archive re-seals the whole session, so every archive reads the whole
//! spool of the session ([`super::super::originals`]) and seals every record
//! again: a record is never consumed here, and the spool is not deleted —
//! deletion at the session's final archive is the session lifecycle's (S8
//! L-4), whose hook point is [`Spool::load_carrying`]'s caller once that archive lands.
//!
//! **Which transcript.** A record is sealed beside the transcript whose line
//! records a `tool_result` for its call — the session's, or a subagent's when
//! the call ran in the subagent's window. The match is made on the raw
//! transcript, whose ids no redaction can have touched, and the line's index is
//! recorded: the sealed transcript is the raw one redacted line for line, so
//! verify reads the same line back. A record no transcript claims is an
//! orphan: sealed all the same, beside its subagent's transcript when that
//! subagent was sealed, else beside the session's, and marked
//! `original_line_absent` — never dropped.
//!
//! **What the client recorded.** Judged from the sealed transcript's line and
//! the sealed original, never from the spool: the client recording the
//! original itself means no rewrite was delivered (`spooled_not_delivered`).
//!
//! **The second pass.** Each body is read as [`SecretBytes`] and sealed through
//! the transcript's own gate and packs ([`rift_shadow::transcript::sealed_original`]):
//! the one constructor, never a reused blob. A record that does not read in the
//! spool's grammar, or whose body is not the digest its header names, is
//! sealed as far as it can be and marked `original_spool_corrupt`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::Path;

use rift_core::SecretBytes;
use rift_shadow::backend::rewrite::{
    delivered_result, parse_original_record, LineRange, ResultText, RewriteForm, Rewritten,
};
use rift_shadow::backend::{
    parse_spool_record, ArchivedOriginal, ArchivedTask, ArchivedTranscript, OriginalState,
    TranscriptBody,
};
use rift_shadow::redact::RulePackSet;

use super::super::originals::{session_spool_dir, SPOOL_SUFFIX};
use super::super::subagent_claims::is_safe_id;
use super::carry::{self, TurnCarry};
use super::tasks::same_file;

/// The largest spool record the archive reads: a 30,000-character original
/// escaped to JSON stays far below it.
const MAX_SPOOL_RECORD_BYTES: u64 = 1024 * 1024;

/// One spooled record as the archive read and re-sealed it.
struct SpoolRecord {
    tool_use_id: String,
    agent_id: Option<String>,
    form: Option<RewriteForm>,
    replaced: Vec<LineRange>,
    /// `Sealed` for a whole record, else why it is not.
    state: OriginalState,
    body: Option<TranscriptBody>,
    /// SHA-256 of the record file as read, for the turn-end sidecar.
    raw_sha256: Option<String>,
}

/// A session's spool, read and sealed once per archive.
#[derive(Default)]
pub(super) struct Spool {
    records: Vec<SpoolRecord>,
}

impl Spool {
    /// Every record spooled for `session_id` in the family whose common dir
    /// is `common_dir`, each through the gate with `packs`, in file-name order.
    /// Empty when the session spooled nothing.
    #[cfg(test)]
    pub(super) fn load(common_dir: &Path, session_id: &str, packs: &RulePackSet) -> Self {
        Self::load_carrying(common_dir, session_id, packs, None)
    }

    /// [`Spool::load_carrying`], on a turn end with the previous shard's originals: a
    /// record whose bytes are unchanged keeps that shard's blob, unredacted.
    pub(super) fn load_carrying(
        common_dir: &Path,
        session_id: &str,
        packs: &RulePackSet,
        carry: Option<&TurnCarry>,
    ) -> Self {
        if !rift_shadow::hooks::is_safe_session_id(session_id) {
            return Self::default();
        }
        let dir = session_spool_dir(common_dir, session_id);
        let Some(spool_root) = dir.parent() else {
            return Self::default();
        };
        let real_dirs = [spool_root, dir.as_path()]
            .into_iter()
            .all(|d| std::fs::symlink_metadata(d).is_ok_and(|m| m.file_type().is_dir()));
        if !real_dirs {
            return Self::default();
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Self::default();
        };
        let mut stems: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_owned();
                let stem = name.strip_suffix(SPOOL_SUFFIX)?;
                is_safe_id(stem).then(|| stem.to_owned())
            })
            .collect();
        stems.sort();
        let records = stems
            .into_iter()
            .map(|stem| {
                let bytes = read_record(&dir.join(format!("{stem}{SPOOL_SUFFIX}")));
                record_of(stem, bytes, packs, carry)
            })
            .collect();
        Self { records }
    }

    /// The originals of the calls `raw` records a result for, each judged
    /// against `sealed`, the same transcript through the gate.
    pub(super) fn claimed_by(&self, raw: &[u8], sealed: &[u8]) -> Vec<ArchivedOriginal> {
        if self.records.is_empty() {
            return Vec::new();
        }
        let ids: BTreeSet<&str> = self
            .records
            .iter()
            .map(|r| r.tool_use_id.as_str())
            .collect();
        let lines = result_lines(raw, &ids);
        self.records
            .iter()
            .filter_map(|record| {
                let line = *lines.get(record.tool_use_id.as_str())?;
                let mut original = record.archived(record.state);
                original.line = u64::try_from(line).ok();
                original.rewritten = record.judged(sealed_line(sealed, line));
                Some(original)
            })
            .collect()
    }

    /// The hash of `tool_use_id`'s record file as read, if it was read.
    pub(super) fn raw_sha256(&self, tool_use_id: &str) -> Option<&str> {
        let record = self.records.iter().find(|r| r.tool_use_id == tool_use_id)?;
        record.raw_sha256.as_deref()
    }

    /// Seal every record no transcript claimed beside the transcript it most
    /// plausibly belongs to: its subagent's when that one was sealed, else the
    /// session's. With neither there is nowhere to seal it this archive; the
    /// record stays in the spool for the next.
    pub(super) fn attach_orphans(
        &self,
        mut transcript: Option<&mut ArchivedTranscript>,
        tasks: &mut [ArchivedTask],
    ) {
        let mut claimed: BTreeSet<String> = BTreeSet::new();
        let pairs = transcript.iter().map(|t| &**t);
        for pair in pairs.chain(tasks.iter().filter_map(|t| t.transcript.as_ref())) {
            claimed.extend(pair.originals.iter().map(|o| o.tool_use_id.clone()));
        }
        for record in self
            .records
            .iter()
            .filter(|r| !claimed.contains(&r.tool_use_id))
        {
            let state = match record.state {
                OriginalState::Sealed => OriginalState::LineAbsent,
                other => other,
            };
            let home = tasks
                .iter_mut()
                .filter(|t| record.agent_id.as_deref() == Some(t.agent_id.as_str()))
                .find_map(|t| t.transcript.as_mut())
                .or(transcript.as_deref_mut());
            let Some(pair) = home else {
                tracing::warn!(
                    tool_use_id = %record.tool_use_id,
                    "a spooled original has no transcript to be sealed beside; kept for the next archive"
                );
                continue;
            };
            pair.originals.push(record.archived(state));
        }
    }
}

impl SpoolRecord {
    /// The record as the shard archives it, in `state`, not yet placed.
    fn archived(&self, state: OriginalState) -> ArchivedOriginal {
        ArchivedOriginal {
            tool_use_id: self.tool_use_id.clone(),
            agent_id: self.agent_id.clone(),
            form: self.form,
            replaced: self.replaced.clone(),
            line: None,
            rewritten: None,
            state,
            body: self.body.clone(),
        }
    }

    /// What the client recorded on `line`, against the sealed original;
    /// `None` when either cannot be read.
    fn judged(&self, line: Option<&[u8]>) -> Option<Rewritten> {
        let form = self.form?;
        let (stdout, stderr, _) = parse_original_record(self.body.as_ref()?.as_slice())?;
        let (out, err) = delivered_result(line?)?;
        let original = ResultText {
            stdout: &stdout,
            stderr: &stderr,
        };
        let delivered = ResultText {
            stdout: &out,
            stderr: &err,
        };
        Some(Rewritten::judge(form, original, delivered))
    }
}

/// Line `index` of a transcript, split as the archive splits it.
pub(super) fn sealed_line(transcript: &[u8], index: usize) -> Option<&[u8]> {
    transcript.split(|b| *b == b'\n').nth(index)
}

/// Tool-use id → the first line of `raw` recording a result for it, for the
/// ids in `wanted`. Only lines that can carry a result are parsed.
fn result_lines<'a>(raw: &[u8], wanted: &BTreeSet<&'a str>) -> BTreeMap<&'a str, usize> {
    let mut found = BTreeMap::new();
    for (index, line) in raw.split(|b| *b == b'\n').enumerate() {
        if !super::spills::mentions(line, "tool_result") {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        let blocks = record
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(serde_json::Value::as_array);
        for block in blocks.into_iter().flatten() {
            if block.get("type").and_then(serde_json::Value::as_str) != Some("tool_result") {
                continue;
            }
            let id = block.get("tool_use_id").and_then(serde_json::Value::as_str);
            if let Some(id) = id.and_then(|id| wanted.get(id)) {
                found.entry(*id).or_insert(index);
            }
        }
    }
    found
}

/// One record's bytes, at most [`MAX_SPOOL_RECORD_BYTES`], never through a
/// link; `None` when it cannot be read as the regular file inspected.
fn read_record(path: &Path) -> Option<Vec<u8>> {
    let inspected = std::fs::symlink_metadata(path).ok()?;
    if !inspected.file_type().is_file() || inspected.len() > MAX_SPOOL_RECORD_BYTES {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let opened = file.metadata().ok()?;
    if !same_file(&inspected, &opened) {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_SPOOL_RECORD_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

/// A record from its file's stem and bytes, its body through the gate.
fn record_of(
    stem: String,
    bytes: Option<Vec<u8>>,
    packs: &RulePackSet,
    carry: Option<&TurnCarry>,
) -> SpoolRecord {
    let mut record = SpoolRecord {
        tool_use_id: stem,
        agent_id: None,
        form: None,
        replaced: Vec::new(),
        state: OriginalState::SpoolCorrupt,
        body: None,
        raw_sha256: None,
    };
    let Some(bytes) = bytes else {
        return record;
    };
    let raw_sha256 = carry.map(|_| carry::raw_key(packs, &bytes));
    let carried = carry.zip(raw_sha256.as_deref());
    let carried = carried.and_then(|(c, h)| c.original(&record.tool_use_id, h));
    record.raw_sha256 = raw_sha256;
    let (header, body) = parse_spool_record(&bytes);
    if let Some(h) = &header {
        record.agent_id = h.agent_id.clone().filter(|a| is_safe_id(a));
        record.form = Some(h.form);
        record.replaced.clone_from(&h.replaced);
    }
    let whole = header.as_ref().is_some_and(|h| {
        h.tool_use_id == record.tool_use_id
            && u64::try_from(body.len()).ok() == Some(h.bytes)
            && rift_shadow::sha256_hex(&[body]) == h.sha256
    }) && parse_original_record(body).is_some();
    // The same bytes sealed at the last turn end: that shard's blob stands for
    // them, and the gate's verdict on them cannot have changed.
    if let Some(carried) = carried {
        record.body = Some(carried);
        if whole {
            record.state = OriginalState::Sealed;
        }
        return record;
    }
    carry::gate_ran();
    let raw = SecretBytes::new(body.to_vec());
    // Corrupt spool evidence stays partial evidence; only an intact owned
    // envelope may become Sealed, and its post-gate shape must still hold.
    let sealed = if whole {
        rift_shadow::transcript::sealed_original(&raw, packs)
    } else {
        rift_shadow::transcript::sealed_spill(&raw, packs)
    };
    match sealed {
        Ok(sealed) => {
            record.body = Some(sealed.into());
            if whole {
                record.state = OriginalState::Sealed;
            }
        }
        // A fallback record cannot stand in for the owned original envelope.
        // Preserve the refusal explicitly rather than claim a sealed original.
        Err(e) => {
            tracing::warn!(error = %e, tool_use_id = %record.tool_use_id, "spooled original not sealed");
            if whole {
                record.state = OriginalState::Unsealed;
            }
        }
    }
    record
}

#[cfg(test)]
mod tests;

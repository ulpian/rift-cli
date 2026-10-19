//! The originals of rewritten tool results, sealed beside the transcript that
//! holds the call.
//!
//! A hook that delivers a rewrite in place of a Bash result spools the
//! original first (`spool_original`, in the binary) and the archive seals each
//! spooled original as `tool-originals/<tool-use-id>.json` beside the
//! transcript whose line carries that call's result — the shard's own for the
//! session, `tasks/<id>/tool-originals/` for a subagent. The transcript is
//! untouched: it stays the client's bytes, redacted, and the original lives
//! only under a Rift path, so it can neither be forged by nor confused with a
//! client field. Every sealed body is a Merkle leaf, bound by the same root as
//! its transcript.
//!
//! **The manifest.** The metadata document beside the transcript lists every
//! original sealed there, as `originals`: the call, its form, the ranges it
//! replaced, the transcript line holding the delivered result, what the
//! client recorded (`rewritten`), and the digest and size of the sealed body,
//! or the closed word for why the original is not whole. The body is the
//! redacted canonical record of [`super::rewrite::original_record_json`], so
//! its digest is the one the hook's receipt named whenever the second pass
//! leaves the bytes as the hook sealed them.

use std::collections::BTreeMap;

use rift_core::Result;

use super::rewrite::{LineRange, RewriteForm, Rewritten};
use super::{ArchivedTranscript, CheckpointArchive, OriginalLeafBytes};

/// The directory a transcript's originals are sealed under, beside it.
pub const ORIGINALS_DIR: &str = "tool-originals";

/// The suffix every sealed original carries: the body is one JSON object.
pub const ORIGINAL_SUFFIX: &str = ".json";

/// How a spooled original reached the shard. Every state but
/// [`Self::Sealed`] has a closed word the manifest records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginalState {
    /// Sealed, and the transcript holds the line of its call.
    Sealed,
    /// Sealed, and no transcript of the session holds a line of its call —
    /// an orphan, kept rather than dropped.
    LineAbsent,
    /// The spool record did not read as one the hook writes; what it held is
    /// sealed as far as it could be.
    SpoolCorrupt,
    /// The redaction gate refused the archive's pass over it; nothing sealed.
    Unsealed,
}

impl OriginalState {
    /// The closed word the manifest records, `None` for a whole seal.
    #[must_use]
    pub fn word(self) -> Option<&'static str> {
        match self {
            Self::Sealed => None,
            Self::LineAbsent => Some("original_line_absent"),
            Self::SpoolCorrupt => Some("original_spool_corrupt"),
            Self::Unsealed => Some("original_unsealed"),
        }
    }
}

/// One spooled original, sealed or accounted for.
///
/// No `Debug`: the body holds [`rift_core::RedactedBytes`], which withholds it.
#[derive(Clone)]
pub struct ArchivedOriginal {
    /// The rewritten call — the sealed file's stem.
    pub tool_use_id: String,
    /// The subagent whose window rewrote it, `None` for the session's own.
    pub agent_id: Option<String>,
    /// The form the hook spooled it under; `None` when the record was corrupt.
    pub form: Option<RewriteForm>,
    /// The original's lines the rewrite replaced.
    pub replaced: Vec<LineRange>,
    /// The 0-based line of the transcript beside it that records the call's
    /// result; `None` for an orphan.
    pub line: Option<u64>,
    /// What the client recorded on that line; `None` for an orphan.
    pub rewritten: Option<Rewritten>,
    /// How the original reached the shard.
    pub state: OriginalState,
    /// The sealed bytes; `None` exactly when the state is
    /// [`OriginalState::Unsealed`]. Sealed now, or carried by oid from the
    /// previous turn-end shard when its spool record is unchanged.
    pub body: Option<super::TranscriptBody>,
}

impl ArchivedOriginal {
    /// The sealed file's name inside [`ORIGINALS_DIR`].
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{}{ORIGINAL_SUFFIX}", self.tool_use_id)
    }

    /// This original's manifest entry.
    fn manifest_entry(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("tool_use_id".into(), self.tool_use_id.clone().into());
        if let Some(agent) = &self.agent_id {
            obj.insert("agent_id".into(), agent.clone().into());
        }
        if let Some(form) = self.form {
            obj.insert("form".into(), form.word().into());
        }
        let ranges: Vec<serde_json::Value> = self.replaced.iter().map(|r| r.to_json()).collect();
        obj.insert("replaced".into(), ranges.into());
        if let Some(line) = self.line {
            obj.insert("line".into(), line.into());
        }
        if let Some(rewritten) = self.rewritten {
            obj.insert("rewritten".into(), rewritten.word().into());
        }
        if let Some(body) = &self.body {
            let bytes = body.as_slice();
            obj.insert("bytes".into(), bytes.len().into());
            obj.insert("sha256".into(), crate::sha256_hex(&[bytes]).into());
        }
        if let Some(word) = self.state.word() {
            obj.insert("original".into(), word.into());
        }
        serde_json::Value::Object(obj)
    }
}

impl ArchivedTranscript {
    /// The originals this transcript carries a sealed body for.
    pub(super) fn sealed_originals(
        &self,
    ) -> impl Iterator<Item = (&ArchivedOriginal, &super::TranscriptBody)> {
        self.originals
            .iter()
            .filter_map(|o| o.body.as_ref().map(|body| (o, body)))
    }
}

impl CheckpointArchive {
    /// Every sealed original's leaf bytes — the session transcript's and
    /// every task's, flat, each naming the transcript it sits beside.
    pub(super) fn original_leaves(&self) -> Vec<OriginalLeafBytes<'_>> {
        let own = self.transcript.iter().map(|t| (None, t));
        let tasks = self.tasks.iter().filter_map(|task| {
            let t = task.transcript.as_ref()?;
            Some((Some(task.tool_use_id.as_str()), t))
        });
        own.chain(tasks)
            .flat_map(|(task, t)| {
                t.sealed_originals()
                    .map(move |(o, body)| OriginalLeafBytes {
                        task,
                        tool_use_id: &o.tool_use_id,
                        body: body.as_slice(),
                    })
            })
            .collect()
    }

    /// Add every sealed original to an artifact map, under the path the
    /// shard writer commits it at.
    pub(super) fn insert_original_artifacts(&self, out: &mut BTreeMap<String, Vec<u8>>) {
        for leaf in self.original_leaves() {
            let dir = leaf
                .task
                .map_or_else(String::new, |task| format!("tasks/{task}/"));
            out.insert(
                format!("{dir}{ORIGINALS_DIR}/{}{ORIGINAL_SUFFIX}", leaf.tool_use_id),
                leaf.body.to_vec(),
            );
        }
    }
}

/// Record a transcript's originals on the metadata document its compact
/// boundary is recorded on, in tool-use-id order. Absent when it has none, so
/// a document without a rewrite is byte-identical to what it was.
pub(super) fn insert_original_manifest(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    originals: &[ArchivedOriginal],
) {
    if originals.is_empty() {
        return;
    }
    let mut sorted: Vec<&ArchivedOriginal> = originals.iter().collect();
    sorted.sort_by(|a, b| a.tool_use_id.as_bytes().cmp(b.tool_use_id.as_bytes()));
    let entries: Vec<serde_json::Value> = sorted
        .into_iter()
        .map(ArchivedOriginal::manifest_entry)
        .collect();
    obj.insert("originals".into(), entries.into());
}

/// Refuse an original set the tree cannot faithfully hold: a tool-use id that
/// is not a safe single file stem, or two originals sharing one id. Fires
/// before any object write, like the spill and task refusals.
pub(super) fn validate_originals(originals: &[ArchivedOriginal]) -> Result<()> {
    let mut ids: Vec<&str> = Vec::with_capacity(originals.len());
    for o in originals {
        let id = o.tool_use_id.as_str();
        let safe = !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !safe {
            return Err(rift_core::RiftError::validity(
                "an original's tool-use id must be a safe file stem",
            ));
        }
        ids.push(id);
    }
    ids.sort_unstable();
    let distinct = ids.len();
    ids.dedup();
    if ids.len() != distinct {
        return Err(rift_core::RiftError::validity(
            "two originals of one transcript share one tool-use id",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::redacted_of;
    use super::super::rewrite::Stream;
    use super::*;

    fn original(id: &str, state: OriginalState, body: Option<&[u8]>) -> ArchivedOriginal {
        ArchivedOriginal {
            tool_use_id: id.to_owned(),
            agent_id: None,
            form: Some(RewriteForm::FoldedOutput),
            replaced: vec![LineRange {
                stream: Stream::Stdout,
                start: 0,
                end: 2,
            }],
            line: (state == OriginalState::Sealed).then_some(3),
            rewritten: (state == OriginalState::Sealed)
                .then_some(Rewritten::Delivered(RewriteForm::FoldedOutput)),
            state,
            body: body.map(|b| redacted_of(b).into()),
        }
    }

    fn manifest(originals: &[ArchivedOriginal]) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        insert_original_manifest(&mut obj, originals);
        serde_json::Value::Object(obj)
    }

    /// A sealed original records the digest and size of exactly the bytes it
    /// seals, its form, ranges, line and what the client recorded; an orphan
    /// records its closed word; the manifest is in id order and absent when
    /// empty.
    #[test]
    fn the_manifest_records_each_original_with_its_form_line_and_digest() {
        assert_eq!(manifest(&[]), serde_json::json!({}));
        let mut subagent = original("toolu_c", OriginalState::Unsealed, None);
        subagent.agent_id = Some("a1".into());
        let m = manifest(&[
            original("toolu_b", OriginalState::LineAbsent, Some(b"orphan")),
            original("toolu_a", OriginalState::Sealed, Some(b"body")),
            subagent,
        ]);
        let rows = m["originals"].as_array().unwrap();
        assert_eq!(rows[0]["tool_use_id"], "toolu_a");
        assert_eq!(rows[0]["sha256"], crate::sha256_hex(&[b"body"]));
        assert_eq!(rows[0]["bytes"], 4);
        assert_eq!(rows[0]["form"], "folded_output");
        assert_eq!(rows[0]["line"], 3);
        assert_eq!(rows[0]["rewritten"], "folded_output");
        assert_eq!(
            rows[0]["replaced"],
            serde_json::json!([{ "end": 2, "start": 0, "stream": "stdout" }])
        );
        assert!(rows[0].get("original").is_none());
        assert_eq!(rows[1]["original"], "original_line_absent");
        assert!(rows[1].get("line").is_none());
        assert!(rows[1].get("sha256").is_some());
        assert_eq!(rows[2]["original"], "original_unsealed");
        assert_eq!(rows[2]["agent_id"], "a1");
        assert!(rows[2].get("sha256").is_none());
        assert_eq!(
            original("x", OriginalState::Sealed, None).file_name(),
            "x.json"
        );
    }

    /// Every state but a whole seal has its own closed word.
    #[test]
    fn every_state_but_a_whole_seal_has_a_closed_word() {
        let words: Vec<Option<&str>> = [
            OriginalState::Sealed,
            OriginalState::LineAbsent,
            OriginalState::SpoolCorrupt,
            OriginalState::Unsealed,
        ]
        .into_iter()
        .map(OriginalState::word)
        .collect();
        assert_eq!(
            words,
            [
                None,
                Some("original_line_absent"),
                Some("original_spool_corrupt"),
                Some("original_unsealed"),
            ]
        );
    }

    /// An original set the tree cannot hold is refused before any write.
    #[test]
    fn an_unsafe_or_duplicate_original_id_is_refused() {
        for bad in ["", "a/b", "..", "a.b"] {
            let set = [original(bad, OriginalState::Unsealed, None)];
            assert!(validate_originals(&set).is_err());
        }
        let dup = [
            original("toolu_1", OriginalState::Unsealed, None),
            original("toolu_1", OriginalState::Unsealed, None),
        ];
        assert!(validate_originals(&dup).is_err());
        assert!(validate_originals(&dup[..1]).is_ok());
    }
}

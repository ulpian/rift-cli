//! Spilled tool outputs: the files a client keeps a large tool result in, sealed
//! beside the transcript that names them.
//!
//! Claude Code writes a tool result over its inline threshold to
//! `<session dir>/tool-results/<name>.txt` and leaves the transcript line a
//! short preview and the file's address. A transcript sealed without the file
//! is a record of the preview, so each such file is sealed as
//! `tool-results/<tool-use-id>.txt` beside the transcript it belongs to — the
//! shard's own for the session, `tasks/<id>/tool-results/` for a subagent. It
//! is part of the checkpoint shard, never a ledger of its own (Q37): every
//! sealed body is a Merkle leaf, bound by the same root as its transcript.
//!
//! **The manifest.** The metadata document the transcript's compact boundary is
//! recorded on also lists every spill the transcript named, as `spills`: the
//! digest and size of each sealed body, or the closed word for why none was
//! sealed. It is the reader's enumeration (the substrate has no tree listing),
//! so a body hidden from it is a leaf missing from `rift verify`'s
//! re-derivation, and a loss is recorded where verify and explain read it
//! rather than left as an absence nothing can tell from "there was none".

use std::collections::BTreeMap;

use rift_core::{RedactedBytes, Result};

use super::{ArchivedTranscript, CheckpointArchive, SpillLeafBytes};

/// The directory a transcript's spills are sealed under, beside it.
pub const SPILLS_DIR: &str = "tool-results";

/// The suffix every sealed spill carries, the client's own.
pub const SPILL_SUFFIX: &str = ".txt";

/// How a spill reached the archive. Every state but [`Self::Sealed`] has a
/// closed word the manifest records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpillState {
    /// Read whole and sealed.
    Sealed,
    /// Read up to the archive's bound and sealed; the rest was not read.
    Truncated,
    /// The file the transcript named was gone at archive.
    Missing,
    /// The file was there and could not be read, or was not a regular file.
    Unreadable,
    /// The address was refused: outside the session's `tool-results`
    /// directory, or reached through a symbolic link.
    Refused,
    /// The file was read and the redaction gate refused to seal it.
    Unsealed,
    /// The transcript's spills had already used the archive's budget, so
    /// this one was not read.
    OverBudget,
}

impl SpillState {
    /// The closed word the manifest records, `None` for a whole seal.
    #[must_use]
    pub fn word(self) -> Option<&'static str> {
        match self {
            Self::Sealed => None,
            Self::Truncated => Some("spill_truncated"),
            Self::Missing => Some("spill_missing"),
            Self::Unreadable => Some("spill_unreadable"),
            Self::Refused => Some("spill_refused"),
            Self::Unsealed => Some("spill_unsealed"),
            Self::OverBudget => Some("spill_over_budget"),
        }
    }
}

/// One spill a transcript named, sealed or accounted for.
///
/// No `Debug`: the body is [`RedactedBytes`], which withholds it.
#[derive(Clone)]
pub struct ArchivedSpill {
    /// The tool call whose result spilled — the sealed file's stem.
    pub tool_use_id: String,
    /// How the file reached the archive.
    pub state: SpillState,
    /// The sealed bytes, present exactly when the state is
    /// [`SpillState::Sealed`] or [`SpillState::Truncated`].
    pub body: Option<RedactedBytes>,
    /// The file's size on disk when it was cut, recorded beside a
    /// [`SpillState::Truncated`] body so the loss has a size.
    pub source_bytes: Option<u64>,
}

impl ArchivedSpill {
    /// The sealed file's name inside [`SPILLS_DIR`].
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{}{SPILL_SUFFIX}", self.tool_use_id)
    }

    /// This spill's manifest entry: its digest and size when a body is
    /// sealed, its closed word when it is not whole.
    fn manifest_entry(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("tool_use_id".into(), self.tool_use_id.clone().into());
        if let Some(body) = &self.body {
            let bytes = body.as_slice();
            obj.insert("bytes".into(), bytes.len().into());
            obj.insert("sha256".into(), crate::sha256_hex(&[bytes]).into());
        }
        if let Some(word) = self.state.word() {
            obj.insert("spill".into(), word.into());
        }
        if let Some(size) = self.source_bytes {
            obj.insert("source_bytes".into(), size.into());
        }
        serde_json::Value::Object(obj)
    }
}

impl ArchivedTranscript {
    /// The spills this transcript carries a sealed body for.
    pub(super) fn sealed_spills(&self) -> impl Iterator<Item = (&ArchivedSpill, &RedactedBytes)> {
        self.spills
            .iter()
            .filter_map(|s| s.body.as_ref().map(|body| (s, body)))
    }
}

impl CheckpointArchive {
    /// Every sealed spill's leaf bytes — the session transcript's and every
    /// task's, flat, each naming the transcript it sits beside.
    pub(super) fn spill_leaves(&self) -> Vec<SpillLeafBytes<'_>> {
        let own = self.transcript.iter().map(|t| (None, t));
        let tasks = self.tasks.iter().filter_map(|task| {
            let t = task.transcript.as_ref()?;
            Some((Some(task.tool_use_id.as_str()), t))
        });
        own.chain(tasks)
            .flat_map(|(task, t)| {
                t.sealed_spills().map(move |(s, body)| SpillLeafBytes {
                    task,
                    tool_use_id: &s.tool_use_id,
                    body: body.as_slice(),
                })
            })
            .collect()
    }

    /// Add every sealed spill to an artifact map, under the path the shard
    /// writer commits it at.
    pub(super) fn insert_spill_artifacts(&self, out: &mut BTreeMap<String, Vec<u8>>) {
        for leaf in self.spill_leaves() {
            let dir = leaf
                .task
                .map_or_else(String::new, |task| format!("tasks/{task}/"));
            out.insert(
                format!("{dir}{SPILLS_DIR}/{}{SPILL_SUFFIX}", leaf.tool_use_id),
                leaf.body.to_vec(),
            );
        }
    }
}

/// Record a transcript's spills on the metadata document its compact
/// boundary is recorded on, in tool-use-id order. Absent when the transcript
/// named none, so a spill-less document is byte-identical to what it was.
pub(super) fn insert_spill_manifest(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    spills: &[ArchivedSpill],
) {
    if spills.is_empty() {
        return;
    }
    let mut sorted: Vec<&ArchivedSpill> = spills.iter().collect();
    sorted.sort_by(|a, b| a.tool_use_id.as_bytes().cmp(b.tool_use_id.as_bytes()));
    let entries: Vec<serde_json::Value> = sorted
        .into_iter()
        .map(ArchivedSpill::manifest_entry)
        .collect();
    obj.insert("spills".into(), entries.into());
}

/// Refuse a spill set the tree cannot faithfully hold: a tool-use id that is
/// not a safe single file stem, or two spills sharing one id. The same
/// refusal [`super::shard`] makes of a task set, for the same reason — it
/// fires before any object write.
pub(super) fn validate_spills(spills: &[ArchivedSpill]) -> Result<()> {
    let mut ids: Vec<&str> = Vec::with_capacity(spills.len());
    for s in spills {
        let id = s.tool_use_id.as_str();
        let safe = !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !safe {
            return Err(rift_core::RiftError::validity(
                "a spill tool-use id must be a safe file stem",
            ));
        }
        ids.push(id);
    }
    ids.sort_unstable();
    let distinct = ids.len();
    ids.dedup();
    if ids.len() != distinct {
        return Err(rift_core::RiftError::validity(
            "two spills of one transcript share one tool-use id",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::redacted_of;
    use super::*;

    fn spill(id: &str, state: SpillState, body: Option<&[u8]>) -> ArchivedSpill {
        ArchivedSpill {
            tool_use_id: id.to_owned(),
            state,
            body: body.map(redacted_of),
            source_bytes: (state == SpillState::Truncated).then_some(99),
        }
    }

    fn manifest(spills: &[ArchivedSpill]) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        insert_spill_manifest(&mut obj, spills);
        serde_json::Value::Object(obj)
    }

    /// A sealed spill records the digest and size of exactly the bytes it
    /// seals; a lost one records its closed word and no digest; the manifest
    /// is in tool-use-id order, and absent when there is nothing to list.
    #[test]
    fn the_manifest_records_each_spill_by_digest_or_by_closed_word() {
        assert_eq!(manifest(&[]), serde_json::json!({}));
        let m = manifest(&[
            spill("toolu_b", SpillState::Missing, None),
            spill("toolu_a", SpillState::Sealed, Some(b"out")),
            spill("toolu_c", SpillState::Truncated, Some(b"cut")),
        ]);
        let rows = m["spills"].as_array().unwrap();
        assert_eq!(rows[0]["tool_use_id"], "toolu_a");
        assert_eq!(rows[0]["sha256"], crate::sha256_hex(&[b"out"]));
        assert_eq!(rows[0]["bytes"], 3);
        assert!(rows[0].get("spill").is_none());
        assert_eq!(rows[1]["spill"], "spill_missing");
        assert!(rows[1].get("sha256").is_none());
        assert_eq!(rows[2]["spill"], "spill_truncated");
        assert_eq!(rows[2]["source_bytes"], 99);
        assert_eq!(spill("x", SpillState::Sealed, None).file_name(), "x.txt");
    }

    /// Every way a spill can fail to arrive whole has its own closed word.
    #[test]
    fn every_state_but_a_whole_seal_has_a_closed_word() {
        let words: Vec<Option<&str>> = [
            SpillState::Sealed,
            SpillState::Truncated,
            SpillState::Missing,
            SpillState::Unreadable,
            SpillState::Refused,
            SpillState::Unsealed,
            SpillState::OverBudget,
        ]
        .into_iter()
        .map(SpillState::word)
        .collect();
        assert_eq!(
            words,
            [
                None,
                Some("spill_truncated"),
                Some("spill_missing"),
                Some("spill_unreadable"),
                Some("spill_refused"),
                Some("spill_unsealed"),
                Some("spill_over_budget"),
            ]
        );
    }

    /// A spill set the tree cannot hold is refused before any write.
    #[test]
    fn an_unsafe_or_duplicate_spill_id_is_refused() {
        for bad in ["", "a/b", "..", "a.b", "a\0b"] {
            assert!(validate_spills(&[spill(bad, SpillState::Missing, None)]).is_err());
        }
        let dup = [
            spill("toolu_1", SpillState::Missing, None),
            spill("toolu_1", SpillState::Missing, None),
        ];
        assert!(validate_spills(&dup).is_err());
        assert!(validate_spills(&dup[..1]).is_ok());
    }
}

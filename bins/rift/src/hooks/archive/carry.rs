//! The turn-end carry: the subagent transcripts and spooled
//! originals the previous turn-end shard already holds, so an unchanged one
//! is written again by its blob oid instead of passing the redaction gate a
//! second time: a turn end's cost must not grow with what earlier turns sealed.
//!
//! The frontier sidecar names each entry's raw-bytes hash and blob oids; the
//! previous shard supplies the bytes, admitted only when the tree's oid is
//! the recorded one and the bytes hash to it. A changed raw file, a missing
//! blob or a sidecar naming another checkpoint carries nothing, and the
//! entry is sealed whole as every other road seals it. Only the turn-end road
//! builds a carry; commit, sweep, session-end and recovery never do.

use std::collections::BTreeMap;
use std::path::Path;

use rift_shadow::backend::{
    ArchivedTask, ArchivedTranscript, CarriedPart, RedactionFallback, TranscriptBody,
    ORIGINALS_DIR, TRANSCRIPT_COMPACT_NAME, TRANSCRIPT_FULL_NAME,
};
pub(super) use rift_shadow::hooks::turn_frontier::CarriedEntry;
use rift_shadow::hooks::turn_frontier::{read_one, CarriedKind, Recorded};

use super::originals::Spool;

/// Reads one blob of the previous shard by its path: its oid and bytes.
pub(super) type BlobRead<'a> = &'a dyn Fn(&str) -> Option<(String, Vec<u8>)>;

/// The previous turn-end shard's reusable entries, by agent id or tool-use id.
#[derive(Default)]
pub(super) struct TurnCarry {
    subagents: BTreeMap<String, CarriedSubagent>,
    originals: BTreeMap<String, CarriedOriginal>,
}

/// A subagent pair the previous shard holds.
pub(super) struct CarriedSubagent {
    raw_sha256: String,
    full: CarriedPart,
    compact: CarriedPart,
    fallback: RedactionFallback,
}

/// A spooled original's body the previous shard holds.
struct CarriedOriginal {
    raw_sha256: String,
    body: CarriedPart,
}

impl TurnCarry {
    /// The carry the sidecar recorded for `checkpoint`, each blob read by
    /// `read` (shard path → oid and bytes); empty when the sidecar names
    /// another checkpoint or cannot be read.
    pub(super) fn after(
        git_dir: &Path,
        session: &str,
        checkpoint: &str,
        read: BlobRead<'_>,
    ) -> Self {
        let sessions = crate::family::run_state::sessions_dir(git_dir);
        match read_one::<serde::de::IgnoredAny>(&sessions, session.to_owned()) {
            Recorded::Frontier(f) if f.checkpoint == checkpoint => Self::of(&f.carried, read),
            Recorded::Frontier(_) | Recorded::Unreadable(_) => Self::default(),
        }
    }

    /// The entries whose every blob reads back as the oid recorded for it.
    pub(super) fn of(entries: &[CarriedEntry], read: BlobRead<'_>) -> Self {
        let mut carry = Self::default();
        for entry in entries {
            let blob = |(path, oid): &(String, String)| {
                let (found, bytes) = read(path)?;
                (found == *oid).then_some(())?;
                CarriedPart::verified(found, bytes)
            };
            let Some(mut parts) = entry.blobs.iter().map(blob).collect::<Option<Vec<_>>>() else {
                continue;
            };
            let raw_sha256 = entry.raw_sha256.clone();
            match (entry.kind, parts.len()) {
                (CarriedKind::Subagent, 2) => {
                    let (Some(compact), Some(full)) = (parts.pop(), parts.pop()) else {
                        continue;
                    };
                    let [lines, placeholders] = entry.fallback;
                    let fallback = RedactionFallback {
                        lines,
                        placeholders,
                    };
                    let sub = CarriedSubagent {
                        raw_sha256,
                        full,
                        compact,
                        fallback,
                    };
                    carry.subagents.insert(entry.id.clone(), sub);
                }
                (CarriedKind::Original, 1) => {
                    let Some(body) = parts.pop() else { continue };
                    let original = CarriedOriginal { raw_sha256, body };
                    carry.originals.insert(entry.id.clone(), original);
                }
                _ => {}
            }
        }
        carry
    }

    /// The subagent pair to reuse when `agent_id`'s raw file still hashes to
    /// what was sealed.
    pub(super) fn subagent(&self, agent_id: &str, raw_sha256: &str) -> Option<&CarriedSubagent> {
        self.subagents
            .get(agent_id)
            .filter(|s| s.raw_sha256 == raw_sha256)
    }

    /// The original body to reuse when `tool_use_id`'s spool record still
    /// hashes to what was sealed.
    pub(super) fn original(&self, tool_use_id: &str, raw_sha256: &str) -> Option<TranscriptBody> {
        let original = self.originals.get(tool_use_id)?;
        (original.raw_sha256 == raw_sha256).then(|| TranscriptBody::Carried(original.body.clone()))
    }
}

impl CarriedSubagent {
    /// The pair as the previous shard holds it, before its spills and
    /// originals are attached.
    pub(super) fn pair(&self) -> ArchivedTranscript {
        ArchivedTranscript {
            full: TranscriptBody::Carried(self.full.clone()),
            compact: TranscriptBody::Carried(self.compact.clone()),
            compact_start: 0,
            spills: Vec::new(),
            originals: Vec::new(),
            redaction_fallback: self.fallback,
            reconciliation: None,
        }
    }
}

/// The sidecar entry for a subagent sealed or carried under `tasks/<name>/`.
pub(super) fn subagent_entry(
    agent_id: &str,
    name: &str,
    raw_sha256: String,
    pair: &ArchivedTranscript,
) -> Option<CarriedEntry> {
    let dir = format!("tasks/{name}/");
    let full = (
        format!("{dir}{TRANSCRIPT_FULL_NAME}"),
        pair.full.blob_oid()?,
    );
    let compact = (
        format!("{dir}{TRANSCRIPT_COMPACT_NAME}"),
        pair.compact.blob_oid()?,
    );
    let f = pair.redaction_fallback;
    Some(CarriedEntry {
        id: agent_id.to_owned(),
        kind: CarriedKind::Subagent,
        raw_sha256,
        blobs: vec![full, compact],
        fallback: [f.lines, f.placeholders],
    })
}

/// The sidecar entries for every original body the shard will hold, at the
/// path beside the transcript it was placed with.
pub(super) fn original_entries(
    spool: &Spool,
    transcript: Option<&ArchivedTranscript>,
    tasks: &[ArchivedTask],
) -> Vec<CarriedEntry> {
    let own = transcript.map(|t| (String::new(), t));
    let nested = tasks.iter().filter_map(|task| {
        let t = task.transcript.as_ref()?;
        Some((format!("tasks/{}/", task.tool_use_id), t))
    });
    own.into_iter()
        .chain(nested)
        .flat_map(|(dir, t)| t.originals.iter().map(move |o| (dir.clone(), o)))
        .filter_map(|(dir, o)| {
            let oid = o.body.as_ref()?.blob_oid()?;
            Some(CarriedEntry {
                id: o.tool_use_id.clone(),
                kind: CarriedKind::Original,
                raw_sha256: spool.raw_sha256(&o.tool_use_id)?.to_owned(),
                blobs: vec![(format!("{dir}{ORIGINALS_DIR}/{}", o.file_name()), oid)],
                fallback: [0, 0],
            })
        })
        .collect()
}

#[cfg(test)]
thread_local! {
    /// Redaction passes over subagent files and spooled originals on this
    /// thread, so a test can prove a carried entry skipped the gate.
    pub(super) static GATE_RUNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The key an entry is carried under: the rule packs' fingerprint and the
/// raw bytes, hashed together, so a pack change reseals every entry.
pub(super) fn raw_key(packs: &rift_shadow::redact::RulePackSet, raw: &[u8]) -> String {
    rift_shadow::sha256_hex(&[packs.fingerprint().as_bytes(), b"\0", raw])
}

/// Note one redaction pass over a subagent file or a spooled original.
pub(super) fn gate_ran() {
    #[cfg(test)]
    GATE_RUNS.with(|c| c.set(c.get() + 1));
}

#[cfg(test)]
mod tests;

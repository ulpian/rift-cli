//! Owned native shard bytes shared by verification and projection readers.
//! Reading and leaf assembly are independent of CLI verdicts and rendering.

mod binding;
mod retained;
pub use retained::RetainedNativeShard;
pub mod manifests;

pub use binding::{check_boundary_gap_binding, shard_header, ShardHeader};
use manifests::{OriginalBytes, SpillBytes};
use rift_core::Result;
use rift_recall_substrate::SubstrateReader;

/// The content artifacts a native shard writes and both native passes read.
///
/// Read ONCE per shard and shared, so the Merkle pass and the id pass cannot
/// end up disagreeing about what the shard holds — and so binding the id to the
/// root cost zero extra object reads: `full.jsonl` moved here from inside the
/// Merkle pass, where it was already being read, and both passes now take it.
pub struct NativeArtifacts {
    /// Optional native diagnostic sidecar retained only by origin-bound loading.
    pub recorded_merkle_leaf: Option<Vec<u8>>,
    /// Retained optional recovery audit bytes, read once for authentication.
    pub boundary_gaps: Option<Vec<u8>>,
    /// Stored content value, unchanged from its artifact.
    pub content: Option<Vec<u8>>,
    /// Stored tool calls value, unchanged from its artifact.
    pub tool_calls: Option<Vec<u8>>,
    /// `serve_audit.jsonl`, absent on every shard that froze no serve — and
    /// on every shard written before the artifact existed, whose leaf
    /// sequence absence therefore re-derives unchanged.
    pub serve_audit: Option<Vec<u8>>,
    /// `usage.jsonl`, absent on every shard whose session settled no model
    /// call and on every shard written before the artifact existed; its leaf
    /// trails the sequence, so absence re-derives the old root.
    pub usage: Option<Vec<u8>>,
    /// `sessions.jsonl`, absent on every shard written before the artifact
    /// existed; absence re-derives the old root.
    pub sessions: Option<Vec<u8>>,
    /// `window_journal.jsonl`, absent on every shard whose windows froze no
    /// row and on every shard written before the artifact existed; its leaf
    /// trails the sequence, so absence re-derives the old root.
    pub window_journal: Option<Vec<u8>>,
    /// `session_worktrees.jsonl`, absent on every shard whose session was
    /// never observed and on every shard written before the artifact existed;
    /// absence re-derives the old root.
    pub session_worktrees: Option<Vec<u8>>,
    /// `tool_results.jsonl`, absent on every shard whose session recorded no
    /// measured tool result and on every shard written before the artifact
    /// existed; absence re-derives the old root.
    pub tool_results: Option<Vec<u8>>,
    /// Stored full value, unchanged from its artifact.
    pub full: Option<Vec<u8>>,
    /// Stored transcript compact value, unchanged from its artifact.
    pub transcript_compact: Option<Vec<u8>>,
    /// Stored transcript full value, unchanged from its artifact.
    pub transcript_full: Option<Vec<u8>>,
    /// The `tasks/<tool-use-id>/` subtrees' bytes,
    /// enumerated from the parent metadata's `tasks` list — `SubstrateReader`
    /// has no tree listing and does not need one: a task hidden from the list
    /// is a leaf missing from the re-derivation, so the root and the id both
    /// refuse.
    pub tasks: Vec<TaskArtifactBytes>,
    /// The `sessions/<index>/metadata.json` bytes of a multi-session shard
    /// of a multi-session shard, `(index_name, bytes)` — empty below two sessions.
    pub session_metas: Vec<(String, Vec<u8>)>,
    /// Session metadata paths that existed, even when their bodies were empty.
    pub session_meta_presence: std::collections::BTreeSet<String>,
    /// The four `digest.*` artifacts, each absent on every
    /// pre-digest shard — absence re-derives the pre-digest leaf sequence.
    pub digest_failures: Option<Vec<u8>>,
    /// Stored digest intent value, unchanged from its artifact.
    pub digest_intent: Option<Vec<u8>>,
    /// Stored digest log value, unchanged from its artifact.
    pub digest_log: Option<Vec<u8>>,
    /// Stored digest shape value, unchanged from its artifact.
    pub digest_shape: Option<Vec<u8>>,
    /// The sealed spills beside the session transcript and beside every
    /// task's, flat, each enumerated by its own transcript's manifest.
    pub spills: Vec<SpillBytes>,
    /// The `originals` manifest entries beside the session transcript and
    /// beside every task's, flat, each with its claimed body.
    pub originals: Vec<OriginalBytes>,
}

/// One task subtree's artifact bytes as read back out of the shard.
pub struct TaskArtifactBytes {
    /// Whether metadata existed; missing still hashes as empty for old leaves.
    pub metadata_present: bool,
    /// The id the parent metadata listed the task under (also its tree name).
    pub tool_use_id: String,
    /// `tasks/<id>/metadata.json` — empty when listed but missing, which can
    /// never reproduce a writer's leaf, so the mismatch surfaces as tamper.
    pub metadata: Vec<u8>,
    /// Stored transcript compact value, unchanged from its artifact.
    pub transcript_compact: Option<Vec<u8>>,
    /// Stored transcript full value, unchanged from its artifact.
    pub transcript_full: Option<Vec<u8>>,
}

struct NativeOrigin<'a, R> {
    reader: &'a R,
    shard: &'a rift_recall_substrate::CheckpointShard,
}

impl NativeArtifacts {
    /// Read every artifact the native passes derive from, once per shard.
    /// `session_count` drives the multi-session dirs: at N ≥ 2 the shard
    /// carries `sessions/<index>/metadata.json` for every index; below that
    /// there are none, by the writer's own contract.
    ///
    /// # Errors
    /// Propagates substrate artifact read errors.
    pub fn read(reader: &impl SubstrateReader, id: &str, header: &ShardHeader) -> Result<Self> {
        let gap = reader.read_artifact(id, crate::backend::BOUNDARY_GAPS_NAME)?;
        Self::read_with_gap(reader, id, header, gap)
    }

    /// Read the complete assembly with gap bytes already retained by a caller.
    ///
    /// # Errors
    /// Propagates substrate artifact read errors.
    pub fn read_with_gap(
        reader: &impl SubstrateReader,
        id: &str,
        header: &ShardHeader,
        boundary_gaps: Option<Vec<u8>>,
    ) -> Result<Self> {
        use crate::backend::{TRANSCRIPT_COMPACT_NAME, TRANSCRIPT_FULL_NAME};
        let (task_ids, session_count) = (&header.tasks, header.sessions.len());
        let own = header.spills.clone();
        let mut spills = manifests::read_spills(reader, id, None, own)?;
        let own = header.originals.clone();
        let mut originals = manifests::read_originals(reader, id, None, own)?;
        let mut session_metas = Vec::new();
        let mut session_meta_presence = std::collections::BTreeSet::new();
        if session_count >= 2 {
            for i in 0..session_count {
                let name = i.to_string();
                let bytes = reader.read_artifact(id, &format!("sessions/{name}/metadata.json"))?;
                if bytes.is_some() {
                    session_meta_presence.insert(name.clone());
                }
                session_metas.push((name, bytes.unwrap_or_default()));
            }
        }
        let mut tasks = Vec::with_capacity(task_ids.len());
        for tid in task_ids {
            let metadata = reader.read_artifact(id, &format!("tasks/{tid}/metadata.json"))?;
            let metadata_present = metadata.is_some();
            let metadata = metadata.unwrap_or_default();
            let doc = serde_json::from_slice::<serde_json::Value>(&metadata).ok();
            let claims = manifests::spill_claims(doc.as_ref());
            spills.extend(manifests::read_spills(reader, id, Some(tid), claims)?);
            let claims = manifests::original_claims(doc.as_ref());
            originals.extend(manifests::read_originals(reader, id, Some(tid), claims)?);
            tasks.push(TaskArtifactBytes {
                metadata_present,
                tool_use_id: tid.clone(),
                metadata,
                transcript_compact: reader
                    .read_artifact(id, &format!("tasks/{tid}/{TRANSCRIPT_COMPACT_NAME}"))?,
                transcript_full: reader
                    .read_artifact(id, &format!("tasks/{tid}/{TRANSCRIPT_FULL_NAME}"))?,
            });
        }
        Ok(Self {
            recorded_merkle_leaf: None,
            boundary_gaps,
            content: reader.read_artifact(id, "content.redacted")?,
            tool_calls: reader.read_artifact(id, "tool_calls.jsonl")?,
            serve_audit: reader.read_artifact(id, "serve_audit.jsonl")?,
            usage: reader.read_artifact(id, "usage.jsonl")?,
            sessions: reader.read_artifact(id, "sessions.jsonl")?,
            window_journal: reader.read_artifact(id, "window_journal.jsonl")?,
            session_worktrees: reader.read_artifact(id, "session_worktrees.jsonl")?,
            tool_results: reader.read_artifact(id, "tool_results.jsonl")?,
            full: reader.read_artifact(id, "full.jsonl")?,
            // The writer's own name constants, so the two sides cannot drift.
            transcript_compact: reader.read_artifact(id, TRANSCRIPT_COMPACT_NAME)?,
            transcript_full: reader.read_artifact(id, TRANSCRIPT_FULL_NAME)?,
            tasks,
            session_metas,
            session_meta_presence,
            digest_failures: reader.read_artifact(id, crate::digest::DIGEST_FAILURES_NAME)?,
            digest_intent: reader.read_artifact(id, crate::digest::DIGEST_INTENT_NAME)?,
            digest_log: reader.read_artifact(id, crate::digest::DIGEST_LOG_NAME)?,
            digest_shape: reader.read_artifact(id, crate::digest::DIGEST_SHAPE_NAME)?,
            spills,
            originals,
        })
    }

    /// The per-session-metadata leaf views [`Self::leaves`] borrows.
    #[must_use]
    pub fn session_meta_leaves(&self) -> Vec<crate::backend::SessionMetaLeaf<'_>> {
        self.session_metas
            .iter()
            .map(|(name, bytes)| crate::backend::SessionMetaLeaf {
                index_name: name,
                metadata: bytes,
            })
            .collect()
    }

    /// The task-subtree leaf views [`Self::leaves`] borrows — a separate call
    /// because the borrowed vector must outlive the `ShardLeafBytes` it feeds.
    #[must_use]
    pub fn task_leaves(&self) -> Vec<crate::backend::TaskLeafBytes<'_>> {
        self.tasks
            .iter()
            .map(|t| crate::backend::TaskLeafBytes {
                tool_use_id: &t.tool_use_id,
                metadata: &t.metadata,
                transcript_compact: t.transcript_compact.as_deref(),
                transcript_full: t.transcript_full.as_deref(),
            })
            .collect()
    }

    /// The leaf view both passes feed to the writer's own derivations.
    #[must_use]
    pub fn leaves<'a>(
        &'a self,
        content: &'a [u8],
        report: &'a [u8],
        tasks: &'a [crate::backend::TaskLeafBytes<'a>],
        session_metas: &'a [crate::backend::SessionMetaLeaf<'a>],
        spills: &'a [crate::backend::SpillLeafBytes<'a>],
        originals: &'a [crate::backend::OriginalLeafBytes<'a>],
    ) -> crate::backend::ShardLeafBytes<'a> {
        crate::backend::ShardLeafBytes {
            boundary_gaps: self.boundary_gaps.as_deref(),
            content_redacted: content,
            full_jsonl: self.full.as_deref(),
            redaction_report: report,
            serve_audit: self.serve_audit.as_deref(),
            sessions: self.sessions.as_deref(),
            tool_calls: self.tool_calls.as_deref(),
            tool_results: self.tool_results.as_deref(),
            transcript_compact: self.transcript_compact.as_deref(),
            transcript_full: self.transcript_full.as_deref(),
            usage: self.usage.as_deref(),
            window_journal: self.window_journal.as_deref(),
            session_worktrees: self.session_worktrees.as_deref(),
            tasks,
            spills,
            originals,
            session_metas,
            digests: crate::backend::DigestLeafBytes {
                failures: self.digest_failures.as_deref(),
                intent: self.digest_intent.as_deref(),
                log: self.digest_log.as_deref(),
                shape: self.digest_shape.as_deref(),
            },
        }
    }
}

impl<R: SubstrateReader> SubstrateReader for NativeOrigin<'_, R> {
    fn is_local_only(&self) -> bool {
        self.reader.is_local_only()
    }
    fn checkpoints(&self) -> Result<Vec<rift_recall_substrate::CheckpointShard>> {
        Ok(vec![self.shard.clone()])
    }
    fn blob(&self, _id: &str) -> Result<Option<Vec<u8>>> {
        Err(rift_core::RiftError::integrity(
            "boundary gap: unsupported native blob access",
        ))
    }
    fn read_artifact(&self, id: &str, rel: &str) -> Result<Option<Vec<u8>>> {
        if id != self.shard.checkpoint_id {
            return Err(rift_core::RiftError::integrity(
                "boundary gap: wrong native checkpoint",
            ));
        }
        self.reader.read_native_artifact(id, rel)
    }
}

/// Load and authenticate a present audit exclusively from its native origin.
/// The initial compatibility lookup is compared before any content is admitted.
///
/// # Errors
/// Refuses missing native origins, changed gap bytes or invalid complete binding.
pub fn load_native_boundary_gaps(
    reader: &impl SubstrateReader,
    shard: &rift_recall_substrate::CheckpointShard,
    detected_gap: &[u8],
) -> Result<(ShardHeader, NativeArtifacts, Option<Vec<u8>>)> {
    let invalid =
        || rift_core::RiftError::integrity("boundary gap: native origin unavailable or invalid");
    if shard.dialect != rift_recall_substrate::Dialect::Rift
        || !reader.checkpoints()?.iter().any(|actual| actual == shard)
    {
        return Err(invalid());
    }
    let source = NativeOrigin { reader, shard };
    let id = &shard.checkpoint_id;
    let gap = source.read_artifact(id, crate::backend::BOUNDARY_GAPS_NAME)?;
    if gap.as_deref() != Some(detected_gap) {
        return Err(invalid());
    }
    let header = shard_header(&source, id)?;
    let mut native = NativeArtifacts::read_with_gap(&source, id, &header, gap)?;
    native.recorded_merkle_leaf = source.read_artifact(id, "merkle_leaf.txt")?;
    let report = source.read_artifact(id, "redaction_report.json")?;
    check_boundary_gap_binding(&header, &native, report.as_deref(), id, shard.dialect)?
        .ok_or_else(invalid)?;
    Ok((header, native, report))
}

#[cfg(test)]
mod tests;

//! What a shard says about itself, read once: its header and the artifact
//! bytes every native pass derives from.
//!
//! Separate because both native passes must see exactly the same bytes; one
//! reader shared between them is what stops the root and the id being computed
//! over two different readings of one shard.

use rift_core::Result;
use rift_recall_substrate::SubstrateReader;

/// The header both era-dependent passes read, parsed from `metadata.json` once.
pub(super) struct ShardHeader {
    /// The canonicalization epoch the shard declares, or **0** when the field
    /// (or the file) is absent.
    ///
    /// The era a shard is verified under is read off the shard itself and
    /// nowhere else. Never a date, never the branch, never "shards after commit
    /// X": a repository can be cloned, rebased, re-imported or restored from a
    /// bundle, and none of those change what a shard says about itself. What
    /// the era can no longer buy is the pre-Rift *exemption* — that is the
    /// DIALECT's (the ref the shard was read from), because the one thing
    /// editing a shard cannot rewrite is where the shard is.
    pub(super) canon_version: u32,
    /// The native session ids the checkpoint covers.
    pub(super) sessions: Vec<String>,
    /// The `tasks/<tool-use-id>/` subtrees the shard claims to carry —
    /// the reader's enumeration; empty for a taskless
    /// shard and for every pre-task shard.
    pub(super) tasks: Vec<String>,
    /// The recorded `task_tokens` rollup `(input, output)`, when present —
    /// re-summed against the task subtrees' own metadata below.
    pub(super) task_tokens: Option<(u64, u64)>,
}

/// The content artifacts a native shard writes and both native passes read.
///
/// Read ONCE per shard and shared, so the Merkle pass and the id pass cannot
/// end up disagreeing about what the shard holds — and so binding the id to the
/// root cost zero extra object reads: `full.jsonl` moved here from inside the
/// Merkle pass, where it was already being read, and both passes now take it.
pub(super) struct NativeArtifacts {
    pub(super) content: Option<Vec<u8>>,
    pub(super) tool_calls: Option<Vec<u8>>,
    /// `serve_audit.jsonl`, absent on every shard that froze no serve — and
    /// on every shard written before the artifact existed, whose leaf
    /// sequence absence therefore re-derives unchanged.
    pub(super) serve_audit: Option<Vec<u8>>,
    /// `usage.jsonl`, absent on every shard whose session settled no model
    /// call and on every shard written before the artifact existed; its leaf
    /// trails the sequence, so absence re-derives the old root.
    pub(super) usage: Option<Vec<u8>>,
    /// `sessions.jsonl`, absent on every shard written before the artifact
    /// existed; absence re-derives the old root.
    pub(super) sessions: Option<Vec<u8>>,
    /// `window_journal.jsonl`, absent on every shard whose windows froze no
    /// row and on every shard written before the artifact existed; its leaf
    /// trails the sequence, so absence re-derives the old root.
    pub(super) window_journal: Option<Vec<u8>>,
    /// `session_worktrees.jsonl`, absent on every shard whose session was
    /// never observed and on every shard written before the artifact existed;
    /// absence re-derives the old root.
    pub(super) session_worktrees: Option<Vec<u8>>,
    /// `tool_results.jsonl`, absent on every shard whose session recorded no
    /// measured tool result and on every shard written before the artifact
    /// existed; absence re-derives the old root.
    pub(super) tool_results: Option<Vec<u8>>,
    pub(super) full: Option<Vec<u8>>,
    pub(super) transcript_compact: Option<Vec<u8>>,
    pub(super) transcript_full: Option<Vec<u8>>,
    /// The `tasks/<tool-use-id>/` subtrees' bytes,
    /// enumerated from the parent metadata's `tasks` list — `SubstrateReader`
    /// has no tree listing and does not need one: a task hidden from the list
    /// is a leaf missing from the re-derivation, so the root and the id both
    /// refuse.
    pub(super) tasks: Vec<TaskArtifactBytes>,
    /// The `sessions/<index>/metadata.json` bytes of a multi-session shard
    /// of a multi-session shard, `(index_name, bytes)` — empty below two sessions.
    pub(super) session_metas: Vec<(String, Vec<u8>)>,
    /// The four `digest.*` artifacts, each absent on every
    /// pre-digest shard — absence re-derives the pre-digest leaf sequence.
    pub(super) digest_failures: Option<Vec<u8>>,
    pub(super) digest_intent: Option<Vec<u8>>,
    pub(super) digest_log: Option<Vec<u8>>,
    pub(super) digest_shape: Option<Vec<u8>>,
}

/// One task subtree's artifact bytes as read back out of the shard.
pub(super) struct TaskArtifactBytes {
    /// The id the parent metadata listed the task under (also its tree name).
    pub(super) tool_use_id: String,
    /// `tasks/<id>/metadata.json` — empty when listed but missing, which can
    /// never reproduce a writer's leaf, so the mismatch surfaces as tamper.
    pub(super) metadata: Vec<u8>,
    pub(super) transcript_compact: Option<Vec<u8>>,
    pub(super) transcript_full: Option<Vec<u8>>,
}

impl NativeArtifacts {
    /// Read every artifact the native passes derive from, once per shard.
    /// `session_count` drives the multi-session dirs: at N ≥ 2 the shard
    /// carries `sessions/<index>/metadata.json` for every index; below that
    /// there are none, by the writer's own contract.
    pub(super) fn read(
        reader: &impl SubstrateReader,
        id: &str,
        task_ids: &[String],
        session_count: usize,
    ) -> Result<Self> {
        use rift_shadow::backend::{TRANSCRIPT_COMPACT_NAME, TRANSCRIPT_FULL_NAME};
        let mut session_metas = Vec::new();
        if session_count >= 2 {
            for i in 0..session_count {
                session_metas.push((
                    i.to_string(),
                    reader
                        .read_artifact(id, &format!("sessions/{i}/metadata.json"))?
                        .unwrap_or_default(),
                ));
            }
        }
        let mut tasks = Vec::with_capacity(task_ids.len());
        for tid in task_ids {
            tasks.push(TaskArtifactBytes {
                tool_use_id: tid.clone(),
                metadata: reader
                    .read_artifact(id, &format!("tasks/{tid}/metadata.json"))?
                    .unwrap_or_default(),
                transcript_compact: reader
                    .read_artifact(id, &format!("tasks/{tid}/{TRANSCRIPT_COMPACT_NAME}"))?,
                transcript_full: reader
                    .read_artifact(id, &format!("tasks/{tid}/{TRANSCRIPT_FULL_NAME}"))?,
            });
        }
        Ok(Self {
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
            digest_failures: reader.read_artifact(id, rift_shadow::digest::DIGEST_FAILURES_NAME)?,
            digest_intent: reader.read_artifact(id, rift_shadow::digest::DIGEST_INTENT_NAME)?,
            digest_log: reader.read_artifact(id, rift_shadow::digest::DIGEST_LOG_NAME)?,
            digest_shape: reader.read_artifact(id, rift_shadow::digest::DIGEST_SHAPE_NAME)?,
        })
    }

    /// The per-session-metadata leaf views [`Self::leaves`] borrows.
    pub(super) fn session_meta_leaves(&self) -> Vec<rift_shadow::backend::SessionMetaLeaf<'_>> {
        self.session_metas
            .iter()
            .map(|(name, bytes)| rift_shadow::backend::SessionMetaLeaf {
                index_name: name,
                metadata: bytes,
            })
            .collect()
    }

    /// The task-subtree leaf views [`Self::leaves`] borrows — a separate call
    /// because the borrowed vector must outlive the `ShardLeafBytes` it feeds.
    pub(super) fn task_leaves(&self) -> Vec<rift_shadow::backend::TaskLeafBytes<'_>> {
        self.tasks
            .iter()
            .map(|t| rift_shadow::backend::TaskLeafBytes {
                tool_use_id: &t.tool_use_id,
                metadata: &t.metadata,
                transcript_compact: t.transcript_compact.as_deref(),
                transcript_full: t.transcript_full.as_deref(),
            })
            .collect()
    }

    /// The leaf view both passes feed to the writer's own derivations.
    pub(super) fn leaves<'a>(
        &'a self,
        content: &'a [u8],
        report: &'a [u8],
        tasks: &'a [rift_shadow::backend::TaskLeafBytes<'a>],
        session_metas: &'a [rift_shadow::backend::SessionMetaLeaf<'a>],
    ) -> rift_shadow::backend::ShardLeafBytes<'a> {
        rift_shadow::backend::ShardLeafBytes {
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
            session_metas,
            digests: rift_shadow::backend::DigestLeafBytes {
                failures: self.digest_failures.as_deref(),
                intent: self.digest_intent.as_deref(),
                log: self.digest_log.as_deref(),
                shape: self.digest_shape.as_deref(),
            },
        }
    }
}

/// Read a shard's header. One `metadata.json` read for both fields — the
/// era pass and the id pass used to be two reads of one file.
pub(super) fn shard_header(reader: &impl SubstrateReader, id: &str) -> Result<ShardHeader> {
    use rift_recall_substrate::parse;

    let Some(bytes) = reader.read_artifact(id, "metadata.json")? else {
        return Ok(ShardHeader {
            canon_version: 0,
            sessions: Vec::new(),
            tasks: Vec::new(),
            task_tokens: None,
        });
    };
    let meta = parse::parse_metadata(&bytes, id).value;
    // The task list rides the same document; parsed leniently and locally
    // (the shared lenient metadata type is another crate's surface, and a
    // malformed list simply enumerates nothing — whose missing leaves the
    // root/id passes then refuse).
    let doc = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
    let tasks = doc
        .as_ref()
        .and_then(|v| v.get("tasks").cloned())
        .and_then(|t| serde_json::from_value::<Vec<String>>(t).ok())
        .unwrap_or_default();
    let task_tokens = doc
        .as_ref()
        .and_then(|v| v.get("task_tokens"))
        .and_then(|t| {
            Some((
                t.get("input_tokens").and_then(serde_json::Value::as_u64)?,
                t.get("output_tokens").and_then(serde_json::Value::as_u64)?,
            ))
        });
    Ok(ShardHeader {
        canon_version: meta.canon_version.unwrap_or(0),
        sessions: meta.sessions,
        tasks,
        task_tokens,
    })
}

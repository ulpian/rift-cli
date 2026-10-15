//! One authenticated assembly's retained read view, never an origin proof.
//! Unknown paths and other checkpoint IDs fail rather than reopening a source.

use std::collections::BTreeMap;
use std::path::PathBuf;

use rift_core::{Result, RiftError};
use rift_recall_substrate::{CheckpointShard, SubstrateReader};

use super::{NativeArtifacts, ShardHeader};

/// A read-only view of supplied retained bytes, not proof of their origin.
pub struct RetainedNativeShard<'a> {
    shard: CheckpointShard,
    local: bool,
    worktree: Option<PathBuf>,
    artifacts: BTreeMap<String, Option<&'a [u8]>>,
}

fn unavailable() -> RiftError {
    RiftError::integrity("boundary gap: artifact outside retained shard view")
}

impl<'a> RetainedNativeShard<'a> {
    /// Build a view without reopening artifact bytes from the source.
    /// Origin authentication must precede any use that requires native authority.
    ///
    /// # Errors
    /// Refuses inconsistent duplicate retained paths.
    pub fn new(
        source: &impl SubstrateReader,
        shard: &CheckpointShard,
        header: &'a ShardHeader,
        native: &'a NativeArtifacts,
        report: Option<&'a [u8]>,
    ) -> Result<Self> {
        let mut view = Self {
            shard: shard.clone(),
            local: source.is_local_only(),
            worktree: source.worktree_root(),
            artifacts: BTreeMap::new(),
        };
        view.top_level(header, native, report)?;
        for (index, bytes) in &native.session_metas {
            let retained = native
                .session_meta_presence
                .contains(index)
                .then_some(bytes.as_slice());
            view.add(format!("sessions/{index}/metadata.json"), retained)?;
        }
        for task in &native.tasks {
            let prefix = format!("tasks/{}/", task.tool_use_id);
            view.add(
                format!("{prefix}metadata.json"),
                task.metadata_present.then_some(task.metadata.as_slice()),
            )?;
            view.add(
                format!("{prefix}transcript.compact.jsonl"),
                task.transcript_compact.as_deref(),
            )?;
            view.add(
                format!("{prefix}transcript.jsonl"),
                task.transcript_full.as_deref(),
            )?;
        }
        for spill in &native.spills {
            view.add(spill.path(), spill.body.as_deref())?;
        }
        for original in &native.originals {
            if original.claim.claims_body() {
                view.add(original.path(), original.body.as_deref())?;
            }
        }
        Ok(view)
    }

    fn top_level(
        &mut self,
        header: &'a ShardHeader,
        native: &'a NativeArtifacts,
        report: Option<&'a [u8]>,
    ) -> Result<()> {
        for (name, bytes) in [
            ("merkle_leaf.txt", native.recorded_merkle_leaf.as_deref()),
            ("metadata.json", header.metadata_bytes.as_deref()),
            ("redaction_report.json", report),
            ("content.redacted", native.content.as_deref()),
            ("full.jsonl", native.full.as_deref()),
            ("tool_calls.jsonl", native.tool_calls.as_deref()),
            ("tool_results.jsonl", native.tool_results.as_deref()),
            ("serve_audit.jsonl", native.serve_audit.as_deref()),
            ("sessions.jsonl", native.sessions.as_deref()),
            ("usage.jsonl", native.usage.as_deref()),
            ("window_journal.jsonl", native.window_journal.as_deref()),
            (
                "session_worktrees.jsonl",
                native.session_worktrees.as_deref(),
            ),
            (
                "transcript.compact.jsonl",
                native.transcript_compact.as_deref(),
            ),
            ("transcript.jsonl", native.transcript_full.as_deref()),
            ("digest.failures.json", native.digest_failures.as_deref()),
            ("digest.intent.json", native.digest_intent.as_deref()),
            ("digest.log.json", native.digest_log.as_deref()),
            ("digest.shape.json", native.digest_shape.as_deref()),
            (
                crate::backend::BOUNDARY_GAPS_NAME,
                native.boundary_gaps.as_deref(),
            ),
        ] {
            self.add(name.to_owned(), bytes)?;
        }
        Ok(())
    }

    fn add(&mut self, path: String, bytes: Option<&'a [u8]>) -> Result<()> {
        if let Some(prior) = self.artifacts.get(&path) {
            if prior != &bytes {
                return Err(unavailable());
            }
        } else {
            self.artifacts.insert(path, bytes);
        }
        Ok(())
    }
}

impl SubstrateReader for RetainedNativeShard<'_> {
    fn is_local_only(&self) -> bool {
        self.local
    }
    fn worktree_root(&self) -> Option<PathBuf> {
        self.worktree.clone()
    }
    fn checkpoints(&self) -> Result<Vec<CheckpointShard>> {
        Ok(vec![self.shard.clone()])
    }
    fn blob(&self, _id: &str) -> Result<Option<Vec<u8>>> {
        Err(unavailable())
    }
    fn read_artifact(&self, id: &str, path: &str) -> Result<Option<Vec<u8>>> {
        if id != self.shard.checkpoint_id {
            return Err(unavailable());
        }
        self.artifacts
            .get(path)
            .map(|bytes| bytes.map(<[u8]>::to_vec))
            .ok_or_else(unavailable)
    }
}

#[cfg(test)]
mod tests;

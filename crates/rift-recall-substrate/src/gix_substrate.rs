//! [`GixSubstrate`] — the production [`SubstrateReader`] backed by [`ShardWalker`].
//!
//! This is the live edge of the seam: it opens a real repository and walks it
//! read-only through `gix`. It implements the gix-agnostic [`SubstrateReader`]
//! trait (ids as hex strings) and additionally exposes the richer walker for the
//! shadow/session views `rift-memd doctor`/`metrics` use.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rift_core::Result;

use crate::{parse, CheckpointOrigin, CheckpointShard, CommitMeta, ShardWalker, SubstrateReader};

/// A read-only, local-only substrate over one repository, backed by `gix`.
/// Native-only reads lazily pin one immutable tree (including initial absence)
/// for this reader's lifetime. Reopen to observe a later native publication.
/// Ordinary compatibility reads and checkpoint enumeration remain live.
pub struct GixSubstrate {
    walker: ShardWalker,
    native_tree: std::cell::OnceCell<Option<String>>,
}

impl GixSubstrate {
    /// Open a repository for read-only substrate access.
    ///
    /// # Errors
    /// [`rift_core::RiftError::Substrate`] if the path is not a readable repository.
    pub fn open(repo_root: &Path) -> Result<Self> {
        Ok(Self {
            walker: ShardWalker::open(repo_root)?,
            native_tree: std::cell::OnceCell::new(),
        })
    }

    /// The underlying walker (for shadow refs, session index, tip resolution).
    #[must_use]
    pub fn walker(&self) -> &ShardWalker {
        &self.walker
    }
}

impl SubstrateReader for GixSubstrate {
    fn is_local_only(&self) -> bool {
        self.walker.is_local_only()
    }
    fn checkpoints(&self) -> Result<Vec<CheckpointShard>> {
        self.walker.shards_all()
    }
    fn checkpoint_origins(&self) -> Result<Vec<CheckpointOrigin>> {
        self.walker.checkpoint_origins()
    }
    fn checkpoint_chain(&self) -> Result<Vec<String>> {
        self.walker.chain_order()
    }
    fn blob(&self, content_id: &str) -> Result<Option<Vec<u8>>> {
        self.walker.blob_by_oid(content_id)
    }
    fn read_artifact(&self, checkpoint_id: &str, rel: &str) -> Result<Option<Vec<u8>>> {
        self.walker.read_artifact_any(checkpoint_id, rel)
    }
    fn read_native_artifact(&self, checkpoint_id: &str, rel: &str) -> Result<Option<Vec<u8>>> {
        if self.native_tree.get().is_none() {
            let root = match self.walker.capture_native_tree() {
                Ok(root) => root,
                Err(error) => {
                    let _ = self.native_tree.set(None);
                    return Err(error);
                }
            };
            self.native_tree.set(root).map_err(|_| {
                rift_core::RiftError::substrate("native tree snapshot is already initialized")
            })?;
        }
        match self.native_tree.get() {
            Some(Some(tree)) => self
                .walker
                .read_native_artifact_at_tree(tree, checkpoint_id, rel),
            Some(None) => Ok(None),
            None => Err(rift_core::RiftError::substrate(
                "native tree snapshot is unavailable",
            )),
        }
    }
    fn integration_tips(&self) -> Result<Vec<String>> {
        self.walker.integration_tips()
    }
    fn branch_tips(&self) -> Result<Vec<(String, String)>> {
        self.walker.branch_tips()
    }
    fn commit_meta(&self, oid: &str) -> Result<Option<CommitMeta>> {
        self.walker.commit_meta(oid)
    }
    fn checkpoint_commit(&self, checkpoint_id: &str) -> Result<Option<String>> {
        // The capture side records the working commit it captured in the
        // checkpoint header (`metadata.json`); resolve it when present. A
        // checkpoint without a recorded commit simply yields no outcome label.
        let Some(bytes) = self
            .walker
            .read_artifact_any(checkpoint_id, "metadata.json")?
        else {
            return Ok(None);
        };
        Ok(parse::parse_metadata(&bytes, checkpoint_id).value.commit)
    }
    fn resolve_commitish(&self, target: &str) -> Result<Option<String>> {
        self.walker.resolve_commitish(target)
    }
    fn worktree_root(&self) -> Option<PathBuf> {
        self.walker.workdir()
    }
    fn path_blob_oids(
        &self,
        paths: &BTreeSet<String>,
        max_commits: usize,
    ) -> Result<BTreeMap<String, Vec<String>>> {
        self.walker.path_blob_oids(paths, max_commits)
    }
    fn commit_message(&self, oid: &str) -> Result<Option<String>> {
        self.walker.commit_message(oid)
    }
}

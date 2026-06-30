//! [`GixSubstrate`] — the production [`SubstrateReader`] backed by [`ShardWalker`].
//!
//! This is the live edge of the seam: it opens a real repository and walks it
//! read-only through `gix`. It implements the gix-agnostic [`SubstrateReader`]
//! trait (ids as hex strings) and additionally exposes the richer walker for the
//! shadow/session views `rift-memd doctor`/`metrics` use.

use std::path::Path;

use rift_core::Result;

use crate::{CheckpointShard, ShardWalker, SubstrateReader};

/// A read-only, local-only substrate over one repository, backed by `gix`.
pub struct GixSubstrate {
    walker: ShardWalker,
}

impl GixSubstrate {
    /// Open a repository for read-only substrate access.
    ///
    /// # Errors
    /// [`rift_core::RiftError::Substrate`] if the path is not a readable repository.
    pub fn open(repo_root: &Path) -> Result<Self> {
        Ok(Self {
            walker: ShardWalker::open(repo_root)?,
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
    fn blob(&self, content_id: &str) -> Result<Option<Vec<u8>>> {
        self.walker.blob_by_oid(content_id)
    }
    fn read_artifact(&self, checkpoint_id: &str, rel: &str) -> Result<Option<Vec<u8>>> {
        self.walker.read_artifact_any(checkpoint_id, rel)
    }
}

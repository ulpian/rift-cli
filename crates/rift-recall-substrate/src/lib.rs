//! Read-only substrate reader over the checkpoint/shadow branches.
//!
//! This crate is the recall plane's **only** door onto the canonical record. The
//! production reader walks `rift/checkpoints/v1` shards and shadow refs through
//! `gix` (pure-Rust, **no checkout, no working-tree materialization, no network**);
//! that backend ([`GixSubstrate`]/[`ShardWalker`]) lives behind the default-on
//! `gix` feature. To keep the crate offline-testable, the reader is a trait
//! ([`SubstrateReader`]) with an in-memory implementation ([`MemorySubstrate`])
//! that builds with `--no-default-features`. **`gix` types never cross the seam**
//! — object ids appear as hex `String`s — so callers and `rift-core` stay
//! gix-agnostic.
//!
//! Ingest is **incremental and idempotent**: a [`Watermark`] records the last
//! checkpoint ingested per branch, so re-ingest is a no-op set-union (content
//! addressing, `04-database-design` §4.4). Local-only: a reader refuses to run
//! against a remote (the shadow data is raw and never leaves the machine).
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![allow(clippy::module_name_repetitions, clippy::doc_markdown)]

use std::collections::BTreeMap;

use rift_core::{Result, RiftError};

pub mod ingest;
pub mod parse;
pub mod session;
pub mod worktree;

#[cfg(feature = "gix")]
mod gix_substrate;
#[cfg(feature = "gix")]
mod shadow;
#[cfg(feature = "gix")]
mod walker;

#[cfg(feature = "gix")]
pub use gix_substrate::GixSubstrate;
#[cfg(feature = "gix")]
pub use shadow::{classify_shadow, ShadowRef};
#[cfg(feature = "gix")]
pub use walker::ShardWalker;

/// Which on-disk dialect a checkpoint branch is in. Both share the shard layout;
/// only the ref name (and a few artifact filenames) differ. Never user-surfaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Dialect {
    /// The Rift-native dialect (`rift/…`). Preferred when an id exists in both.
    #[default]
    Rift,
    /// The Entire v0.1 dialect (`entire/…`), mirrored forward.
    Entire,
}

impl Dialect {
    /// The orphan checkpoint branch ref for this dialect.
    #[must_use]
    pub fn checkpoint_ref(self) -> &'static str {
        match self {
            Self::Rift => "refs/heads/rift/checkpoints/v1",
            Self::Entire => "refs/heads/entire/checkpoints/v1",
        }
    }

    /// The local shadow-branch prefix for this dialect.
    #[must_use]
    pub fn shadow_prefix(self) -> &'static str {
        match self {
            Self::Rift => "refs/heads/rift/",
            Self::Entire => "refs/heads/entire/",
        }
    }

    /// The per-session tag namespace for this dialect.
    #[must_use]
    pub fn session_tag_ns(self) -> &'static str {
        match self {
            Self::Rift => "refs/tags/rift/sessions/",
            Self::Entire => "refs/tags/entire/sessions/",
        }
    }

    /// Short label for display (`"rift"` / `"entire"`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Rift => "rift",
            Self::Entire => "entire",
        }
    }
}

/// A checkpoint shard, addressed by its content id, with enough context to fetch
/// its artifacts. `ordinal` is the deterministic walk position (ids are content
/// hashes, so this is sort order, **not** creation order); `prev` is the prior id
/// in that order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointShard {
    /// The content-addressed checkpoint id (hex).
    pub checkpoint_id: String,
    /// Which dialect this shard was read from.
    pub dialect: Dialect,
    /// The prior checkpoint id in walk order (`None` = first).
    pub prev: Option<String>,
    /// Ordinal position in the deterministic walk (for ordering).
    pub ordinal: u64,
}

/// Read-only access to the canonical substrate. `gix` types never appear here:
/// object ids are hex `String`s so the trait stays backend-agnostic.
pub trait SubstrateReader {
    /// Whether this reader is bound to a strictly-local repository (no remote).
    fn is_local_only(&self) -> bool;

    /// Walk checkpoint shards in deterministic (sorted) order.
    ///
    /// # Errors
    /// Implementation-defined read errors.
    fn checkpoints(&self) -> Result<Vec<CheckpointShard>>;

    /// Fetch an immutable blob by content id (hex object id).
    ///
    /// # Errors
    /// Implementation-defined read errors.
    fn blob(&self, content_id: &str) -> Result<Option<Vec<u8>>>;

    /// Fetch an artifact blob *inside* a checkpoint's subtree by relative path
    /// (e.g. `("ab34…", "tool_calls.jsonl")`). Object access only — never opens a
    /// filesystem path. Returns `None` if the artifact is absent.
    ///
    /// # Errors
    /// Implementation-defined read errors.
    fn read_artifact(&self, checkpoint_id: &str, rel: &str) -> Result<Option<Vec<u8>>>;
}

/// The last-ingested watermark per branch (for incremental ingest).
#[derive(Clone, Debug, Default)]
pub struct Watermark {
    last_ordinal: BTreeMap<String, u64>,
}

impl Watermark {
    /// A fresh watermark (nothing ingested).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The shards in `reader` newer than the watermark for `branch`, in order.
    /// Re-running after advancing the watermark yields nothing (idempotent).
    ///
    /// # Errors
    /// Propagates [`SubstrateReader::checkpoints`] errors; refuses a non-local
    /// reader with [`RiftError::PolicyDenied`] (raw shadow data is local-only).
    pub fn pending(
        &self,
        branch: &str,
        reader: &impl SubstrateReader,
    ) -> Result<Vec<CheckpointShard>> {
        if !reader.is_local_only() {
            return Err(RiftError::policy_denied(
                "refusing to read raw substrate from a non-local repository",
            ));
        }
        let last = self.last_ordinal.get(branch).copied().unwrap_or(0);
        let mut shards: Vec<CheckpointShard> = reader
            .checkpoints()?
            .into_iter()
            .filter(|c| c.ordinal > last)
            .collect();
        shards.sort_by_key(|c| c.ordinal);
        Ok(shards)
    }

    /// Advance the watermark for `branch` past the given shards.
    pub fn advance(&mut self, branch: &str, shards: &[CheckpointShard]) {
        if let Some(max) = shards.iter().map(|c| c.ordinal).max() {
            let entry = self.last_ordinal.entry(branch.to_string()).or_insert(0);
            *entry = (*entry).max(max);
        }
    }
}

/// An in-memory substrate for tests / headless use. Always available, including
/// with `--no-default-features` (the gix backend off).
#[derive(Default)]
pub struct MemorySubstrate {
    /// Whether this stands in for a local repo (default true).
    pub local: bool,
    shards: Vec<CheckpointShard>,
    blobs: BTreeMap<String, Vec<u8>>,
    artifacts: BTreeMap<(String, String), Vec<u8>>,
}

impl MemorySubstrate {
    /// A local in-memory substrate.
    #[must_use]
    pub fn local() -> Self {
        Self {
            local: true,
            ..Self::default()
        }
    }

    /// Add a checkpoint shard.
    #[must_use]
    pub fn with_shard(mut self, shard: CheckpointShard) -> Self {
        self.shards.push(shard);
        self
    }

    /// Add a blob, addressed by content id.
    #[must_use]
    pub fn with_blob(mut self, id: &str, bytes: &[u8]) -> Self {
        self.blobs.insert(id.to_string(), bytes.to_vec());
        self
    }

    /// Add an artifact under a checkpoint's subtree (`checkpoint_id`, `rel`).
    #[must_use]
    pub fn with_artifact(mut self, checkpoint_id: &str, rel: &str, bytes: &[u8]) -> Self {
        self.artifacts
            .insert((checkpoint_id.to_string(), rel.to_string()), bytes.to_vec());
        self
    }
}

impl SubstrateReader for MemorySubstrate {
    fn is_local_only(&self) -> bool {
        self.local
    }
    fn checkpoints(&self) -> Result<Vec<CheckpointShard>> {
        Ok(self.shards.clone())
    }
    fn blob(&self, content_id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.blobs.get(content_id).cloned())
    }
    fn read_artifact(&self, checkpoint_id: &str, rel: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .artifacts
            .get(&(checkpoint_id.to_string(), rel.to_string()))
            .cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shard(id: &str, prev: Option<&str>, ord: u64) -> CheckpointShard {
        CheckpointShard {
            checkpoint_id: id.into(),
            dialect: Dialect::Rift,
            prev: prev.map(str::to_string),
            ordinal: ord,
        }
    }

    fn sub() -> MemorySubstrate {
        MemorySubstrate::local()
            .with_shard(shard("c1", None, 1))
            .with_shard(shard("c2", Some("c1"), 2))
            .with_blob("blob-a", b"hello")
            .with_artifact("c1", "tool_calls.jsonl", b"{}")
    }

    #[test]
    fn incremental_ingest_is_idempotent() {
        let s = sub();
        let mut wm = Watermark::new();
        let first = wm.pending("rift/checkpoints/v1", &s).unwrap();
        assert_eq!(first.len(), 2);
        wm.advance("rift/checkpoints/v1", &first);
        let second = wm.pending("rift/checkpoints/v1", &s).unwrap();
        assert!(second.is_empty(), "re-ingest after watermark is a no-op");
    }

    #[test]
    fn only_new_shards_are_pending() {
        let s = sub();
        let mut wm = Watermark::new();
        wm.advance("rift/checkpoints/v1", &[shard("c1", None, 1)]);
        let pending = wm.pending("rift/checkpoints/v1", &s).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].checkpoint_id, "c2");
    }

    #[test]
    fn refuses_non_local_substrate() {
        let mut s = sub();
        s.local = false;
        let wm = Watermark::new();
        assert!(matches!(
            wm.pending("b", &s),
            Err(RiftError::PolicyDenied(_))
        ));
    }

    #[test]
    fn blob_lookup() {
        assert_eq!(sub().blob("blob-a").unwrap(), Some(b"hello".to_vec()));
        assert_eq!(sub().blob("missing").unwrap(), None);
    }

    #[test]
    fn artifact_lookup() {
        assert_eq!(
            sub().read_artifact("c1", "tool_calls.jsonl").unwrap(),
            Some(b"{}".to_vec())
        );
        assert_eq!(sub().read_artifact("c1", "missing.json").unwrap(), None);
        assert_eq!(sub().read_artifact("cX", "tool_calls.jsonl").unwrap(), None);
    }
}

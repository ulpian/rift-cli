//! The gix-backed shard walker (Stage 3 Phase 1 Step 1).
//!
//! Opens a repository for **object access only** — no index, no worktree, no
//! checkout, no network — and enumerates every checkpoint under the orphan
//! `<dialect>/checkpoints/v1` branch by reading the `<id[:2]>/<id[2:]>/` shard
//! tree as Git objects. `gix` types never escape this module: ids leave as hex
//! `String`s, blobs as `Vec<u8>`.

use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};

use crate::{CheckpointShard, CommitMeta, Dialect};

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Read-only handle over one repository's checkpoint substrate.
pub struct ShardWalker {
    repo: gix::Repository,
    git_dir: PathBuf,
}

impl ShardWalker {
    /// Open the repo for object access only. Never requests an index/worktree/
    /// checkout and makes no network calls.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] if the path is not a readable repository.
    pub fn open(repo_root: &Path) -> Result<Self> {
        let repo =
            gix::open(repo_root).map_err(|e| RiftError::substrate(format!("open repo: {e}")))?;
        let git_dir = repo.git_dir().to_path_buf();
        Ok(Self { repo, git_dir })
    }

    /// The `.git` directory of the opened repo (for the out-of-tree session reader).
    #[must_use]
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// Whether this reader is bound to a strictly-local repository. Opening by
    /// path is always local; the substrate makes no network calls and the shadow
    /// reader refuses remote-tracking refs.
    #[must_use]
    pub fn is_local_only(&self) -> bool {
        true
    }

    pub(crate) fn repo(&self) -> &gix::Repository {
        &self.repo
    }

    /// Which dialects' checkpoint branches are present.
    #[must_use]
    pub fn present_dialects(&self) -> Vec<Dialect> {
        [Dialect::Rift, Dialect::Entire]
            .into_iter()
            .filter(|d| self.repo.find_reference(d.checkpoint_ref()).is_ok())
            .collect()
    }

    /// The tip commit's root tree for a dialect's checkpoint branch, if present.
    fn root_tree(&self, dialect: Dialect) -> Result<Option<gix::Tree<'_>>> {
        let Ok(reference) = self.repo.find_reference(dialect.checkpoint_ref()) else {
            return Ok(None); // fresh repo: no such branch
        };
        let id = reference
            .into_fully_peeled_id()
            .map_err(|e| RiftError::substrate(format!("peel ref: {e}")))?;
        let commit = id
            .object()
            .map_err(|e| RiftError::substrate(format!("read object: {e}")))?
            .try_into_commit()
            .map_err(|e| RiftError::substrate(format!("not a commit: {e}")))?;
        let tree = commit
            .tree()
            .map_err(|e| RiftError::substrate(format!("read tree: {e}")))?;
        Ok(Some(tree))
    }

    /// The tip commit oid (hex) of a dialect's checkpoint branch, if present.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn tip_hex(&self, dialect: Dialect) -> Result<Option<String>> {
        let Ok(reference) = self.repo.find_reference(dialect.checkpoint_ref()) else {
            return Ok(None);
        };
        let id = reference
            .into_fully_peeled_id()
            .map_err(|e| RiftError::substrate(format!("peel ref: {e}")))?;
        Ok(Some(id.detach().to_string()))
    }

    /// The tip oid (hex) of an arbitrary fully-qualified ref, if it exists.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a peel failure.
    pub fn ref_tip_hex(&self, full_ref: &str) -> Result<Option<String>> {
        let Ok(reference) = self.repo.find_reference(full_ref) else {
            return Ok(None);
        };
        let id = reference
            .into_fully_peeled_id()
            .map_err(|e| RiftError::substrate(format!("peel ref: {e}")))?;
        Ok(Some(id.detach().to_string()))
    }

    /// The integration-branch tip oid(s) (`main`/`master`), in that preference
    /// order — the merged-into targets for the outcome-label deriver (P4 S2).
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn integration_tips(&self) -> Result<Vec<String>> {
        let mut tips = Vec::new();
        for name in ["refs/heads/main", "refs/heads/master"] {
            if let Some(h) = self.ref_tip_hex(name)? {
                tips.push(h);
            }
        }
        Ok(tips)
    }

    /// Every local working branch tip as `(short_name, tip_oid_hex)`, EXCLUDING
    /// the `rift/` and `entire/` shadow + checkpoint namespaces.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn branch_tips(&self) -> Result<Vec<(String, String)>> {
        let platform = self
            .repo
            .references()
            .map_err(|e| RiftError::substrate(format!("references: {e}")))?;
        let iter = platform
            .all()
            .map_err(|e| RiftError::substrate(format!("iterate refs: {e}")))?;
        let mut out = Vec::new();
        for r in iter {
            let reference = r.map_err(|e| RiftError::substrate(format!("ref: {e}")))?;
            let full = reference.name().as_bstr().to_string();
            let Some(short) = full.strip_prefix("refs/heads/") else {
                continue;
            };
            if short.starts_with("rift/") || short.starts_with("entire/") {
                continue; // shadow + checkpoint namespaces are not working branches
            }
            let short = short.to_string();
            let id = reference
                .into_fully_peeled_id()
                .map_err(|e| RiftError::substrate(format!("peel ref: {e}")))?;
            out.push((short, id.detach().to_string()));
        }
        Ok(out)
    }

    /// Commit metadata (parents, committer time, summary) by hex oid, if it
    /// resolves to a commit object.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a decode failure of an existing commit.
    pub fn commit_meta(&self, hex: &str) -> Result<Option<CommitMeta>> {
        let Ok(oid) = gix::ObjectId::from_hex(hex.as_bytes()) else {
            return Ok(None);
        };
        let Ok(object) = self.repo.find_object(oid) else {
            return Ok(None);
        };
        let Ok(commit) = object.try_into_commit() else {
            return Ok(None);
        };
        let parents: Vec<String> = commit
            .parent_ids()
            .map(|id| id.detach().to_string())
            .collect();
        let seconds = commit
            .committer()
            .map_err(|e| RiftError::substrate(format!("commit committer: {e}")))?
            .seconds();
        let committed_ms = seconds.saturating_mul(1000);
        let raw = commit
            .message_raw()
            .map_err(|e| RiftError::substrate(format!("commit message: {e}")))?
            .to_string();
        let summary = raw.lines().next().unwrap_or("").to_string();
        Ok(Some(CommitMeta {
            oid: hex.to_string(),
            parents,
            committed_ms,
            summary,
        }))
    }

    /// Enumerate every checkpoint id for one dialect, in deterministic id order.
    /// Pure object access: tip commit → root tree → shard subtrees → checkpoint
    /// subtrees. A malformed/junk entry is skipped, never fatal.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn shard_ids(&self, dialect: Dialect) -> Result<Vec<String>> {
        let Some(root) = self.root_tree(dialect)? else {
            return Ok(Vec::new());
        };
        let root_ref = root
            .decode()
            .map_err(|e| RiftError::substrate(format!("decode root tree: {e}")))?;
        let mut ids = Vec::new();
        for shard_entry in &root_ref.entries {
            if !shard_entry.mode.is_tree() {
                continue;
            }
            let prefix = shard_entry.filename.to_string();
            if prefix.len() != 2 || !is_hex(&prefix) {
                continue;
            }
            let shard_tree = self
                .repo
                .find_object(shard_entry.oid)
                .map_err(|e| RiftError::substrate(format!("find shard tree: {e}")))?
                .try_into_tree()
                .map_err(|e| RiftError::substrate(format!("shard not a tree: {e}")))?;
            // Corrupt shard subtree: skip, sibling shards still yield.
            let Ok(shard_ref) = shard_tree.decode() else {
                continue;
            };
            for cp_entry in &shard_ref.entries {
                if !cp_entry.mode.is_tree() {
                    continue;
                }
                let rest = cp_entry.filename.to_string();
                let id = format!("{prefix}{rest}");
                if is_hex(&id) {
                    ids.push(id);
                }
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// Enumerate checkpoint shards for one dialect (id order, with ordinal/prev).
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn shards(&self, dialect: Dialect) -> Result<Vec<CheckpointShard>> {
        Ok(to_shards(self.shard_ids(dialect)?, dialect))
    }

    /// Enumerate checkpoint shards across **all** present dialects, unioned by id
    /// (preferring [`Dialect::Rift`] on overlap), in deterministic id order.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn shards_all(&self) -> Result<Vec<CheckpointShard>> {
        use std::collections::BTreeMap;
        let mut by_id: BTreeMap<String, Dialect> = BTreeMap::new();
        // Insert Entire first, then let Rift overwrite on overlap (Rift preferred).
        for id in self.shard_ids(Dialect::Entire)? {
            by_id.insert(id, Dialect::Entire);
        }
        for id in self.shard_ids(Dialect::Rift)? {
            by_id.insert(id, Dialect::Rift);
        }
        let mut shards = Vec::with_capacity(by_id.len());
        let mut prev: Option<String> = None;
        for (ordinal, (id, dialect)) in by_id.into_iter().enumerate() {
            shards.push(CheckpointShard {
                checkpoint_id: id.clone(),
                dialect,
                prev: prev.take(),
                ordinal: ordinal as u64 + 1,
            });
            prev = Some(id);
        }
        Ok(shards)
    }

    /// Read an artifact blob *inside* a checkpoint's subtree by relative path.
    /// Object access only — a `..`/escaping component just fails the tree lookup,
    /// never touches the filesystem. Returns `None` if absent or not a blob.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn read_artifact_in(
        &self,
        dialect: Dialect,
        checkpoint_id: &str,
        rel: &str,
    ) -> Result<Option<Vec<u8>>> {
        if checkpoint_id.len() < 3 || !is_hex(checkpoint_id) {
            return Ok(None);
        }
        let Some(root) = self.root_tree(dialect)? else {
            return Ok(None);
        };
        let rel_path = Path::new(&checkpoint_id[..2])
            .join(&checkpoint_id[2..])
            .join(rel);
        let entry = root
            .lookup_entry_by_path(&rel_path)
            .map_err(|e| RiftError::substrate(format!("tree lookup: {e}")))?;
        match entry {
            Some(e) if e.mode().is_blob() => {
                let obj = e
                    .object()
                    .map_err(|e| RiftError::substrate(format!("read blob: {e}")))?;
                Ok(Some(obj.data.clone()))
            }
            _ => Ok(None),
        }
    }

    /// Read an artifact across whichever present dialect carries the checkpoint
    /// (Rift preferred). Used by the trait impl, which has no dialect context.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn read_artifact_any(&self, checkpoint_id: &str, rel: &str) -> Result<Option<Vec<u8>>> {
        for dialect in [Dialect::Rift, Dialect::Entire] {
            if let Some(bytes) = self.read_artifact_in(dialect, checkpoint_id, rel)? {
                return Ok(Some(bytes));
            }
        }
        Ok(None)
    }

    /// Fetch an immutable blob by hex object id.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn blob_by_oid(&self, content_id: &str) -> Result<Option<Vec<u8>>> {
        let Ok(oid) = gix::ObjectId::from_hex(content_id.as_bytes()) else {
            return Ok(None);
        };
        match self.repo.find_object(oid) {
            Ok(obj) => Ok(Some(obj.data.clone())),
            Err(_) => Ok(None),
        }
    }
}

/// Turn a sorted id list into shards with deterministic ordinal/prev links.
fn to_shards(mut ids: Vec<String>, dialect: Dialect) -> Vec<CheckpointShard> {
    ids.sort();
    ids.dedup();
    let mut prev: Option<String> = None;
    let mut out = Vec::with_capacity(ids.len());
    for (i, id) in ids.into_iter().enumerate() {
        out.push(CheckpointShard {
            checkpoint_id: id.clone(),
            dialect,
            prev: prev.take(),
            ordinal: i as u64 + 1,
        });
        prev = Some(id);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_shards_assigns_ordinal_and_prev() {
        let shards = to_shards(vec!["bb".into(), "aa".into()], Dialect::Rift);
        assert_eq!(shards[0].checkpoint_id, "aa");
        assert_eq!(shards[0].ordinal, 1);
        assert_eq!(shards[0].prev, None);
        assert_eq!(shards[1].checkpoint_id, "bb");
        assert_eq!(shards[1].prev.as_deref(), Some("aa"));
    }
}

//! The gix-backed shard walker.
//!
//! Opens a repository for **object access only** — no index, no worktree, no
//! checkout, no network — and enumerates every checkpoint under the orphan
//! `<dialect>/checkpoints/v1` branch by reading the `<id[:2]>/<id[2:]>/` shard
//! tree as Git objects. `gix` types never escape this module: ids leave as hex
//! `String`s, blobs as `Vec<u8>`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};

use crate::{CheckpointOrigin, CheckpointShard, CheckpointStorageKind, CommitMeta, Dialect};

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The legacy per-checkpoint ref namespace.
///
/// A **value**, not a name: the on-disk spelling of repositories Rift does not
/// own. Renaming it would not rename anyone's refs — it would stop Rift finding
/// them. Pinned by
/// `the_legacy_per_checkpoint_namespace_is_frozen`.
const LEGACY_CHECKPOINT_REF_PREFIX: &str = concat!("refs/", "ent", "ire", "/checkpoints/");

/// The chain ref carrying the canonical checkpoint order. Frozen:
/// its name is a published artifact other tools read.
const CHAIN_REF: &str = "refs/rift/chain/v1";

/// The trailer a chain entry spells its checkpoint id with.
const CHAIN_TRAILER: &str = "Rift-Chain-Checkpoint";

/// Read a chain entry's checkpoint id out of its commit message.
fn chain_trailer(body: &str) -> Option<String> {
    let prefix = format!("{CHAIN_TRAILER}:");
    for line in body.lines() {
        if let Some(rest) = line.trim().strip_prefix(&prefix) {
            let id = rest.trim();
            if id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Some(id.to_owned());
            }
        }
    }
    None
}

/// The shortest id this walker will accept from a per-checkpoint ref's tree.
/// Both legacy spellings clear it (12-hex and 26-char ULID); it exists to stop a
/// stray two-or-three-character directory being read as a checkpoint.
const MIN_REF_SHARD_ID_LEN: usize = 12;

/// Enumerate `<id[:2]>/<id[2:]>` shard ids inside one ref's commit tree.
///
/// Deliberately looser than [`ShardWalker::shard_ids`], which requires hex
/// because the branch layout's ids are hex. This layout's ids are ULIDs, so the
/// test is "plausible id" — ASCII alphanumeric and long enough — and strict
/// classification stays where it belongs, in the importer. Being loose here is
/// safe in a way it would not be on the branch walk: a junk entry becomes a
/// shard that then fails to parse and is REPORTED as refused, never silently
/// counted.
fn shard_ids_in_tree(repo: &gix::Repository, root: &gix::Tree<'_>) -> Vec<String> {
    let Ok(root_ref) = root.decode() else {
        return Vec::new();
    };
    let mut ids = Vec::new();
    for shard_entry in &root_ref.entries {
        if !shard_entry.mode.is_tree() {
            continue;
        }
        let prefix = shard_entry.filename.to_string();
        if prefix.len() != 2 || !prefix.bytes().all(|b| b.is_ascii_alphanumeric()) {
            continue;
        }
        let Ok(obj) = repo.find_object(shard_entry.oid) else {
            continue;
        };
        let Ok(shard_tree) = obj.try_into_tree() else {
            continue;
        };
        let Ok(shard_ref) = shard_tree.decode() else {
            continue;
        };
        for cp_entry in &shard_ref.entries {
            if !cp_entry.mode.is_tree() {
                continue;
            }
            let id = format!("{prefix}{}", cp_entry.filename);
            if id.len() >= MIN_REF_SHARD_ID_LEN && id.bytes().all(|b| b.is_ascii_alphanumeric()) {
                ids.push(id);
            }
        }
    }
    ids.sort();
    ids.dedup();
    ids
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
        [Dialect::Rift, Dialect::Legacy]
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
    /// order — the merged-into targets for the outcome-label deriver.
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
    /// the native and retired shadow + checkpoint namespaces.
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
            if short.starts_with("rift/") || short.starts_with(concat!("ent", "ire", "/")) {
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

    /// Every legacy **per-checkpoint ref**, as `(full_ref, tip_oid_hex)`, sorted.
    ///
    /// The second legacy layout: one ref per checkpoint under
    /// the retired per-checkpoint namespace rather than one shared orphan
    /// branch. Nothing walked these before — [`branch_tips`]
    /// excludes the namespace by contract and [`shard_ids`] only ever reads a
    /// dialect's single checkpoint *branch* — so a repository that migrated to
    /// this layout was invisible to every reader in the tree.
    ///
    /// [`branch_tips`]: ShardWalker::branch_tips
    /// [`shard_ids`]: ShardWalker::shard_ids
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn legacy_checkpoint_refs(&self) -> Result<Vec<(String, String)>> {
        let platform = self
            .repo
            .references()
            .map_err(|e| RiftError::substrate(format!("open refs: {e}")))?;
        let iter = platform
            .prefixed(LEGACY_CHECKPOINT_REF_PREFIX)
            .map_err(|e| RiftError::substrate(format!("iter refs: {e}")))?;
        let mut out = Vec::new();
        for r in iter {
            let r = r.map_err(|e| RiftError::substrate(format!("read ref: {e}")))?;
            let name = r.name().as_bstr().to_string();
            let Ok(id) = r.into_fully_peeled_id() else {
                continue; // a ref that will not peel is not a checkpoint
            };
            out.push((name, id.detach().to_string()));
        }
        out.sort();
        Ok(out)
    }

    /// The shard ids carried by the legacy per-checkpoint refs, each paired with
    /// the ref that carries it, in deterministic order.
    ///
    /// Each ref's commit holds the same `<id[:2]>/<id[2:]>/` shard tree the
    /// branch layout uses, so the id comes from the TREE — where the artifacts
    /// actually are — rather than from the ref name, which a writer is free to
    /// spell differently. Junk entries are skipped, never fatal.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn legacy_ref_shard_ids(&self) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        for (name, _oid) in self.legacy_checkpoint_refs()? {
            let Some(root) = self.tree_of_ref(&name)? else {
                continue;
            };
            for id in shard_ids_in_tree(&self.repo, &root) {
                out.push((id, name.clone()));
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// Every physical storage origin for every discovered checkpoint. Unlike
    /// `shards_all`, this deliberately does not union duplicate ids.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a ref/tree read failure.
    pub fn checkpoint_origins(&self) -> Result<Vec<CheckpointOrigin>> {
        let mut out = Vec::new();
        for dialect in [Dialect::Rift, Dialect::Legacy] {
            let tip_oid = self.tip_hex(dialect)?;
            for checkpoint_id in self.shard_ids(dialect)? {
                out.push(CheckpointOrigin {
                    checkpoint_id,
                    dialect,
                    storage_ref: dialect.checkpoint_ref().to_owned(),
                    kind: CheckpointStorageKind::Branch,
                    tip_oid: tip_oid.clone(),
                });
            }
        }
        for (checkpoint_id, storage_ref) in self.legacy_ref_shard_ids()? {
            let tip_oid = self.ref_tip_hex(&storage_ref)?;
            out.push(CheckpointOrigin {
                checkpoint_id,
                dialect: Dialect::Legacy,
                storage_ref,
                kind: CheckpointStorageKind::PerCheckpointRef,
                tip_oid,
            });
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// The tip commit's root tree for an arbitrary fully-qualified ref.
    fn tree_of_ref(&self, full_ref: &str) -> Result<Option<gix::Tree<'_>>> {
        let Ok(reference) = self.repo.find_reference(full_ref) else {
            return Ok(None);
        };
        let Ok(id) = reference.into_fully_peeled_id() else {
            return Ok(None);
        };
        let Ok(obj) = id.object() else {
            return Ok(None);
        };
        let Ok(commit) = obj.try_into_commit() else {
            return Ok(None); // a ref pointing at a non-commit is not a checkpoint
        };
        let tree = commit
            .tree()
            .map_err(|e| RiftError::substrate(format!("read tree: {e}")))?;
        Ok(Some(tree))
    }

    /// The checkpoint ids in **chain order**, oldest first, read from
    /// `refs/rift/chain/v1`.
    ///
    /// The canonical order lives in that ref. Read here with gix rather than
    /// through `rift-git` on purpose: a new crate edge would move the reviewable
    /// adjacency snapshot, which is a reviewed surface, and this needs only
    /// the commit objects the walker already has a repository handle for.
    ///
    /// Empty when the ref does not exist — a repository that has not run
    /// `rift chain init` yet has no order, and callers must say so rather than
    /// invent one.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn chain_order(&self) -> Result<Vec<String>> {
        let Ok(reference) = self.repo.find_reference(CHAIN_REF) else {
            return Ok(Vec::new());
        };
        let Ok(id) = reference.into_fully_peeled_id() else {
            return Ok(Vec::new());
        };
        let mut newest_first = Vec::new();
        let mut cursor = Some(id.detach());
        // First-parent walk. No blob or tree is touched: the id is in the commit
        // message trailer precisely so a treeless consumer can read the order.
        while let Some(oid) = cursor.take() {
            let Ok(obj) = self.repo.find_object(oid) else {
                break;
            };
            let Ok(commit) = obj.try_into_commit() else {
                break;
            };
            if let Ok(message) = commit.message_raw() {
                if let Some(cid) = chain_trailer(&message.to_string()) {
                    newest_first.push(cid);
                }
            }
            cursor = commit.parent_ids().next().map(gix::Id::detach);
        }
        newest_first.reverse();
        Ok(newest_first)
    }

    /// Enumerate checkpoint shards across **all** present dialects, unioned by id
    /// (preferring [`Dialect::Rift`] on overlap), in deterministic id order.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn shards_all(&self) -> Result<Vec<CheckpointShard>> {
        use std::collections::BTreeMap;
        let mut by_id: BTreeMap<String, Dialect> = BTreeMap::new();
        // Insert the legacy backend first, then let Rift overwrite on overlap (Rift preferred).
        for id in self.shard_ids(Dialect::Legacy)? {
            by_id.insert(id, Dialect::Legacy);
        }
        // …and the per-checkpoint-ref layout, which carries the same dialect.
        for (id, _ref_name) in self.legacy_ref_shard_ids()? {
            by_id.insert(id, Dialect::Legacy);
        }
        for id in self.shard_ids(Dialect::Rift)? {
            by_id.insert(id, Dialect::Rift);
        }
        // ORDER COMES FROM THE CHAIN REF, NOT FROM SORTING IDS.
        //
        // This used to hand `by_id`'s BTreeMap iteration order straight out —
        // i.e. sorted content hashes — and call it the chain. Content hashes
        // have no relation to write time, so a newly arrived checkpoint sorted
        // into the middle about half the time and was assigned the SAME
        // predecessor as an existing one: a fork that append-only could never
        // correct. `refs/rift/chain/v1` records the real order.
        //
        // A checkpoint absent from the chain keeps its id-sorted position AFTER
        // every chained one, and is exactly what `rift chain init` has not yet
        // covered — visible rather than silently interleaved.
        let order = self.chain_order()?;
        let mut ordered: Vec<(String, Dialect)> = Vec::with_capacity(by_id.len());
        for id in &order {
            if let Some(d) = by_id.remove(id) {
                ordered.push((id.clone(), d));
            }
        }
        ordered.extend(by_id);

        let mut shards = Vec::with_capacity(ordered.len());
        let mut prev: Option<String> = None;
        for (ordinal, (id, dialect)) in ordered.into_iter().enumerate() {
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

    /// A native checkpoint's artifact with the blob oid its tree entry names,
    /// so a caller can carry it forward by that oid (a turn's earlier segments).
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn read_native_entry(
        &self,
        checkpoint_id: &str,
        rel: &str,
    ) -> Result<Option<(String, Vec<u8>)>> {
        if checkpoint_id.len() < 3 || !is_hex(checkpoint_id) {
            return Ok(None);
        }
        let Some(root) = self.root_tree(Dialect::Rift)? else {
            return Ok(None);
        };
        let path = Path::new(&checkpoint_id[..2])
            .join(&checkpoint_id[2..])
            .join(rel);
        let entry = root
            .lookup_entry_by_path(&path)
            .map_err(|e| RiftError::substrate(format!("tree lookup: {e}")))?;
        let Some(entry) = entry.filter(|e| e.mode().is_blob()) else {
            return Ok(None);
        };
        let oid = entry.oid().to_string();
        let blob = entry
            .object()
            .map_err(|e| RiftError::substrate(format!("read blob: {e}")))?;
        Ok(Some((oid, blob.data.clone())))
    }

    /// Capture one immutable native tree without exposing gix types.
    pub(crate) fn capture_native_tree(&self) -> Result<Option<String>> {
        Ok(self
            .root_tree(Dialect::Rift)?
            .map(|tree| tree.id.to_string()))
    }

    /// Read only the captured native tree; never consult a ref or legacy origin.
    pub(crate) fn read_native_artifact_at_tree(
        &self,
        tree_oid: &str,
        checkpoint_id: &str,
        rel: &str,
    ) -> Result<Option<Vec<u8>>> {
        if checkpoint_id.len() < 3 || !is_hex(checkpoint_id) {
            return Ok(None);
        }
        let invalid = || RiftError::substrate("captured native tree is unavailable or invalid");
        let oid = gix::ObjectId::from_hex(tree_oid.as_bytes()).map_err(|_| invalid())?;
        let root = self
            .repo
            .find_object(oid)
            .map_err(|_| invalid())?
            .try_into_tree()
            .map_err(|_| invalid())?;
        let path = Path::new(&checkpoint_id[..2])
            .join(&checkpoint_id[2..])
            .join(rel);
        let entry = root.lookup_entry_by_path(&path).map_err(|_| invalid())?;
        match entry {
            Some(entry) if entry.mode().is_blob() => {
                Ok(Some(entry.object().map_err(|_| invalid())?.data.clone()))
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
        for dialect in [Dialect::Rift, Dialect::Legacy] {
            if let Some(bytes) = self.read_artifact_in(dialect, checkpoint_id, rel)? {
                return Ok(Some(bytes));
            }
        }
        // Neither checkpoint BRANCH carries it — try the per-checkpoint refs.
        // Without this leg a shard could be enumerated and then read as empty,
        // which is a worse failure than not enumerating it: the caller would see
        // a checkpoint with no artifacts and conclude the shard was malformed.
        self.read_artifact_in_legacy_refs(checkpoint_id, rel)
    }

    /// Read an artifact out of whichever legacy per-checkpoint ref carries it.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] on a read failure.
    pub fn read_artifact_in_legacy_refs(
        &self,
        checkpoint_id: &str,
        rel: &str,
    ) -> Result<Option<Vec<u8>>> {
        if checkpoint_id.len() < 3 || !checkpoint_id.is_char_boundary(2) {
            return Ok(None);
        }
        let rel_path = Path::new(&checkpoint_id[..2])
            .join(&checkpoint_id[2..])
            .join(rel);
        for (name, _oid) in self.legacy_checkpoint_refs()? {
            let Some(root) = self.tree_of_ref(&name)? else {
                continue;
            };
            let Ok(Some(e)) = root.lookup_entry_by_path(&rel_path) else {
                continue;
            };
            if e.mode().is_blob() {
                let obj = e
                    .object()
                    .map_err(|e| RiftError::substrate(format!("read blob: {e}")))?;
                return Ok(Some(obj.data.clone()));
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

    /// The checkout this repository is opened over, when it has one. A bare
    /// repository has none, and the answer is then `None` rather than a guess —
    /// a caller that wants the file as it is now has nowhere to read it from.
    #[must_use]
    pub fn workdir(&self) -> Option<PathBuf> {
        self.repo.workdir().map(Path::to_path_buf)
    }

    /// Every distinct blob object id each of `paths` carried over the most
    /// recent `max_commits` commits of the checked-out branch's first-parent
    /// chain, newest first.
    ///
    /// This is the candidate set for the question "are these recorded bytes in
    /// git at all": the caller hashes each candidate blob and keeps the one
    /// whose SHA-256 is the revision it is looking for. First-parent and
    /// bounded, because the answer is a saving and not a proof — a revision
    /// older than the bound, or on a side of a merge this branch did not walk,
    /// is simply not found, which costs a re-read and never names wrong bytes.
    ///
    /// An unborn HEAD, an unreadable commit or an entry that is not a blob all
    /// yield fewer candidates, never an error: absence of evidence stays
    /// absence.
    ///
    /// # Errors
    /// [`RiftError::Substrate`] if HEAD exists but its commit cannot be read.
    pub fn path_blob_oids(
        &self,
        paths: &BTreeSet<String>,
        max_commits: usize,
    ) -> Result<BTreeMap<String, Vec<String>>> {
        let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if paths.is_empty() || max_commits == 0 {
            return Ok(out);
        }
        let Ok(head) = self.repo.head_commit() else {
            return Ok(out); // unborn HEAD: nothing committed, nothing to offer
        };
        let mut commit = head;
        let mut seen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for _ in 0..max_commits {
            let tree = commit
                .tree()
                .map_err(|e| RiftError::substrate(format!("read tree: {e}")))?;
            for path in paths {
                let Ok(Some(entry)) = tree.lookup_entry_by_path(path) else {
                    continue;
                };
                if !entry.mode().is_blob() {
                    continue;
                }
                let oid = entry.oid().to_string();
                if seen.entry(path.clone()).or_default().insert(oid.clone()) {
                    out.entry(path.clone()).or_default().push(oid);
                }
            }
            let Some(parent) = commit.parent_ids().next() else {
                break;
            };
            let Ok(object) = parent.object() else {
                break;
            };
            let Ok(next) = object.try_into_commit() else {
                break;
            };
            commit = next;
        }
        Ok(out)
    }

    /// Resolve a local revision expression to a commit oid using gix's
    /// in-process revision parser. Appending `^{commit}` rejects blobs/trees and
    /// does not invoke a subprocess or contact a remote.
    ///
    /// # Errors
    /// Resolution failure is `Ok(None)`; object decode failures are reported.
    pub fn resolve_commitish(&self, target: &str) -> Result<Option<String>> {
        if target.is_empty() || target.starts_with('-') || target.bytes().any(|b| b == 0) {
            return Ok(None);
        }
        let spec = format!("{target}^{{commit}}");
        let Ok(id) = self.repo.rev_parse_single(spec.as_str()) else {
            return Ok(None);
        };
        Ok(Some(id.detach().to_string()))
    }

    /// Read the full message of one commit. The message is used only for
    /// bounded trailer parsing and is never rendered by the Recall Viewer.
    ///
    /// # Errors
    /// Existing-but-undecodable commit data is a substrate error.
    pub fn commit_message(&self, oid: &str) -> Result<Option<String>> {
        let Ok(oid) = gix::ObjectId::from_hex(oid.as_bytes()) else {
            return Ok(None);
        };
        let Ok(object) = self.repo.find_object(oid) else {
            return Ok(None);
        };
        let Ok(commit) = object.try_into_commit() else {
            return Ok(None);
        };
        let message = commit
            .message_raw()
            .map_err(|e| RiftError::substrate(format!("commit message: {e}")))?;
        Ok(Some(message.to_string()))
    }
}

/// Turn an id list into shards for **enumeration**. Ids are sorted for
/// determinism; `prev` is left `None`.
///
/// ENUMERATION IS NOT ORDER. This function used to chain the sorted ids —
/// `prev = the previous id in hash order` — and that invented linkage is the
/// defect the chain ref exists to fix: content hashes have no relation to write
/// time, so a later checkpoint sorting into the middle was handed the same
/// predecessor as an existing one, forking a chain append-only could not mend.
///
/// The only place a `prev` is now derived is [`ShardWalker::shards_all`], from
/// `refs/rift/chain/v1`. `to_shards_never_invents_an_order` fails if a sort
/// sneaks back into this position.
fn to_shards(mut ids: Vec<String>, dialect: Dialect) -> Vec<CheckpointShard> {
    ids.sort();
    ids.dedup();
    ids.into_iter()
        .enumerate()
        .map(|(i, id)| CheckpointShard {
            checkpoint_id: id,
            dialect,
            prev: None,
            ordinal: i as u64 + 1,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_shards_never_invents_an_order() {
        // Sorted for determinism, but NOT chained: a hash sort is not a history.
        let shards = to_shards(vec!["bb".into(), "aa".into()], Dialect::Rift);
        assert_eq!(shards[0].checkpoint_id, "aa");
        assert_eq!(shards[1].checkpoint_id, "bb");
        assert!(
            shards.iter().all(|s| s.prev.is_none()),
            "enumeration must not manufacture linkage; the chain ref is the order"
        );
    }

    /// `is_hex` is the branch walk's whole defence against reading a stray
    /// directory as a checkpoint id: empty is not hex, and a single non-hex
    /// byte anywhere disqualifies the whole string.
    #[test]
    fn the_hex_test_refuses_empty_and_any_non_hex_byte() {
        for ok in ["0", "abcdef", "ABCDEF", "0123456789abcdefABCDEF"] {
            assert!(is_hex(ok), "{ok} is hex");
        }
        for bad in ["", "g", "abcdefg", "ab cd", "ab-cd", "０", "abcdef\n"] {
            assert!(!is_hex(bad), "{bad:?} is not hex");
        }
    }

    /// The chain trailer is read out of a commit message by name, and the id
    /// must be a full 64-hex digest — a truncated, over-long or non-hex value
    /// is not a checkpoint id and the entry reads as having none, rather than
    /// putting a junk id into the canonical order.
    #[test]
    fn the_chain_trailer_accepts_only_a_full_hex_digest() {
        let id = "a".repeat(64);
        let good = format!("subject\n\nbody\n\n{CHAIN_TRAILER}: {id}\n");
        assert_eq!(chain_trailer(&good), Some(id.clone()));
        // Leading whitespace on the trailer line is tolerated (the line is
        // trimmed before the prefix test) and so is trailing whitespace.
        assert_eq!(
            chain_trailer(&format!("s\n\n   {CHAIN_TRAILER}:  {id}   \n")),
            Some(id.clone())
        );
        // The first well-formed trailer wins, and a malformed one before it
        // does not shadow it.
        let second = "b".repeat(64);
        assert_eq!(
            chain_trailer(&format!(
                "s\n\n{CHAIN_TRAILER}: short\n{CHAIN_TRAILER}: {second}\n"
            )),
            Some(second)
        );
        for bad in [
            String::new(),
            "subject only".to_owned(),
            format!("{CHAIN_TRAILER} {id}"), // no colon
            format!("{CHAIN_TRAILER}: {}", "a".repeat(63)), // short
            format!("{CHAIN_TRAILER}: {}", "a".repeat(65)), // long
            format!("{CHAIN_TRAILER}: {}", "z".repeat(64)), // not hex
            format!("X-{CHAIN_TRAILER}: {id}"), // a different trailer
        ] {
            assert_eq!(chain_trailer(&bad), None, "{bad:?}");
        }
    }
}

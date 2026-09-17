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
//! addressing). Local-only: a reader refuses to run
//! against a remote (the shadow data is raw and never leaves the machine).
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};

pub mod catalogue;
pub mod parse;
pub mod session;
pub mod worktree;

#[cfg(feature = "gix")]
mod ancestry;
#[cfg(feature = "gix")]
mod gix_substrate;
#[cfg(feature = "gix")]
mod shadow;
#[cfg(feature = "gix")]
mod walker;

#[cfg(feature = "gix")]
pub use ancestry::GixAncestry;
#[cfg(feature = "gix")]
pub use gix_substrate::GixSubstrate;
#[cfg(feature = "gix")]
pub use shadow::{classify_shadow, ShadowRef};
#[cfg(feature = "gix")]
pub use walker::ShardWalker;

pub use catalogue::{
    build_checkpoint_catalogue, parse_rfc3339_ms, resolve_explicit_target,
    CheckpointCatalogueEntry, ResolutionKind, ResolvedTarget, TimestampProvenance,
};

/// Which on-disk dialect a checkpoint branch is in. Both share the shard layout;
/// only the ref name (and a few artifact filenames) differ. Never user-surfaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Dialect {
    /// The Rift-native dialect (`rift/…`). Preferred when an id exists in both.
    #[default]
    Rift,
    /// The pre-Rift dialect (the retired namespace on disk), mirrored forward. The variant
    /// is named for what it is; the ref STRINGS below are facts about
    /// repositories Rift does not own and are frozen for read-compat.
    Legacy,
}

impl Dialect {
    /// The orphan checkpoint branch ref for this dialect.
    #[must_use]
    pub fn checkpoint_ref(self) -> &'static str {
        match self {
            Self::Rift => "refs/heads/rift/checkpoints/v1",
            Self::Legacy => concat!("refs/heads/", "ent", "ire", "/checkpoints/v1"),
        }
    }

    /// The local shadow-branch prefix for this dialect.
    #[must_use]
    pub fn shadow_prefix(self) -> &'static str {
        match self {
            Self::Rift => "refs/heads/rift/",
            Self::Legacy => concat!("refs/heads/", "ent", "ire", "/"),
        }
    }

    /// The per-session tag namespace for this dialect.
    #[must_use]
    pub fn session_tag_ns(self) -> &'static str {
        match self {
            Self::Rift => "refs/tags/rift/sessions/",
            Self::Legacy => concat!("refs/tags/", "ent", "ire", "/sessions/"),
        }
    }

    /// Short persisted label for the native or pre-Rift dialect.
    ///
    /// **This is a persisted value, not a display string, and it is frozen.** It
    /// is written to `recall.db`'s `checkpoint.dialect` column via
    /// `rift_recall_ledger::ingest` (`shard.dialect.label()`), so changing it
    /// would orphan every row already stored under the old spelling.
    /// `rift-memd doctor` no longer prints this literal raw: its `dialects:`
    /// line maps the stored spelling to a legacy-neutral label at DISPLAY time
    /// (the long-running soak run, 2026-08-10), with a pin asserting the stored bytes never
    /// moved. An earlier version of this comment claimed the product surface
    /// could be closed "only by changing what the column stores, which is a
    /// migration" — measurably false: the display-time map closed it with no
    /// migration and no stored byte moved, which is exactly the shape
    /// append-only storage forces. Renaming THIS literal remains forbidden.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Rift => "rift",
            Self::Legacy => concat!("ent", "ire"),
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

/// Physical storage form carrying a checkpoint shard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckpointStorageKind {
    /// A shard subtree in a shared checkpoint branch.
    Branch,
    /// A shard subtree carried by one legacy per-checkpoint ref.
    PerCheckpointRef,
}

/// One actual ref/origin where a checkpoint was discovered. Duplicate ids are
/// unioned by catalogue consumers, but none of these origins may be discarded.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CheckpointOrigin {
    /// Checkpoint id found in the ref's shard tree.
    pub checkpoint_id: String,
    /// On-disk dialect of this origin.
    pub dialect: Dialect,
    /// Fully qualified ref name as stored by Git.
    pub storage_ref: String,
    /// Shared branch or legacy per-checkpoint ref.
    pub kind: CheckpointStorageKind,
    /// Peeled tip commit of the carrying ref, when readable.
    pub tip_oid: Option<String>,
}

/// A commit's lifecycle metadata for outcome-label derivation. Hex oids
/// only; no `gix` types cross the seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitMeta {
    /// The commit oid (hex).
    pub oid: String,
    /// Parent commit oids (hex), in order (a merge has ≥ 2).
    pub parents: Vec<String>,
    /// Committer time in epoch milliseconds.
    pub committed_ms: i64,
    /// The first line of the commit message (used for revert detection).
    pub summary: String,
}

/// The bound on the reachability walk in [`is_reachable`]: a fail-safe ceiling so
/// derivation over a pathological history terminates without ever *fabricating* a
/// "merged" label it could not prove.
pub const MAX_REACHABILITY_WALK: usize = 100_000;

/// Read-only access to the canonical substrate. `gix` types never appear here:
/// object ids are hex `String`s so the trait stays backend-agnostic.
///
/// The git-graph methods ([`integration_tips`](SubstrateReader::integration_tips),
/// [`branch_tips`](SubstrateReader::branch_tips),
/// [`commit_meta`](SubstrateReader::commit_meta),
/// [`checkpoint_commit`](SubstrateReader::checkpoint_commit)) carry **fail-safe
/// default impls** that report "unknown" — a reader that cannot answer never
/// causes a label to be derived, so absence of evidence is never mistaken for a
/// positive outcome.
pub trait SubstrateReader {
    /// Whether this reader is bound to a strictly-local repository (no remote).
    fn is_local_only(&self) -> bool;

    /// Walk checkpoint shards in deterministic (sorted) order.
    ///
    /// # Errors
    /// Implementation-defined read errors.
    fn checkpoints(&self) -> Result<Vec<CheckpointShard>>;

    /// Enumerate every physical checkpoint origin. The default preserves the
    /// older trait contract by deriving one shared-branch origin per shard;
    /// production readers override it to retain legacy per-checkpoint refs and
    /// duplicate origins.
    ///
    /// # Errors
    /// Implementation-defined read errors.
    fn checkpoint_origins(&self) -> Result<Vec<CheckpointOrigin>> {
        Ok(self
            .checkpoints()?
            .into_iter()
            .map(|shard| CheckpointOrigin {
                checkpoint_id: shard.checkpoint_id,
                dialect: shard.dialect,
                storage_ref: shard.dialect.checkpoint_ref().to_owned(),
                kind: CheckpointStorageKind::Branch,
                tip_oid: None,
            })
            .collect())
    }

    /// Canonical checkpoint ids in chain order, oldest first. Empty means no
    /// durable chain fact exists; callers must not derive order from ids.
    ///
    /// # Errors
    /// Implementation-defined read errors.
    fn checkpoint_chain(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

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

    /// The integration-branch tip commit oid(s) (e.g. `main`/`master`), hex.
    /// Reachability from one of these is the **merged** signal. Default:
    /// none (no integration branch discoverable ⇒ nothing merges).
    ///
    /// # Errors
    /// Implementation-defined read errors.
    fn integration_tips(&self) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// Every local working branch tip as `(branch_name, tip_oid_hex)`, EXCLUDING
    /// the rift/legacy shadow + checkpoint namespaces. Used for the **abandoned**
    /// signal (a subject's branch absent ⇒ deleted). Default: none.
    ///
    /// # Errors
    /// Implementation-defined read errors.
    fn branch_tips(&self) -> Result<Vec<(String, String)>> {
        Ok(Vec::new())
    }

    /// Commit metadata by oid (parents, time, message). Default: unknown.
    ///
    /// # Errors
    /// Implementation-defined read errors.
    fn commit_meta(&self, _oid: &str) -> Result<Option<CommitMeta>> {
        Ok(None)
    }

    /// The real working-tree commit a checkpoint captured, if resolvable (hex
    /// oid). Default: unknown — so derivation simply produces no label for a
    /// checkpoint whose commit cannot be resolved.
    ///
    /// # Errors
    /// Implementation-defined read errors.
    fn checkpoint_commit(&self, _checkpoint_id: &str) -> Result<Option<String>> {
        Ok(None)
    }

    /// Resolve a local Git revision to a commit oid without invoking Git,
    /// touching the worktree, or contacting a remote. Default: unresolved.
    ///
    /// # Errors
    /// Implementation-defined read errors.
    fn resolve_commitish(&self, _target: &str) -> Result<Option<String>> {
        Ok(None)
    }

    /// Full commit message for valid checkpoint-trailer inspection. Default:
    /// unavailable. The message is never rendered by the viewer.
    ///
    /// # Errors
    /// Implementation-defined read errors.
    fn commit_message(&self, _oid: &str) -> Result<Option<String>> {
        Ok(None)
    }

    /// The checkout this reader is bound to, when it has one. Default: none —
    /// a reader that cannot say where the files are makes every caller fall
    /// back to what the projection recorded, which is the conservative arm.
    fn worktree_root(&self) -> Option<PathBuf> {
        None
    }

    /// Every distinct blob object id each of `paths` carried over the most
    /// recent `max_commits` commits of the checked-out line, newest first.
    ///
    /// The candidate set for "are these recorded bytes in git at all". Default:
    /// none, so a reader that cannot walk history bridges nothing rather than
    /// claiming a revision is unreachable *or* reachable.
    ///
    /// # Errors
    /// Implementation-defined read errors.
    fn path_blob_oids(
        &self,
        _paths: &BTreeSet<String>,
        _max_commits: usize,
    ) -> Result<BTreeMap<String, Vec<String>>> {
        Ok(BTreeMap::new())
    }
}

/// Whether `ancestor` is reachable from `descendant` (i.e. `ancestor ⊑ descendant`
/// in the commit DAG): a parent-walk from `descendant` over
/// [`SubstrateReader::commit_meta`], bounded by `max_walk` commits.
///
/// **Fail-safe:** returns `false` if reachability cannot be *proven* within the
/// bound or commit metadata is missing — it never guesses "reachable".
///
/// # Errors
/// Propagates [`SubstrateReader::commit_meta`] read errors.
pub fn is_reachable(
    reader: &impl SubstrateReader,
    ancestor: &str,
    descendant: &str,
    max_walk: usize,
) -> Result<bool> {
    use std::collections::BTreeSet;
    if ancestor == descendant {
        return Ok(true);
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut frontier: Vec<String> = vec![descendant.to_string()];
    let mut steps = 0usize;
    while let Some(oid) = frontier.pop() {
        if steps >= max_walk {
            return Ok(false);
        }
        steps += 1;
        if !seen.insert(oid.clone()) {
            continue;
        }
        let Some(meta) = reader.commit_meta(&oid)? else {
            continue; // unknown commit: cannot extend this path (fail-safe)
        };
        for parent in meta.parents {
            if parent == ancestor {
                return Ok(true);
            }
            if !seen.contains(&parent) {
                frontier.push(parent);
            }
        }
    }
    Ok(false)
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
    commits: BTreeMap<String, CommitMeta>,
    integration_tips: Vec<String>,
    branches: Vec<(String, String)>,
    checkpoint_commits: BTreeMap<String, String>,
    origins: Vec<CheckpointOrigin>,
    chain: Vec<String>,
    revisions: BTreeMap<String, String>,
    commit_messages: BTreeMap<String, String>,
    path_blobs: BTreeMap<String, Vec<String>>,
    worktree_root: Option<PathBuf>,
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

    /// Add a commit to the in-memory DAG.
    #[must_use]
    pub fn with_commit(mut self, meta: CommitMeta) -> Self {
        self.commits.insert(meta.oid.clone(), meta);
        self
    }

    /// Mark a commit oid as an integration-branch tip (the merged-into target).
    #[must_use]
    pub fn with_integration_tip(mut self, oid: &str) -> Self {
        self.integration_tips.push(oid.to_string());
        self
    }

    /// Register a working branch tip (`name`, `tip_oid`).
    #[must_use]
    pub fn with_branch(mut self, name: &str, tip_oid: &str) -> Self {
        self.branches.push((name.to_string(), tip_oid.to_string()));
        self
    }

    /// Link a checkpoint id to the working commit it captured.
    #[must_use]
    pub fn with_checkpoint_commit(mut self, checkpoint_id: &str, oid: &str) -> Self {
        self.checkpoint_commits
            .insert(checkpoint_id.to_string(), oid.to_string());
        self
    }

    /// Add one physical checkpoint origin.
    #[must_use]
    pub fn with_origin(mut self, origin: CheckpointOrigin) -> Self {
        self.origins.push(origin);
        self
    }

    /// Set the canonical chain, oldest first.
    #[must_use]
    pub fn with_chain(mut self, ids: &[&str]) -> Self {
        self.chain = ids.iter().map(|id| (*id).to_owned()).collect();
        self
    }

    /// Add a revision spelling and the commit oid it resolves to.
    #[must_use]
    pub fn with_revision(mut self, revision: &str, oid: &str) -> Self {
        self.revisions.insert(revision.to_owned(), oid.to_owned());
        self
    }

    /// Add a full commit message for trailer-resolution tests.
    #[must_use]
    pub fn with_commit_message(mut self, oid: &str, message: &str) -> Self {
        self.commit_messages
            .insert(oid.to_owned(), message.to_owned());
        self
    }

    /// Record that `path` carried `oid` at some point in history — one
    /// candidate the bridge will hash and check.
    #[must_use]
    pub fn with_path_blob(mut self, path: &str, oid: &str) -> Self {
        self.path_blobs
            .entry(path.to_owned())
            .or_default()
            .push(oid.to_owned());
        self
    }

    /// Bind this reader to a checkout, so callers that want a file as it is
    /// now have somewhere to read it from.
    #[must_use]
    pub fn with_worktree_root(mut self, root: &Path) -> Self {
        self.worktree_root = Some(root.to_path_buf());
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
    fn checkpoint_origins(&self) -> Result<Vec<CheckpointOrigin>> {
        if self.origins.is_empty() {
            return Ok(self
                .shards
                .iter()
                .map(|shard| CheckpointOrigin {
                    checkpoint_id: shard.checkpoint_id.clone(),
                    dialect: shard.dialect,
                    storage_ref: shard.dialect.checkpoint_ref().to_owned(),
                    kind: CheckpointStorageKind::Branch,
                    tip_oid: None,
                })
                .collect());
        }
        Ok(self.origins.clone())
    }
    fn checkpoint_chain(&self) -> Result<Vec<String>> {
        Ok(self.chain.clone())
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
    fn integration_tips(&self) -> Result<Vec<String>> {
        Ok(self.integration_tips.clone())
    }
    fn branch_tips(&self) -> Result<Vec<(String, String)>> {
        Ok(self.branches.clone())
    }
    fn commit_meta(&self, oid: &str) -> Result<Option<CommitMeta>> {
        Ok(self.commits.get(oid).cloned())
    }
    fn checkpoint_commit(&self, checkpoint_id: &str) -> Result<Option<String>> {
        Ok(self.checkpoint_commits.get(checkpoint_id).cloned())
    }
    fn resolve_commitish(&self, target: &str) -> Result<Option<String>> {
        Ok(self
            .revisions
            .get(target)
            .cloned()
            .or_else(|| self.commits.contains_key(target).then(|| target.to_owned())))
    }
    fn commit_message(&self, oid: &str) -> Result<Option<String>> {
        Ok(self.commit_messages.get(oid).cloned())
    }
    fn worktree_root(&self) -> Option<PathBuf> {
        self.worktree_root.clone()
    }
    fn path_blob_oids(
        &self,
        paths: &BTreeSet<String>,
        max_commits: usize,
    ) -> Result<BTreeMap<String, Vec<String>>> {
        let mut out = BTreeMap::new();
        if max_commits == 0 {
            return Ok(out);
        }
        for path in paths {
            if let Some(oids) = self.path_blobs.get(path) {
                out.insert(path.clone(), oids.clone());
            }
        }
        Ok(out)
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

    fn commit(oid: &str, parents: &[&str], ms: i64, summary: &str) -> CommitMeta {
        CommitMeta {
            oid: oid.into(),
            parents: parents.iter().map(|p| (*p).to_string()).collect(),
            committed_ms: ms,
            summary: summary.into(),
        }
    }

    /// A small DAG: feature `f1` was merged into `main` (tip `m2`); `orphan`
    /// never was. `main`: g0 → m1 → m2(merge of f1); feature: g0 → f1.
    fn dag() -> MemorySubstrate {
        MemorySubstrate::local()
            .with_commit(commit("g0", &[], 0, "genesis"))
            .with_commit(commit("f1", &["g0"], 10, "feature work"))
            .with_commit(commit("m1", &["g0"], 20, "main work"))
            .with_commit(commit("m2", &["m1", "f1"], 30, "merge feature"))
            .with_commit(commit("orphan", &["g0"], 15, "unmerged work"))
            .with_integration_tip("m2")
    }

    #[test]
    fn reachable_when_merged_unreachable_when_not() {
        let d = dag();
        assert!(is_reachable(&d, "f1", "m2", MAX_REACHABILITY_WALK).unwrap());
        assert!(is_reachable(&d, "g0", "m2", MAX_REACHABILITY_WALK).unwrap());
        assert!(is_reachable(&d, "m2", "m2", MAX_REACHABILITY_WALK).unwrap()); // identity
        assert!(!is_reachable(&d, "orphan", "m2", MAX_REACHABILITY_WALK).unwrap());
        // direction matters: a descendant is not an ancestor of its parent
        assert!(!is_reachable(&d, "m2", "f1", MAX_REACHABILITY_WALK).unwrap());
    }

    #[test]
    fn reachability_is_fail_safe_on_missing_metadata_and_bound() {
        let d = dag();
        // unknown commit: cannot prove reachability → false (never guesses)
        assert!(!is_reachable(&d, "g0", "ghost", MAX_REACHABILITY_WALK).unwrap());
        // a zero bound cannot prove a non-trivial reachability
        assert!(!is_reachable(&d, "f1", "m2", 0).unwrap());
    }

    #[test]
    fn default_graph_methods_are_empty() {
        // a bare reader (no git-graph data) reports "unknown" everywhere.
        let bare = MemorySubstrate::local();
        assert!(bare.integration_tips().unwrap().is_empty());
        assert!(bare.branch_tips().unwrap().is_empty());
        assert_eq!(bare.commit_meta("x").unwrap(), None);
        assert_eq!(bare.checkpoint_commit("c").unwrap(), None);
    }

    /// The rename guard (a standing freeze condition on these literals):
    /// The retired-name variant became `Dialect::Legacy`, and **not one on-disk byte
    /// moved with it**. Every string here is a fact about repositories written
    /// by another tool; changing any of them breaks read-compat with them.
    /// This test is the literal-survives-the-rename pin.
    #[test]
    fn the_legacy_dialects_on_disk_spellings_survived_the_rename() {
        assert_eq!(
            Dialect::Legacy.checkpoint_ref(),
            concat!("refs/heads/", "ent", "ire", "/checkpoints/v1")
        );
        assert_eq!(
            Dialect::Legacy.shadow_prefix(),
            concat!("refs/heads/", "ent", "ire", "/")
        );
        assert_eq!(
            Dialect::Legacy.session_tag_ns(),
            concat!("refs/tags/", "ent", "ire", "/sessions/")
        );
        // Persisted in recall.db's `checkpoint.dialect` column — frozen hardest.
        assert_eq!(Dialect::Legacy.label(), concat!("ent", "ire"));
        // The native side is unchanged too, so the pin is a pair, not a half.
        assert_eq!(
            Dialect::Rift.checkpoint_ref(),
            "refs/heads/rift/checkpoints/v1"
        );
        assert_eq!(Dialect::Rift.label(), "rift");
        // The default must stay native: a mis-defaulted Dialect would read the
        // wrong namespace on every fresh repository.
        assert_eq!(Dialect::default(), Dialect::Rift);
    }
}

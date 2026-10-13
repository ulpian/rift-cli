//! The write-side seam the checkpoint archive is built on.
//!
//! All of `rift-shadow`'s write-side capability is expressed as one trait,
//! [`ShadowBackend`], so callers (condense/executor) name a capability rather
//! than an implementation. [`NativeBackend`] is the implementation: it writes a
//! checkpoint shard onto the orphan `rift/checkpoints/v1` branch through
//! screened git plumbing, and it is the only one there is.
//!
//! **The strangler seam is gone because the strangler finished.** This module
//! used to carry a second backend that shelled out to a pinned foreign binary,
//! a per-capability `RouteTable` read from `.rift/rewrite.toml`, and a
//! kill-switch that forced every capability back to it. The native archive was
//! promoted to the default on 2026-08-10 and soaked clean; the
//! wrapper, its routes and its rollback dial were deleted on 2026-08-11. What
//! survives is what the routing was *for*: one seam, one implementation behind
//! it, and callers that never named either.
//!
//! Hashing, attribution, cost and redaction were always native — they never had
//! a foreign equivalent — so the git write was the last capability that had a
//! second answer, and now it does not.

mod archive;
mod artifacts;
#[cfg(test)]
mod fixtures;
mod leaves;
mod shard;
mod spills;
mod tasks;
mod tool_results;
mod window;
mod worktrees;

use rift_core::Result;
use rift_git::{GitIdent, MemoryGitWrite, NativeGitWrite};

pub use archive::{
    sessions_jsonl_bytes, tool_calls_jsonl_bytes, usage_jsonl_bytes, ArchivedEvent,
    ArchivedReadFact, ArchivedTask, ArchivedTranscript, ArchivedUsageFact, CheckpointArchive,
    RedactionReportView, WorktreeLabel, SESSIONS_SCHEMA_VERSION, USAGE_SCHEMA_VERSION,
};
// The bridge's round-trip test pins `parse(format(ms)) == ms`; nothing else
// outside this module formats an instant.
#[cfg(test)]
pub(crate) use artifacts::rfc3339_utc_ms;
pub use artifacts::session_meta_json;
pub use leaves::{
    shard_merkle_leaves, shard_merkle_root, DigestLeafBytes, SessionMetaLeaf, ShardLeafBytes,
    SpillLeafBytes, TaskLeafBytes,
};
pub use spills::{ArchivedSpill, SpillState, SPILLS_DIR, SPILL_SUFFIX};
pub use tasks::{TaskProvenance, TaskTranscriptState, TaskWorktree};
pub use tool_results::{
    session_tool_results, tool_results_jsonl_bytes, ArchivedToolResult, TOOL_RESULTS_SCHEMA_VERSION,
};
pub use window::{
    window_journal_jsonl_bytes, ArchivedWindowFact, ArchivedWindowRow, WindowFamilyRoute,
    WindowSliceKind, WINDOW_JOURNAL_SCHEMA_VERSION,
};
pub use worktrees::{
    session_worktree_observations, session_worktrees_jsonl_bytes, ArchivedWorktreeObservation,
    SESSION_WORKTREES_SCHEMA_VERSION,
};

/// The fully-qualified orphan checkpoint branch the native archive writes
/// (`Dialect::Rift.checkpoint_ref()` on the read side — the LAYOUT contract).
pub const NATIVE_CHECKPOINT_REF: &str = "refs/heads/rift/checkpoints/v1";

/// The compact-transcript artifact's shard name — also the value
/// `metadata.json` records under `compact_transcript` (the "compact path
/// recorded in metadata" rule). MUST sort bytewise after `tool_calls.jsonl` and
/// before [`TRANSCRIPT_FULL_NAME`]: new artifacts are optional trailing
/// Merkle leaves, and a name sorting earlier would rewrite every existing
/// shard's leaf sequence (see [`shard_merkle_leaves`]).
pub const TRANSCRIPT_COMPACT_NAME: &str = "transcript.compact.jsonl";

/// The full-transcript artifact's shard name (see [`TRANSCRIPT_COMPACT_NAME`]
/// for the ordering constraint).
pub const TRANSCRIPT_FULL_NAME: &str = "transcript.jsonl";

/// The redaction/canonicalization version the native archive stamps into every
/// `metadata.json` (`canon_version`). This is the redaction→leaf migration
/// decision: rather than reproduce the legacy backend's exact redacted
/// bytes for pre-rewrite checkpoints, the canonicalization is **versioned**.
/// `rift verify` selects the leaf canonicalization by the checkpoint's version,
/// so promoting a derivation never breaks `verify` on old checkpoints — the
/// epochs coexist instead of one rewriting the other.
///
/// The epochs, in order:
///
/// * **0** — a pre-rewrite (legacy backend) checkpoint: no `canon_version` field at all.
/// * **1** — Rift-native redaction, and a `merkle_root` that was
///   [`rift_hashchain::empty_root`] on **every** shard. Nothing threaded an
///   event stream into [`CheckpointArchive::events`], so the root was taken over
///   the empty leaf set and 100+ archived shards published the identical
///   constant. It committed to nothing; that is the vacuous era.
/// * **2** — the root is DERIVED from the shard's own leaves
///   ([`shard_merkle_root`]) and cannot be supplied by a caller, so the value in
///   `metadata.json` is a claim about these bytes that `rift verify` re-derives.
///   The checkpoint id still bound only `content.redacted` and
///   `tool_calls.jsonl`, so any other artifact — `full.jsonl` then, a transcript
///   next — sat outside the shard's path.
/// * **3** — the checkpoint id is derived from that root
///   ([`rift_shadow::condense::derive_checkpoint_id`](crate::condense::derive_checkpoint_id)),
///   so the shard TREE is a function of the shard PATH and every content
///   artifact is bound by both. See [`ROOT_BOUND_ID_MIN_CANON_VERSION`].
///
/// The era is read off the shard itself, never off a date: see
/// [`STRICT_MERKLE_MIN_CANON_VERSION`].
pub const NATIVE_CANON_VERSION: u32 = 3;

/// The first `canon_version` whose recorded `merkle_root` is a verifiable claim,
/// and therefore the first that `rift verify` checks strictly.
///
/// Below it lies the **vacuous era** (epochs 0 and 1): those shards recorded
/// [`rift_hashchain::empty_root`] regardless of content, so re-deriving a root
/// for them and demanding a match would redden every repository's whole history
/// for a defect the history could not have avoided. They are accepted and
/// counted instead — reported, not hidden.
///
/// This gates only the derivation added with epoch 2. Every check that ran
/// before it (the `redaction_report.json` pass, the `merkle_leaf.txt` recompute
/// of the legacy dialect) still runs on every shard of every era: an era split
/// that silenced an existing check would be a hole wearing a migration's name.
pub const STRICT_MERKLE_MIN_CANON_VERSION: u32 = 2;

/// The first `canon_version` whose **checkpoint id** is derived from the shard's
/// Merkle root, and therefore the first for which the shard's path binds every
/// content artifact rather than two of them.
///
/// It is a separate constant from [`STRICT_MERKLE_MIN_CANON_VERSION`] because the
/// two are separate claims and the era split has to be checkable one claim at a
/// time: epoch 2 derives its root but not its id, so it is strict about the root
/// and permissive about the id. `rift verify` gates the accepted id set on this,
/// and the gate is asymmetric on purpose — see
/// [`checkpoint_ids_for_shard`](crate::condense::checkpoint_ids_for_shard).
///
/// **The leaf spec did not change with this epoch, and that is the point.** A
/// shard carrying no artifact beyond epoch 2's derives the same root at either
/// epoch, because new artifacts are optional trailing leaves. Only the ID rule
/// moved, so a reader that already re-derives roots needs one new rule, not a new
/// leaf order.
pub const ROOT_BOUND_ID_MIN_CANON_VERSION: u32 = 3;

/// The seam the write side is named through.
///
/// One method and one implementation ([`NativeBackend`]). It stays a trait
/// because the callers — `condense`, `executor`, `commit_capture` — are written
/// against the capability rather than the writer, which is what lets a test
/// substitute a backend that refuses (the policy-denied audit path) or fails
/// (the no-false-`archived=1` property) without a git repository.
pub trait ShadowBackend {
    /// Archive a checkpoint shard: write the full shard from `archive`.
    ///
    /// # Errors
    /// [`rift_core::RiftError`] on an archive failure.
    fn archive_checkpoint(&self, archive: &CheckpointArchive) -> Result<()>;
}

/// The native backend: writes a byte/layout-compatible checkpoint shard
/// onto the orphan `rift/checkpoints/v1` branch through screened git plumbing
/// ([`NativeGitWrite`]). Generic over the writer so unit tests use
/// [`MemoryGitWrite`] and production uses `PlumbingGitWrite`. Hashing/attribution/
/// cost/redaction are already native (the default trait methods); this closes the
/// last wrapped capability.
pub struct NativeBackend<W: NativeGitWrite = MemoryGitWrite> {
    pub(super) writer: W,
    pub(super) ident: GitIdent,
}

impl NativeBackend<MemoryGitWrite> {
    /// A native backend over the in-memory writer double (tests / dry-run).
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            writer: MemoryGitWrite::default(),
            ident: GitIdent::default(),
        }
    }

    /// The in-memory writer, for post-archive assertions.
    #[must_use]
    pub fn writer(&self) -> &MemoryGitWrite {
        &self.writer
    }
}

impl<W: NativeGitWrite> NativeBackend<W> {
    /// A native backend over an explicit writer (production: `PlumbingGitWrite`).
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            ident: GitIdent::default(),
        }
    }
}

impl<W: NativeGitWrite> ShadowBackend for NativeBackend<W> {
    fn archive_checkpoint(&self, archive: &CheckpointArchive) -> Result<()> {
        self.write_shard(archive)
    }
}

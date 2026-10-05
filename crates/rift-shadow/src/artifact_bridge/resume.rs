//! Which shards a walk may leave unopened because a completed earlier walk
//! projected them already.
//!
//! The worker levels the projection after every drain that wrote a checkpoint,
//! so a walk that re-read the whole branch each time cost more with every
//! checkpoint the branch gained. Kept apart from the walk because the rule is
//! the part a reviewer must be able to check on its own.
//!
//! **Why the walk position alone is not the rule.** The walk order is the
//! chain ref's order followed by every shard the chain does not name, in id
//! order (`ShardWalker::shards_all`). A new checkpoint is appended to the
//! chain, so it sorts after every chained shard but BEFORE every unchained one
//! (a pre-Rift shard, a shard whose chain entry has not landed, or every shard
//! of a repository with no chain ref), and in a repository without a chain ref
//! a new id lands anywhere. Skipping "everything at or before the watermark"
//! would then skip a shard no walk has projected. So a shard is skipped only
//! when it is at or before the recorded watermark **and** its `checkpoint` row
//! exists. The row is the shard's last write (see `ingest_shard`), so it also
//! marks a shard whose projection completed, which keeps a crash mid-shard
//! re-projecting that shard on the next walk.
//!
//! A shard that takes no chain position (an id that is not a SHA-256, or a
//! header without a Merkle root) has no row and is always re-read, as is a
//! refused one; both are rare, and re-reading them is what the walk did before.

use std::collections::BTreeSet;

use rift_core::Result;
use rift_recall_substrate::CheckpointShard;
use rift_store::{latest_ingest_watermark, CanonicalStore};

use super::CHECKPOINT_BRANCH;

/// The ids of the shards in `shards` (walk order) this walk need not open.
///
/// Empty when the projection has no watermark for the branch (a rebuild into an
/// empty projection is a full walk) or the watermark names a shard no longer on
/// the branch (a rewritten branch is walked in full).
pub(super) async fn already_projected(
    store: &impl CanonicalStore,
    shards: &[CheckpointShard],
) -> Result<BTreeSet<String>> {
    let Some(tip) = latest_ingest_watermark(store, CHECKPOINT_BRANCH).await? else {
        return Ok(BTreeSet::new());
    };
    let Some(tip_idx) = shards.iter().position(|s| s.checkpoint_id == tip) else {
        return Ok(BTreeSet::new());
    };
    let recorded = checkpoint_rows(store).await?;
    Ok(shards
        .iter()
        .take(tip_idx + 1)
        .map(|s| &s.checkpoint_id)
        .filter(|id| recorded.contains(*id))
        .cloned()
        .collect())
}

/// Every checkpoint id the projection holds, in one read rather than one per
/// shard.
async fn checkpoint_rows(store: &impl CanonicalStore) -> Result<BTreeSet<String>> {
    let rows = store
        .query("SELECT checkpoint_id FROM checkpoint", &[])
        .await?;
    rows.iter().map(|r| r.get_text(0)).collect()
}

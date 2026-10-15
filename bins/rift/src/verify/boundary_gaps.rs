//! Gap-bearing shards require complete content binding and explicit carriers.

use rift_shadow::backend::native_read::{check_boundary_gap_binding, NativeArtifacts, ShardHeader};

use super::verdict::Verdict;

pub(super) fn unsupported(id: &str) -> Verdict {
    Verdict::Tamper(format!(
        "{id}: boundary gap artifact is unsupported or unauthenticated"
    ))
}

pub(super) fn gap_pass(
    id: &str,
    header: &ShardHeader,
    native: &NativeArtifacts,
    report: Option<&[u8]>,
) -> Option<Verdict> {
    check_boundary_gap_binding(
        header,
        native,
        report,
        id,
        rift_recall_substrate::Dialect::Rift,
    )
    .err()
    .map(|_| unsupported(id))
}

pub(super) fn present_gap_pass(
    reader: &impl rift_recall_substrate::SubstrateReader,
    shard: &rift_recall_substrate::CheckpointShard,
    gap: &[u8],
    found: &mut super::passes::ShardPasses,
) -> rift_core::Result<()> {
    use rift_shadow::backend::native_read::{load_native_boundary_gaps, RetainedNativeShard};
    let id = &shard.checkpoint_id;
    let Ok((header, native, report)) = load_native_boundary_gaps(reader, shard, gap) else {
        found.worst.keep_worse(unsupported(id));
        return Ok(());
    };
    let view = RetainedNativeShard::new(reader, shard, &header, &native, report.as_deref())?;
    found
        .worst
        .keep_worse(super::passes::redaction_report_pass(id, report.as_deref()));
    for verdict in super::merkle::recorded_leaf_pass(&view, id)? {
        found.worst.keep_worse(verdict);
    }
    super::passes::native_shard_passes(
        &view,
        id,
        report.as_deref(),
        native.boundary_gaps.clone(),
        found,
    )
}

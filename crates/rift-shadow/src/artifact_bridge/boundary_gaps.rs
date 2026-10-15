//! Present recovery audits are checked before native projection or resume.
//! Projection facts alone do not authorize any physical queue retirement.

use rift_core::{Result, Sha256Id};
use rift_recall_substrate::{CheckpointShard, SubstrateReader};
use rift_store::{check_boundary_gap_batch, record_boundary_gap_batch, CanonicalStore};

use super::{BridgeReport, IngestCtx, RefusedShard};
use crate::backend::native_read::{
    check_boundary_gap_binding, load_native_boundary_gaps, RetainedNativeShard,
};

fn refuse(report: &mut BridgeReport, id: &str) -> bool {
    report.refused.push(RefusedShard {
        checkpoint_id: id.to_owned(),
        reason:
            "boundary gap artifact is unsupported, unavailable, unauthenticated or conflicts with projection"
                .into(),
    });
    false
}

pub(super) async fn ingest_gap<S: CanonicalStore, R: SubstrateReader>(
    ctx: &IngestCtx<'_, S, R>,
    report: &mut BridgeReport,
    shard: &CheckpointShard,
    parsed_id: Option<Sha256Id>,
    prev: Option<Sha256Id>,
    gap: Vec<u8>,
) -> Result<bool> {
    let id = shard.checkpoint_id.as_str();
    let Ok((header, native, redaction)) = load_native_boundary_gaps(ctx.sub, shard, &gap) else {
        return Ok(refuse(report, id));
    };
    let Ok(Some(envelopes)) =
        check_boundary_gap_binding(&header, &native, redaction.as_deref(), id, shard.dialect)
    else {
        return Ok(refuse(report, id));
    };
    let Ok(retained) =
        RetainedNativeShard::new(ctx.sub, shard, &header, &native, redaction.as_deref())
    else {
        return Ok(refuse(report, id));
    };
    // Check every batch first; a known conflict in a later batch cannot leave
    // new facts for an earlier one. Operational/concurrent failures afterward
    // can still leave immutable partial evidence; this is not a transaction.
    for envelope in &envelopes {
        if check_boundary_gap_batch(ctx.store, envelope.batch())
            .await
            .is_err()
        {
            return Ok(refuse(report, id));
        }
    }
    for envelope in &envelopes {
        record_boundary_gap_batch(ctx.store, envelope.batch()).await?;
    }
    let retained_ctx = IngestCtx {
        store: ctx.store,
        sub: &retained,
        repo_id: ctx.repo_id,
        default_agent: ctx.default_agent,
        now_ms: ctx.now_ms,
        registry: ctx.registry,
    };
    super::ingest_shard(&retained_ctx, report, id, parsed_id, prev).await?;
    Ok(true)
}

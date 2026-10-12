//! The artifact bridge: the **authoritative** canonical-event source.
//!
//! Walks the redacted checkpoint branch through the [`SubstrateReader`] seam,
//! **refuses any shard without a valid redaction report**, re-derives canonical
//! events from `full.jsonl`, projects them through exactly the same writers and
//! id derivations the live hook path uses (`crate::project` + `crate::ids`),
//! and advances the append-only ingest watermark. Because every row id is
//! content-addressed, ingest is `INSERT … OR IGNORE`: re-running the bridge —
//! after a crash, over a hook-fed projection, or from scratch — is a set-union
//! no-op. The bridge never sees the raw shadow branch: the reader it is handed
//! opens only the checkpoint ref, and a non-local reader is refused outright.
//!
//! The walk lives here; the report it fills, the per-shard fact recording, the
//! event projection, the settled usage, the worktree observations, the
//! measured tool results and the shard's own clock each live in a module of their own so a new shard
//! artifact lands beside the concern it feeds.

mod created_at;
mod events;
mod facts;
mod reads;
#[cfg(test)]
mod reads_tests;
mod report;
mod resume;
#[cfg(test)]
mod resume_tests;
mod sessions;
#[cfg(test)]
mod sessions_tests;
#[cfg(test)]
mod tests;
mod tool_results;
#[cfg(test)]
mod tool_results_tests;
mod usage;
#[cfg(test)]
mod usage_tests;
mod worktrees;
#[cfg(test)]
mod worktrees_tests;

use rift_core::{Result, RiftError, Sha256Id};
use rift_recall_substrate::parse;
use rift_recall_substrate::{CheckpointShard, Dialect, SubstrateReader};
use rift_recall_types::checkpoint::CheckpointMetadata;
use rift_shadow_hooks::AdapterRegistry;
use rift_store::{record_ingest_watermark, CanonicalStore, IngestWatermarkRow};

use crate::ids;

use events::project_full_events;
use facts::{record_chain_facts, record_sessions, seal_checkpoint};
use reads::project_read_lines;
pub use report::{BridgeReport, RefusedShard, MAX_PARSE_NOTES};
use sessions::{read_session_agents, record_named_sessions};
use tool_results::restore_tool_results;
use usage::project_usage_lines;
use worktrees::restore_worktree_observations;

/// The checkpoint branch the bridge ingests.
pub const CHECKPOINT_BRANCH: &str = "rift/checkpoints/v1";

/// `checkpoint.created_at` for a shard that records no time of its own —
/// **declared by [`rift_store`], re-exported here.**
///
/// It moved to the crate that stores the column, so the window-scoped readers
/// over that column can bind it without depending on this one. Every existing
/// spelling (`rift_shadow::CREATED_AT_UNKNOWN`) still resolves, which is the
/// whole reason this line is a `pub use` and not a second `const`: a sentinel
/// with two declarations is a sentinel that will be right in one of them.
/// `an_unknown_time_is_an_impossible_time_not_the_epoch` below asserts the
/// VALUE rather than the declaration site, so it reddens either way.
pub use rift_store::CREATED_AT_UNKNOWN;

/// What one ingest run holds fixed across every shard it walks.
struct IngestCtx<'a, S, R> {
    store: &'a S,
    sub: &'a R,
    repo_id: &'a str,
    default_agent: &'a str,
    now_ms: i64,
    registry: &'a AdapterRegistry,
}

/// One addressable shard's header: the checkpoint it names and what it claims.
struct ShardHeader<'a> {
    id: &'a str,
    ckpt_id: Sha256Id,
    metadata: &'a CheckpointMetadata,
    metadata_bytes: Option<&'a [u8]>,
}

/// Ingest everything on the checkpoint branch into the projection.
/// Crash-resumable and idempotent; refuses non-local readers and unredacted
/// shards. A shard a completed earlier walk projected is not opened again
/// (`resume`), so levelling after each condense costs the new shards only. `default_agent` names the adapter used for usage extraction when a
/// record does not carry its own agent token.
///
/// # Errors
/// [`rift_core::RiftError`] on a reader or storage failure. A malformed shard
/// is **not** an error — it is refused and reported.
pub async fn ingest_from_substrate(
    store: &impl CanonicalStore,
    sub: &impl SubstrateReader,
    repo_id: &str,
    default_agent: &str,
    now_ms: i64,
) -> Result<BridgeReport> {
    ingest_dialect(store, sub, repo_id, default_agent, now_ms, None).await
}

/// [`ingest_from_substrate`], restricted to one dialect.
///
/// `rift import --from-legacy` needs exactly this and nothing more. **Measured
/// before it was written** — the standing measure-first rule — because the
/// finding it answers is narrower than "import writes nothing": the
/// event-projection half of the shard walk below is NOT inside the
/// `Sha256Id::parse_hex(id)` guard, so `rift rebuild` has always projected a
/// pre-Rift shard's events — the same rows an import would write — while
/// `rift import --from-legacy` reported and wrote nothing.
/// The rows were reachable; the verb the operator was told to run was not the
/// verb that wrote them.
///
/// What the restriction buys is that import stays import: an unfiltered call
/// would also ingest this repository's own native checkpoints, which is
/// `rift rebuild`'s job and would make an import report numbers about work
/// nobody imported.
///
/// **A pre-Rift shard still takes no chain position and no `checkpoint` row.**
/// Its id is not a SHA-256 (see `GitCheckpointSource::from_reader`), and
/// minting a Rift id for it would put a row in the projection that the git side
/// cannot produce — `rift verify` would read the surplus row as tamper on every
/// subsequent run. That is "imported checkpoints are claims, not chain
/// anchors" as a mechanical fact rather than a slogan, and giving them a
/// durable home needs a table of its own.
///
/// # Errors
/// [`rift_core::RiftError`] on a reader or storage failure. A malformed shard
/// is **not** an error — it is refused and reported.
pub async fn ingest_dialect(
    store: &impl CanonicalStore,
    sub: &impl SubstrateReader,
    repo_id: &str,
    default_agent: &str,
    now_ms: i64,
    only: Option<Dialect>,
) -> Result<BridgeReport> {
    if !sub.is_local_only() {
        return Err(RiftError::PolicyDenied(
            "the artifact bridge only ingests from a local substrate".into(),
        ));
    }
    let mut shards: Vec<CheckpointShard> = sub.checkpoints()?;
    if let Some(dialect) = only {
        shards.retain(|s| s.dialect == dialect);
    }
    shards.sort_by(|a, b| {
        a.ordinal
            .cmp(&b.ordinal)
            .then(a.checkpoint_id.cmp(&b.checkpoint_id))
    });
    let mut report = BridgeReport {
        branch: CHECKPOINT_BRANCH.to_owned(),
        shards_seen: shards.len(),
        ..BridgeReport::default()
    };
    let projected = resume::already_projected(store, &shards).await?;
    let registry = AdapterRegistry::with_builtin();
    let ctx = IngestCtx {
        store,
        sub,
        repo_id,
        default_agent,
        now_ms,
        registry: &registry,
    };

    // The nearest preceding shard whose id is a SHA-256 — the one derivation of
    // `prev` (see `record_chain_facts`), advanced before the shard is judged so
    // a shard this walk refuses still occupies its place in the linkage.
    // Refusal is a fact about a shard's report, not about where it sits in the
    // walk, and the checker's own re-derivation does not consult the report at
    // all: if the two advanced on different conditions they would disagree the
    // moment a shard was refused. A shard an earlier walk projected advances it
    // too, so the first shard this walk opens links to the same predecessor a
    // full walk would give it.
    let mut last_parseable: Option<Sha256Id> = None;
    for shard in &shards {
        let id = shard.checkpoint_id.as_str();
        let parsed_id = Sha256Id::parse_hex(id).ok();
        let prev = last_parseable;
        last_parseable = parsed_id.or(last_parseable);
        if projected.contains(id) {
            report.shards_skipped += 1;
            continue;
        }
        ingest_shard(&ctx, &mut report, id, parsed_id, prev).await?;
    }

    advance_watermark(&ctx, &mut report, &shards).await?;
    Ok(report)
}

/// Judge one shard, then project what it carries: the chain facts from its
/// header, its session links under each session's own agent, its archived
/// event stream, its settled usage, its restorable reads, its named sessions,
/// their worktree observations and their measured tool results. `parsed_id`
/// is the
/// shard's id when it is a SHA-256; `prev` is the nearest preceding shard whose
/// id is one.
async fn ingest_shard<S: CanonicalStore, R: SubstrateReader>(
    ctx: &IngestCtx<'_, S, R>,
    report: &mut BridgeReport,
    id: &str,
    parsed_id: Option<Sha256Id>,
    prev: Option<Sha256Id>,
) -> Result<()> {
    if let Some(reason) = redaction_refusal(ctx.sub, report, id)? {
        report.refused.push(RefusedShard {
            checkpoint_id: id.to_owned(),
            reason: reason.into(),
        });
        return Ok(());
    }

    // The BYTES are kept beside the parsed view: the worktree member is read
    // from them directly rather than through the recall plane's lenient
    // `CheckpointMetadata`, which belongs to another crate and another
    // projection (`recall.db` keeps its own twin).
    let metadata_bytes = ctx.sub.read_artifact(id, "metadata.json")?;
    let metadata = match &metadata_bytes {
        Some(bytes) => {
            let out = parse::parse_metadata(bytes, id);
            report.parse_issues += out.issues.len();
            out.value
        }
        None => CheckpointMetadata::default(),
    };
    let merkle_root = metadata
        .merkle_root
        .as_deref()
        .and_then(|h| Sha256Id::parse_hex(h).ok());
    let chained = match (parsed_id, merkle_root) {
        (Some(ckpt_id), Some(root)) => Some((
            ShardHeader {
                id,
                ckpt_id,
                metadata: &metadata,
                metadata_bytes: metadata_bytes.as_deref(),
            },
            root,
        )),
        _ => None,
    };
    if let Some((header, _)) = &chained {
        record_chain_facts(ctx.store, report, header, prev).await?;
    }
    count_task_subtrees(ctx.sub, report, id, &metadata)?;
    let agents = read_session_agents(ctx, report, id)?;
    record_sessions(ctx, id, &metadata, &agents).await?;
    project_full_events(ctx, report, id).await?;
    // After the events, so a session or turn an archived event opened keeps
    // the live row's start and model; the usage rows only fill what is absent.
    project_usage_lines(ctx, report, id, &agents).await?;
    project_read_lines(ctx, id, &metadata, &agents).await?;
    // After the usage, so a usage line's model and instant win over the
    // empty model and the shard's own time a session line would give.
    record_named_sessions(ctx, &agents, &metadata).await?;
    // After the sessions, so every observation lands on a session row.
    restore_worktree_observations(ctx, report, id, &agents).await?;
    // After the sessions too, so every result lands on a session row.
    restore_tool_results(ctx, report, id, &agents).await?;
    if let Some((header, root)) = &chained {
        seal_checkpoint(ctx.store, header, *root, prev).await?;
    }
    report.shards_projected += 1;
    Ok(())
}

/// Parse and count the shard's `tasks/<tool-use-id>/` subtrees, enumerated
/// from its own task list. Nothing is stored: a subagent transcript's record is
/// the subtree itself, and a rebuild only proves it still reads.
fn count_task_subtrees(
    sub: &impl SubstrateReader,
    report: &mut BridgeReport,
    id: &str,
    metadata: &CheckpointMetadata,
) -> Result<()> {
    for task in &metadata.tasks {
        let Some(bytes) = sub.read_artifact(id, &format!("tasks/{task}/metadata.json"))? else {
            report.parse_issues += 1;
            continue;
        };
        let parsed = parse::parse_task_metadata(&bytes, id);
        report.parse_issues += parsed.issues.len();
        if parsed.issues.is_empty() {
            report.task_subtrees += 1;
        }
    }
    Ok(())
}

/// STRICT LAYER: a shard without a valid redaction report is never projected.
/// The lenient parser enumerates; this judges — `Some` is the refusal reason.
fn redaction_refusal(
    sub: &impl SubstrateReader,
    report: &mut BridgeReport,
    id: &str,
) -> Result<Option<&'static str>> {
    let Some(report_bytes) = sub.read_artifact(id, "redaction_report.json")? else {
        return Ok(Some("missing redaction report"));
    };
    let parsed_report = parse::parse_redaction_report(&report_bytes, id);
    report.parse_issues += parsed_report.issues.len();
    if !parsed_report.value.is_valid() {
        return Ok(Some("invalid redaction report"));
    }
    Ok(None)
}

/// Advance the append-only watermark to the branch's high-water shard.
/// Written LAST: a crash before this step re-runs as a pure no-op.
async fn advance_watermark<S: CanonicalStore, R>(
    ctx: &IngestCtx<'_, S, R>,
    report: &mut BridgeReport,
    shards: &[CheckpointShard],
) -> Result<()> {
    let Some(tip) = shards.last().map(|s| s.checkpoint_id.clone()) else {
        return Ok(());
    };
    record_ingest_watermark(
        ctx.store,
        &IngestWatermarkRow {
            watermark_id: ids::watermark_id(CHECKPOINT_BRANCH, &tip),
            branch: CHECKPOINT_BRANCH.to_owned(),
            branch_tip: tip.clone(),
            ingested_at: ctx.now_ms,
            schema_version: 1,
        },
    )
    .await?;
    report.tip = Some(tip);
    Ok(())
}

//! The archived canonical-event stream, re-derived from `full.jsonl` and run
//! through the same projector the live hook path uses.

use rift_core::{CanonicalEvent, EventKind, Result, Sha256Id};
use rift_recall_substrate::{parse, SubstrateReader};
use rift_recall_types::checkpoint::FullEventRecord;
use rift_shadow_hooks::{idempotency_key, AgentId};
use rift_store::{record_captured_event, CanonicalStore, CapturedEventRow};

use crate::project::{
    payload_facts, project_event, project_main_context, project_usage, ProjectionCtx,
};

use super::report::{note, truncate_token, BridgeReport};
use super::IngestCtx;

/// The archived canonical-event stream → the SAME projector the live hook path
/// runs (identical ids ⇒ the union is a set-union no-op). A header-only shard
/// has no `full.jsonl`: its chain facts landed, and it has no events.
pub(super) async fn project_full_events<S: CanonicalStore, R: SubstrateReader>(
    ctx: &IngestCtx<'_, S, R>,
    report: &mut BridgeReport,
    id: &str,
) -> Result<()> {
    let Some(full_bytes) = ctx.sub.read_artifact(id, "full.jsonl")? else {
        return Ok(());
    };
    let full = parse::parse_full_events(&full_bytes, id);
    report.parse_issues += full.issues.len();
    for rec in &full.value {
        let Some(ev) = event_from_record(rec) else {
            note_dropped_record(report, id, rec);
            continue;
        };
        report.events_derived += 1;
        project_record(ctx, report, rec, &ev).await?;
    }
    Ok(())
}

/// A dropped record says WHY it was dropped, and says it with the token that
/// caused it. This arm used to bump an anonymous counter, which is how the
/// dot-spelled dialect vanished without a word: the number went up and nothing
/// named a kind.
fn note_dropped_record(report: &mut BridgeReport, id: &str, rec: &FullEventRecord) {
    report.parse_issues += 1;
    note(
        report,
        format!(
            "{id}: dropped a record with an unrecognised kind {:?} \
             (session {}, turn {}, event {})",
            truncate_token(&rec.kind),
            truncate_token(&rec.session),
            rec.turn_ordinal,
            rec.event_ordinal
        ),
    );
}

/// Write one re-derived event: its `captured_event` row (keyed by the same
/// idempotency key the live hook path derives), then its projection, its usage
/// and its main-context usage when the record's adapter reads them.
async fn project_record<S: CanonicalStore, R>(
    ctx: &IngestCtx<'_, S, R>,
    report: &mut BridgeReport,
    rec: &FullEventRecord,
    ev: &CanonicalEvent,
) -> Result<()> {
    let agent_token = rec
        .agent
        .clone()
        .unwrap_or_else(|| ctx.default_agent.to_owned());
    let agent_id = AgentId::new(&agent_token);
    let key = idempotency_key(
        &agent_id,
        &ev.session,
        &rec.verb,
        ev.turn_ordinal,
        ev.event_ordinal,
        &ev.payload_sha256,
    );
    let inserted = record_captured_event(
        ctx.store,
        &CapturedEventRow {
            idempotency_key: key.to_hex(),
            session_id: ev.session.clone(),
            turn_ordinal: i64::from(ev.turn_ordinal),
            event_ordinal: i64::from(ev.event_ordinal),
            kind: ev.kind.as_str().to_owned(),
            trust: ev.trust.as_str().to_owned(),
            payload_sha256: ev.payload_sha256.to_hex(),
            occurred_at_ms: ev.occurred_at_ms,
            schema_version: i64::from(ev.schema_version),
        },
    )
    .await?;
    if inserted {
        report.events_inserted += 1;
    }

    let facts = payload_facts(&rec.payload);
    let pctx = ProjectionCtx {
        agent: &agent_token,
        repo_id: ctx.repo_id,
    };
    project_event(ctx.store, ev, &facts, pctx).await?;
    if let Some(usage) = ctx
        .registry
        .get(&agent_id)
        .and_then(|a| a.usage(&rec.verb, &rec.payload))
    {
        project_usage(ctx.store, ev, &facts, pctx, &usage, ctx.now_ms).await?;
    }
    if let Some(usage) = ctx
        .registry
        .get(&agent_id)
        .and_then(|a| a.main_context_usage(&rec.verb, &rec.payload))
    {
        project_main_context(ctx.store, ev, &rec.verb, &facts, pctx, &usage).await?;
    }
    Ok(())
}

/// Reconstruct a [`CanonicalEvent`] from one archived `full.jsonl` record.
///
/// The kind is parsed with [`EventKind::from_wire_token`], which accepts the
/// dot-separated dialect the committed read-compat fixtures carry as well as
/// the canonical snake_case spelling, and normalises to the latter — so what
/// reaches `captured_event.kind` is one spelling regardless of which the shard
/// used. An unrecognised token is still `None`: never guessed, and now never
/// silent either (the caller names it).
fn event_from_record(rec: &FullEventRecord) -> Option<CanonicalEvent> {
    let kind = EventKind::from_wire_token(&rec.kind)?;
    let payload_sha256 = rec
        .payload_sha256
        .as_deref()
        .and_then(|h| Sha256Id::parse_hex(h).ok())
        .unwrap_or_else(|| {
            // The archived record carries no explicit address: derive it from
            // the (redacted) payload bytes exactly the way ingest does.
            use sha2::{Digest as _, Sha256};
            let bytes = serde_json::to_vec(&rec.payload).unwrap_or_default();
            let mut h = Sha256::new();
            h.update(&bytes);
            let mut b = [0u8; 32];
            b.copy_from_slice(&h.finalize());
            Sha256Id::from_bytes(b)
        });
    Some(CanonicalEvent {
        schema_version: 1,
        session: rec.session.clone(),
        turn_ordinal: rec.turn_ordinal,
        event_ordinal: rec.event_ordinal,
        kind,
        trust: kind.default_trust(),
        occurred_at_ms: rec.occurred_at_ms,
        payload_sha256,
    })
}

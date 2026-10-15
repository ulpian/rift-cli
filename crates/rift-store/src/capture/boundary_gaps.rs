//! Complete checked gap projection, never authenticated retirement authority.
//! Partial immutable rows remain evidence; only an exact expected parentless
//! subset may be resumed. Existing parents require complete full equality.

use std::collections::BTreeMap;

use rift_core::boundary_gap::{
    BoundaryEdge, ContextDigest, EvidenceDigest, GapBatch, GapId, GapTicket, GapTicketFields,
    MemberKey, SelectionDigest, SessionKey, SourceKey, TicketDigest, MAX_TICKETS, SCHEMA_VERSION,
};
use rift_core::{RepoId, Result, RiftError};

use crate::{CanonicalStore, Row, Value};

const TICKET_SELECT: &str = "SELECT source_key, context_sha256, ticket_sha256, session_key, \
    turn_ordinal, boundary, head_oid, captured_at, queued_at, attempts, schema_version \
    FROM turn_boundary_gap_ticket";

fn inconsistent() -> RiftError {
    RiftError::storage("boundary gap: incomplete or conflicting projection")
}

/// Append an exact checked member set, then its batch parent last.
/// Identical complete replay is a no-op. An exact parentless subset can resume;
/// a divergent, extra, corrupt or incomplete parent state is never repaired.
/// This is not a transaction or evidence of authenticated archive publication.
///
/// # Errors
/// Storage failure, incompatible existing facts, or a failed whole-set recheck.
pub async fn record_boundary_gap_batch(
    store: &impl CanonicalStore,
    batch: &GapBatch,
) -> Result<bool> {
    if let Some(parent) = parent_row(store, batch.gap_id()).await? {
        let existing = decode_parent(parent, load_tickets(store, batch.gap_id()).await?)?;
        return if existing == *batch {
            Ok(false)
        } else {
            Err(inconsistent())
        };
    }
    let existing = load_tickets(store, batch.gap_id()).await?;
    let expected: BTreeMap<_, _> = batch
        .tickets()
        .iter()
        .map(|ticket| (ticket.source_key(), ticket))
        .collect();
    for member in &existing {
        if expected.get(&member.source_key()).copied() != Some(member) {
            return Err(inconsistent());
        }
    }
    let present: BTreeMap<_, _> = existing
        .iter()
        .map(|ticket| (ticket.source_key(), ticket))
        .collect();
    for member in batch.tickets() {
        if present.contains_key(&member.source_key()) {
            continue;
        }
        if !append_ticket(store, batch.gap_id(), member).await?
            && ticket_for_key(store, batch.gap_id(), member.source_key())
                .await?
                .as_ref()
                != Some(member)
        {
            return Err(inconsistent());
        }
    }
    if load_tickets(store, batch.gap_id()).await? != batch.tickets() {
        return Err(inconsistent());
    }
    let inserted = append_batch(store, batch).await?;
    // An insert-ignore loser must compare the winning parent's full value,
    // including original recorded time, plus the entire actual member set.
    if boundary_gap_batch(store, batch.gap_id()).await?.as_ref() != Some(batch) {
        return Err(inconsistent());
    }
    Ok(inserted)
}

/// Read one complete checked projection batch, including exact membership.
/// Absence means neither parent nor members exist. Orphans and every format,
/// count, commitment or set inconsistency refuse rather than certify facts.
/// No result from this projection alone authorizes queue retirement.
///
/// # Errors
/// Storage failure or incomplete/inconsistent immutable facts.
pub async fn boundary_gap_batch(
    store: &impl CanonicalStore,
    id: GapId,
) -> Result<Option<GapBatch>> {
    let parent = parent_row(store, id).await?;
    let tickets = load_tickets(store, id).await?;
    match parent {
        Some(row) => decode_parent(row, tickets).map(Some),
        None if tickets.is_empty() => Ok(None),
        None => Err(inconsistent()),
    }
}

async fn parent_row(store: &impl CanonicalStore, id: GapId) -> Result<Option<Row>> {
    let mut rows = store
        .query(
            "SELECT gap_id, repo_id, member_key, selection_sha256, evidence_sha256, \
         ticket_count, recorded_at, reason, schema_version FROM turn_boundary_gap_batch \
         WHERE gap_id = ?1 LIMIT 2",
            &[Value::text(id.to_hex())],
        )
        .await?;
    if rows.len() > 1 {
        return Err(inconsistent());
    }
    Ok(rows.pop())
}

async fn load_tickets(store: &impl CanonicalStore, id: GapId) -> Result<Vec<GapTicket>> {
    let rows = store
        .query(
            &format!("{TICKET_SELECT} WHERE gap_id = ?1 ORDER BY source_key LIMIT ?2"),
            &[
                Value::text(id.to_hex()),
                Value::Int(i64::try_from(MAX_TICKETS + 1).map_err(|_| inconsistent())?),
            ],
        )
        .await?;
    if rows.len() > MAX_TICKETS {
        return Err(inconsistent());
    }
    rows.into_iter().map(decode_ticket).collect()
}

async fn ticket_for_key(
    store: &impl CanonicalStore,
    id: GapId,
    source: SourceKey,
) -> Result<Option<GapTicket>> {
    let mut rows = store
        .query(
            &format!("{TICKET_SELECT} WHERE gap_id = ?1 AND source_key = ?2 LIMIT 2"),
            &[Value::text(id.to_hex()), Value::text(source.to_hex())],
        )
        .await?;
    if rows.len() > 1 {
        return Err(inconsistent());
    }
    rows.pop().map(decode_ticket).transpose()
}

fn decode_ticket(row: Row) -> Result<GapTicket> {
    if row.get_i64(10)? != i64::from(SCHEMA_VERSION) {
        return Err(inconsistent());
    }
    GapTicket::new(GapTicketFields {
        source_key: SourceKey::parse_hex(&row.get_text(0)?)?,
        context_digest: ContextDigest::parse_hex(&row.get_text(1)?)?,
        ticket_digest: TicketDigest::parse_hex(&row.get_text(2)?)?,
        session_key: SessionKey::parse_hex(&row.get_text(3)?)?,
        turn_ordinal: u32::try_from(row.get_i64(4)?).map_err(|_| inconsistent())?,
        boundary: BoundaryEdge::parse(&row.get_text(5)?)?,
        head_oid: row.get_opt_text(6)?,
        captured_at: row.get_i64(7)?,
        queued_at: row.get_i64(8)?,
        attempts: u32::try_from(row.get_i64(9)?).map_err(|_| inconsistent())?,
    })
}

fn decode_parent(row: Row, tickets: Vec<GapTicket>) -> Result<GapBatch> {
    if row.get_i64(8)? != i64::from(SCHEMA_VERSION) || row.get_text(7)? != "uncaptured-backlog" {
        return Err(inconsistent());
    }
    let batch = GapBatch::new(
        RepoId::new(row.get_text(1)?).map_err(|_| inconsistent())?,
        MemberKey::parse_hex(&row.get_text(2)?)?,
        EvidenceDigest::parse_hex(&row.get_text(4)?)?,
        row.get_i64(6)?,
        tickets,
    )?;
    batch.verify_stored(
        GapId::parse_hex(&row.get_text(0)?)?,
        SelectionDigest::parse_hex(&row.get_text(3)?)?,
        u32::try_from(row.get_i64(5)?).map_err(|_| inconsistent())?,
    )?;
    Ok(batch)
}

async fn append_ticket(store: &impl CanonicalStore, id: GapId, ticket: &GapTicket) -> Result<bool> {
    store
        .insert_or_ignore(
            "turn_boundary_gap_ticket",
            &[
                "gap_id",
                "source_key",
                "context_sha256",
                "ticket_sha256",
                "session_key",
                "turn_ordinal",
                "boundary",
                "head_oid",
                "captured_at",
                "queued_at",
                "attempts",
                "schema_version",
            ],
            &[
                Value::text(id.to_hex()),
                Value::text(ticket.source_key().to_hex()),
                Value::text(ticket.context_digest().to_hex()),
                Value::text(ticket.ticket_digest().to_hex()),
                Value::text(ticket.session_key().to_hex()),
                Value::Int(i64::from(ticket.turn_ordinal())),
                Value::text(ticket.boundary().as_str()),
                ticket.head_oid().map_or(Value::Null, Value::text),
                Value::Int(ticket.captured_at()),
                Value::Int(ticket.queued_at()),
                Value::Int(i64::from(ticket.attempts())),
                Value::Int(i64::from(SCHEMA_VERSION)),
            ],
        )
        .await
}

async fn append_batch(store: &impl CanonicalStore, batch: &GapBatch) -> Result<bool> {
    store
        .insert_or_ignore(
            "turn_boundary_gap_batch",
            &[
                "gap_id",
                "repo_id",
                "member_key",
                "selection_sha256",
                "evidence_sha256",
                "ticket_count",
                "recorded_at",
                "reason",
                "schema_version",
            ],
            &[
                Value::text(batch.gap_id().to_hex()),
                Value::text(batch.repo_id().as_str()),
                Value::text(batch.member_key().to_hex()),
                Value::text(batch.selection_digest().to_hex()),
                Value::text(batch.evidence_digest().to_hex()),
                Value::Int(i64::from(batch.ticket_count())),
                Value::Int(batch.recorded_at()),
                Value::text(batch.reason().as_str()),
                Value::Int(i64::from(SCHEMA_VERSION)),
            ],
        )
        .await
}

#[cfg(test)]
mod tests;

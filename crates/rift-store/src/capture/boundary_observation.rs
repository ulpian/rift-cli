//! Local collection audit, never evidence of event-time bytes or authorship.
//! A companion belongs only to the attempt that newly inserts its snapshot.

use std::collections::BTreeMap;

use rift_core::{Result, RiftError};
use sha2::{Digest as _, Sha256};

use crate::{
    record_turn_boundary_file_with_kind, record_turn_boundary_snapshot,
    turn_boundary_file_kinds_for_snapshot, turn_boundary_files_for_snapshot, BoundaryFileKind,
    CanonicalStore, TurnBoundaryFileRow, TurnBoundarySnapshotRow, Value,
};

/// Bounds of one collection attempt, including path discovery and addressing.
/// They do not describe an atomic snapshot or a read instant for every file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundaryCollectionInterval {
    /// Start of the attempt, epoch milliseconds.
    pub started_at: i64,
    /// Completion of collection and object addressing, epoch milliseconds.
    pub finished_at: i64,
}

/// Projection-only audit for one newly inserted observation set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundaryObservationRow {
    /// The logical boundary event, not an observation-time identity.
    pub snapshot_id: String,
    /// Exact existing snapshot content-set hash; no new hash preimage.
    pub worktree_hash: String,
    /// Collection attempt bounds, independent of the event's captured_at.
    pub interval: BoundaryCollectionInterval,
}

/// Append a new snapshot, its exact files/kinds, then its audit companion.
/// An existing snapshot is a no-op: no partial filling or legacy backfill.
/// A crash before the last append leaves audit provenance unknown forever.
///
/// # Errors
/// Storage failure, invalid interval, or mismatched observation identities.
pub async fn record_turn_boundary_collection(
    store: &impl CanonicalStore,
    snapshot: &TurnBoundarySnapshotRow,
    files: &[(TurnBoundaryFileRow, BoundaryFileKind)],
    interval: BoundaryCollectionInterval,
) -> Result<bool> {
    if interval.started_at < 0 || interval.finished_at < interval.started_at {
        return Err(RiftError::storage("invalid boundary collection interval"));
    }
    validate_input(snapshot, files)?;
    if turn_boundary_observation_for_snapshot(store, &snapshot.snapshot_id)
        .await?
        .is_some()
    {
        return Ok(false);
    }
    if !record_turn_boundary_snapshot(store, snapshot).await? {
        return Ok(false);
    }
    for (file, kind) in files {
        if !record_turn_boundary_file_with_kind(store, file, *kind).await? {
            return Err(RiftError::storage(
                "boundary collection file already exists",
            ));
        }
    }
    validate_persisted(store, snapshot, files).await?;
    let appended = store
        .insert_or_ignore(
            "turn_boundary_observation",
            &[
                "snapshot_id",
                "worktree_hash",
                "collection_started_at",
                "collection_finished_at",
                "schema_version",
            ],
            &[
                Value::text(snapshot.snapshot_id.clone()),
                Value::text(snapshot.worktree_hash.clone()),
                Value::Int(interval.started_at),
                Value::Int(interval.finished_at),
                Value::Int(1),
            ],
        )
        .await?;
    if !appended {
        return Err(RiftError::storage(
            "boundary collection audit already exists",
        ));
    }
    Ok(true)
}

fn validate_input(
    snapshot: &TurnBoundarySnapshotRow,
    files: &[(TurnBoundaryFileRow, BoundaryFileKind)],
) -> Result<()> {
    if snapshot.schema_version != 1 {
        return Err(RiftError::storage("unsupported boundary snapshot schema"));
    }
    let mut previous = None;
    let mut hash = Sha256::new();
    for (file, _) in files {
        if file.schema_version != 1
            || file.snapshot_id != snapshot.snapshot_id
            || previous.is_some_and(|path: &str| path >= file.path.as_str())
        {
            return Err(RiftError::storage(
                "invalid boundary collection file identity",
            ));
        }
        previous = Some(file.path.as_str());
        hash.update(file.path.as_bytes());
        hash.update([0]);
        hash.update(file.content_sha256.as_bytes());
        hash.update(b"\n");
    }
    if format!("{:x}", hash.finalize()) != snapshot.worktree_hash {
        return Err(RiftError::storage(
            "boundary collection content set hash differs",
        ));
    }
    Ok(())
}

async fn validate_persisted(
    store: &impl CanonicalStore,
    snapshot: &TurnBoundarySnapshotRow,
    expected: &[(TurnBoundaryFileRow, BoundaryFileKind)],
) -> Result<()> {
    let files = turn_boundary_files_for_snapshot(store, &snapshot.snapshot_id).await?;
    let kinds = turn_boundary_file_kinds_for_snapshot(store, &snapshot.snapshot_id).await?;
    let expected_files: Vec<_> = expected.iter().map(|(file, _)| file.clone()).collect();
    let expected_kinds: BTreeMap<_, _> = expected
        .iter()
        .map(|(file, kind)| (file.path.clone(), *kind))
        .collect();
    let snapshots = store
        .query_opt_i64(
            "SELECT COUNT(*) FROM turn_boundary_snapshot WHERE snapshot_id = ?1 \
         AND session_id = ?2 AND turn_ordinal = ?3 AND boundary = ?4 AND actor = ?5 \
         AND worktree_hash = ?6 AND captured_at = ?7 AND schema_version = ?8",
            &[
                Value::text(snapshot.snapshot_id.clone()),
                Value::text(snapshot.session_id.clone()),
                Value::Int(snapshot.turn_ordinal),
                Value::text(snapshot.boundary.clone()),
                Value::text(snapshot.actor.clone()),
                Value::text(snapshot.worktree_hash.clone()),
                Value::Int(snapshot.captured_at),
                Value::Int(snapshot.schema_version),
            ],
        )
        .await?;
    if snapshots != Some(1) || files != expected_files || kinds != expected_kinds {
        return Err(RiftError::storage(
            "persisted boundary collection identity differs",
        ));
    }
    Ok(())
}

/// A bounded audit read distinguishes an absent fact from a limited check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoundedBoundaryObservation {
    /// Legacy or interrupted write without an audit companion.
    Missing,
    /// A complete, checked local collection fact, never author evidence.
    Observed(BoundaryObservationRow),
    /// The actual bounded file or kind fetch exceeded the requested limit.
    Limited,
}

/// Read a collection audit with complete file/kind identity validation.
///
/// # Errors
/// A storage failure or inconsistent audit/observation set.
pub async fn turn_boundary_observation_for_snapshot(
    store: &impl CanonicalStore,
    snapshot_id: &str,
) -> Result<Option<BoundaryObservationRow>> {
    match read_observation(store, snapshot_id, i64::MAX).await? {
        BoundedBoundaryObservation::Missing => Ok(None),
        BoundedBoundaryObservation::Observed(row) => Ok(Some(row)),
        BoundedBoundaryObservation::Limited => {
            Err(RiftError::storage("boundary collection audit too large"))
        }
    }
}

/// Check at most row_limit+1 actual file rows and kind rows for one snapshot.
/// A concurrent append cannot make either fetch exceed this SQL bound.
///
/// # Errors
/// Invalid limit, storage failure or inconsistent metadata/file/kind facts.
pub async fn bounded_turn_boundary_observation(
    store: &impl CanonicalStore,
    snapshot_id: &str,
    row_limit: usize,
) -> Result<BoundedBoundaryObservation> {
    let limit = i64::try_from(row_limit)
        .ok()
        .and_then(|n| n.checked_add(1))
        .ok_or_else(|| RiftError::storage("boundary audit row limit overflows"))?;
    read_observation(store, snapshot_id, limit).await
}

async fn read_observation(
    store: &impl CanonicalStore,
    snapshot_id: &str,
    fetch_limit: i64,
) -> Result<BoundedBoundaryObservation> {
    let rows = store
        .query(
            "SELECT o.worktree_hash, o.collection_started_at, o.collection_finished_at, \
         s.worktree_hash, o.schema_version, s.schema_version FROM turn_boundary_observation o \
         LEFT JOIN turn_boundary_snapshot s ON s.snapshot_id = o.snapshot_id \
         WHERE o.snapshot_id = ?1",
            &[Value::text(snapshot_id.to_owned())],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(BoundedBoundaryObservation::Missing);
    };
    let audit = decode_audit(snapshot_id, row)?;
    let params = &[Value::text(snapshot_id.to_owned()), Value::Int(fetch_limit)];
    let files = store
        .query(
            "SELECT path, content_sha256, schema_version FROM turn_boundary_file \
        WHERE snapshot_id = ?1 ORDER BY path LIMIT ?2",
            params,
        )
        .await?;
    let kinds = store
        .query(
            "SELECT path, kind, schema_version FROM turn_boundary_file_kind \
        WHERE snapshot_id = ?1 ORDER BY path LIMIT ?2",
            params,
        )
        .await?;
    if fetch_limit != i64::MAX
        && (files.len() >= usize::try_from(fetch_limit).unwrap_or(usize::MAX)
            || kinds.len() >= usize::try_from(fetch_limit).unwrap_or(usize::MAX))
    {
        return Ok(BoundedBoundaryObservation::Limited);
    }
    validate_audit_set(&audit, &files, &kinds)?;
    Ok(BoundedBoundaryObservation::Observed(audit))
}

fn decode_audit(snapshot_id: &str, row: &crate::Row) -> Result<BoundaryObservationRow> {
    let worktree_hash = row.get_text(0)?;
    let interval = BoundaryCollectionInterval {
        started_at: row.get_i64(1)?,
        finished_at: row.get_i64(2)?,
    };
    if worktree_hash != row.get_text(3)?
        || row.get_i64(4)? != 1
        || row.get_i64(5)? != 1
        || interval.started_at < 0
        || interval.finished_at < interval.started_at
    {
        return Err(RiftError::storage("invalid boundary collection audit"));
    }
    Ok(BoundaryObservationRow {
        snapshot_id: snapshot_id.to_owned(),
        worktree_hash,
        interval,
    })
}

fn validate_audit_set(
    audit: &BoundaryObservationRow,
    files: &[crate::Row],
    kinds: &[crate::Row],
) -> Result<()> {
    if files.len() != kinds.len() {
        return Err(RiftError::storage(
            "boundary collection audit kind set differs",
        ));
    }
    let mut hash = Sha256::new();
    for (file, kind) in files.iter().zip(kinds) {
        let path = file.get_text(0)?;
        if path != kind.get_text(0)?
            || file.get_i64(2)? != 1
            || kind.get_i64(2)? != 1
            || !matches!(
                kind.get_text(1)?.as_str(),
                "regular" | "link" | "absent" | "unknown"
            )
        {
            return Err(RiftError::storage(
                "invalid boundary collection audit file kind",
            ));
        }
        hash.update(path.as_bytes());
        hash.update([0]);
        hash.update(file.get_text(1)?.as_bytes());
        hash.update(b"\n");
    }
    if format!("{:x}", hash.finalize()) != audit.worktree_hash {
        return Err(RiftError::storage(
            "boundary collection audit file set differs",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "boundary_observation_tests.rs"]
mod tests;

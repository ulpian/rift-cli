//! The ingest watermark: the append-only record of the last checkpoint commit
//! ingested per branch, and where the bridge resumes from — with, on the
//! recall projection, the stream position that tip held when it was recorded.

use rift_core::{Result, RiftError};

use crate::{CanonicalStore, Value};

/// An `ingest_watermark` row: the append-only record of the last checkpoint
/// commit ingested per branch. Re-ingest from the watermark is a no-op on
/// already-seen content; the latest row per branch is the high-water mark.
#[derive(Debug, Clone)]
pub struct IngestWatermarkRow {
    /// `= sha256(branch ‖ branch_tip)` (hex) — re-recording the same tip is a no-op.
    pub watermark_id: String,
    /// The ingested branch (`rift/checkpoints/v1`).
    pub branch: String,
    /// The branch tip (git oid, hex) at ingest time.
    pub branch_tip: String,
    /// Transaction-time (epoch ms).
    pub ingested_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record an `ingest_watermark` advance (idempotent on `(branch, tip)`).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_ingest_watermark(
    store: &impl CanonicalStore,
    r: &IngestWatermarkRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "ingest_watermark",
            &[
                "watermark_id",
                "branch",
                "branch_tip",
                "ingested_at",
                "schema_version",
            ],
            &[
                Value::text(r.watermark_id.clone()),
                Value::text(r.branch.clone()),
                Value::text(r.branch_tip.clone()),
                Value::Int(r.ingested_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// The most recently recorded branch tip for `branch`, if any — where the
/// bridge resumes from. The latest `ingested_at` wins, and a tie goes to the
/// row written last.
///
/// The tie is the common case, not a corner: the recall plane stamps every
/// row with one constant time so a rebuild is deterministic, and the capture
/// plane can append twice within a millisecond. The ids are hashes, so
/// breaking the tie by `watermark_id` picked an arbitrary row. `rowid` is the
/// order of writing instead, and for this relation that order is true: it is
/// append-only (its guards abort every `UPDATE` and `DELETE`), so no row is
/// ever removed and SQLite hands each insert a `rowid` above every row already
/// there. A `VACUUM` may renumber the rowids of a table without an integer
/// key, but it copies rows in `rowid` order, so their relative order survives.
/// Inventing timestamps in the recall writer was the alternative rejected: it
/// would put a clock into a rebuild that must not depend on one.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn latest_ingest_watermark(
    store: &impl CanonicalStore,
    branch: &str,
) -> Result<Option<String>> {
    let rows = store
        .query(
            "SELECT branch_tip FROM ingest_watermark WHERE branch = ?1 \
             ORDER BY ingested_at DESC, rowid DESC LIMIT 1",
            &[Value::text(branch.to_owned())],
        )
        .await?;
    match rows.first() {
        Some(r) => Ok(Some(r.get_text(0)?)),
        None => Ok(None),
    }
}

/// Record where in its dialect's walk the tip of watermark `watermark_id`
/// stood: `chain_seq` is the tip's 1-based position, as
/// [`crate::StreamCursor`] defines it. Recall projection only.
///
/// A re-recorded tip keeps its **first** position, and that is the point
/// rather than a side effect of `insert_or_ignore`. The same `(branch, tip)`
/// hashes to the same `watermark_id`, so a second position for one id is the
/// same checkpoint found somewhere else in the walk — exactly the rewrite the
/// cursor's id-at-position check exists to catch. Letting the later position
/// win would re-anchor the cursor on the rewritten stream and hide it.
///
/// # Errors
/// [`RiftError::Validity`] for position 0, which is the origin and names no
/// checkpoint, or one past `i64::MAX`; [`RiftError::Storage`] on a
/// non-conflict insert failure.
pub async fn record_ingest_watermark_position(
    store: &impl CanonicalStore,
    watermark_id: &str,
    chain_seq: u64,
) -> Result<bool> {
    if chain_seq == 0 {
        return Err(RiftError::validity(
            "ingest watermark position: chain_seq 0 is the origin",
        ));
    }
    let Ok(position) = i64::try_from(chain_seq) else {
        return Err(RiftError::validity(
            "ingest watermark position: chain_seq out of range",
        ));
    };
    store
        .insert_or_ignore(
            "ingest_watermark_position",
            &["watermark_id", "chain_seq", "schema_version"],
            &[
                Value::text(watermark_id),
                Value::Int(position),
                Value::Int(1),
            ],
        )
        .await
}

/// [`latest_ingest_watermark`] with the stream position recorded beside the
/// tip, chosen by the same tie rule. Recall projection only.
///
/// The position is `None` for a row written before positions were kept, and
/// for a stored value no cursor could hold (0 or negative): either way the
/// caller has only the tip, and locates it in the walk as it always did.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn latest_ingest_watermark_with_position(
    store: &impl CanonicalStore,
    branch: &str,
) -> Result<Option<(String, Option<u64>)>> {
    let rows = store
        .query(
            "SELECT w.branch_tip, p.chain_seq FROM ingest_watermark w \
             LEFT JOIN ingest_watermark_position p ON p.watermark_id = w.watermark_id \
             WHERE w.branch = ?1 \
             ORDER BY w.ingested_at DESC, w.rowid DESC LIMIT 1",
            &[Value::text(branch.to_owned())],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let position = match row.get_opt_i64(1)?.map(u64::try_from) {
        Some(Ok(seq)) if seq > 0 => Some(seq),
        _ => None,
    };
    Ok(Some((row.get_text(0)?, position)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::fixtures::store;

    fn watermark(id: &str, branch: &str, tip: &str, at: i64) -> IngestWatermarkRow {
        IngestWatermarkRow {
            watermark_id: id.into(),
            branch: branch.into(),
            branch_tip: tip.into(),
            ingested_at: at,
            schema_version: 1,
        }
    }

    #[tokio::test]
    async fn the_ingest_watermark_is_the_latest_tip_of_its_own_branch() {
        let s = store();
        assert_eq!(
            latest_ingest_watermark(&s, "rift/checkpoints/v1")
                .await
                .unwrap(),
            None
        );

        // Append out of order: the reader must pick by `ingested_at`, not by
        // insertion order — the bridge re-ingests and re-appends freely.
        record_ingest_watermark(&s, &watermark("w2", "rift/checkpoints/v1", "tipB", 20))
            .await
            .unwrap();
        record_ingest_watermark(&s, &watermark("w1", "rift/checkpoints/v1", "tipA", 10))
            .await
            .unwrap();
        record_ingest_watermark(
            &s,
            &watermark("w3", concat!("ent", "ire", "/checkpoints/v1"), "tipZ", 30),
        )
        .await
        .unwrap();

        assert_eq!(
            latest_ingest_watermark(&s, "rift/checkpoints/v1")
                .await
                .unwrap()
                .as_deref(),
            Some("tipB"),
            "the newest tip of THIS branch"
        );
        assert_eq!(
            latest_ingest_watermark(&s, concat!("ent", "ire", "/checkpoints/v1"))
                .await
                .unwrap()
                .as_deref(),
            Some("tipZ"),
            "a second branch resumes from its own watermark, not the newest row"
        );
        assert_eq!(
            latest_ingest_watermark(&s, "no/such/branch").await.unwrap(),
            None
        );
        // Re-recording an identical (branch, tip) is a set-union no-op.
        assert!(
            !record_ingest_watermark(&s, &watermark("w2", "rift/checkpoints/v1", "tipB", 20))
                .await
                .unwrap()
        );
    }

    /// The recall plane stamps every watermark with one constant time so a
    /// rebuild is deterministic; its ids are hashes. The tip it resumes from
    /// must still be the one it wrote last, not the one whose hash sorts last.
    #[tokio::test]
    async fn rows_with_one_ingest_time_resume_from_the_last_one_written() {
        let s = crate::SqliteStore::open_in_memory().unwrap();
        crate::apply_migrations(&s, &crate::recall_migrations()).unwrap();
        let branch = "rift/checkpoints/v1";
        record_ingest_watermark(&s, &watermark("ffff", branch, "tipA", 0))
            .await
            .unwrap();
        record_ingest_watermark(&s, &watermark("0000", branch, "tipB", 0))
            .await
            .unwrap();
        assert_eq!(
            latest_ingest_watermark(&s, branch)
                .await
                .unwrap()
                .as_deref(),
            Some("tipB")
        );
    }

    /// A tip carries the position recorded with it; a tip written before
    /// positions were kept carries none; and the first position recorded for a
    /// tip stands, because a second one is the rewrite the cursor must see.
    #[tokio::test]
    async fn the_latest_tip_carries_the_first_position_recorded_for_it() {
        let s = crate::SqliteStore::open_in_memory().unwrap();
        crate::apply_migrations(&s, &crate::recall_migrations()).unwrap();
        let branch = "rift/checkpoints/v1";
        record_ingest_watermark(&s, &watermark("w1", branch, "tipA", 0))
            .await
            .unwrap();
        let unpositioned = latest_ingest_watermark_with_position(&s, branch)
            .await
            .unwrap();
        assert_eq!(unpositioned, Some(("tipA".to_owned(), None)));

        assert!(record_ingest_watermark_position(&s, "w1", 3).await.unwrap());
        assert!(!record_ingest_watermark_position(&s, "w1", 5).await.unwrap());
        let positioned = latest_ingest_watermark_with_position(&s, branch)
            .await
            .unwrap();
        assert_eq!(positioned, Some(("tipA".to_owned(), Some(3))));
        assert!(record_ingest_watermark_position(&s, "w1", 0).await.is_err());
        assert_eq!(
            latest_ingest_watermark_with_position(&s, "no/such/branch")
                .await
                .unwrap(),
            None
        );
    }
}

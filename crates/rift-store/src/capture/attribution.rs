//! The attribution relations: the per-file rollup, the per-hunk detail it
//! sums, and the no-float boundary that stores a confidence as milli-units.

use rift_core::Result;

use crate::{CanonicalStore, Value};

/// A per-file `attribution` rollup row, keyed by `(checkpoint, file_revision)`.
/// `confidence_milli ∈ 0..=1000` (writer-clamped; the float `[0,1]`
/// score scaled to integer milli-units so no float reaches the store).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributionRow {
    /// Checkpoint this attribution is seen in.
    pub checkpoint_id: String,
    /// File revision attributed.
    pub file_revision_id: String,
    /// Agent-added line count.
    pub agent_added: i64,
    /// Human-added line count.
    pub human_added: i64,
    /// Agent lines a human later modified.
    pub human_modified_agent: i64,
    /// Agent lines a human deleted before commit.
    pub agent_removed_by_human: i64,
    /// Confidence in milli-units (0..=1000).
    pub confidence_milli: i64,
    /// Attribution algorithm version (provenance/reproducibility).
    pub method_version: String,
    /// Prior `(ckpt,rev)` this corrects, if any.
    pub supersedes_pk: Option<String>,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a per-file `attribution` rollup (idempotent on its composite PK).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_attribution(store: &impl CanonicalStore, r: &AttributionRow) -> Result<bool> {
    store
        .insert_or_ignore(
            "attribution",
            &[
                "checkpoint_id",
                "file_revision_id",
                "agent_added",
                "human_added",
                "human_modified_agent",
                "agent_removed_by_human",
                "confidence_milli",
                "method_version",
                "supersedes_pk",
                "schema_version",
            ],
            &[
                Value::text(r.checkpoint_id.clone()),
                Value::text(r.file_revision_id.clone()),
                Value::Int(r.agent_added),
                Value::Int(r.human_added),
                Value::Int(r.human_modified_agent),
                Value::Int(r.agent_removed_by_human),
                Value::Int(r.confidence_milli),
                Value::text(r.method_version.clone()),
                r.supersedes_pk.clone().map_or(Value::Null, Value::text),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// Clamp a `[0,1]` confidence to integer milli-units (`0..=1000`) for storage —
/// the no-float boundary for the attribution score.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn confidence_to_milli(confidence: f64) -> i64 {
    // c ∈ [0,1] ⇒ c*1000 ∈ [0,1000] ⇒ round ∈ 0..=1000, always fits i64.
    let c = confidence.clamp(0.0, 1.0);
    (c * 1000.0).round() as i64
}

/// An `attribution_hunk` row: the per-hunk detail the per-file rollup sums.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributionHunkRow {
    /// The anchoring checkpoint.
    pub checkpoint_id: String,
    /// The file revision.
    pub file_revision_id: String,
    /// Hunk order within the file's diff.
    pub hunk_ordinal: i64,
    /// New-side start line.
    pub new_start: i64,
    /// New-side length.
    pub new_len: i64,
    /// One of `agent_added` | `human_added` | `human_modified_agent` |
    /// `agent_removed_by_human` | `unknown`.
    pub category: String,
    /// Lines in this hunk attributed to `category`.
    pub line_count: i64,
    /// Confidence × 1000 (0..=1000).
    pub confidence_milli: i64,
    /// The attribution method version that produced this row.
    pub method_version: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record an `attribution_hunk` (idempotent on the composite key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_attribution_hunk(
    store: &impl CanonicalStore,
    r: &AttributionHunkRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "attribution_hunk",
            &[
                "checkpoint_id",
                "file_revision_id",
                "hunk_ordinal",
                "new_start",
                "new_len",
                "category",
                "line_count",
                "confidence_milli",
                "method_version",
                "schema_version",
            ],
            &[
                Value::text(r.checkpoint_id.clone()),
                Value::text(r.file_revision_id.clone()),
                Value::Int(r.hunk_ordinal),
                Value::Int(r.new_start),
                Value::Int(r.new_len),
                Value::text(r.category.clone()),
                Value::Int(r.line_count),
                Value::Int(r.confidence_milli),
                Value::text(r.method_version.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// Read the complete immutable rollup for one exact attribution key.
///
/// # Errors
/// A storage or typed-column decoding failure.
pub async fn attribution_for_key(
    store: &impl CanonicalStore,
    checkpoint: &str,
    revision: &str,
) -> Result<Option<AttributionRow>> {
    let rows = store
        .query(
            "SELECT agent_added, human_added, human_modified_agent, agent_removed_by_human, \
         confidence_milli, method_version, supersedes_pk, schema_version FROM attribution \
         WHERE checkpoint_id = ?1 AND file_revision_id = ?2",
            &[
                Value::text(checkpoint.to_owned()),
                Value::text(revision.to_owned()),
            ],
        )
        .await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(AttributionRow {
        checkpoint_id: checkpoint.to_owned(),
        file_revision_id: revision.to_owned(),
        agent_added: row.get_i64(0)?,
        human_added: row.get_i64(1)?,
        human_modified_agent: row.get_i64(2)?,
        agent_removed_by_human: row.get_i64(3)?,
        confidence_milli: row.get_i64(4)?,
        method_version: row.get_text(5)?,
        supersedes_pk: row.get_opt_text(6)?,
        schema_version: row.get_i64(7)?,
    }))
}

/// Read all immutable detail rows for an exact key, in ordinal/category order.
/// The caller determines whether the set is complete or conflicting.
///
/// # Errors
/// A storage or typed-column decoding failure.
pub async fn attribution_hunks_for_key(
    store: &impl CanonicalStore,
    checkpoint: &str,
    revision: &str,
) -> Result<Vec<AttributionHunkRow>> {
    let rows = store
        .query(
            "SELECT hunk_ordinal, new_start, new_len, category, line_count, confidence_milli, \
         method_version, schema_version FROM attribution_hunk \
         WHERE checkpoint_id = ?1 AND file_revision_id = ?2 ORDER BY hunk_ordinal, category",
            &[
                Value::text(checkpoint.to_owned()),
                Value::text(revision.to_owned()),
            ],
        )
        .await?;
    rows.iter()
        .map(|row| {
            Ok(AttributionHunkRow {
                checkpoint_id: checkpoint.to_owned(),
                file_revision_id: revision.to_owned(),
                hunk_ordinal: row.get_i64(0)?,
                new_start: row.get_i64(1)?,
                new_len: row.get_i64(2)?,
                category: row.get_text(3)?,
                line_count: row.get_i64(4)?,
                confidence_milli: row.get_i64(5)?,
                method_version: row.get_text(6)?,
                schema_version: row.get_i64(7)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::fixtures::store;

    #[test]
    fn confidence_milli_clamps_and_rounds() {
        assert_eq!(confidence_to_milli(0.0), 0);
        assert_eq!(confidence_to_milli(1.0), 1000);
        assert_eq!(confidence_to_milli(0.611), 611);
        assert_eq!(confidence_to_milli(1.5), 1000, "clamped above");
        assert_eq!(confidence_to_milli(-0.5), 0, "clamped below");
    }

    #[tokio::test]
    async fn an_attribution_rollup_round_trips_with_a_milli_confidence() {
        let s = store();
        record_attribution(
            &s,
            &AttributionRow {
                checkpoint_id: "ckpt1".into(),
                file_revision_id: "rev1".into(),
                agent_added: 10,
                human_added: 3,
                human_modified_agent: 2,
                agent_removed_by_human: 1,
                confidence_milli: confidence_to_milli(0.61),
                method_version: "attr@1".into(),
                supersedes_pk: None,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        assert_eq!(s.count("attribution").await.unwrap(), 1);
    }

    fn attribution_hunk(ordinal: i64, category: &str) -> AttributionHunkRow {
        AttributionHunkRow {
            checkpoint_id: "ckpt1".into(),
            file_revision_id: "rev1".into(),
            hunk_ordinal: ordinal,
            new_start: 10 + ordinal,
            new_len: 3,
            category: category.into(),
            line_count: 3,
            confidence_milli: 900,
            method_version: "v1".into(),
            schema_version: 1,
        }
    }

    #[tokio::test]
    async fn attribution_key_readers_preserve_every_field_and_detail_order() {
        let s = store();
        assert!(attribution_for_key(&s, "ckpt1", "rev1")
            .await
            .unwrap()
            .is_none());
        let row = AttributionRow {
            checkpoint_id: "ckpt1".into(),
            file_revision_id: "rev1".into(),
            agent_added: 1,
            human_added: 2,
            human_modified_agent: 3,
            agent_removed_by_human: 4,
            confidence_milli: 731,
            method_version: "fixture@2".into(),
            supersedes_pk: Some("historical-key".into()),
            schema_version: 1,
        };
        record_attribution(&s, &row).await.unwrap();
        assert_eq!(
            attribution_for_key(&s, "ckpt1", "rev1").await.unwrap(),
            Some(row)
        );
        let first = attribution_hunk(0, "agent_added");
        let second = attribution_hunk(1, "unknown");
        record_attribution_hunk(&s, &second).await.unwrap();
        record_attribution_hunk(&s, &first).await.unwrap();
        assert_eq!(
            attribution_hunks_for_key(&s, "ckpt1", "rev1")
                .await
                .unwrap(),
            vec![first, second]
        );
        assert!(attribution_hunks_for_key(&s, "other", "rev1")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn attribution_hunks_accumulate_per_ordinal_and_re_record_as_no_ops() {
        let s = store();
        assert!(
            record_attribution_hunk(&s, &attribution_hunk(0, "agent_added"))
                .await
                .unwrap()
        );
        assert!(
            !record_attribution_hunk(&s, &attribution_hunk(0, "agent_added"))
                .await
                .unwrap(),
            "a replayed attribution pass must not double-count a hunk"
        );
        assert!(
            record_attribution_hunk(&s, &attribution_hunk(1, "human_added"))
                .await
                .unwrap()
        );
        assert_eq!(s.count("attribution_hunk").await.unwrap(), 2);
    }
}

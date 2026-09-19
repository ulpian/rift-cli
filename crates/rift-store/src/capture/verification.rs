//! The verification relations: one `rift verify` run, the divergences it
//! found and the condensations it witnessed.

use rift_core::Result;

use crate::{CanonicalStore, Value};

/// A `verification_run` row: one `rift verify` / dual-run soak record.
#[derive(Debug, Clone)]
pub struct VerificationRunRow {
    /// Surrogate run id.
    pub run_id: String,
    /// The branch verified (`rift/checkpoints/v1`).
    pub branch: String,
    /// The run's exit code (0 = clean).
    pub exit_code: i64,
    /// Transaction-time (epoch ms).
    pub checked_at: i64,
    /// SHA-256 over the run outcome (hex).
    pub outcome_sha256: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `verification_run` (idempotent on `run_id`).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_verification_run(
    store: &impl CanonicalStore,
    r: &VerificationRunRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "verification_run",
            &[
                "run_id",
                "branch",
                "exit_code",
                "checked_at",
                "outcome_sha256",
                "schema_version",
            ],
            &[
                Value::text(r.run_id.clone()),
                Value::text(r.branch.clone()),
                Value::Int(r.exit_code),
                Value::Int(r.checked_at),
                Value::text(r.outcome_sha256.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `verification_divergence` row: one parity divergence in a run.
#[derive(Debug, Clone)]
pub struct VerificationDivergenceRow {
    /// The run this divergence belongs to.
    pub run_id: String,
    /// Order within the run.
    pub ordinal: i64,
    /// The checkpoint involved (may be empty for capability-level divergences).
    pub checkpoint_id: String,
    /// The layer that diverged (`Leaf` | `CheckpointRoot` | `git_write` | …).
    pub layer: String,
    /// The comparison site (`GitVsRecomputed` | `DualRunWrapperVsNative` | …).
    pub site: String,
    /// A human-readable detail (identifiers/hashes only — never transcript bytes).
    pub detail: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `verification_divergence` (idempotent on `(run_id, ordinal)`).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_verification_divergence(
    store: &impl CanonicalStore,
    r: &VerificationDivergenceRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "verification_divergence",
            &[
                "run_id",
                "ordinal",
                "checkpoint_id",
                "layer",
                "site",
                "detail",
                "schema_version",
            ],
            &[
                Value::text(r.run_id.clone()),
                Value::Int(r.ordinal),
                Value::text(r.checkpoint_id.clone()),
                Value::text(r.layer.clone()),
                Value::text(r.site.clone()),
                Value::text(r.detail.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `verification_witness` row (v12): the condensation a verification run
/// actually witnessed.
///
/// **Why this row exists at all.** `verification_run` records that a verify
/// happened; it does not record what it was a verify *of*. `record_verification`
/// mints its `run_id` from `leaf_hash("{exit_code}:{checkpoints}:{now}")`, which
/// takes no archive input, so a run and the archive decision beside it share no
/// key in either direction. An owner decision of 2026-08-18 makes the soak's
/// population CONDENSATIONS rather than verify invocations, and this row is the
/// only place that distinction is written down: a run with a witness accrued
/// from real work, a run without one is a bare `rift verify`.
///
/// One run may carry SEVERAL witnesses — doctor's stale-session sweep runs one
/// verify after finalizing N sessions — which is why the primary key is
/// composite.
#[derive(Debug, Clone)]
pub struct VerificationWitnessRow {
    /// The run that witnessed (FK: `verification_run`).
    pub run_id: String,
    /// The checkpoint it witnessed (FK: `checkpoint`).
    pub checkpoint_id: String,
    /// The `refs/rift/chain/v1` head at witness time, when it could be read.
    ///
    /// `None` means UNREADABLE, never "empty" — the column is nullable for that
    /// reason and this field is `Option` for the same one. A soak counter is the
    /// last place to let absence read as a value.
    pub chain_head: Option<String>,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `verification_witness` (idempotent on `(run_id, checkpoint_id)`).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_verification_witness(
    store: &impl CanonicalStore,
    r: &VerificationWitnessRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "verification_witness",
            &["run_id", "checkpoint_id", "chain_head", "schema_version"],
            &[
                Value::text(r.run_id.clone()),
                Value::text(r.checkpoint_id.clone()),
                r.chain_head.clone().map_or(Value::Null, Value::text),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::fixtures::store;

    fn witness(run: &str, checkpoint: &str, head: Option<&str>) -> VerificationWitnessRow {
        VerificationWitnessRow {
            run_id: run.into(),
            checkpoint_id: checkpoint.into(),
            chain_head: head.map(std::borrow::ToOwned::to_owned),
            schema_version: 1,
        }
    }

    /// One run may witness several checkpoints — doctor's sweep runs one verify
    /// after finalizing N sessions — and re-recording either pair is a no-op,
    /// because a drain that is retried must not double-count a condensation.
    #[tokio::test]
    async fn a_run_witnesses_many_checkpoints_and_re_recording_one_is_a_no_op() {
        let s = store();
        assert!(
            record_verification_witness(&s, &witness("run-1", "ck-a", Some("head1")))
                .await
                .unwrap()
        );
        assert!(
            record_verification_witness(&s, &witness("run-1", "ck-b", Some("head1")))
                .await
                .unwrap()
        );
        assert!(
            !record_verification_witness(&s, &witness("run-1", "ck-a", Some("head1")))
                .await
                .unwrap(),
            "a retried drain must not double-count the condensation it already witnessed"
        );
        assert_eq!(s.count("verification_witness").await.unwrap(), 2);
    }

    /// An unreadable chain head is stored as NULL, never as the empty string —
    /// the zero-vs-absent distinction this program has already lost twice, and
    /// the soak is the last counter that should lose it a third time.
    #[tokio::test]
    async fn an_unreadable_chain_head_is_null_and_not_an_empty_string() {
        let s = store();
        record_verification_witness(&s, &witness("run-2", "ck-c", None))
            .await
            .unwrap();
        let rows = s
            .query(
                "SELECT chain_head IS NULL, COALESCE(chain_head, 'ABSENT') FROM \
                 verification_witness WHERE run_id = ?1",
                &[Value::text("run-2")],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_i64(0).unwrap(), 1, "chain_head must be NULL");
        assert_eq!(rows[0].get_text(1).unwrap(), "ABSENT");
    }
}

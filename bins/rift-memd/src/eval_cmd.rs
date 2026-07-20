//! `rift-memd eval` — the Lost-in-Delta entry point (ADR-0042, M6-07).
//!
//! Runs the deterministic conditions end-to-end — the **served** condition
//! (real in-process `McpServer` dispatch over the fixture corpus) and the
//! **seeded regression** (the gate-proves-it-gates evidence) — appends one
//! append-only bitemporal `eval_run` row per condition to the repo's
//! `recall.db`, and prints the rendered go/no-go reports. The run id is
//! content-addressed (task-set ‖ condition ‖ code version), so re-running an
//! identical experiment is an idempotent no-op; the reviewable contract is
//! the insta report snapshot committed in `rift-recall-eval`.

use std::path::Path;

use rift_core::{Result, Timestamp};
use rift_recall_eval::{
    deterministic_corpus, render_report, replay_served, seeded_regression_runs, EvalRunRecord,
    GaThresholds, PairedRun,
};
use rift_store::{CanonicalStore, Value};

/// The corpus identity stamped into `task_set` (bump when the corpus grows).
const TASK_SET: &str = "deterministic-corpus-v1";

/// Append one flattened record as an `eval_run` row (idempotent on the
/// content-addressed id). `now_ms` stamps both bitemporal columns: the
/// verdict becomes valid when computed and is recorded in the same breath.
async fn append_record(
    store: &impl CanonicalStore,
    rec: &EvalRunRecord,
    now_ms: i64,
) -> Result<()> {
    store
        .insert_or_ignore(
            "eval_run",
            &[
                "run_id",
                "task_set",
                "condition",
                "code_version",
                "thresholds_version",
                "runs",
                "p10_token_savings_bp",
                "p50_token_savings_bp",
                "p90_token_savings_bp",
                "p50_cost_savings_bp",
                "baseline_tokens",
                "delta_tokens",
                "emitted_avoided_tokens",
                "reconcile_over_estimates",
                "max_latency_steps",
                "verdict",
                "failing_dimensions",
                "valid_from",
                "valid_to",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text(&rec.run_id),
                Value::text(&rec.task_set),
                Value::text(&rec.condition),
                Value::text(&rec.code_version),
                Value::Int(i64::from(rec.thresholds_version)),
                Value::Int(i64::from(rec.runs)),
                Value::Int(rec.p10_token_savings_bp),
                Value::Int(rec.p50_token_savings_bp),
                Value::Int(rec.p90_token_savings_bp),
                Value::Int(rec.p50_cost_savings_bp),
                Value::Int(i64::try_from(rec.baseline_tokens).unwrap_or(i64::MAX)),
                Value::Int(i64::try_from(rec.delta_tokens).unwrap_or(i64::MAX)),
                Value::Int(i64::try_from(rec.emitted_avoided_tokens).unwrap_or(i64::MAX)),
                Value::Int(i64::from(rec.reconcile_over_estimates)),
                Value::Int(i64::from(rec.max_latency_steps)),
                Value::text(&rec.verdict),
                rec.failing_dimensions
                    .as_deref()
                    .map_or(Value::Null, Value::text),
                Value::Int(now_ms),
                Value::Null,
                Value::Int(now_ms),
                Value::Int(1),
            ],
        )
        .await?;
    Ok(())
}

/// The deterministic conditions in verdict order: `(condition, runs)`.
///
/// # Errors
/// A served-replay fixture or dispatch failure.
fn conditions() -> Result<Vec<(&'static str, Vec<PairedRun>)>> {
    let served: Vec<PairedRun> = deterministic_corpus()
        .iter()
        .map(replay_served)
        .collect::<Result<_>>()?;
    Ok(vec![
        ("served", served),
        ("seeded-regression", seeded_regression_runs()),
    ])
}

/// Run the eval verb: score both conditions, append the rows, print the
/// reports. Synchronous by contract — `replay_served` owns its internal
/// runtimes, so this must be called with NO ambient tokio runtime (main
/// dispatches it before building one); the store appends get a local one.
///
/// # Errors
/// A store or replay failure (identifier/reason only).
pub(crate) fn run_eval(repo: &Path) -> Result<()> {
    let both = conditions()?;
    let db = crate::recall_db_path(repo)?;
    let store = crate::open_recall_db(&db)?;
    let now_ms = Timestamp::now().as_millis();
    let code_version = env!("CARGO_PKG_VERSION");
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|e| rift_core::RiftError::storage(format!("eval runtime: {e}")))?;
    for (condition, runs) in both {
        let rec =
            EvalRunRecord::from_runs(TASK_SET, condition, code_version, &GaThresholds::V1, &runs);
        rt.block_on(append_record(&store, &rec, now_ms))?;
        print!("{}", render_report(condition, &GaThresholds::V1, &runs));
        println!("recorded: eval_run {} ({})\n", rec.run_id, rec.verdict);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_store::{apply_migrations, recall_migrations, SqliteStore};

    /// The ADR-0042 row contract: both deterministic conditions land as
    /// append-only bitemporal rows, idempotently (a re-run of the identical
    /// experiment appends nothing), with the expected verdicts.
    #[test]
    fn eval_rows_land_bitemporally_and_idempotently() {
        let store = SqliteStore::open_in_memory().unwrap();
        apply_migrations(&store, &recall_migrations()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        let both = conditions().unwrap();
        for (condition, runs) in &both {
            let rec = EvalRunRecord::from_runs(
                TASK_SET,
                condition,
                "test-version",
                &GaThresholds::V1,
                runs,
            );
            rt.block_on(append_record(&store, &rec, 1_000)).unwrap();
            // Idempotent: the content-addressed id makes a re-run a no-op.
            rt.block_on(append_record(&store, &rec, 2_000)).unwrap();
        }
        let rows = rt
            .block_on(store.query(
                "SELECT condition, verdict, failing_dimensions, valid_from, recorded_at \
                 FROM eval_run ORDER BY condition",
                &[],
            ))
            .unwrap();
        assert_eq!(rows.len(), 2, "one row per condition, re-runs append none");
        assert_eq!(rows[0].get_text(0).unwrap(), "seeded-regression");
        assert_eq!(rows[0].get_text(1).unwrap(), "no-go");
        assert!(rows[0].get_text(2).unwrap().contains("token_savings"));
        assert_eq!(rows[1].get_text(0).unwrap(), "served");
        assert_eq!(rows[1].get_text(1).unwrap(), "go");
        // Bitemporal stamps come from the FIRST append (append-only history).
        assert_eq!(rows[0].get_i64(3).unwrap(), 1_000);
        assert_eq!(rows[0].get_i64(4).unwrap(), 1_000);
    }
}

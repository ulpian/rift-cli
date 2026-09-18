//! `rift-memd eval` — the Lost-in-Delta entry point.
//!
//! Runs the deterministic conditions end-to-end — the **served** condition
//! (real in-process `ServeDispatcher` dispatch over the fixture corpus) and the
//! **seeded regression** (the gate-proves-it-gates evidence) — appends one
//! append-only bitemporal `eval_run` row per condition to the repo's
//! `recall.db`, and prints the rendered go/no-go reports. It then prints the
//! **intercepted** condition: the same corpus replayed against a window that
//! already holds it, with the read lever enforcing.
//!
//! The intercepted condition prints and does not append. `eval_run.condition`
//! is a closed vocabulary written into the migration that created the table,
//! and widening it would be a migration on an append-only canonical relation —
//! which this work does not make. Its reviewable contract is therefore the
//! committed report snapshot in `rift-recall-eval` rather than a row, and the
//! verdict printed here is the one that snapshot pins. The run id is
//! content-addressed (task-set ‖ condition ‖ code version), so re-running an
//! identical experiment is an idempotent no-op; the reviewable contract is
//! the insta report snapshot committed in `rift-recall-eval`.

use std::path::Path;

use rift_core::{Result, Timestamp};
use rift_recall_eval::{
    corpus_honesty_figures, deterministic_corpus, render_report, replay_intercepted, replay_served,
    seeded_regression_runs, EvalRunRecord, GaThresholds, PairedRun, INTERCEPTED,
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

/// Print the intercepted condition: its report, and beside it the two gross
/// figures the honesty gate is a statement about.
///
/// The savings percent in the report and the gate below it are different
/// claims and are never folded together — a mechanism that saved a thousand
/// tokens by spending nine hundred of them on reasons saved a hundred.
///
/// # Errors
/// A policy or fixture-store failure inside the replay.
fn print_intercepted() -> Result<()> {
    let corpus = deterministic_corpus();
    let runs: Vec<PairedRun> = corpus
        .iter()
        .map(replay_intercepted)
        .collect::<Result<_>>()?;
    print!("{}", render_report(INTERCEPTED, &GaThresholds::V1, &runs));
    let (avoided, returned) = corpus_honesty_figures(&corpus)?;
    println!(
        "honesty gate: the declines avoided {avoided} tokens and their reasons cost \
         {returned} — {}",
        if avoided > returned {
            "held"
        } else {
            "BREACHED"
        }
    );
    println!("not recorded: `eval_run.condition` is a closed vocabulary\n");
    Ok(())
}

/// Run the eval verb: score both recorded conditions, append the rows, print the
/// reports, then print the intercepted condition. Synchronous by contract — `replay_served` owns its internal
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
    print_intercepted()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_store::{apply_migrations, recall_migrations, SqliteStore};

    /// The row contract: both **recorded** deterministic conditions land as
    /// append-only bitemporal rows, idempotently (a re-run of the identical
    /// experiment appends nothing), with the expected verdicts. The intercepted
    /// condition is deliberately not among them — see the module doc — and
    /// [`the_intercepted_condition_is_published_and_never_written_as_a_row`]
    /// holds that boundary from the other side.
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

    /// The intercepted condition is published, not written: the relation's
    /// condition vocabulary is closed by the migration that created the table,
    /// so a row for it could only exist by widening that migration — which is
    /// the one thing this work may not do. The verb prints the verdict instead,
    /// and the boundary is asserted rather than left to a reviewer's memory.
    #[test]
    fn the_intercepted_condition_is_published_and_never_written_as_a_row() {
        let vocabulary = rift_store::recall_canonical_table("eval_run")
            .and_then(|t| t.columns.iter().find(|c| c.name == "condition"))
            .map(|c| c.one_of)
            .expect("the relation declares its condition vocabulary");
        assert!(
            !vocabulary.contains(&INTERCEPTED),
            "if the vocabulary ever gains this condition, the verb should append \
             the row rather than only print it"
        );
        assert!(
            !conditions()
                .unwrap()
                .iter()
                .any(|(condition, _)| *condition == INTERCEPTED),
            "an appended condition must be one the relation accepts"
        );
        print_intercepted().unwrap();
    }
}

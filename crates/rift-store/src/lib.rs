//! Append-only store for Rift's projection layer.
//!
//! The production target is **Turso** (async, MVCC, SQLite-compatible). Because
//! Turso is BETA and async, this crate ships a synchronous **rusqlite** reference
//! implementation ([`SqliteStore`]) that runs the *same SQL and the same
//! triggers* Turso will, so the immutability guarantees are testable offline and
//! the SQL transfers verbatim. Both implementations sit behind the
//! [`CanonicalStore`] trait — the swappable seam the build plan mandates because
//! Turso is pre-1.0.
//!
//! On top of the store this crate provides:
//! * the additive, checksum-locked [`migrate`] engine;
//! * the hash-chain & Merkle [`integrity`] store and the git cross-check seam;
//! * application-layer column encryption ([`crypto`]).
//!
//! Immutability is enforced in three layers:
//! 1. **Type layer** — [`CanonicalStore`] exposes only [`CanonicalStore::insert`].
//! 2. **SQL layer** — every canonical table gets `BEFORE UPDATE`/`BEFORE DELETE`
//!    triggers that `RAISE(ABORT)` (see [`schema`]).
//! 3. **Test layer** — a registry-driven property test asserts every table in
//!    [`schema::canonical_tables`] rejects mutation, so a new table without its
//!    guard fails CI.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod bundle;
pub mod capture;
pub mod crypto;
pub mod cursor;
pub mod durable;
pub mod encrypted_dao;
pub mod identity_store;
pub mod integrity;
pub mod key_metadata;
pub mod keychain;
#[cfg(feature = "keyring")]
pub mod keychain_os;
pub mod migrate;
pub mod paths;
mod projection_lock;
pub mod rebuild;
pub mod schema;
pub mod sensitive;
mod sqlite_store;
pub mod store;
// NOTE — the Turso engine is DEFERRED: a native-async `TursoStore` implements
// the `store::Store` seam, but the `turso` dependency is intentionally not wired
// (it pulls advisory-bearing git2/paste, and turso 0.1.5 lacks CREATE TRIGGER /
// STRICT / encryption). The reference impl and the measurements behind that
// call were kept out of tree; re-home them here and re-add the dep/feature only
// after re-review. The rusqlite `SqliteStore` is the shipping engine; any new
// engine plugs in behind `store::Store` without touching callers.
mod value;
pub mod window;

pub use bundle::{
    attaches_by, export_segment, import_segment, ChainSegment, ImportReport,
    CHAIN_SEGMENT_SCHEMA_VERSION,
};
pub use capture::{
    attribution_for_key, attribution_hunks_for_key, confidence_to_milli, content_blob_oid_map,
    effective_prices, latest_archive_ms, latest_ingest_watermark, latest_turn_boundary_files,
    latest_turn_boundary_snapshot, quote_usage_costs, record_agent_enablement,
    record_agent_hook_event, record_archive_decision, record_attribution, record_attribution_hunk,
    record_capture_session_identity, record_captured_event, record_changed_hunk,
    record_checkpoint_worktree_if_new, record_file_read_tokens, record_file_revision,
    record_ingest_watermark, record_main_context_observation, record_model_call,
    record_model_observation, record_price, record_session, record_session_event,
    record_session_in_checkpoint, record_session_transition, record_session_worktree,
    record_subagent_session, record_substrate_block_event, record_token_usage, record_tool_call,
    record_tool_edits_file, record_tool_reads_file, record_tool_result_tokens, record_turn,
    record_turn_boundary_file, record_turn_boundary_file_with_kind, record_turn_boundary_snapshot,
    record_turn_diff_hunk, record_verification_divergence, record_verification_run,
    record_verification_witness, regular_turn_boundary_files_for_session_path, session_read_facts,
    turn_boundary_counts, turn_boundary_file_kinds_for_snapshot,
    turn_boundary_files_for_session_path, turn_boundary_files_for_snapshot,
    turn_diff_hunks_for_session, unproven_turn_hunk_paths, AgentEnablementRow, AgentHookEventRow,
    ArchiveDecisionRow, AttributionHunkRow, AttributionRow, BoundaryFileKind,
    CaptureSessionIdentityRow, CapturedEventRow, ChangedHunkRow, CostQuoteScope, EffectivePrices,
    FileRevisionRow, IngestWatermarkRow, MainContextObservationRow, ModelCallRow,
    ModelObservationRow, PriceTableRow, SessionEventRow, SessionInCheckpointRow, SessionReadFact,
    SessionRow, SessionTransitionRow, SessionWorktreeRow, SubagentSessionRow,
    SubstrateBlockEventRow, TokenUsageRow, ToolCallRow, TurnBoundaryFileRow,
    TurnBoundarySnapshotRow, TurnDiffHunkRow, TurnRow, VerificationDivergenceRow,
    VerificationRunRow, VerificationWitnessRow,
};
pub use capture::{
    boundary_gap_batch, bounded_turn_boundary_observation, check_boundary_gap_batch,
    record_boundary_gap_batch, record_turn_boundary_collection,
    turn_boundary_observation_for_snapshot, BoundaryCollectionInterval, BoundaryObservationRow,
    BoundedBoundaryObservation,
};
pub use crypto::{
    decrypt_column, encrypt_column, ColumnCipher, DekCipher, KeyStore, MemoryKeyStore, PlainCipher,
};
pub use cursor::{
    cursor_of, head_by, parse_canonical_u64, suffix_after_by, StreamCursor, CURSOR_SCHEMA_VERSION,
};
pub use durable::{DurableWrite, LocalOnlyShadowWrite, ObjectKind, SyncWrite};
pub use encrypted_dao::EncryptingStore;
pub use identity_store::{
    latest_binding_for_session, latest_identity_snapshot, record_identity_snapshot,
    record_session_binding, unbound_session_ids,
};
pub use integrity::{
    cost_belief_as_of, current_cost_micros, latest_booked_cost_at, record_checkpoint,
    record_checkpoint_commit_if_new, record_checkpoint_if_new, record_cost_fact,
    record_cost_if_changed, record_imported_checkpoint_if_new, verify_against_source, BookedCost,
    ChainSource, CheckpointRecord, CostFact, ImportedCheckpointRecord, VerifyReport,
};
pub use key_metadata::{
    key_metadata_for_generation, latest_key_metadata, record_key_metadata, KeyMetadataRow,
};
pub use keychain::{KeychainBackend, KeyringKeyStore, MemoryKeychain};
#[cfg(feature = "keyring")]
pub use keychain_os::OsKeychain;
pub use migrate::{
    apply_migrations, apply_migrations_on, canopy_canonical_schema_sql, canopy_migrations,
    default_migrations, open_capture_projection, open_existing_migrated, open_migrated,
    open_recall_projection, open_replacement_migrated, recall_canonical_schema_sql,
    recall_migrations, Migration, MigrationOutcome,
};
pub use paths::{
    canopy_ui_db_from, canopy_ui_db_under, capture_projection_db_for_repo, capture_repo_id,
    connection_state_dir_for_repo, connection_state_dir_under, default_canopy_ui_db,
    default_projection_db, default_projection_dir, default_recall_db, projection_db_for_family,
    projection_db_for_family_under, projection_db_under, projection_dir_under,
    recall_db_for_family, recall_db_for_family_under, recall_db_for_repo, recall_db_from,
    recall_db_under, recall_repo_id, resolve_cache_root,
};
pub use projection_lock::{HistoricalProjectionSnapshot, ProjectionReplacementGuard};
pub use rebuild::rebuild;
pub use schema::{
    any_registry_table, canonical_table, canonical_tables, canopy_canonical_table,
    canopy_canonical_tables, is_canonical_table, recall_canonical_table, recall_canonical_tables,
    CanonicalTable, Column, RECALL_EDGE_KINDS, RECALL_NODE_KINDS,
};
pub use sensitive::{is_sensitive_column, sensitive_columns, SensitiveColumn};
pub use sqlite_store::SqliteStore;
pub use store::{NotifyCap, Row, Store, StoreConn, StoreDescriptor, StoreTx, TxMode};
pub use value::Value;
pub use window::CREATED_AT_UNKNOWN;

use async_trait::async_trait;
use rift_core::Result;

/// The append-only repository, layered over any [`Store`] engine.
///
/// Deliberately exposes no `update` or `delete`: the only mutating operations are
/// inserts. Corrections are modelled as new, superseding rows. A blanket impl
/// provides it for every [`Store`], so the rusqlite reference engine and the
/// Turso engine get the identical insert-only surface — the type-layer half of the
/// immutability guarantee. Read helpers are included so
/// the integrity/cost modules query through the engine seam, not a concrete store.
#[async_trait]
pub trait CanonicalStore: Send + Sync {
    /// Create a canonical table and its append-only guard triggers (idempotent).
    ///
    /// # Errors
    /// Returns [`rift_core::RiftError::Migration`] if the DDL fails.
    async fn register_canonical_table(&self, table: &CanonicalTable) -> Result<()>;

    /// Insert a single row.
    ///
    /// # Errors
    /// Returns [`rift_core::RiftError::Storage`] if `columns`/`values` lengths
    /// differ or the engine rejects the row (e.g. duplicate primary key),
    /// [`rift_core::RiftError::ConstraintRefused`] if a CHECK, NOT NULL or
    /// datatype constraint refuses it, or
    /// [`rift_core::RiftError::Immutability`] if an append-only guard fires.
    async fn insert(&self, table: &str, columns: &[&str], values: &[Value]) -> Result<()>;

    /// Insert a row, **skipping a primary-key or UNIQUE conflict** — the
    /// idempotent set-union used for content-addressed ingest (re-ingesting the
    /// same fact is a no-op). Returns `true` if a new row was
    /// inserted, `false` if a row with the same key was already present.
    ///
    /// Only a key conflict is skipped: a row a CHECK, NOT NULL or datatype
    /// constraint forbids is an error, so `false` always means "already on
    /// record" and never "refused".
    ///
    /// # Errors
    /// As [`CanonicalStore::insert`], except a key conflict is **not** an
    /// error; [`rift_core::RiftError::ConstraintRefused`] names the table and
    /// the constraint when the engine refuses the row itself.
    async fn insert_or_ignore(
        &self,
        table: &str,
        columns: &[&str],
        values: &[Value],
    ) -> Result<bool>;

    /// Count rows in a table.
    ///
    /// # Errors
    /// Returns [`rift_core::RiftError::Storage`] if the query fails.
    async fn count(&self, table: &str) -> Result<i64>;

    /// Run a parameterized read, materializing all rows.
    ///
    /// # Errors
    /// [`rift_core::RiftError::Storage`] on failure.
    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>>;

    /// Read a single optional `i64` (e.g. a "current belief" cost).
    ///
    /// # Errors
    /// [`rift_core::RiftError::Storage`] on failure.
    async fn query_opt_i64(&self, sql: &str, params: &[Value]) -> Result<Option<i64>>;

    /// Insert a typed row: table/column identifiers are the row type's
    /// compile-time constants (no free-string identifiers at the call site).
    ///
    /// # Errors
    /// As [`CanonicalStore::insert`].
    async fn insert_row<R: CanonicalRow + Sync>(&self, row: &R) -> Result<()>
    where
        Self: Sized,
    {
        self.insert(R::TABLE, R::COLUMNS, &row.values()).await
    }

    /// [`CanonicalStore::insert_or_ignore`] for a typed row (idempotent
    /// set-union for content-addressed keys).
    ///
    /// # Errors
    /// As [`CanonicalStore::insert_or_ignore`].
    async fn insert_row_or_ignore<R: CanonicalRow + Sync>(&self, row: &R) -> Result<bool>
    where
        Self: Sized,
    {
        self.insert_or_ignore(R::TABLE, R::COLUMNS, &row.values())
            .await
    }

    /// A cheap, monotone-under-append **cursor** over `table`: it increases when
    /// rows are appended and *resets backward* only when the projection is rebuilt
    /// from git (the file is recreated). A change observer polls this to
    /// decide *when* to re-read — the value is advisory, never authoritative (git
    /// stays canonical), so a missed or duplicated read only delays or repeats a
    /// query. The reference engine derives it from [`CanonicalStore::count`]; a
    /// native-notify engine may override it with an engine cursor.
    ///
    /// # Errors
    /// [`rift_core::RiftError::Storage`] if the query fails.
    async fn read_watermark(&self, table: &str) -> Result<i64> {
        self.count(table).await
    }

    /// The projection's **rebuild epoch**: bumped by
    /// [`crate::rebuild::rebuild`], never by ingest — so `{cursor, epoch}` is a
    /// complete watermark and a rebuild that lands *more* rows than before is
    /// still detected. Absent (a pre-epoch projection, a missing row, or an
    /// unreadable meta table) reads as **zero**; availability is the cursor
    /// read's job, so this read never turns a probe into an error.
    ///
    /// # Errors
    /// None in practice (absence and unreadability both read as zero); the
    /// `Result` is kept for engine implementations with fallible transports.
    async fn read_rebuild_epoch(&self) -> Result<i64> {
        let read = self
            .query_opt_i64(
                "SELECT value FROM projection_meta WHERE key = 'rebuild_epoch'",
                &[],
            )
            .await;
        Ok(read.unwrap_or(None).unwrap_or(0))
    }

    /// This engine's change-notification capability (see [`NotifyCap`]). The
    /// rusqlite reference engine is [`NotifyCap::PollOnly`]; a future Turso engine
    /// overrides this to [`NotifyCap::Native`] without touching any caller.
    fn notify_capability(&self) -> NotifyCap {
        NotifyCap::PollOnly
    }
}

/// A typed, insertable canonical row: the table and column identifiers are
/// **compile-time constants bound to the entity** — the identifier-binding half
/// of the type layer: table and column names come from the impls, never from
/// user input. Values bind as `?` parameters. Rows are
/// [`rift_core::Immutable`]: the only write paths are the insert methods below.
pub trait CanonicalRow: rift_core::Immutable {
    /// The canonical table this row belongs to (must be registered).
    const TABLE: &'static str;
    /// The column list, in [`CanonicalRow::values`] order.
    const COLUMNS: &'static [&'static str];
    /// The row's parameter values, in [`CanonicalRow::COLUMNS`] order.
    fn values(&self) -> Vec<Value>;
}

/// Refuse any table name that resolves in no registry, and any column that is
/// not on the resolved table's spec — identifiers never reach SQL unvalidated.
fn validate_identifiers(table: &str, columns: &[&str]) -> Result<()> {
    let Some(spec) = schema::any_registry_table(table) else {
        return Err(rift_core::RiftError::storage(format!(
            "unknown canonical table `{table}` (not in any registry)"
        )));
    };
    for col in columns {
        if !spec.columns.iter().any(|c| c.name == *col) {
            return Err(rift_core::RiftError::storage(format!(
                "`{col}` is not a column of canonical table `{table}`"
            )));
        }
    }
    Ok(())
}

/// One source of truth for the value-bearing INSERT SQL: builds `?` placeholders
/// and never interpolates a value (the `all_queries_are_parameterized` posture);
/// identifiers are pre-validated against the registries by the callers.
fn insert_sql(verb: &str, table: &str, columns: &[&str]) -> String {
    let placeholders = vec!["?"; columns.len()].join(", ");
    format!(
        "{verb} INTO {table} ({}) VALUES ({placeholders})",
        columns.join(", ")
    )
}

/// Name the table on a constraint refusal. The engine's CHECK message names
/// only the failing expression, and the insert is the one place that knows
/// which table it wrote, so every refusal names the table the same way.
fn refused_in(table: &str, error: rift_core::RiftError) -> rift_core::RiftError {
    match error {
        rift_core::RiftError::ConstraintRefused { constraint, .. } => {
            rift_core::RiftError::ConstraintRefused {
                table: table.to_owned(),
                constraint,
            }
        }
        other => other,
    }
}

#[async_trait]
impl<S: Store + ?Sized> CanonicalStore for S {
    async fn register_canonical_table(&self, table: &CanonicalTable) -> Result<()> {
        let conn = self.conn().await?;
        conn.execute_batch(&table.create_table_sql()).await?;
        conn.execute_batch(&table.guard_trigger_sql()).await?;
        Ok(())
    }

    async fn insert(&self, table: &str, columns: &[&str], values: &[Value]) -> Result<()> {
        validate_identifiers(table, columns)?;
        if columns.len() != values.len() {
            return Err(rift_core::RiftError::storage(format!(
                "insert into {table}: {} columns but {} values",
                columns.len(),
                values.len()
            )));
        }
        let sql = insert_sql("INSERT", table, columns);
        match self.conn().await?.execute(&sql, values).await {
            Ok(_) => Ok(()),
            Err(error) => Err(refused_in(table, error)),
        }
    }

    async fn insert_or_ignore(
        &self,
        table: &str,
        columns: &[&str],
        values: &[Value],
    ) -> Result<bool> {
        validate_identifiers(table, columns)?;
        if columns.len() != values.len() {
            return Err(rift_core::RiftError::storage(format!(
                "insert into {table}: {} columns but {} values",
                columns.len(),
                values.len()
            )));
        }
        // `ON CONFLICT DO NOTHING` with no target skips every PRIMARY KEY and
        // UNIQUE conflict, as `INSERT OR IGNORE` did, but — unlike `OR IGNORE`
        // — lets a CHECK, NOT NULL or datatype failure raise, so a refused row
        // can never read as an already-present one.
        let sql = format!(
            "{} ON CONFLICT DO NOTHING",
            insert_sql("INSERT", table, columns)
        );
        match self.conn().await?.execute(&sql, values).await {
            Ok(changed) => Ok(changed > 0),
            Err(error) => Err(refused_in(table, error)),
        }
    }

    async fn count(&self, table: &str) -> Result<i64> {
        validate_identifiers(table, &[])?;
        let sql = format!("SELECT COUNT(*) FROM {table}");
        let rows = self.conn().await?.query(&sql, &[]).await?;
        rows.first().map_or(Ok(0), |r| r.get_i64(0))
    }

    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        self.conn().await?.query(sql, params).await
    }

    async fn query_opt_i64(&self, sql: &str, params: &[Value]) -> Result<Option<i64>> {
        let rows = self.conn().await?.query(sql, params).await?;
        match rows.first() {
            Some(r) => Ok(Some(r.get_i64(0)?)),
            None => Ok(None),
        }
    }
}

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
//! Immutability is enforced in three layers (see
//! `build-plan/00-overview/04-database-design-4nf-5nf-immutability.md`):
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
// Style-only pedantic lints we deliberately accept (not correctness/security).
#![allow(clippy::module_name_repetitions, clippy::doc_markdown)]

pub mod capture;
pub mod crypto;
pub mod durable;
pub mod encrypted_dao;
pub mod integrity;
pub mod key_metadata;
pub mod keychain;
#[cfg(feature = "keyring")]
pub mod keychain_os;
pub mod migrate;
pub mod paths;
pub mod rebuild;
pub mod schema;
pub mod sensitive;
mod sqlite_store;
pub mod store;
// NOTE — Turso engine DEFERRED (P3 review): a native-async `TursoStore` implements
// the `store::Store` seam, but the `turso` dependency is intentionally not wired
// (it pulls advisory-bearing git2/paste, and turso 0.1.5 lacks CREATE TRIGGER /
// STRICT / encryption). The reference impl + rationale live at
// `plan_v1/build-plan/stage-01-foundations/phase-03-turso-storage-foundation/`
// (`turso_store.reference.rs` + `SPIKE-RESULTS.md`). Re-home it here and re-add
// the dep/feature only after re-review. The rusqlite `SqliteStore` is the shipping
// engine; any new engine plugs in behind `store::Store` without touching callers.
mod value;

pub use capture::{
    confidence_to_milli, record_agent_enablement, record_agent_hook_event, record_archive_decision,
    record_attribution, record_captured_event, record_changed_hunk, record_file_revision,
    record_price, record_session_transition, record_substrate_block_event,
    record_substrate_install, record_token_usage, record_verification_divergence,
    record_verification_run, AgentEnablementRow, AgentHookEventRow, ArchiveDecisionRow,
    AttributionRow, CapturedEventRow, ChangedHunkRow, FileRevisionRow, PriceTableRow,
    SessionTransitionRow, SubstrateBlockEventRow, SubstrateInstallRow, TokenUsageRow,
    VerificationDivergenceRow, VerificationRunRow,
};
pub use crypto::{decrypt_column, encrypt_column, KeyStore, MemoryKeyStore};
pub use durable::{DurableWrite, LocalOnlyShadowWrite, ObjectKind, SyncWrite};
pub use encrypted_dao::EncryptingStore;
pub use integrity::{
    cost_belief_as_of, current_cost_micros, record_checkpoint, record_cost_fact,
    verify_against_source, ChainSource, CheckpointRecord, CostFact, VerifyReport,
};
pub use key_metadata::{
    key_metadata_for_generation, latest_key_metadata, record_key_metadata, KeyMetadataRow,
};
pub use keychain::{KeychainBackend, KeyringKeyStore, MemoryKeychain};
#[cfg(feature = "keyring")]
pub use keychain_os::OsKeychain;
pub use migrate::{apply_migrations, default_migrations, Migration, MigrationOutcome};
pub use paths::{
    default_projection_db, default_projection_dir, projection_db_under, projection_dir_under,
    resolve_cache_root,
};
pub use rebuild::rebuild;
pub use schema::{canonical_table, canonical_tables, is_canonical_table, CanonicalTable, Column};
pub use sensitive::{is_sensitive_column, sensitive_columns, SensitiveColumn};
pub use sqlite_store::SqliteStore;
pub use store::{Row, Store, StoreConn, StoreDescriptor, TxMode};
pub use value::Value;

use async_trait::async_trait;
use rift_core::Result;

/// The append-only repository, layered over any [`Store`] engine.
///
/// Deliberately exposes no `update` or `delete`: the only mutating operations are
/// inserts. Corrections are modelled as new, superseding rows. A blanket impl
/// provides it for every [`Store`], so the rusqlite reference engine and the
/// Turso engine get the identical insert-only surface — the type-layer half of the
/// immutability guarantee (`04-database-design` §4.3). Read helpers are included so
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
    /// differ or the engine rejects the row (e.g. duplicate primary key), or
    /// [`rift_core::RiftError::Immutability`] if an append-only guard fires.
    async fn insert(&self, table: &str, columns: &[&str], values: &[Value]) -> Result<()>;

    /// Insert a row, **ignoring a duplicate-primary-key conflict** — the
    /// idempotent set-union used for content-addressed ingest (re-ingesting the
    /// same fact is a no-op, db-design §4.4/§4.7). Returns `true` if a new row was
    /// inserted, `false` if an identical key was already present.
    ///
    /// # Errors
    /// As [`CanonicalStore::insert`], except a duplicate PK is **not** an error.
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
}

/// One source of truth for the value-bearing INSERT SQL: builds `?` placeholders
/// and never interpolates a value (the `all_queries_are_parameterized` posture).
fn insert_sql(verb: &str, table: &str, columns: &[&str]) -> String {
    let placeholders = vec!["?"; columns.len()].join(", ");
    format!(
        "{verb} INTO {table} ({}) VALUES ({placeholders})",
        columns.join(", ")
    )
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
        if columns.len() != values.len() {
            return Err(rift_core::RiftError::storage(format!(
                "insert into {table}: {} columns but {} values",
                columns.len(),
                values.len()
            )));
        }
        let sql = insert_sql("INSERT", table, columns);
        self.conn().await?.execute(&sql, values).await?;
        Ok(())
    }

    async fn insert_or_ignore(
        &self,
        table: &str,
        columns: &[&str],
        values: &[Value],
    ) -> Result<bool> {
        if columns.len() != values.len() {
            return Err(rift_core::RiftError::storage(format!(
                "insert into {table}: {} columns but {} values",
                columns.len(),
                values.len()
            )));
        }
        let sql = insert_sql("INSERT OR IGNORE", table, columns);
        let changed = self.conn().await?.execute(&sql, values).await?;
        Ok(changed > 0)
    }

    async fn count(&self, table: &str) -> Result<i64> {
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

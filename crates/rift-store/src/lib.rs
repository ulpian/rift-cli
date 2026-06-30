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

pub mod crypto;
pub mod durable;
pub mod integrity;
pub mod keychain;
#[cfg(feature = "keyring")]
pub mod keychain_os;
pub mod migrate;
pub mod paths;
pub mod schema;
mod sqlite_store;
mod value;

pub use crypto::{decrypt_column, encrypt_column, KeyStore, MemoryKeyStore};
pub use durable::{DurableWrite, LocalOnlyShadowWrite, ObjectKind, SyncWrite};
pub use integrity::{
    cost_belief_as_of, current_cost_micros, record_checkpoint, record_cost_fact,
    verify_against_source, ChainSource, CheckpointRecord, CostFact, VerifyReport,
};
pub use keychain::{KeychainBackend, KeyringKeyStore, MemoryKeychain};
#[cfg(feature = "keyring")]
pub use keychain_os::OsKeychain;
pub use migrate::{apply_migrations, default_migrations, Migration, MigrationOutcome};
pub use paths::{
    default_projection_db, default_projection_dir, projection_db_under, projection_dir_under,
    resolve_cache_root,
};
pub use schema::{canonical_table, canonical_tables, is_canonical_table, CanonicalTable, Column};
pub use sqlite_store::SqliteStore;
pub use value::Value;

use rift_core::Result;

/// Append-only store abstraction.
///
/// Deliberately exposes no `update` or `delete`: the only mutating operation is
/// [`CanonicalStore::insert`]. Corrections are modelled as new, superseding rows.
pub trait CanonicalStore {
    /// Create a canonical table and its append-only guard triggers (idempotent).
    ///
    /// # Errors
    /// Returns [`rift_core::RiftError::Migration`] if the DDL fails.
    fn register_canonical_table(&self, table: &CanonicalTable) -> Result<()>;

    /// Insert a single row.
    ///
    /// # Errors
    /// Returns [`rift_core::RiftError::Storage`] if `columns`/`values` lengths
    /// differ or the engine rejects the row (e.g. duplicate primary key), or
    /// [`rift_core::RiftError::Immutability`] if an append-only guard fires.
    fn insert(&self, table: &str, columns: &[&str], values: &[Value]) -> Result<()>;

    /// Count rows in a table.
    ///
    /// # Errors
    /// Returns [`rift_core::RiftError::Storage`] if the query fails.
    fn count(&self, table: &str) -> Result<i64>;
}

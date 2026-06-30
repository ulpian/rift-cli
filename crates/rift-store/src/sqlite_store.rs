//! Synchronous SQLite reference implementation of the [`Store`] seam.
//!
//! Runs the exact DDL and triggers from [`crate::schema`]. Because SQLite and
//! Turso share SQL semantics, the append-only guarantees proven here transfer to
//! the production Turso store unchanged. The store also carries the
//! migration-engine and introspection primitives the [`crate::migrate`] module
//! builds on.
//!
//! The seam is async (so the async-native Turso engine fits), but this reference
//! impl wraps a single `rusqlite::Connection` behind a [`std::sync::Mutex`]: each
//! async method locks, runs synchronously (rusqlite is sync), and releases the
//! guard *before* returning — no guard is ever held across an `.await` (there are
//! none), so the futures are `Send` and the default build needs no async runtime.

use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use rift_core::{Result, RiftError};
use rusqlite::Connection;

use crate::schema::canonical_tables;
use crate::store::{Row, Store, StoreConn, StoreDescriptor};
use crate::value::Value;

/// A SQLite-backed append-only store (the always-available reference engine).
pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
    desc: StoreDescriptor,
}

impl SqliteStore {
    /// Open an in-memory store (tests, ephemeral tooling).
    ///
    /// # Errors
    /// Returns [`RiftError::Storage`] if the connection or pragmas fail.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(map_sql)?;
        Self::init(conn, ":memory:")
    }

    /// Open (or create) a store at `path`.
    ///
    /// # Errors
    /// Returns [`RiftError::Storage`] if the database cannot be opened.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let p = path.as_ref();
        let display = p.display().to_string();
        let conn = Connection::open(p).map_err(map_sql)?;
        Self::init(conn, display)
    }

    fn init(conn: Connection, path: impl Into<String>) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)
            .map_err(map_sql)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            desc: StoreDescriptor::new(path),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| RiftError::storage("sqlite connection mutex poisoned"))
    }

    /// Run `f` with the locked connection (test-only sync read path used by the
    /// migration tests; canonical *writes* still go through the append-only DAO,
    /// and the triggers guard against mutation regardless).
    ///
    /// # Errors
    /// [`RiftError::Storage`] mapping any rusqlite error.
    #[cfg(test)]
    pub(crate) fn with_conn<R>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<R>,
    ) -> Result<R> {
        let conn = self.lock()?;
        f(&conn).map_err(map_sql)
    }

    /// Create every canonical table from the registry plus its append-only guard
    /// triggers (idempotent). The simple registry path; production uses
    /// [`crate::migrate::apply_migrations`], which produces the same tables.
    ///
    /// # Errors
    /// Returns [`RiftError::Migration`] if any DDL fails.
    pub fn create_canonical_schema(&self) -> Result<()> {
        let conn = self.lock()?;
        for table in canonical_tables() {
            conn.execute_batch(&table.create_table_sql())
                .map_err(map_migration)?;
            conn.execute_batch(&table.guard_trigger_sql())
                .map_err(map_migration)?;
        }
        Ok(())
    }

    /// Names of every user table in the projection (`type='table'`), excluding
    /// SQLite's internal `sqlite_*` tables.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the query fails.
    pub fn table_names(&self) -> Result<Vec<String>> {
        self.master_names("table")
    }

    /// Names of every trigger in the projection (`type='trigger'`).
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the query fails.
    pub fn trigger_names(&self) -> Result<Vec<String>> {
        self.master_names("trigger")
    }

    fn master_names(&self, kind: &str) -> Result<Vec<String>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type = ? AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .map_err(map_sql)?;
        let rows = stmt
            .query_map([kind], |row| row.get::<_, String>(0))
            .map_err(map_sql)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_sql)?);
        }
        Ok(out)
    }

    /// Create the migration meta tables (idempotent).
    ///
    /// # Errors
    /// [`RiftError::Migration`] if the DDL fails.
    pub fn ensure_meta(&self) -> Result<()> {
        let conn = self.lock()?;
        conn.execute_batch(crate::migrate::META_SQL)
            .map_err(map_migration)
    }

    /// Read applied migrations as `(version, checksum_hex)`.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the query fails.
    pub(crate) fn applied_migrations(&self) -> Result<Vec<(u32, String)>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT version, checksum FROM _migrations ORDER BY version")
            .map_err(map_sql)?;
        let rows = stmt
            .query_map([], |row| {
                let v: i64 = row.get(0)?;
                let c: String = row.get(1)?;
                Ok((u32::try_from(v).unwrap_or(0), c))
            })
            .map_err(map_sql)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_sql)?);
        }
        Ok(out)
    }

    /// Apply one migration's DDL and record it atomically (DDL + `_migrations`
    /// row commit together; a failure rolls back leaving no orphan schema).
    ///
    /// # Errors
    /// [`RiftError::Migration`] if the DDL or record fails.
    pub(crate) fn apply_migration(
        &self,
        sql: &str,
        version: u32,
        name: &str,
        checksum: &str,
        applied_at: &str,
    ) -> Result<()> {
        let conn = self.lock()?;
        let tx = conn.unchecked_transaction().map_err(map_migration)?;
        tx.execute_batch(sql).map_err(map_migration)?;
        tx.execute(
            "INSERT INTO _migrations(version, name, checksum, applied_at) VALUES (?, ?, ?, ?)",
            rusqlite::params![i64::from(version), name, checksum, applied_at],
        )
        .map_err(map_migration)?;
        tx.commit().map_err(map_migration)?;
        Ok(())
    }

    /// Diagnostic / test-only raw execution. Deliberately **not** part of the
    /// append-only contract: it exists so tests can prove the database triggers
    /// reject mutation even when SQL bypasses the DAO.
    #[cfg(test)]
    pub(crate) fn execute_raw(&self, sql: &str) -> Result<usize> {
        let conn = self.lock()?;
        conn.execute(sql, ()).map_err(map_sql)
    }
}

/// A connection handle sharing the store's single rusqlite connection (serialized
/// by the mutex). For the reference engine, "per-call connection" is a cheap clone
/// of the `Arc`.
struct SqliteConn {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteConn {
    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| RiftError::storage("sqlite connection mutex poisoned"))
    }
}

#[async_trait]
impl Store for SqliteStore {
    async fn conn(&self) -> Result<Box<dyn StoreConn>> {
        Ok(Box::new(SqliteConn {
            conn: Arc::clone(&self.conn),
        }))
    }

    fn descriptor(&self) -> &StoreDescriptor {
        &self.desc
    }
}

#[async_trait]
impl StoreConn for SqliteConn {
    async fn execute(&self, sql: &str, params: &[Value]) -> Result<u64> {
        let conn = self.lock()?;
        let bound: Vec<rusqlite::types::Value> = params.iter().map(Into::into).collect();
        let n = conn
            .execute(sql, rusqlite::params_from_iter(bound))
            .map_err(map_sql)?;
        Ok(n as u64)
    }

    async fn execute_batch(&self, sql: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute_batch(sql).map_err(map_migration)
    }

    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        let conn = self.lock()?;
        let bound: Vec<rusqlite::types::Value> = params.iter().map(Into::into).collect();
        let mut stmt = conn.prepare(sql).map_err(map_sql)?;
        let col_count = stmt.column_count();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(bound), |row| {
                let mut cols = Vec::with_capacity(col_count);
                for i in 0..col_count {
                    cols.push(from_rusqlite(row.get::<_, rusqlite::types::Value>(i)?));
                }
                Ok(Row::new(cols))
            })
            .map_err(map_sql)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_sql)?);
        }
        Ok(out)
    }
}

/// Convert a rusqlite value into the engine-neutral [`Value`]. `REAL` never
/// occurs (floats never reach the store, `04 §4.2`); it is rendered as text
/// defensively rather than silently truncated.
fn from_rusqlite(v: rusqlite::types::Value) -> Value {
    match v {
        rusqlite::types::Value::Null => Value::Null,
        rusqlite::types::Value::Integer(i) => Value::Int(i),
        rusqlite::types::Value::Text(s) => Value::Text(s),
        rusqlite::types::Value::Blob(b) => Value::Blob(b),
        rusqlite::types::Value::Real(r) => Value::Text(r.to_string()),
    }
}

/// Map a rusqlite error into [`RiftError`], classifying append-only trigger
/// aborts as [`RiftError::Immutability`] so callers can distinguish them.
fn map_sql(error: rusqlite::Error) -> RiftError {
    let msg = error.to_string();
    if msg.contains("append-only") {
        RiftError::Immutability(msg)
    } else {
        RiftError::Storage(msg)
    }
}

fn map_migration(error: rusqlite::Error) -> RiftError {
    let msg = error.to_string();
    if msg.contains("append-only") {
        RiftError::Immutability(msg)
    } else {
        RiftError::Migration(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{canonical_tables, CanonicalTable};
    use crate::CanonicalStore;
    use proptest::prelude::*;

    /// Drive an async seam call to completion from a synchronous test body.
    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime")
            .block_on(f)
    }

    /// Build a valid row for a table from its column types.
    fn sample_values(table: &CanonicalTable) -> Vec<Value> {
        table
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| match c.sql_type {
                "INTEGER" => Value::Int(i64::try_from(i).unwrap()),
                "BLOB" => Value::Blob(vec![1, 2, 3]),
                _ => Value::text(format!("{}-{i}", c.name)),
            })
            .collect()
    }

    fn migrated_store() -> SqliteStore {
        let store = SqliteStore::open_in_memory().unwrap();
        store.create_canonical_schema().unwrap();
        store
    }

    /// The core invariant: EVERY canonical table rejects both UPDATE and DELETE,
    /// the row survives, and only INSERT (append) is possible.
    #[test]
    fn every_canonical_table_rejects_update_and_delete() {
        let store = migrated_store();
        for table in canonical_tables() {
            let cols: Vec<&str> = table.columns.iter().map(|c| c.name).collect();
            block(store.insert(table.name, &cols, &sample_values(table))).unwrap();
            assert_eq!(block(store.count(table.name)).unwrap(), 1);

            let first = table.columns.first().expect("table has columns").name;
            let upd = store.execute_raw(&format!("UPDATE {} SET {first} = {first}", table.name));
            assert!(
                matches!(upd, Err(RiftError::Immutability(_))),
                "UPDATE on {} should be rejected, got {upd:?}",
                table.name
            );

            let del = store.execute_raw(&format!("DELETE FROM {}", table.name));
            assert!(
                matches!(del, Err(RiftError::Immutability(_))),
                "DELETE on {} should be rejected, got {del:?}",
                table.name
            );

            assert_eq!(block(store.count(table.name)).unwrap(), 1);
        }
    }

    #[test]
    fn insert_with_mismatched_column_value_counts_is_rejected() {
        let store = migrated_store();
        let err = block(store.insert("session", &["session_id", "agent"], &[Value::text("s")]));
        assert!(
            matches!(err, Err(RiftError::Storage(_))),
            "length mismatch should be a Storage error, got {err:?}"
        );
    }

    #[test]
    fn introspection_lists_user_tables_and_triggers() {
        let store = migrated_store();
        let tables = store.table_names().unwrap();
        assert!(tables.iter().any(|t| t == "checkpoint"));
        assert!(!tables.iter().any(|t| t.starts_with("sqlite_")));
        let triggers = store.trigger_names().unwrap();
        assert!(triggers.iter().any(|t| t == "checkpoint_no_update"));
    }

    /// Appending is allowed; a "correction" is a new, superseding row.
    #[test]
    fn append_and_supersede_are_allowed() {
        let store = migrated_store();
        let cols = [
            "checkpoint_id",
            "strategy",
            "created_at",
            "merkle_root",
            "prev_hash",
            "schema_version",
        ];
        block(store.insert(
            "checkpoint",
            &cols,
            &[
                Value::text("ckpt_1"),
                Value::text("manual-commit"),
                Value::Int(1),
                Value::text("root_a"),
                Value::Null,
                Value::Int(1),
            ],
        ))
        .unwrap();
        block(store.insert(
            "checkpoint",
            &cols,
            &[
                Value::text("ckpt_2"),
                Value::text("manual-commit"),
                Value::Int(2),
                Value::text("root_b"),
                Value::text("ckpt_1"),
                Value::Int(1),
            ],
        ))
        .unwrap();
        assert_eq!(block(store.count("checkpoint")).unwrap(), 2);
    }

    /// Re-inserting the same primary key is rejected (uniqueness holds; real
    /// ingest uses content-addressed keys for idempotent set-union).
    #[test]
    fn duplicate_primary_key_is_rejected() {
        let store = migrated_store();
        let cols = [
            "checkpoint_id",
            "strategy",
            "created_at",
            "merkle_root",
            "prev_hash",
            "schema_version",
        ];
        let row = [
            Value::text("ckpt_1"),
            Value::text("s"),
            Value::Int(1),
            Value::text("r"),
            Value::Null,
            Value::Int(1),
        ];
        block(store.insert("checkpoint", &cols, &row)).unwrap();
        assert!(block(store.insert("checkpoint", &cols, &row)).is_err());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(96))]

        /// Property: for any canonical table and any mutation kind, the guard
        /// rejects it and the row is preserved.
        #[test]
        fn no_canonical_table_can_be_mutated(
            table_idx in 0usize..canonical_tables().len(),
            as_update in any::<bool>(),
        ) {
            let tables = canonical_tables();
            let table = *tables.get(table_idx).expect("index within range");
            let store = migrated_store();
            let cols: Vec<&str> = table.columns.iter().map(|c| c.name).collect();
            block(store.insert(table.name, &cols, &sample_values(&table))).unwrap();

            let stmt = if as_update {
                let first = table.columns.first().expect("table has columns").name;
                format!("UPDATE {} SET {first} = {first}", table.name)
            } else {
                format!("DELETE FROM {}", table.name)
            };
            let result = store.execute_raw(&stmt);
            prop_assert!(matches!(result, Err(RiftError::Immutability(_))));
            prop_assert_eq!(block(store.count(table.name)).unwrap(), 1);
        }
    }
}

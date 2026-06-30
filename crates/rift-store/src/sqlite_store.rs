//! Synchronous, SQLite-backed reference implementation of [`CanonicalStore`].
//!
//! Runs the exact DDL and triggers from [`crate::schema`]. Because SQLite and
//! Turso share SQL semantics, the append-only guarantees proven here transfer to
//! the production Turso store unchanged. The store also carries the
//! migration-engine and integrity-read primitives the [`crate::migrate`] and
//! [`crate::integrity`] modules build on.

use rift_core::{Result, RiftError};
use rusqlite::Connection;

use crate::schema::{canonical_tables, CanonicalTable};
use crate::value::Value;
use crate::CanonicalStore;

/// A SQLite-backed append-only store.
pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    /// Open an in-memory store (tests, ephemeral tooling).
    ///
    /// # Errors
    /// Returns [`RiftError::Storage`] if the connection or pragmas fail.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(map_sql)?;
        Self::init(conn)
    }

    /// Open (or create) a store at `path`.
    ///
    /// # Errors
    /// Returns [`RiftError::Storage`] if the database cannot be opened.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(map_sql)?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)
            .map_err(map_sql)?;
        Ok(Self { conn })
    }

    /// Borrow the underlying connection (crate-internal; for migrations and
    /// read-side integrity/cost queries — canonical *writes* still go through
    /// [`CanonicalStore::insert`], and the triggers guard against mutation
    /// regardless of path).
    #[allow(dead_code)] // used by migration tests + future read-side modules (Stage 3)
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Create every canonical table from the registry plus its append-only guard
    /// triggers (idempotent). The simple registry path; production uses
    /// [`crate::migrate::apply_migrations`], which produces the same tables.
    ///
    /// # Errors
    /// Returns [`RiftError::Migration`] if any DDL fails.
    pub fn create_canonical_schema(&self) -> Result<()> {
        for table in canonical_tables() {
            self.register_canonical_table(table)?;
        }
        Ok(())
    }

    /// Names of every user table in the projection (`type='table'`), excluding
    /// SQLite's internal `sqlite_*` tables. Read-only introspection used by the
    /// immutability keystone test and projection doctor.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the query fails.
    pub fn table_names(&self) -> Result<Vec<String>> {
        self.master_names("table")
    }

    /// Names of every trigger in the projection (`type='trigger'`). Read-only.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the query fails.
    pub fn trigger_names(&self) -> Result<Vec<String>> {
        self.master_names("trigger")
    }

    /// `sqlite_master` names of a given object `kind`, excluding `sqlite_*`.
    fn master_names(&self, kind: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
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
        self.conn
            .execute_batch(crate::migrate::META_SQL)
            .map_err(map_migration)
    }

    /// Read applied migrations as `(version, checksum_hex)`.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the query fails.
    pub(crate) fn applied_migrations(&self) -> Result<Vec<(u32, String)>> {
        let mut stmt = self
            .conn
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
        let tx = self.conn.unchecked_transaction().map_err(map_migration)?;
        tx.execute_batch(sql).map_err(map_migration)?;
        tx.execute(
            "INSERT INTO _migrations(version, name, checksum, applied_at) VALUES (?, ?, ?, ?)",
            rusqlite::params![i64::from(version), name, checksum, applied_at],
        )
        .map_err(map_migration)?;
        tx.commit().map_err(map_migration)?;
        Ok(())
    }

    /// Read the stored checkpoint chain as `(checkpoint_id, merkle_root,
    /// prev_hash)` ordered by `created_at`.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the query fails.
    pub(crate) fn stored_checkpoint_links(&self) -> Result<Vec<(String, String, Option<String>)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT checkpoint_id, merkle_root, prev_hash FROM checkpoint \
                 ORDER BY created_at, checkpoint_id",
            )
            .map_err(map_sql)?;
        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let root: String = row.get(1)?;
                let prev: Option<String> = row.get(2)?;
                Ok((id, root, prev))
            })
            .map_err(map_sql)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(map_sql)?);
        }
        Ok(out)
    }

    /// Query a single optional `i64` (e.g. "current belief" cost).
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the query fails.
    pub(crate) fn query_opt_i64(&self, sql: &str, params: &[Value]) -> Result<Option<i64>> {
        let bound: Vec<rusqlite::types::Value> = params.iter().map(Into::into).collect();
        let mut stmt = self.conn.prepare(sql).map_err(map_sql)?;
        let mut rows = stmt
            .query(rusqlite::params_from_iter(bound))
            .map_err(map_sql)?;
        match rows.next().map_err(map_sql)? {
            Some(row) => Ok(Some(row.get(0).map_err(map_sql)?)),
            None => Ok(None),
        }
    }

    /// Diagnostic / test-only raw execution. Deliberately **not** part of the
    /// public append-only contract: it exists so tests can prove the database
    /// triggers reject mutation even when SQL bypasses [`CanonicalStore::insert`].
    #[cfg(test)]
    pub(crate) fn execute_raw(&self, sql: &str) -> Result<usize> {
        self.conn.execute(sql, ()).map_err(map_sql)
    }
}

impl CanonicalStore for SqliteStore {
    fn register_canonical_table(&self, table: &CanonicalTable) -> Result<()> {
        self.conn
            .execute_batch(&table.create_table_sql())
            .map_err(map_migration)?;
        self.conn
            .execute_batch(&table.guard_trigger_sql())
            .map_err(map_migration)?;
        Ok(())
    }

    fn insert(&self, table: &str, columns: &[&str], values: &[Value]) -> Result<()> {
        if columns.len() != values.len() {
            return Err(RiftError::storage(format!(
                "insert into {table}: {} columns but {} values",
                columns.len(),
                values.len()
            )));
        }
        let placeholders = vec!["?"; columns.len()].join(", ");
        let sql = format!(
            "INSERT INTO {table} ({}) VALUES ({placeholders})",
            columns.join(", ")
        );
        let bound: Vec<rusqlite::types::Value> = values.iter().map(Into::into).collect();
        self.conn
            .execute(&sql, rusqlite::params_from_iter(bound))
            .map_err(map_sql)?;
        Ok(())
    }

    fn count(&self, table: &str) -> Result<i64> {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        self.conn
            .query_row(&sql, (), |row| row.get(0))
            .map_err(map_sql)
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
    use proptest::prelude::*;

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
            store
                .insert(table.name, &cols, &sample_values(table))
                .unwrap();
            assert_eq!(store.count(table.name).unwrap(), 1);

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

            assert_eq!(store.count(table.name).unwrap(), 1);
        }
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
        store
            .insert(
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
            )
            .unwrap();
        store
            .insert(
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
            )
            .unwrap();
        assert_eq!(store.count("checkpoint").unwrap(), 2);
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
        store.insert("checkpoint", &cols, &row).unwrap();
        assert!(store.insert("checkpoint", &cols, &row).is_err());
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
            store.insert(table.name, &cols, &sample_values(&table)).unwrap();

            let stmt = if as_update {
                let first = table.columns.first().expect("table has columns").name;
                format!("UPDATE {} SET {first} = {first}", table.name)
            } else {
                format!("DELETE FROM {}", table.name)
            };
            let result = store.execute_raw(&stmt);
            prop_assert!(matches!(result, Err(RiftError::Immutability(_))));
            prop_assert_eq!(store.count(table.name).unwrap(), 1);
        }
    }
}

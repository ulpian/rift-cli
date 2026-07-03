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
    /// triggers (idempotent). **Test/tooling fast-path only**: it bypasses the
    /// checksum-locked `_migrations` ledger and `_schema_version` tracking, so
    /// production code must open projections via
    /// [`crate::migrate::open_migrated`] / [`crate::migrate::open_capture_projection`]
    /// instead (the `ci-meta` gate greps `bins/` for violations).
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

/// Statement execution against a locked connection (shared by the connection
/// and transaction handles).
fn exec_on(conn: &Connection, sql: &str, params: &[Value]) -> Result<u64> {
    let bound: Vec<rusqlite::types::Value> = params.iter().map(Into::into).collect();
    let n = conn
        .execute(sql, rusqlite::params_from_iter(bound))
        .map_err(map_sql)?;
    Ok(n as u64)
}

/// Query execution against a locked connection (shared by the connection and
/// transaction handles).
fn query_on(conn: &Connection, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
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

#[async_trait]
impl StoreConn for SqliteConn {
    async fn execute(&self, sql: &str, params: &[Value]) -> Result<u64> {
        let conn = self.lock()?;
        exec_on(&conn, sql, params)
    }

    async fn execute_batch(&self, sql: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute_batch(sql).map_err(map_migration)
    }

    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        let conn = self.lock()?;
        query_on(&conn, sql, params)
    }

    async fn begin(&self, _mode: crate::TxMode) -> Result<Box<dyn crate::StoreTx + '_>> {
        // The reference engine treats `Concurrent` as `Deferred` (single
        // mutex-serialized connection; MVCC is a native-engine capability).
        self.lock()?
            .execute_batch("BEGIN DEFERRED")
            .map_err(map_sql)?;
        Ok(Box::new(SqliteTx {
            conn: Arc::clone(&self.conn),
            finished: false,
        }))
    }
}

/// A transaction over the shared reference-engine connection. Finishing runs
/// `COMMIT`/`ROLLBACK`; dropping unfinished rolls back best-effort so a failed
/// migration never leaves the connection stuck inside a transaction.
struct SqliteTx {
    conn: Arc<Mutex<Connection>>,
    finished: bool,
}

impl SqliteTx {
    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| RiftError::storage("sqlite connection mutex poisoned"))
    }

    fn finish(&mut self, sql: &str) -> Result<()> {
        let out = self.lock()?.execute_batch(sql).map_err(map_sql);
        self.finished = true;
        out
    }
}

#[async_trait]
impl crate::StoreTx for SqliteTx {
    async fn execute(&self, sql: &str, params: &[Value]) -> Result<u64> {
        let conn = self.lock()?;
        exec_on(&conn, sql, params)
    }

    async fn execute_batch(&self, sql: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute_batch(sql).map_err(map_migration)
    }

    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        let conn = self.lock()?;
        query_on(&conn, sql, params)
    }

    async fn commit(mut self: Box<Self>) -> Result<()> {
        self.finish("COMMIT")
    }

    async fn rollback(mut self: Box<Self>) -> Result<()> {
        self.finish("ROLLBACK")
    }
}

impl Drop for SqliteTx {
    fn drop(&mut self) {
        if !self.finished {
            if let Ok(conn) = self.conn.lock() {
                let _ = conn.execute_batch("ROLLBACK");
            }
        }
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

/// Classify a rusqlite error into a **stable, content-free category** (step-01
/// error mapping): the produced message names an error class + numeric code,
/// never the engine's message text — which is the only channel that could ever
/// echo row/blob bytes. The one exception is the append-only trigger abort,
/// whose message is *our own compile-time constant* (`rift: <table> is
/// append-only …`), carried verbatim so callers see which table refused.
fn classify(error: &rusqlite::Error) -> (bool, String) {
    use rusqlite::ffi::ErrorCode;
    if let rusqlite::Error::SqliteFailure(e, msg) = error {
        if let Some(m) = msg.as_deref() {
            if m.contains("append-only") {
                return (true, m.to_string());
            }
        }
        let category = match e.code {
            ErrorCode::ConstraintViolation => "constraint-violation",
            ErrorCode::ReadOnly => "read-only",
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => "busy",
            ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => "corrupt",
            ErrorCode::DiskFull => "disk-full",
            ErrorCode::CannotOpen => "cannot-open",
            ErrorCode::PermissionDenied => "permission-denied",
            _ => "engine-error",
        };
        return (
            false,
            format!("sqlite {category} (code {})", e.extended_code),
        );
    }
    // Non-engine (API-layer) failures: carry the variant class only.
    let class = match error {
        rusqlite::Error::QueryReturnedNoRows => "query-returned-no-rows",
        rusqlite::Error::InvalidColumnIndex(_) | rusqlite::Error::InvalidColumnType(..) => {
            "column-mismatch"
        }
        rusqlite::Error::InvalidParameterCount(..) => "parameter-count-mismatch",
        _ => "api-error",
    };
    (false, format!("sqlite {class}"))
}

/// Map a rusqlite error into [`RiftError`], classifying append-only trigger
/// aborts as [`RiftError::Immutability`] so callers can distinguish them.
fn map_sql(error: rusqlite::Error) -> RiftError {
    match classify(&error) {
        (true, msg) => RiftError::Immutability(msg),
        (false, msg) => RiftError::Storage(msg),
    }
}

fn map_migration(error: rusqlite::Error) -> RiftError {
    match classify(&error) {
        (true, msg) => RiftError::Immutability(msg),
        (false, msg) => RiftError::Migration(msg),
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

    /// Build a valid row for a table from its column types. A `one_of` column
    /// (a `CHECK (… IN (…))` enum, e.g. a recall graph `kind`) must use a member
    /// of the set or the insert would violate the constraint.
    fn sample_values(table: &CanonicalTable) -> Vec<Value> {
        table
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if let Some(first) = c.one_of.first() {
                    Value::text((*first).to_string())
                } else {
                    match c.sql_type {
                        "INTEGER" => Value::Int(i64::try_from(i).unwrap()),
                        "BLOB" => Value::Blob(vec![1, 2, 3]),
                        _ => Value::text(format!("{}-{i}", c.name)),
                    }
                }
            })
            .collect()
    }

    fn migrated_store() -> SqliteStore {
        let store = SqliteStore::open_in_memory().unwrap();
        store.create_canonical_schema().unwrap();
        store
    }

    /// A store migrated with the Stage 3 recall (`recall.db`) schema.
    fn migrated_recall_store() -> SqliteStore {
        let store = SqliteStore::open_in_memory().unwrap();
        crate::migrate::apply_migrations(&store, &crate::migrate::recall_migrations()).unwrap();
        store
    }

    /// A strategy over every [`Value`] variant (REAL never reaches the store;
    /// text is NUL-free as SQLite's C layer terminates on NUL).
    fn any_value() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Null),
            proptest::num::i64::ANY.prop_map(Value::Int),
            "[^\x00]{0,64}".prop_map(Value::Text),
            proptest::collection::vec(proptest::num::u8::ANY, 0..64).prop_map(Value::Blob),
        ]
    }

    proptest! {
        /// The step-01 value-adapter contract: ANY value binds as a `?`
        /// parameter and reads back identical through the seam — the adapter
        /// (Value → rusqlite → Value) is lossless for the store's value domain.
        #[test]
        fn param_roundtrip_any_value(v in any_value()) {
            let store = SqliteStore::open_in_memory().unwrap();
            block(async {
                let conn = Store::conn(&store).await.unwrap();
                // STRICT: a bare `ANY` column takes NUMERIC affinity and would
                // coerce Text("0") → Int(0); STRICT `ANY` stores values as-is
                // (which is why every canonical table is STRICT).
                conn.execute_batch(
                    "CREATE TABLE IF NOT EXISTS _param_probe \
                     (id INTEGER PRIMARY KEY, v ANY) STRICT;",
                )
                .await
                .unwrap();
                conn.execute(
                    "INSERT INTO _param_probe (id, v) VALUES (1, ?)",
                    std::slice::from_ref(&v),
                )
                .await
                .unwrap();
                let rows = conn
                    .query("SELECT v FROM _param_probe WHERE id = 1", &[])
                    .await
                    .unwrap();
                prop_assert_eq!(rows[0].len(), 1);
                let got = match &v {
                    Value::Null => rows[0].get_opt_text(0).unwrap().map_or(Value::Null, Value::Text),
                    Value::Int(_) => Value::Int(rows[0].get_i64(0).unwrap()),
                    Value::Text(_) => Value::Text(rows[0].get_text(0).unwrap()),
                    Value::Blob(_) => Value::Blob(rows[0].get_blob(0).unwrap()),
                };
                prop_assert_eq!(&got, &v);
                Ok(())
            })?;
        }
    }

    /// The step-01 error-mapping contract: an engine failure surfaces as a
    /// stable category — never the engine's message text, which could echo
    /// row/blob bytes. The failing insert here carries a sentinel value that
    /// must not appear in the error's Display/Debug output.
    #[test]
    fn storage_error_contains_no_row_bytes() {
        let store = migrated_store();
        let sentinel = "ROW-BYTES-SENTINEL-4dc9";
        let cols = [
            "session_id",
            "agent",
            "model_primary",
            "repo_id",
            "started_at",
            "schema_version",
        ];
        let row = |id: &str| {
            [
                Value::text(id),
                Value::text(sentinel),
                Value::text("m"),
                Value::text("r"),
                Value::Int(1),
                Value::Int(1),
            ]
        };
        block(store.insert("session", &cols, &row(sentinel))).unwrap();
        // Duplicate PK (the sentinel is the conflicting key) → constraint error.
        let err = block(store.insert("session", &cols, &row(sentinel))).unwrap_err();
        let rendered = format!("{err} / {err:?}");
        assert!(
            !rendered.contains(sentinel),
            "engine error must not echo row bytes: {rendered}"
        );
        assert!(
            rendered.contains("constraint-violation"),
            "expected a stable category, got: {rendered}"
        );
    }

    /// The seam transaction contract: statements inside an uncommitted or
    /// rolled-back transaction leave no trace; committed ones persist.
    #[test]
    fn seam_transactions_commit_and_roll_back() {
        let store = migrated_store();
        let vals = |id: &str| {
            [
                Value::text(id),
                Value::text("a"),
                Value::text("m"),
                Value::text("r"),
                Value::Int(1),
                Value::Int(1),
            ]
        };
        let sql = "INSERT INTO session (session_id, agent, model_primary, repo_id, \
                   started_at, schema_version) VALUES (?, ?, ?, ?, ?, ?)";
        block(async {
            let conn = Store::conn(&store).await.unwrap();

            // rollback discards
            let tx = conn.begin(crate::TxMode::Deferred).await.unwrap();
            tx.execute(sql, &vals("s-rollback")).await.unwrap();
            tx.rollback().await.unwrap();

            // drop-without-finish discards (best-effort auto-rollback)
            let tx = conn.begin(crate::TxMode::Deferred).await.unwrap();
            tx.execute(sql, &vals("s-dropped")).await.unwrap();
            drop(tx);

            // commit persists
            let tx = conn.begin(crate::TxMode::Concurrent).await.unwrap();
            tx.execute(sql, &vals("s-committed")).await.unwrap();
            tx.commit().await.unwrap();
        });
        assert_eq!(block(store.count("session")).unwrap(), 1);
        let rows = block(store.query("SELECT session_id FROM session", &[])).unwrap();
        assert_eq!(rows[0].get_text(0).unwrap(), "s-committed");
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

    /// The change-observer seam: `read_watermark` is a monotone-under-append
    /// cursor, and the reference engine reports it must be polled.
    #[test]
    fn read_watermark_tracks_appends_and_capability_is_poll_only() {
        let store = migrated_store();
        assert_eq!(store.notify_capability(), crate::NotifyCap::PollOnly);
        assert_eq!(block(store.read_watermark("checkpoint")).unwrap(), 0);
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
                Value::text("c1"),
                Value::text("s"),
                Value::Int(1),
                Value::text("r"),
                Value::Null,
                Value::Int(1),
            ],
        ))
        .unwrap();
        assert_eq!(block(store.read_watermark("checkpoint")).unwrap(), 1);
        block(store.insert(
            "checkpoint",
            &cols,
            &[
                Value::text("c2"),
                Value::text("s"),
                Value::Int(2),
                Value::text("r2"),
                Value::text("c1"),
                Value::Int(1),
            ],
        ))
        .unwrap();
        // Appends advance the cursor monotonically (the ingest signal).
        assert_eq!(block(store.read_watermark("checkpoint")).unwrap(), 2);
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

    /// The Stage 3 recall tables are append-only too: every one rejects UPDATE
    /// and DELETE. This is how the "every Stage 3 table is immutable" exit
    /// criterion is met — by registering them in the recall registry.
    #[test]
    fn every_recall_table_rejects_update_and_delete() {
        let store = migrated_recall_store();
        for table in crate::schema::recall_canonical_tables() {
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

    /// The recall migration creates every registered recall table.
    #[test]
    fn recall_migration_creates_all_v1_tables() {
        let store = migrated_recall_store();
        let tables = store.table_names().unwrap();
        for t in crate::schema::recall_canonical_tables() {
            assert!(tables.iter().any(|n| n == t.name), "missing {}", t.name);
        }
        // a representative secondary index exists
        let idx = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='index' AND name='idx_edge_from'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
            })
            .unwrap();
        assert_eq!(idx, 1, "idx_edge_from should be created");
    }

    /// The `CHECK (kind IN …)` on `graph_edge` refuses a semantic edge kind in
    /// v1 — `similar_to`/`touches`/`frame` cannot be inserted until a Phase-7
    /// migration widens the set. A valid mechanical kind inserts fine.
    #[test]
    fn edge_kind_check_rejects_semantic_kind_in_v1() {
        let store = migrated_recall_store();
        let cols = ["edge_id", "from_node", "to_node", "kind", "created_at"];
        // a valid mechanical kind is accepted
        block(store.insert(
            "graph_edge",
            &cols,
            &[
                Value::text("e1"),
                Value::text("a"),
                Value::text("b"),
                Value::text("read"),
                Value::Int(1),
            ],
        ))
        .unwrap();
        // a semantic kind is rejected by the CHECK
        let bad = block(store.insert(
            "graph_edge",
            &cols,
            &[
                Value::text("e2"),
                Value::text("a"),
                Value::text("b"),
                Value::text("similar_to"),
                Value::Int(1),
            ],
        ));
        assert!(bad.is_err(), "similar_to must be rejected, got {bad:?}");
        assert_eq!(block(store.count("graph_edge")).unwrap(), 1);
    }

    /// Every user table in a migrated `recall.db` is either a registered recall
    /// table or a migration-meta (infra) table — the forgotten-trigger guard.
    #[test]
    fn every_recall_db_table_is_registered_or_infra() {
        let store = migrated_recall_store();
        let infra = ["_migrations", "_schema_version"];
        for name in store.table_names().unwrap() {
            let ok = crate::schema::recall_canonical_table(&name).is_some()
                || infra.contains(&name.as_str());
            assert!(ok, "unregistered recall table: {name}");
        }
    }

    /// A store migrated with the Stage 4 canopy UI-state (`ui-state.db`) schema.
    fn migrated_canopy_store() -> SqliteStore {
        let store = SqliteStore::open_in_memory().unwrap();
        crate::migrate::apply_migrations(&store, &crate::migrate::canopy_migrations()).unwrap();
        store
    }

    /// The canopy UI-state tables are append-only too: every one rejects UPDATE
    /// and DELETE (the "canopy_* tables are immutable" exit criterion, met by
    /// registering them in the canopy registry).
    #[test]
    fn every_canopy_table_rejects_update_and_delete() {
        let store = migrated_canopy_store();
        for table in crate::schema::canopy_canonical_tables() {
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

    /// Every user table in a migrated `ui-state.db` is a registered canopy table
    /// or a migration-meta (infra) table — the forgotten-trigger guard for canopy.
    #[test]
    fn every_canopy_db_table_is_registered_or_infra() {
        let store = migrated_canopy_store();
        let infra = ["_migrations", "_schema_version"];
        for name in store.table_names().unwrap() {
            let ok = crate::schema::canopy_canonical_table(&name).is_some()
                || infra.contains(&name.as_str());
            assert!(ok, "unregistered canopy table: {name}");
        }
    }

    /// The canopy migration creates exactly the registered canopy tables, and a
    /// superseding correction is a new row (append-only history preserved).
    #[test]
    fn canopy_project_event_appends_and_supersedes() {
        let store = migrated_canopy_store();
        let tables = store.table_names().unwrap();
        for t in crate::schema::canopy_canonical_tables() {
            assert!(tables.iter().any(|n| n == t.name), "missing {}", t.name);
        }
        let cols = [
            "event_id",
            "project_id",
            "op",
            "repo_id",
            "path",
            "label",
            "order_key",
            "supersedes",
            "created_at",
        ];
        block(store.insert(
            "canopy_project_event",
            &cols,
            &[
                Value::text("ev_1"),
                Value::text("proj_1"),
                Value::text("added"),
                Value::text("repo_1"),
                Value::text("/r/a"),
                Value::text("a"),
                Value::Null,
                Value::Null,
                Value::Int(1),
            ],
        ))
        .unwrap();
        // A relabel is a NEW superseding row, never an UPDATE.
        block(store.insert(
            "canopy_project_event",
            &cols,
            &[
                Value::text("ev_2"),
                Value::text("proj_1"),
                Value::text("relabeled"),
                Value::Null,
                Value::Null,
                Value::text("alpha"),
                Value::Null,
                Value::text("ev_1"),
                Value::Int(2),
            ],
        ))
        .unwrap();
        assert_eq!(block(store.count("canopy_project_event")).unwrap(), 2);
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

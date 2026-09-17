//! The engine-agnostic, async storage seam (`Store` / `StoreConn`).
//!
//! The rest of the workspace depends on *this* interface, never on a concrete
//! engine. The always-available synchronous-rusqlite [`crate::SqliteStore`]
//! implements it and is the **shipping engine**, so the immutability/migration/
//! integrity logic is proven offline. The async-native Turso engine was the
//! intended production target but its dependency is **deferred** after review
//! of the storage spike; its reference impl is archived out of tree and plugs
//! back in behind this seam, without touching callers, once it is
//! re-reviewed.
//!
//! The seam is `async` because Turso is async-native and the future Canopy/serve
//! readers run on a tokio runtime; a synchronous seam would force a `block_on`
//! inside their async context (a deadlock/panic hazard). All SQL is
//! parameterized: values bind through `&[Value]` and `?` placeholders — never
//! string-interpolated (the security model, and the `all_queries_are_parameterized`
//! gate).

use async_trait::async_trait;
use rift_core::{Result, RiftError};

use crate::value::Value;

/// Where a projection lives, for logging/rebuild (never carries row bytes).
#[derive(Debug, Clone)]
pub struct StoreDescriptor {
    path: String,
}

impl StoreDescriptor {
    /// A descriptor for the store at `path` (`":memory:"` for an ephemeral store).
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }

    /// The store's path/identity.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// One row of a query result, as engine-neutral [`Value`]s in column order.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    cols: Vec<Value>,
}

impl Row {
    /// Build a row from its column values.
    #[must_use]
    pub fn new(cols: Vec<Value>) -> Self {
        Self { cols }
    }

    /// Number of columns.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cols.len()
    }

    /// Whether the row has no columns.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cols.is_empty()
    }

    fn col(&self, i: usize) -> Result<&Value> {
        self.cols
            .get(i)
            .ok_or_else(|| RiftError::storage(format!("row column index {i} out of range")))
    }

    /// Read column `i` as an `i64`.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the index is out of range or the value is not an
    /// integer.
    pub fn get_i64(&self, i: usize) -> Result<i64> {
        match self.col(i)? {
            Value::Int(v) => Ok(*v),
            _ => Err(RiftError::storage(format!("column {i} is not an integer"))),
        }
    }

    /// Read column `i` as an optional `i64` (`NULL` → `None`).
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the index is out of range or the value is neither
    /// an integer nor `NULL`.
    pub fn get_opt_i64(&self, i: usize) -> Result<Option<i64>> {
        match self.col(i)? {
            Value::Int(v) => Ok(Some(*v)),
            Value::Null => Ok(None),
            _ => Err(RiftError::storage(format!(
                "column {i} is not an integer/null"
            ))),
        }
    }

    /// Read column `i` as text.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the index is out of range or the value is not text.
    pub fn get_text(&self, i: usize) -> Result<String> {
        match self.col(i)? {
            Value::Text(v) => Ok(v.clone()),
            _ => Err(RiftError::storage(format!("column {i} is not text"))),
        }
    }

    /// Read column `i` as optional text (`NULL` → `None`).
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the index is out of range or the value is neither
    /// text nor `NULL`.
    pub fn get_opt_text(&self, i: usize) -> Result<Option<String>> {
        match self.col(i)? {
            Value::Text(v) => Ok(Some(v.clone())),
            Value::Null => Ok(None),
            _ => Err(RiftError::storage(format!("column {i} is not text/null"))),
        }
    }

    /// Read column `i` as a blob.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the index is out of range or the value is not a blob.
    pub fn get_blob(&self, i: usize) -> Result<Vec<u8>> {
        match self.col(i)? {
            Value::Blob(v) => Ok(v.clone()),
            _ => Err(RiftError::storage(format!("column {i} is not a blob"))),
        }
    }
}

/// Transaction mode. `Concurrent` requests MVCC where the engine supports it
/// (Turso's `with_mvcc`); the rusqlite reference impl treats it as `Deferred`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxMode {
    /// A normal deferred transaction.
    Deferred,
    /// A concurrent (MVCC) transaction where supported.
    Concurrent,
}

/// Whether a store engine can *push* change notifications or must be *polled*.
///
/// The always-available rusqlite reference engine is [`NotifyCap::PollOnly`]: a
/// change observer watches an append cursor
/// ([`crate::CanonicalStore::read_watermark`]) on an interval. A future Turso
/// engine can report [`NotifyCap::Native`] and drive the *same* observer from its
/// native subscription without polling — the seam keeps the observer engine-blind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyCap {
    /// The engine pushes change notifications (Turso native subscribe).
    Native,
    /// The engine must be polled (the rusqlite reference engine).
    PollOnly,
}

/// A transaction handle: statements grouped atomically, finished by exactly one
/// of [`StoreTx::commit`] / [`StoreTx::rollback`] (dropping an unfinished
/// transaction rolls back, best-effort). The reference engine serializes all
/// statements on one connection, so isolation between concurrently-held handles
/// is cooperative — the projection's writers are single-threaded by design.
#[async_trait]
pub trait StoreTx: Send {
    /// Execute a parameterized statement inside the transaction.
    ///
    /// # Errors
    /// As [`StoreConn::execute`].
    async fn execute(&self, sql: &str, params: &[Value]) -> Result<u64>;

    /// Execute a (multi-statement) DDL batch inside the transaction.
    ///
    /// # Errors
    /// [`RiftError::Migration`] on failure.
    async fn execute_batch(&self, sql: &str) -> Result<()>;

    /// Run a parameterized query inside the transaction.
    ///
    /// # Errors
    /// [`RiftError::Storage`] on failure.
    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>>;

    /// Commit the transaction.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the commit fails (the transaction is rolled back).
    async fn commit(self: Box<Self>) -> Result<()>;

    /// Roll the transaction back explicitly.
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the rollback itself fails.
    async fn rollback(self: Box<Self>) -> Result<()>;
}

/// A connection handle. Parameterized read/write over the engine; never exposes
/// engine types to callers.
#[async_trait]
pub trait StoreConn: Send + Sync {
    /// Execute a parameterized statement; returns the affected-row count.
    ///
    /// # Errors
    /// [`RiftError::Immutability`] if an append-only guard fires, else
    /// [`RiftError::Storage`] — never embedding row/blob bytes.
    async fn execute(&self, sql: &str, params: &[Value]) -> Result<u64>;

    /// Execute a (multi-statement) DDL batch — no parameters.
    ///
    /// # Errors
    /// [`RiftError::Migration`] on failure.
    async fn execute_batch(&self, sql: &str) -> Result<()>;

    /// Run a parameterized query, materializing all rows.
    ///
    /// # Errors
    /// [`RiftError::Storage`] on failure.
    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>>;

    /// Begin a transaction. [`TxMode::Concurrent`] requests MVCC where the
    /// engine supports it; the rusqlite reference engine treats it as
    /// [`TxMode::Deferred`].
    ///
    /// # Errors
    /// [`RiftError::Storage`] if the transaction cannot be started.
    async fn begin(&self, mode: TxMode) -> Result<Box<dyn StoreTx + '_>>;
}

/// An engine-agnostic store. Hands out [`StoreConn`] handles; the append-only
/// repository ([`crate::CanonicalStore`]) is layered on top via a blanket impl.
#[async_trait]
pub trait Store: Send + Sync {
    /// Acquire a connection handle (cheap).
    ///
    /// # Errors
    /// [`RiftError::Storage`] if a connection cannot be obtained.
    async fn conn(&self) -> Result<Box<dyn StoreConn>>;

    /// The store's descriptor (path/identity).
    fn descriptor(&self) -> &StoreDescriptor;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_typed_accessors() {
        let r = Row::new(vec![
            Value::Int(7),
            Value::text("hi"),
            Value::Null,
            Value::Blob(vec![1, 2, 3]),
        ]);
        assert_eq!(r.len(), 4);
        assert!(!r.is_empty());
        assert_eq!(r.get_i64(0).unwrap(), 7);
        assert_eq!(r.get_text(1).unwrap(), "hi");
        assert_eq!(r.get_opt_text(2).unwrap(), None);
        assert_eq!(r.get_blob(3).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn row_type_and_range_errors() {
        let r = Row::new(vec![Value::text("x")]);
        assert!(matches!(r.get_i64(0), Err(RiftError::Storage(_))));
        assert!(matches!(r.get_blob(0), Err(RiftError::Storage(_))));
        assert!(matches!(r.get_text(9), Err(RiftError::Storage(_))));
    }

    #[test]
    fn descriptor_roundtrip() {
        let d = StoreDescriptor::new(":memory:");
        assert_eq!(d.path(), ":memory:");
    }
}

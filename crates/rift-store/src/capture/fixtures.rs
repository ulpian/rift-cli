//! The in-memory store every capture test writes into: a fresh SQLite with
//! the canonical schema applied, so each test starts from an empty projection.

use crate::SqliteStore;

pub(super) fn store() -> SqliteStore {
    let s = SqliteStore::open_in_memory().unwrap();
    s.create_canonical_schema().unwrap();
    s
}

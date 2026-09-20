//! The bare in-memory store every migration test starts from: a fresh SQLite
//! with no schema at all, so the migrations under test are what create it.
//! (The capture fixture, by contrast, applies the canonical schema up front.)

use crate::SqliteStore;

pub(super) fn store() -> SqliteStore {
    SqliteStore::open_in_memory().unwrap()
}

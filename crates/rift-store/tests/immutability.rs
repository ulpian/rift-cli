//! Integration test: exercise the public append-only API exactly as a dependent
//! crate (e.g. `rift-shadow`) would. The fact that this file cannot call an
//! `update`/`delete` is itself the compile-time half of the immutability
//! guarantee — there is no such method on [`rift_store::CanonicalStore`].
#![allow(clippy::unwrap_used, clippy::expect_used)]

use rift_store::{canonical_tables, CanonicalStore, SqliteStore, Value};

#[test]
fn public_api_is_append_only_and_usable() {
    let store = SqliteStore::open_in_memory().expect("open in-memory store");
    store
        .create_canonical_schema()
        .expect("create canonical schema");

    let cols = ["session_id", "agent", "model_primary", "started_at"];
    store
        .insert(
            "session",
            &cols,
            &[
                Value::text("sess_1"),
                Value::text("claude_code"),
                Value::text("claude-sonnet"),
                Value::Int(100),
            ],
        )
        .expect("insert session");

    // A correction is a NEW row that supersedes — the only mutation is INSERT.
    store
        .insert(
            "session",
            &cols,
            &[
                Value::text("sess_1b"),
                Value::text("claude_code"),
                Value::text("claude-opus"),
                Value::Int(101),
            ],
        )
        .expect("insert superseding row");

    assert_eq!(store.count("session").expect("count"), 2);
    assert!(!canonical_tables().is_empty());
}

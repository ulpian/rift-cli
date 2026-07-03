//! Committed DDL snapshots of all three projections (step-02 "schema snapshot
//! committed" exit criterion): the full `sqlite_master` SQL — tables, guard
//! triggers, indexes — after each projection's shipping migration set. Any
//! schema change shows up as a reviewable snapshot diff; an *unintentional*
//! change fails CI.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use rift_store::{
    apply_migrations, canopy_migrations, default_migrations, recall_migrations, Migration,
    SqliteStore,
};

/// Every `sqlite_master` entry (name, type, SQL) after applying `migs`, in a
/// deterministic order.
fn schema_dump(migs: &[Migration]) -> String {
    let store = SqliteStore::open_in_memory().unwrap();
    apply_migrations(&store, migs).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let rows = rt
        .block_on(rift_store::CanonicalStore::query(
            &store,
            "SELECT type, name, COALESCE(sql, '') FROM sqlite_master \
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
            &[],
        ))
        .unwrap();
    let mut out = String::new();
    for r in rows {
        out.push_str(&format!(
            "-- {} {}\n{}\n\n",
            r.get_text(0).unwrap(),
            r.get_text(1).unwrap(),
            r.get_text(2).unwrap()
        ));
    }
    out
}

#[test]
fn capture_projection_ddl_snapshot() {
    insta::assert_snapshot!("capture_projection_ddl", schema_dump(&default_migrations()));
}

#[test]
fn recall_projection_ddl_snapshot() {
    insta::assert_snapshot!("recall_projection_ddl", schema_dump(&recall_migrations()));
}

#[test]
fn canopy_projection_ddl_snapshot() {
    insta::assert_snapshot!("canopy_projection_ddl", schema_dump(&canopy_migrations()));
}

//! The projections run in WAL mode: a writable open switches the file, a
//! read-only open reads through the log a live writer has not checkpointed,
//! and the in-memory store is left alone.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use rift_store::{open_capture_projection, SqliteStore, Store};

fn pragma(store: &SqliteStore, name: &str) -> String {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let conn = store.conn().await.unwrap();
            let rows = conn.query(&format!("PRAGMA {name}"), &[]).await.unwrap();
            let row = rows.first().unwrap();
            row.get_text(0)
                .or_else(|_| row.get_i64(0).map(|n| n.to_string()))
                .unwrap()
        })
}

fn exec(store: &SqliteStore, sql: &str) {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            store
                .conn()
                .await
                .unwrap()
                .execute_batch(sql)
                .await
                .unwrap();
        });
}

fn sidecar(db: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut s = db.as_os_str().to_owned();
    s.push(suffix);
    s.into()
}

#[test]
fn a_writable_open_puts_the_projection_in_wal_with_normal_sync() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_capture_projection(dir.path().join("projection.db")).unwrap();
    assert_eq!(pragma(&store, "journal_mode"), "wal");
    // 1 = NORMAL; FULL (2) is the default this replaces.
    assert_eq!(pragma(&store, "synchronous"), "1");
}

#[test]
fn a_read_only_open_sees_commits_still_in_a_live_writers_log() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("projection.db");
    let writer = SqliteStore::open(&db).unwrap();
    exec(
        &writer,
        "PRAGMA wal_autocheckpoint=0; CREATE TABLE probe (n INTEGER); INSERT INTO probe VALUES (7)",
    );
    assert!(
        sidecar(&db, "-wal").is_file(),
        "the commit lives in the log"
    );

    let reader = SqliteStore::open_read_only(&db).unwrap();
    assert_eq!(pragma(&reader, "journal_mode"), "wal");
    let n = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let rows = reader
                .conn()
                .await
                .unwrap()
                .query("SELECT n FROM probe", &[])
                .await
                .unwrap();
            rows.first().unwrap().get_i64(0).unwrap()
        });
    assert_eq!(n, 7);
}

#[test]
fn a_passive_checkpoint_folds_the_log_into_the_database_file() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("projection.db");
    let writer = SqliteStore::open(&db).unwrap();
    exec(
        &writer,
        "PRAGMA wal_autocheckpoint=0; CREATE TABLE probe (n INTEGER); INSERT INTO probe VALUES (7)",
    );
    let before = std::fs::metadata(&db).unwrap().len();
    writer.checkpoint_wal().unwrap();
    assert!(std::fs::metadata(&db).unwrap().len() > before);
}

#[test]
fn the_in_memory_store_keeps_its_memory_journal() {
    let store = SqliteStore::open_in_memory().unwrap();
    assert_eq!(pragma(&store, "journal_mode"), "memory");
    store.checkpoint_wal().unwrap();
}

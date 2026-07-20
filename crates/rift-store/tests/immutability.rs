//! Integration test: exercise the public append-only API exactly as a dependent
//! crate (e.g. `rift-shadow`) would. The fact that this file cannot call an
//! `update`/`delete` is itself the compile-time half of the immutability
//! guarantee — there is no such method on [`rift_store::CanonicalStore`].
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeSet;

use rift_store::{
    apply_migrations, canonical_tables, default_migrations, CanonicalStore, Migration, SqliteStore,
    Value,
};

#[tokio::test]
async fn public_api_is_append_only_and_usable() {
    let store = SqliteStore::open_in_memory().expect("open in-memory store");
    store
        .create_canonical_schema()
        .expect("create canonical schema");

    let cols = [
        "session_id",
        "agent",
        "model_primary",
        "repo_id",
        "started_at",
        "schema_version",
    ];
    store
        .insert(
            "session",
            &cols,
            &[
                Value::text("sess_1"),
                Value::text("claude_code"),
                Value::text("claude-sonnet"),
                Value::text("repo_1"),
                Value::Int(100),
                Value::Int(1),
            ],
        )
        .await
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
                Value::text("repo_1"),
                Value::Int(101),
                Value::Int(1),
            ],
        )
        .await
        .expect("insert superseding row");

    assert_eq!(store.count("session").await.expect("count"), 2);
    assert!(!canonical_tables().is_empty());
}

/// Identifier binding (04 §4.3 type layer): a table or column that resolves in
/// no registry is refused before any SQL is built — the DAO's identifier
/// surface cannot be used as an injection vector even by careless future code.
#[tokio::test]
async fn unregistered_identifiers_are_refused() {
    let store = SqliteStore::open_in_memory().expect("open store");
    store.create_canonical_schema().expect("schema");

    let err = store
        .insert("sqlite_master; --", &["x"], &[Value::Int(1)])
        .await;
    assert!(err.is_err(), "unknown table must be refused");
    let err = store
        .insert(
            "session",
            &["session_id; DROP TABLE session"],
            &[Value::Int(1)],
        )
        .await;
    assert!(err.is_err(), "unknown column must be refused");
    assert!(store.count("not_a_table").await.is_err());
}

/// Real (non-infra) tables that are **not** registered canonical tables — i.e.
/// tables that exist in the projection without an append-only guard. Infra tables
/// (`_migrations`, `_schema_version`) are underscore-prefixed and exempt. Empty
/// ⇒ healthy; non-empty ⇒ a table was created without being registered.
fn unguarded_tables(store: &SqliteStore) -> Vec<String> {
    let registered: BTreeSet<&str> = canonical_tables().iter().map(|t| t.name).collect();
    store
        .table_names()
        .expect("read table names")
        .into_iter()
        .filter(|n| !n.starts_with('_') && !registered.contains(n.as_str()))
        .collect()
}

/// The forgotten-registration keystone: after a full production migration, every
/// real table must be a registered canonical table, and every registered table
/// must carry **both** append-only guard triggers. This makes the project
/// guarantee executable — *add a canonical table without its guard and CI goes
/// red* — because the registry is the single source of truth the triggers and
/// this assertion both derive from.
#[test]
fn every_db_table_is_registered_or_infra() {
    let store = SqliteStore::open_in_memory().expect("open store");
    apply_migrations(&store, &default_migrations()).expect("apply migrations");

    // (1) No real table exists without being registered (hence without a guard).
    assert!(
        unguarded_tables(&store).is_empty(),
        "unregistered (unguarded) tables present: {:?} — register them in \
         canonical_tables() so they get append-only triggers",
        unguarded_tables(&store)
    );

    // (2) Every registered table actually exists and has both guard triggers.
    let tables: BTreeSet<String> = store.table_names().expect("tables").into_iter().collect();
    let triggers: BTreeSet<String> = store
        .trigger_names()
        .expect("triggers")
        .into_iter()
        .collect();
    for table in canonical_tables() {
        let name = table.name;
        assert!(
            tables.contains(name),
            "registered table `{name}` is missing"
        );
        assert!(
            triggers.contains(&format!("{name}_no_update")),
            "`{name}` is missing its append-only UPDATE guard"
        );
        assert!(
            triggers.contains(&format!("{name}_no_delete")),
            "`{name}` is missing its append-only DELETE guard"
        );
    }
    assert!(!canonical_tables().is_empty());
}

/// Canary proving the keystone *bites*: a migration that creates a canonical-shaped
/// table but never registers it (so it never gets triggers) is flagged.
#[test]
fn forgotten_registration_is_caught() {
    let store = SqliteStore::open_in_memory().expect("open store");
    apply_migrations(&store, &default_migrations()).expect("apply base migrations");
    // A developer adds a data table via a migration but forgets canonical_tables().
    let rogue = vec![Migration {
        version: 8, // one past the shipped set (v7 = capture_system_truth)
        name: "rogue_unregistered",
        sql: "CREATE TABLE IF NOT EXISTS audit_log \
              (id INTEGER PRIMARY KEY, note TEXT) STRICT;"
            .to_string(),
    }];
    apply_migrations(&store, &rogue).expect("apply rogue migration");
    assert_eq!(
        unguarded_tables(&store),
        vec!["audit_log".to_string()],
        "the keystone must flag an unregistered table"
    );
}

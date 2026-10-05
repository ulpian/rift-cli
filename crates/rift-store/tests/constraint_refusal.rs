//! Integration test: `insert_or_ignore` skips a row whose key is already
//! present and refuses, with a typed error, a row a constraint forbids.
//!
//! Driven by the schema registries rather than by a list of tables, so a new
//! table or a new closed-vocabulary column is covered the day it is declared:
//! every table must take a sample row once and report its redelivery as a
//! no-op, and every `text_one_of` column must refuse a word outside its
//! vocabulary with [`RiftError::ConstraintRefused`] naming the table and the
//! column — never the silent `Ok(false)` a duplicate earns.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use rift_core::RiftError;
use rift_store::schema::{
    canonical_tables, canopy_canonical_tables, recall_canonical_tables, CanonicalTable, Column,
};
use rift_store::{CanonicalStore, SqliteStore, Value};

/// A word no closed vocabulary in any registry admits.
const OUTSIDER: &str = "not-a-member-of-any-vocabulary";

#[tokio::test]
async fn every_registry_table_takes_a_row_once_and_refuses_any_word_outside_a_vocabulary() {
    let registries = [
        canonical_tables(),
        recall_canonical_tables(),
        canopy_canonical_tables(),
    ];
    let mut vocabularies = 0;
    for registry in registries {
        let store = SqliteStore::open_in_memory().unwrap();
        for table in registry {
            store.register_canonical_table(table).await.unwrap();
            assert_duplicate_is_a_no_op(&store, table).await;
            assert_unique_conflict_is_a_no_op(&store, table).await;
            vocabularies += assert_every_vocabulary_refuses_an_outsider(&store, table).await;
        }
    }
    assert!(
        vocabularies > 0,
        "the registries declare closed vocabularies"
    );
}

#[tokio::test]
async fn a_null_in_a_required_column_is_refused_naming_the_column() {
    let store = capture_store();
    let mut values = sample_row(session_table(), 1);
    values[1] = Value::Null;
    let refused = insert(&store, session_table(), &values).await;
    assert_refused(&refused, "session", "not-null(agent)");
}

#[tokio::test]
async fn a_value_of_the_wrong_storage_class_is_refused_naming_the_column() {
    let store = capture_store();
    let table = session_table();
    let started_at = column_index(table, "started_at");
    let mut values = sample_row(table, 1);
    values[started_at] = Value::text("not-a-number");
    let refused = insert(&store, table, &values).await;
    assert_refused(&refused, "session", "datatype(started_at)");
}

#[tokio::test]
async fn a_plain_insert_of_a_present_key_keeps_its_storage_error() {
    let store = capture_store();
    let table = session_table();
    let values = sample_row(table, 1);
    let columns = column_names(table);
    store.insert(table.name, &columns, &values).await.unwrap();
    let again = store.insert(table.name, &columns, &values).await;
    assert!(matches!(again, Err(RiftError::Storage(_))), "{again:?}");
}

async fn assert_duplicate_is_a_no_op(store: &SqliteStore, table: &CanonicalTable) {
    let values = sample_row(table, 1);
    let first = insert(store, table, &values).await;
    assert!(matches!(first, Ok(true)), "{}: {first:?}", table.name);
    let replay = insert(store, table, &values).await;
    assert!(matches!(replay, Ok(false)), "{}: {replay:?}", table.name);
}

/// A row with a fresh primary key that repeats a secondary UNIQUE key is
/// skipped exactly as a repeated primary key is.
async fn assert_unique_conflict_is_a_no_op(store: &SqliteStore, table: &CanonicalTable) {
    let present = sample_row(table, 1);
    for unique in table.unique {
        let mut values = sample_row(table, 3);
        for (idx, column) in table.columns.iter().enumerate() {
            if unique.contains(&column.name) {
                values[idx] = present[idx].clone();
            }
        }
        let skipped = insert(store, table, &values).await;
        assert!(matches!(skipped, Ok(false)), "{}: {skipped:?}", table.name);
    }
}

/// Plants the outsider in each closed-vocabulary column of a fresh row (fresh
/// so no key conflict can mask the refusal) and returns how many it planted.
async fn assert_every_vocabulary_refuses_an_outsider(
    store: &SqliteStore,
    table: &CanonicalTable,
) -> usize {
    let mut planted = 0;
    for (idx, column) in table.columns.iter().enumerate() {
        if column.one_of.is_empty() {
            continue;
        }
        let mut values = sample_row(table, 2);
        values[idx] = Value::text(OUTSIDER);
        let refused = insert(store, table, &values).await;
        assert_refused(&refused, table.name, &format!("check({})", column.name));
        planted += 1;
    }
    planted
}

fn assert_refused(refused: &rift_core::Result<bool>, table: &str, constraint: &str) {
    match refused {
        Err(RiftError::ConstraintRefused {
            table: refused_table,
            constraint: refused_constraint,
        }) => {
            assert_eq!(refused_table, table);
            assert_eq!(refused_constraint, constraint);
        }
        other => panic!("{table}: expected a refusal of {constraint}, got {other:?}"),
    }
}

async fn insert(
    store: &SqliteStore,
    table: &CanonicalTable,
    values: &[Value],
) -> rift_core::Result<bool> {
    store
        .insert_or_ignore(table.name, &column_names(table), values)
        .await
}

/// One valid row built from the column metadata alone; `seed` varies every
/// free value, so two seeds never collide on any key.
fn sample_row(table: &CanonicalTable, seed: i64) -> Vec<Value> {
    table
        .columns
        .iter()
        .map(|c| sample_value(c, seed))
        .collect()
}

fn sample_value(column: &Column, seed: i64) -> Value {
    if let Some(first) = column.one_of.first() {
        return Value::text(*first);
    }
    match column.sql_type {
        "INTEGER" => Value::Int(seed),
        "BLOB" => Value::Blob(format!("sample-{seed}").into_bytes()),
        _ => Value::text(format!("sample-{seed}")),
    }
}

fn column_names(table: &CanonicalTable) -> Vec<&'static str> {
    table.columns.iter().map(|c| c.name).collect()
}

fn column_index(table: &CanonicalTable, name: &str) -> usize {
    table.columns.iter().position(|c| c.name == name).unwrap()
}

fn session_table() -> &'static CanonicalTable {
    canonical_tables()
        .iter()
        .find(|t| t.name == "session")
        .unwrap()
}

fn capture_store() -> SqliteStore {
    let store = SqliteStore::open_in_memory().unwrap();
    store.create_canonical_schema().unwrap();
    store
}

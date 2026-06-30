//! Additive-only, checksum-locked migration engine.
//!
//! The Turso projection is rebuildable from canonical git, so a "migration" is
//! either creating new tables/columns or bumping a version and rebuilding —
//! never a destructive in-place rewrite (`04-database-design` §4.6). Migration
//! files are append-only and **checksum-locked**: editing an already-applied
//! migration changes its SHA-256 and the runner refuses to run
//! ([`RiftError::Integrity`]). A new schema requires a *new* migration.
//!
//! An additive guard rejects `DROP TABLE` / `DROP COLUMN` / `RENAME` so a
//! destructive migration cannot land. (Statement-level `DELETE`/`UPDATE`
//! detection against canonical tables is left to a real SQL parser — **(verify)**
//! `sqlparser` — to avoid false positives on the append-only trigger bodies,
//! which legitimately contain `BEFORE UPDATE`/`DELETE`.)

use std::collections::BTreeMap;

use rift_core::{Result, RiftError, Timestamp};
use sha2::{Digest, Sha256};

use crate::schema::canonical_tables;
use crate::sqlite_store::SqliteStore;

/// The migration meta tables. Themselves insert-only in the runner (a re-run
/// skips on an existing version, never updates).
pub const META_SQL: &str = "\
CREATE TABLE IF NOT EXISTS _migrations (\n\
  version    INTEGER PRIMARY KEY,\n\
  name       TEXT    NOT NULL,\n\
  checksum   TEXT    NOT NULL,\n\
  applied_at TEXT    NOT NULL\n\
) STRICT;\n\
CREATE TABLE IF NOT EXISTS _schema_version (\n\
  table_name     TEXT PRIMARY KEY,\n\
  schema_version INTEGER NOT NULL\n\
) STRICT;";

/// One migration: an ordered, checksum-locked unit of additive DDL.
#[derive(Debug, Clone)]
pub struct Migration {
    /// Strictly-increasing version (1, 2, 3, …).
    pub version: u32,
    /// Human-readable name.
    pub name: &'static str,
    /// The SQL applied (additive only).
    pub sql: String,
}

/// The result of a migration run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationOutcome {
    /// Number of migrations applied this run.
    pub applied: usize,
    /// Number already applied (skipped).
    pub skipped: usize,
    /// The highest version present after the run.
    pub high_water: u32,
}

fn checksum_hex(sql: &str) -> String {
    let mut h = Sha256::new();
    h.update(sql.as_bytes());
    let out = h.finalize();
    let mut s = String::with_capacity(out.len() * 2);
    use core::fmt::Write as _;
    for b in out {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Reject destructive DDL (`DROP TABLE`/`DROP COLUMN`/`RENAME`).
///
/// # Errors
/// [`RiftError::Migration`] if the SQL contains a forbidden destructive form.
pub fn is_additive(sql: &str) -> Result<()> {
    let upper = sql.to_uppercase();
    for forbidden in ["DROP TABLE", "DROP COLUMN", "RENAME TO", "RENAME COLUMN"] {
        if upper.contains(forbidden) {
            return Err(RiftError::Migration(format!(
                "non-additive migration: contains {forbidden}"
            )));
        }
    }
    Ok(())
}

/// Build the canonical-schema migration SQL from the table registry (one source
/// of truth shared with [`SqliteStore::create_canonical_schema`]).
#[must_use]
pub fn canonical_schema_sql() -> String {
    let mut sql = String::new();
    for table in canonical_tables() {
        sql.push_str(&table.create_table_sql());
        sql.push('\n');
        sql.push_str(&table.guard_trigger_sql());
        sql.push('\n');
    }
    for table in canonical_tables() {
        sql.push_str(&format!(
            "INSERT INTO _schema_version(table_name, schema_version) VALUES ('{}', 1);\n",
            table.name
        ));
    }
    sql
}

/// The default migration set: meta tables, then the canonical schema.
#[must_use]
pub fn default_migrations() -> Vec<Migration> {
    vec![
        Migration {
            version: 1,
            name: "init_meta",
            sql: META_SQL.to_string(),
        },
        Migration {
            version: 2,
            name: "canonical_core",
            sql: canonical_schema_sql(),
        },
    ]
}

/// Apply pending migrations in order, idempotently and atomically.
///
/// # Errors
/// * [`RiftError::Integrity`] if an already-applied migration's checksum changed.
/// * [`RiftError::Migration`] on a non-additive migration, a version gap, or a
///   DDL failure.
pub fn apply_migrations(store: &SqliteStore, migs: &[Migration]) -> Result<MigrationOutcome> {
    store.ensure_meta()?;
    let applied: BTreeMap<u32, String> = store.applied_migrations()?.into_iter().collect();
    let mut high_water = applied.keys().copied().max().unwrap_or(0);

    let mut ordered: Vec<&Migration> = migs.iter().collect();
    ordered.sort_by_key(|m| m.version);

    let mut applied_count = 0;
    let mut skipped = 0;
    for m in ordered {
        is_additive(&m.sql)?;
        let cs = checksum_hex(&m.sql);
        if let Some(prev) = applied.get(&m.version) {
            if prev != &cs {
                return Err(RiftError::integrity(format!(
                    "migration {} checksum changed since it was applied",
                    m.version
                )));
            }
            skipped += 1;
            continue;
        }
        if m.version != high_water + 1 {
            return Err(RiftError::Migration(format!(
                "migration {} out of order or gap (expected {})",
                m.version,
                high_water + 1
            )));
        }
        store.apply_migration(
            &m.sql,
            m.version,
            m.name,
            &cs,
            &Timestamp::now().to_string(),
        )?;
        high_water = m.version;
        applied_count += 1;
    }
    Ok(MigrationOutcome {
        applied: applied_count,
        skipped,
        high_water,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CanonicalStore, Value};

    fn store() -> SqliteStore {
        SqliteStore::open_in_memory().unwrap()
    }

    #[tokio::test]
    async fn applies_pending_migrations_in_order() {
        let s = store();
        let out = apply_migrations(&s, &default_migrations()).unwrap();
        assert_eq!(out.applied, 2);
        assert_eq!(out.high_water, 2);
        // canonical tables exist (insert succeeds)
        s.insert(
            "session",
            &[
                "session_id",
                "agent",
                "model_primary",
                "repo_id",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text("sess_1"),
                Value::text("a"),
                Value::text("m"),
                Value::text("repo_1"),
                Value::Int(1),
                Value::Int(1),
            ],
        )
        .await
        .unwrap();
    }

    #[test]
    fn rerun_is_idempotent() {
        let s = store();
        apply_migrations(&s, &default_migrations()).unwrap();
        let out = apply_migrations(&s, &default_migrations()).unwrap();
        assert_eq!(out.applied, 0);
        assert_eq!(out.skipped, 2);
    }

    #[test]
    fn edited_applied_migration_is_refused() {
        let s = store();
        let migs = vec![Migration {
            version: 1,
            name: "m1",
            sql: META_SQL.to_string(),
        }];
        apply_migrations(&s, &migs).unwrap();
        let edited = vec![Migration {
            version: 1,
            name: "m1",
            sql: format!("{META_SQL}\n-- tampered comment"),
        }];
        let err = apply_migrations(&s, &edited);
        assert!(matches!(err, Err(RiftError::Integrity(_))), "got {err:?}");
    }

    #[test]
    fn out_of_order_or_gap_version_is_rejected() {
        let s = store();
        let migs = vec![
            Migration {
                version: 1,
                name: "m1",
                sql: META_SQL.to_string(),
            },
            Migration {
                version: 4,
                name: "m4",
                sql: "CREATE TABLE IF NOT EXISTS extra (x INTEGER PRIMARY KEY) STRICT;".to_string(),
            },
        ];
        let err = apply_migrations(&s, &migs);
        assert!(matches!(err, Err(RiftError::Migration(_))), "got {err:?}");
    }

    #[test]
    fn additive_check_rejects_drop_table() {
        assert!(is_additive("CREATE TABLE x (a INTEGER PRIMARY KEY) STRICT;").is_ok());
        assert!(is_additive("DROP TABLE checkpoint;").is_err());
        assert!(is_additive("ALTER TABLE x RENAME TO y;").is_err());
        // a legitimate append-only trigger body must NOT be flagged
        assert!(is_additive(&canonical_schema_sql()).is_ok());
    }

    /// The destructive-DDL CI gate, made explicit and order-independent: every
    /// migration in the **shipping** set must be additive. A future migration that
    /// sneaks a `DROP`/`RENAME` in fails here, not only when it happens to apply.
    #[test]
    fn every_default_migration_is_additive() {
        for m in default_migrations() {
            is_additive(&m.sql).unwrap_or_else(|e| {
                panic!(
                    "shipping migration {} ({}) is non-additive: {e}",
                    m.version, m.name
                )
            });
        }
    }

    #[test]
    fn per_table_schema_version_is_tracked() {
        let s = store();
        apply_migrations(&s, &default_migrations()).unwrap();
        let n: i64 = s
            .with_conn(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM _schema_version WHERE schema_version = 1",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert!(n >= 1, "expected per-table schema_version rows, got {n}");
    }
}

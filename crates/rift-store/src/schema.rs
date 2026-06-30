//! Canonical-table specifications and the SQL they generate.
//!
//! Every canonical table is **append-only**: it gets `BEFORE UPDATE` and
//! `BEFORE DELETE` triggers that `RAISE(ABORT)`, so even raw SQL that bypasses
//! the [`crate::CanonicalStore`] DAO cannot mutate a row. The set of canonical
//! tables is a registry ([`canonical_tables`]); the property test in
//! `sqlite_store.rs` iterates it and asserts each table rejects `UPDATE` and
//! `DELETE`, so adding a table without its guard fails CI.
//!
//! Tables use SQLite `STRICT` mode for column-type enforcement. The same DDL is
//! valid for Turso (SQLite-compatible), so this transfers to production verbatim.

/// A column in a canonical table.
#[derive(Debug, Clone, Copy)]
pub struct Column {
    /// Column name.
    pub name: &'static str,
    /// Column type (`STRICT`-compatible: `TEXT`, `INTEGER`, `BLOB`, `REAL`, `ANY`).
    pub sql_type: &'static str,
    /// Whether the column is `NOT NULL`.
    pub not_null: bool,
}

impl Column {
    /// A `NOT NULL TEXT` column.
    #[must_use]
    pub const fn text(name: &'static str) -> Self {
        Self {
            name,
            sql_type: "TEXT",
            not_null: true,
        }
    }

    /// A nullable `TEXT` column (e.g. a genesis `prev_hash`, an open `valid_to`,
    /// or a `supersedes_id` for an original row).
    #[must_use]
    pub const fn text_null(name: &'static str) -> Self {
        Self {
            name,
            sql_type: "TEXT",
            not_null: false,
        }
    }

    /// A `NOT NULL INTEGER` column.
    #[must_use]
    pub const fn int(name: &'static str) -> Self {
        Self {
            name,
            sql_type: "INTEGER",
            not_null: true,
        }
    }

    /// A nullable `INTEGER` column.
    #[must_use]
    pub const fn int_null(name: &'static str) -> Self {
        Self {
            name,
            sql_type: "INTEGER",
            not_null: false,
        }
    }

    /// A `NOT NULL BLOB` column.
    #[must_use]
    pub const fn blob(name: &'static str) -> Self {
        Self {
            name,
            sql_type: "BLOB",
            not_null: true,
        }
    }
}

/// A canonical (append-only) table specification.
#[derive(Debug, Clone, Copy)]
pub struct CanonicalTable {
    /// Table name (must match `[A-Za-z0-9_]`).
    pub name: &'static str,
    /// Columns, in declaration order.
    pub columns: &'static [Column],
    /// Primary-key column name(s).
    pub primary_key: &'static [&'static str],
}

impl CanonicalTable {
    /// `CREATE TABLE IF NOT EXISTS … STRICT;` DDL for this table.
    #[must_use]
    pub fn create_table_sql(&self) -> String {
        let cols = self
            .columns
            .iter()
            .map(|c| {
                let null = if c.not_null { " NOT NULL" } else { "" };
                format!("  {} {}{null}", c.name, c.sql_type)
            })
            .collect::<Vec<_>>()
            .join(",\n");
        format!(
            "CREATE TABLE IF NOT EXISTS {} (\n{cols},\n  PRIMARY KEY ({})\n) STRICT;",
            self.name,
            self.primary_key.join(", ")
        )
    }

    /// The two append-only guard triggers (`UPDATE` + `DELETE`) for this table.
    ///
    /// They `RAISE(ABORT)` with a message containing `append-only`, which the
    /// store classifies as [`rift_core::RiftError::Immutability`].
    #[must_use]
    pub fn guard_trigger_sql(&self) -> String {
        let t = self.name;
        format!(
            "CREATE TRIGGER IF NOT EXISTS {t}_no_update \
             BEFORE UPDATE ON {t} BEGIN \
             SELECT RAISE(ABORT, 'rift: {t} is append-only (UPDATE forbidden)'); END;\n\
             CREATE TRIGGER IF NOT EXISTS {t}_no_delete \
             BEFORE DELETE ON {t} BEGIN \
             SELECT RAISE(ABORT, 'rift: {t} is append-only (DELETE forbidden)'); END;"
        )
    }
}

// --- The registry of canonical tables -------------------------------------
// A 4NF/5NF slice of the rift-shadow + integrity projection schema (see
// build-plan stage-01 phase-03/04 and stage-02 phase-02). Independent facts live
// in separate tables; bitemporal facts (cost) never UPDATE — reconciliations
// supersede via new rows.

const CHECKPOINT_COLUMNS: &[Column] = &[
    Column::text("checkpoint_id"),  // Sha256Id (content-addressed)
    Column::text("strategy"),       // manual-commit, …
    Column::int("created_at"),      // transaction-time (epoch ms)
    Column::text("merkle_root"),    // sha256 over this checkpoint's event leaves
    Column::text_null("prev_hash"), // prior checkpoint_id (NULL = genesis) — the chain
    Column::int("schema_version"),
];

const SESSION_COLUMNS: &[Column] = &[
    Column::text("session_id"),
    Column::text("agent"),
    Column::text("model_primary"),
    Column::int("started_at"),
];

const TOKEN_USAGE_COLUMNS: &[Column] = &[
    Column::text("call_id"),
    Column::int("uncached_input"),
    Column::int("cached_input"),
    Column::int("cache_creation"),
    Column::int("output"),
];

// Bitemporal cost fact (04 §4.3 rule 4): never UPDATE; reconciliations supersede.
const COST_FACT_COLUMNS: &[Column] = &[
    Column::text("cost_fact_id"),
    Column::text("call_id"),
    Column::int("amount_micros"),
    Column::int("valid_from"),    // valid-time: when the price was true
    Column::int_null("valid_to"), // NULL = still believed valid
    Column::int("recorded_at"),   // transaction-time: when Rift learned it
    Column::text_null("supersedes_id"),
    Column::int("schema_version"),
];

// Encryption-at-rest key metadata (Phase 3 Step 5): insert-only; rotation is a
// superseding generation row so old-key data still decrypts.
const KEY_METADATA_COLUMNS: &[Column] = &[
    Column::text("key_id"),
    Column::text("repo_id"),
    Column::blob("wrapped_dek"),
    Column::text("kek_ref"),
    Column::text("cipher"),
    Column::int("generation"),
    Column::int("recorded_at"),
    Column::text_null("supersedes_id"),
    Column::int("schema_version"),
];

const TABLES: &[CanonicalTable] = &[
    CanonicalTable {
        name: "checkpoint",
        columns: CHECKPOINT_COLUMNS,
        primary_key: &["checkpoint_id"],
    },
    CanonicalTable {
        name: "session",
        columns: SESSION_COLUMNS,
        primary_key: &["session_id"],
    },
    CanonicalTable {
        name: "token_usage",
        columns: TOKEN_USAGE_COLUMNS,
        primary_key: &["call_id"],
    },
    CanonicalTable {
        name: "cost_fact",
        columns: COST_FACT_COLUMNS,
        primary_key: &["cost_fact_id"],
    },
    CanonicalTable {
        name: "key_metadata",
        columns: KEY_METADATA_COLUMNS,
        primary_key: &["key_id"],
    },
];

/// The registry of canonical (append-only) tables. The immutability property
/// test iterates this, so every entry must have its guard triggers.
#[must_use]
pub fn canonical_tables() -> &'static [CanonicalTable] {
    TABLES
}

/// Look up a canonical table by name.
#[must_use]
pub fn canonical_table(name: &str) -> Option<&'static CanonicalTable> {
    TABLES.iter().find(|t| t.name == name)
}

/// Whether `name` is a registered canonical table (used by the additive-migration
/// guard so a migration cannot destructively rewrite canonical data).
#[must_use]
pub fn is_canonical_table(name: &str) -> bool {
    canonical_table(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_nonempty_and_well_formed() {
        let tables = canonical_tables();
        assert!(!tables.is_empty());
        for t in tables {
            assert!(!t.columns.is_empty(), "{} has no columns", t.name);
            assert!(!t.primary_key.is_empty(), "{} has no primary key", t.name);
        }
    }

    #[test]
    fn ddl_mentions_table_columns_and_strict() {
        let t = canonical_tables().first().expect("registry non-empty");
        let ddl = t.create_table_sql();
        assert!(ddl.contains(t.name));
        assert!(ddl.contains("STRICT"));
        assert!(ddl.contains("PRIMARY KEY"));
        for c in t.columns {
            assert!(ddl.contains(c.name), "ddl missing column {}", c.name);
        }
    }

    #[test]
    fn nullable_column_omits_not_null() {
        let ddl = canonical_table("checkpoint").unwrap().create_table_sql();
        assert!(
            ddl.contains("prev_hash TEXT,"),
            "prev_hash must be nullable: {ddl}"
        );
        assert!(ddl.contains("merkle_root TEXT NOT NULL"));
    }

    #[test]
    fn every_table_generates_both_guard_triggers() {
        for t in canonical_tables() {
            let sql = t.guard_trigger_sql();
            assert!(
                sql.contains("BEFORE UPDATE"),
                "{} missing UPDATE guard",
                t.name
            );
            assert!(
                sql.contains("BEFORE DELETE"),
                "{} missing DELETE guard",
                t.name
            );
            assert!(sql.contains("append-only"));
        }
    }

    #[test]
    fn canonical_table_lookup() {
        assert!(is_canonical_table("cost_fact"));
        assert!(!is_canonical_table("_migrations"));
    }
}

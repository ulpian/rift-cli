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
//!
//! ## What this models: the capture projection
//!
//! The registry below is the full 4NF/5NF capture projection: sessions/turns/
//! model-&-tool-calls/file-revisions, checkpoints + session-in-checkpoint,
//! tokens/price/cost, attribution per-file/per-hunk + changed hunks, the
//! shadow-branch lifecycle, the hook/lifecycle audit facts, the retired
//! wrapper backend's audit facts, the ingest watermark, and the verification
//! run/divergence log. Independent facts
//! live in **separate** relations (4NF); bitemporal facts (cost) never UPDATE —
//! reconciliations supersede via new rows.
//!
//! ### Scoping notes
//! * **Foreign keys** between these relations are documented in comments rather
//!   than emitted as SQL `REFERENCES`. The projection is rebuildable and the
//!   event bridge is the single writer that guarantees referential
//!   integrity; emitting real FK constraints (with `PRAGMA foreign_keys=ON`)
//!   would require global insert-ordering and is deferred to a later hardening
//!   pass.
//! * **`CHECK` constraints** on closed vocabularies are in the DDL: a
//!   [`Column::text_one_of`] column emits `CHECK (<name> IN (…))`, and the
//!   engine refuses any other word with
//!   [`rift_core::RiftError::ConstraintRefused`], through
//!   `insert_or_ignore` as well as `insert`. Ranges and counts (confidence,
//!   non-negative counts) are enforced at the Rust writer layer instead
//!   (clamped scores, `u32` counts), so the registry-driven tests can build a
//!   generic sample row for every table from its column metadata alone.
//! * **Floats** never reach the store: confidence is an integer in milli-units
//!   (`confidence_milli ∈ 0..=1000`), money is integer micro-units — consistent
//!   with the no-float money posture.

mod canopy;
mod capture;
mod recall;

pub use canopy::{canopy_canonical_table, canopy_canonical_tables};
pub use capture::{
    canonical_table, canonical_tables, IMPORTED_CHECKPOINT_LAYOUTS,
    IMPORTED_CHECKPOINT_ROOT_CHECKS, SESSION_WORKTREE_BASES, TOOL_RESULT_BASES,
    TOOL_RESULT_CHANNELS,
};
pub(crate) use capture::{
    capture_checkpoint_commit_tables, capture_identity_tables, capture_imported_checkpoint_tables,
    capture_model_observation_tables, capture_projection_meta_tables, capture_read_tokens_tables,
    capture_system_tables, capture_tool_result_tables, capture_turn_boundary_tables,
    capture_v1_tables, capture_verification_witness_tables, capture_worktree_tables,
};
pub use recall::{
    recall_canonical_table, recall_canonical_tables, EVAL_RUN_CONDITIONS, EVAL_RUN_VERDICTS,
    OUTCOME_LABEL_KINDS, OUTCOME_LABEL_SUBJECT_KINDS, RECALL_AUDIT_CITATION_INDEX_SQL,
    RECALL_CALL_EVENT_INDEX_SQL, RECALL_CONNECTION_EVENT_INDEX_SQL, RECALL_EDGE_KINDS,
    RECALL_INDEX_SQL, RECALL_NODE_KINDS, RECALL_OUTCOME_INDEX_SQL, RECALL_SERVE_CHANNEL_INDEX_SQL,
    SERVE_CHANNELS,
};
pub(crate) use recall::{
    recall_v10_audit_citation_tables, recall_v11_serve_channel_tables,
    recall_v12_worktree_scope_tables, recall_v1_tables, recall_v2_outcome_tables,
    recall_v3_custody_tables, recall_v4_audit_tables, recall_v5_blob_map_tables,
    recall_v6_watermark_tables, recall_v7_call_tables, recall_v8_eval_tables,
    recall_v9_connection_tables,
};

/// A column in a canonical table.
#[derive(Debug, Clone, Copy)]
pub struct Column {
    /// Column name.
    pub name: &'static str,
    /// Column type (`STRICT`-compatible: `TEXT`, `INTEGER`, `BLOB`, `REAL`, `ANY`).
    pub sql_type: &'static str,
    /// Whether the column is `NOT NULL`.
    pub not_null: bool,
    /// A closed value set. When non-empty, the DDL emits
    /// `CHECK (<name> IN ('a','b',…))` so the engine rejects any other value
    /// (the recall graph's `kind` enums use this — semantic kinds are refused
    /// until a later migration widens the set), and the registry-driven
    /// immutability test seeds a *valid* sample from `one_of[0]`.
    pub one_of: &'static [&'static str],
}

impl Column {
    /// A `NOT NULL TEXT` column.
    #[must_use]
    pub const fn text(name: &'static str) -> Self {
        Self {
            name,
            sql_type: "TEXT",
            not_null: true,
            one_of: &[],
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
            one_of: &[],
        }
    }

    /// A `NOT NULL TEXT` column constrained to a closed value set via a SQL
    /// `CHECK (… IN (…))` (e.g. a graph node/edge `kind`).
    #[must_use]
    pub const fn text_one_of(name: &'static str, values: &'static [&'static str]) -> Self {
        Self {
            name,
            sql_type: "TEXT",
            not_null: true,
            one_of: values,
        }
    }

    /// A `NOT NULL INTEGER` column.
    #[must_use]
    pub const fn int(name: &'static str) -> Self {
        Self {
            name,
            sql_type: "INTEGER",
            not_null: true,
            one_of: &[],
        }
    }

    /// A nullable `INTEGER` column.
    #[must_use]
    pub const fn int_null(name: &'static str) -> Self {
        Self {
            name,
            sql_type: "INTEGER",
            not_null: false,
            one_of: &[],
        }
    }

    /// A `NOT NULL BLOB` column.
    #[must_use]
    pub const fn blob(name: &'static str) -> Self {
        Self {
            name,
            sql_type: "BLOB",
            not_null: true,
            one_of: &[],
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
    /// Zero or more table-level `UNIQUE` constraints (each a column list).
    pub unique: &'static [&'static [&'static str]],
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
                let check = if c.one_of.is_empty() {
                    String::new()
                } else {
                    let set = c
                        .one_of
                        .iter()
                        .map(|v| format!("'{v}'"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(" CHECK ({} IN ({set}))", c.name)
                };
                format!("  {} {}{null}{check}", c.name, c.sql_type)
            })
            .collect::<Vec<_>>()
            .join(",\n");
        let mut uniques = String::new();
        for u in self.unique {
            use core::fmt::Write as _;
            let _ = write!(uniques, ",\n  UNIQUE ({})", u.join(", "));
        }
        format!(
            "CREATE TABLE IF NOT EXISTS {} (\n{cols},\n  PRIMARY KEY ({}){uniques}\n) STRICT;",
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

/// A table with no `UNIQUE` constraint: the value every registry entry
/// without one names, so the three registries share a single spelling.
pub(super) const NO_UNIQUE: &[&[&str]] = &[];

/// Look up a table across **all three** registries (capture, recall, canopy) —
/// the identifier-validation source for the insert DAO: a table name that
/// resolves nowhere is refused before any SQL is built.
#[must_use]
pub fn any_registry_table(name: &str) -> Option<&'static CanonicalTable> {
    canonical_table(name)
        .or_else(|| recall_canonical_table(name))
        .or_else(|| canopy_canonical_table(name))
}

/// Whether `name` is a registered canonical table (used by the additive-migration
/// guard so a migration cannot destructively rewrite canonical data).
#[must_use]
pub fn is_canonical_table(name: &str) -> bool {
    canonical_table(name).is_some()
        || recall_canonical_table(name).is_some()
        || canopy_canonical_table(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn composite_primary_key_is_emitted() {
        let ddl = canonical_table("session_in_checkpoint")
            .unwrap()
            .create_table_sql();
        assert!(
            ddl.contains("PRIMARY KEY (checkpoint_id, session_id)"),
            "{ddl}"
        );
    }

    #[test]
    fn unique_constraint_is_emitted() {
        let ddl = canonical_table("turn").unwrap().create_table_sql();
        assert!(ddl.contains("UNIQUE (session_id, ordinal)"), "{ddl}");
        // a table without a UNIQUE emits none
        let ddl2 = canonical_table("session").unwrap().create_table_sql();
        assert!(!ddl2.contains("UNIQUE"), "{ddl2}");
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
        // An unregistered name has no entry (the None arm of canonical_table).
        assert!(canonical_table("does_not_exist").is_none());
    }

    #[test]
    fn column_builders_set_type_and_nullability() {
        // The registry builds columns in `const` context (no runtime coverage),
        // so exercise the builders at runtime here.
        assert_eq!(Column::text("a").sql_type, "TEXT");
        assert!(Column::text("a").not_null);
        assert_eq!(Column::text_null("b").sql_type, "TEXT");
        assert!(!Column::text_null("b").not_null);
        assert_eq!(Column::int("c").sql_type, "INTEGER");
        assert!(Column::int("c").not_null);
        assert_eq!(Column::int_null("d").sql_type, "INTEGER");
        assert!(!Column::int_null("d").not_null);
        assert_eq!(Column::blob("e").sql_type, "BLOB");
        assert!(Column::blob("e").not_null);
    }
}

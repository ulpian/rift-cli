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
//! ## What this models (M1 capture, Stage 2 Phases 1–5)
//!
//! The registry below is the full 4NF/5NF capture projection: sessions/turns/
//! model-&-tool-calls/file-revisions (P2 S3), checkpoints + session-in-checkpoint
//! (P2 S2), tokens/price/cost (P2 S4), attribution per-file/per-hunk + changed
//! hunks (P2 S5/P4 S1), shadow-branch lifecycle (P2 S1), the hook/lifecycle audit
//! facts (P3 S2/S3/S4), the wrapper audit facts (P1 S1/S3), the ingest watermark
//! (P1 S4), and the verification run/divergence log (P5 S4). Independent facts
//! live in **separate** relations (4NF); bitemporal facts (cost) never UPDATE —
//! reconciliations supersede via new rows.
//!
//! ### Scoping notes (M1)
//! * **Foreign keys** between these relations are documented in comments rather
//!   than emitted as SQL `REFERENCES`. The projection is rebuildable and the
//!   event bridge (P1 S4) is the single writer that guarantees referential
//!   integrity; emitting real FK constraints (with `PRAGMA foreign_keys=ON`)
//!   would require global insert-ordering and is deferred to Stage-6 hardening.
//! * **`CHECK` constraints** (confidence range, category enum, non-negative
//!   counts) are enforced at the Rust writer layer for M1 (clamped scores, typed
//!   enums, `u32` counts); SQL-level `CHECK` is a Stage-6 hardening item. (Kept
//!   out of the DDL so the registry-driven immutability test can insert a generic
//!   sample row into every table.)
//! * **Floats** never reach the store: confidence is an integer in milli-units
//!   (`confidence_milli ∈ 0..=1000`), money is integer micro-units — consistent
//!   with the no-float money posture (04 §4.2).

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
                format!("  {} {}{null}", c.name, c.sql_type)
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

// --- The registry of canonical tables -------------------------------------
// All hashes/oids are stored as lowercase hex `TEXT` (matching the integrity
// store's `Sha256Id::to_hex`/`parse_hex`); timestamps are epoch-ms `INTEGER`.

// ----- Integrity / chain (Stage 1 P3/P4; extended by P2 S2) ----------------

const CHECKPOINT_COLUMNS: &[Column] = &[
    Column::text("checkpoint_id"), // Sha256Id (content-addressed = sha256(metadata))
    Column::text("strategy"),      // manual-commit, on-pr, scheduled, policy-driven
    Column::int("created_at"),     // transaction-time (epoch ms)
    Column::text("merkle_root"),   // sha256 over this checkpoint's event leaves
    Column::text_null("prev_hash"), // prior checkpoint_id (NULL = genesis) — the chain
    Column::int("schema_version"),
];

// M:N checkpoint↔session, its own relation (4NF). FK: checkpoint_id→checkpoint,
// session_id→session. `ordinal` = order within the checkpoint.
const SESSION_IN_CHECKPOINT_COLUMNS: &[Column] = &[
    Column::text("checkpoint_id"),
    Column::text("session_id"),
    Column::int("ordinal"),
    Column::int("schema_version"),
];

// ----- Sessions / turns / calls / files (P2 S3) ----------------------------

const SESSION_COLUMNS: &[Column] = &[
    Column::text("session_id"),    // ULID
    Column::text("agent"),         // 'claude-code' | …
    Column::text("model_primary"), // headline model
    Column::text("repo_id"),
    Column::int("started_at"), // valid-time (epoch ms)
    Column::int("schema_version"),
];

// Lifecycle as EVENTS, not a mutable status column (04 §4.3 rule 2). FK: session.
const SESSION_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),
    Column::text("session_id"),
    Column::text("kind"), // 'start' | 'end' | 'compaction'
    Column::int("occurred_at"),
    Column::int("schema_version"),
];

const TURN_COLUMNS: &[Column] = &[
    Column::text("turn_id"),
    Column::text("session_id"), // FK: session
    Column::int("ordinal"),     // order within the session
    Column::int("started_at"),
    Column::int("schema_version"),
];

const MODEL_CALL_COLUMNS: &[Column] = &[
    Column::text("call_id"),
    Column::text("turn_id"), // FK: turn
    Column::text("model"),   // may differ from session.model_primary (subagent/aux)
    Column::int("started_at"),
    Column::int("schema_version"),
];

const TOOL_CALL_COLUMNS: &[Column] = &[
    Column::text("tool_call_id"),
    Column::text("turn_id"),   // FK: turn
    Column::text("tool_name"), // agent-namespaced
    Column::int("started_at"),
    Column::int("schema_version"),
];

// A path@blob_oid is ONE immutable, content-addressed fact (04 §4.4).
const FILE_REVISION_COLUMNS: &[Column] = &[
    Column::text("file_revision_id"), // = sha256(path || 0x00 || blob_oid)
    Column::text("path"),
    Column::text("blob_oid"),       // git blob oid (hex)
    Column::text("content_sha256"), // byte-compatible content hash
    Column::int("schema_version"),
];

// M:N tool→file reads, its own relation (4NF). FK: tool_call, file_revision.
const TOOL_READS_FILE_COLUMNS: &[Column] = &[
    Column::text("tool_call_id"),
    Column::text("file_revision_id"),
];

// M:N tool→file edits, distinct from reads (they vary independently).
const TOOL_EDITS_FILE_COLUMNS: &[Column] = &[
    Column::text("tool_call_id"),
    Column::text("file_revision_id"),
];

// Subagents modeled as sessions linked to a parent turn. FK: session, turn.
const SUBAGENT_SESSION_COLUMNS: &[Column] = &[
    Column::text("child_session_id"),
    Column::text("parent_turn_id"),
    Column::int("schema_version"),
];

// ----- Tokens / price / cost (P2 S4) ---------------------------------------

// 1:1 with model_call yet a separate relation (sparse, evolves independently).
const TOKEN_USAGE_COLUMNS: &[Column] = &[
    Column::text("call_id"), // FK: model_call (1:1)
    Column::int("uncached_input"),
    Column::int("cached_input"),
    Column::int("cache_creation"),
    Column::int("output"),
    Column::text("counter_method"), // 'provider_reported' | 'tiktoken' | 'estimator:<model>'
    Column::int("schema_version"),
];

// Versioned price catalog (append-only; a price change is a superseding row).
const PRICE_TABLE_COLUMNS: &[Column] = &[
    Column::text("price_id"),
    Column::text("model"),
    Column::text("token_class"),      // one of the four classes
    Column::int("unit_price_micros"), // micro-units of currency per MILLION tokens
    Column::text("currency"),
    Column::int("valid_from"), // valid-time start (provider effective date)
    Column::int_null("valid_to"), // valid-time end (NULL = current)
    Column::int("recorded_at"), // transaction-time
    Column::text_null("supersedes_id"),
    Column::int("schema_version"),
];

// Bitemporal cost estimate (04 §4.3 rule 4): never UPDATE; reconciliations
// supersede. (This is the P2 S4 `cost_estimate`; kept under the existing name.)
const COST_FACT_COLUMNS: &[Column] = &[
    Column::text("cost_fact_id"),
    Column::text("call_id"), // FK: model_call
    Column::int("amount_micros"),
    Column::int("valid_from"),    // valid-time: when the price was true
    Column::int_null("valid_to"), // NULL = still believed valid
    Column::int("recorded_at"),   // transaction-time: when Rift learned it
    Column::text_null("supersedes_id"),
    Column::int("schema_version"),
];

// ----- Attribution (P2 S5 / P4) --------------------------------------------

// Per-file authorship, keyed by (checkpoint, file_revision). Counts are u32 at
// the writer; confidence is milli-units (0..=1000). FK: checkpoint, file_revision.
const ATTRIBUTION_COLUMNS: &[Column] = &[
    Column::text("checkpoint_id"),
    Column::text("file_revision_id"),
    Column::int("agent_added"),
    Column::int("human_added"),
    Column::int("human_modified_agent"),
    Column::int("agent_removed_by_human"),
    Column::int("confidence_milli"), // ∈ 0..=1000 (writer-clamped; CHECK deferred)
    Column::text("method_version"),
    Column::text_null("supersedes_pk"),
    Column::int("schema_version"),
];

// Per-hunk attribution detail (the irreducible fact the per-file rollup sums).
const ATTRIBUTION_HUNK_COLUMNS: &[Column] = &[
    Column::text("checkpoint_id"),
    Column::text("file_revision_id"),
    Column::int("hunk_ordinal"),
    Column::int("new_start"),
    Column::int("new_len"),
    Column::text("category"), // one of the 5 categories (writer-enum; CHECK deferred)
    Column::int("line_count"),
    Column::int("confidence_milli"),
    Column::text("method_version"),
    Column::int("schema_version"),
];

// The committed-diff hunks for `C` (P4 S1). FK: file_revision.
const CHANGED_HUNK_COLUMNS: &[Column] = &[
    Column::text("hunk_id"), // = hash(commit_id, file_revision_id, old_start, new_start)
    Column::text("commit_id"),
    Column::text("file_revision_id"),
    Column::int("old_start"),
    Column::int("old_len"),
    Column::int("new_start"),
    Column::int("new_len"),
    Column::int("schema_version"),
];

// ----- Shadow-branch lifecycle (P2 S1) -------------------------------------

const SHADOW_BRANCH_COLUMNS: &[Column] = &[
    Column::text("shadow_branch_id"), // = hash(repo_id, base_commit, worktree_hash)
    Column::text("repo_id"),
    Column::text("base_commit"),   // full oid (name uses [:7])
    Column::text("worktree_hash"), // full hash (name uses [:6])
    Column::text("branch_name"),   // 'rift/<c7>-<wt6>'
    Column::int("schema_version"),
];

const SHADOW_BRANCH_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),
    Column::text("shadow_branch_id"), // FK: shadow_branch
    Column::text("kind"),             // 'created' | 'condensed' | 'gc_eligible' | 'deleted'
    Column::int("occurred_at"),
    Column::int("schema_version"),
];

// ----- Hook / lifecycle audit (P3 S2/S3/S4) --------------------------------

// Session state transitions as event rows (current phase = latest row). The
// `.git/rift-sessions/<id>.json` file is a cache of this; these rows are truth.
const SESSION_TRANSITION_COLUMNS: &[Column] = &[
    Column::text("transition_id"),
    Column::text("session_id"), // FK: session
    Column::text("from_state"),
    Column::text("to_state"),
    Column::text("trigger_kind"), // 'trigger' is a SQL keyword → renamed
    Column::int("occurred_at"),
    Column::int("schema_version"),
];

// The `rift hooks <agent> <verb>` contract audit (content-addressed → idempotent).
const AGENT_HOOK_EVENT_COLUMNS: &[Column] = &[
    Column::text("hook_event_id"), // hash(agent, verb, session, ordinal, payload_hash)
    Column::text("agent"),
    Column::text("verb"),
    Column::text_null("session_id"), // may be NULL pre-session-start
    Column::int("received_at"),
    Column::text("outcome"), // 'recorded' | 'deferred' | 'skipped-unknown-verb'
    Column::int("schema_version"),
];

// Checkpoint-strategy decision audit (P3 S4).
const ARCHIVE_DECISION_COLUMNS: &[Column] = &[
    Column::text("decision_id"),
    Column::text_null("session_id"),
    Column::text("strategy"),
    Column::text("reason"),  // 'commit' | 'pr' | 'schedule' | 'sensitive-path'
    Column::int("archived"), // 1 if a checkpoint was produced (writer-bool; CHECK deferred)
    Column::text_null("checkpoint_id"),
    Column::int("decided_at"),
    Column::int("schema_version"),
];

// ----- Wrapper audit (P1 S1/S3) --------------------------------------------

const SUBSTRATE_INSTALL_COLUMNS: &[Column] = &[
    Column::text("install_id"),
    Column::text("substrate_sha256"), // the verified hash (content-addressed identity)
    Column::text("rift_release"),
    Column::text("install_root"),
    Column::int("verified_at"),
    Column::int("schema_version"),
];

const SUBSTRATE_BLOCK_EVENT_COLUMNS: &[Column] = &[
    Column::text("block_id"),
    Column::text("verb"),   // the refused root token (never the upstream brand)
    Column::text("reason"), // 'forbidden' | 'not-allowlisted' | 'networky-arg'
    Column::int("occurred_at"),
    Column::int("schema_version"),
];

// ----- Ingest watermark (P1 S4) --------------------------------------------

const INGEST_WATERMARK_COLUMNS: &[Column] = &[
    Column::text("watermark_id"),
    Column::text("branch"),     // 'rift/checkpoints/v1'
    Column::text("branch_tip"), // git oid at time of ingest (hex)
    Column::int("ingested_at"),
    Column::int("schema_version"),
];

// ----- Verification log (P5 S4) --------------------------------------------

const VERIFICATION_RUN_COLUMNS: &[Column] = &[
    Column::text("run_id"),
    Column::text("branch"),
    Column::int("exit_code"),
    Column::int("checked_at"),
    Column::text("outcome_sha256"),
    Column::int("schema_version"),
];

const VERIFICATION_DIVERGENCE_COLUMNS: &[Column] = &[
    Column::text("run_id"), // FK: verification_run
    Column::int("ordinal"),
    Column::text("checkpoint_id"),
    Column::text("layer"), // Leaf | SessionRoot | CheckpointRoot | CommitChain | Redaction | Signature
    Column::text("site"),  // GitVsRecomputed | ProjectionVsRecomputed | GitVsProjection
    Column::text("detail"),
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

const NO_UNIQUE: &[&[&str]] = &[];

const TABLES: &[CanonicalTable] = &[
    // integrity / chain
    CanonicalTable {
        name: "checkpoint",
        columns: CHECKPOINT_COLUMNS,
        primary_key: &["checkpoint_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "session_in_checkpoint",
        columns: SESSION_IN_CHECKPOINT_COLUMNS,
        primary_key: &["checkpoint_id", "session_id"],
        unique: NO_UNIQUE,
    },
    // sessions / turns / calls / files
    CanonicalTable {
        name: "session",
        columns: SESSION_COLUMNS,
        primary_key: &["session_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "session_event",
        columns: SESSION_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "turn",
        columns: TURN_COLUMNS,
        primary_key: &["turn_id"],
        unique: &[&["session_id", "ordinal"]],
    },
    CanonicalTable {
        name: "model_call",
        columns: MODEL_CALL_COLUMNS,
        primary_key: &["call_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "tool_call",
        columns: TOOL_CALL_COLUMNS,
        primary_key: &["tool_call_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "file_revision",
        columns: FILE_REVISION_COLUMNS,
        primary_key: &["file_revision_id"],
        unique: &[&["path", "blob_oid"]],
    },
    CanonicalTable {
        name: "tool_reads_file",
        columns: TOOL_READS_FILE_COLUMNS,
        primary_key: &["tool_call_id", "file_revision_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "tool_edits_file",
        columns: TOOL_EDITS_FILE_COLUMNS,
        primary_key: &["tool_call_id", "file_revision_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "subagent_session",
        columns: SUBAGENT_SESSION_COLUMNS,
        primary_key: &["child_session_id"],
        unique: NO_UNIQUE,
    },
    // tokens / price / cost
    CanonicalTable {
        name: "token_usage",
        columns: TOKEN_USAGE_COLUMNS,
        primary_key: &["call_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "price_table",
        columns: PRICE_TABLE_COLUMNS,
        primary_key: &["price_id"],
        unique: &[&["model", "token_class", "valid_from", "recorded_at"]],
    },
    CanonicalTable {
        name: "cost_fact",
        columns: COST_FACT_COLUMNS,
        primary_key: &["cost_fact_id"],
        unique: NO_UNIQUE,
    },
    // attribution
    CanonicalTable {
        name: "attribution",
        columns: ATTRIBUTION_COLUMNS,
        primary_key: &["checkpoint_id", "file_revision_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "attribution_hunk",
        columns: ATTRIBUTION_HUNK_COLUMNS,
        primary_key: &[
            "checkpoint_id",
            "file_revision_id",
            "hunk_ordinal",
            "category",
        ],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "changed_hunk",
        columns: CHANGED_HUNK_COLUMNS,
        primary_key: &["hunk_id"],
        unique: NO_UNIQUE,
    },
    // shadow-branch lifecycle
    CanonicalTable {
        name: "shadow_branch",
        columns: SHADOW_BRANCH_COLUMNS,
        primary_key: &["shadow_branch_id"],
        unique: &[&["repo_id", "base_commit", "worktree_hash"]],
    },
    CanonicalTable {
        name: "shadow_branch_event",
        columns: SHADOW_BRANCH_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    // hook / lifecycle audit
    CanonicalTable {
        name: "session_transition",
        columns: SESSION_TRANSITION_COLUMNS,
        primary_key: &["transition_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "agent_hook_event",
        columns: AGENT_HOOK_EVENT_COLUMNS,
        primary_key: &["hook_event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "archive_decision",
        columns: ARCHIVE_DECISION_COLUMNS,
        primary_key: &["decision_id"],
        unique: NO_UNIQUE,
    },
    // wrapper audit
    CanonicalTable {
        name: "substrate_install",
        columns: SUBSTRATE_INSTALL_COLUMNS,
        primary_key: &["install_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "substrate_block_event",
        columns: SUBSTRATE_BLOCK_EVENT_COLUMNS,
        primary_key: &["block_id"],
        unique: NO_UNIQUE,
    },
    // ingest watermark
    CanonicalTable {
        name: "ingest_watermark",
        columns: INGEST_WATERMARK_COLUMNS,
        primary_key: &["watermark_id"],
        unique: NO_UNIQUE,
    },
    // verification log
    CanonicalTable {
        name: "verification_run",
        columns: VERIFICATION_RUN_COLUMNS,
        primary_key: &["run_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "verification_divergence",
        columns: VERIFICATION_DIVERGENCE_COLUMNS,
        primary_key: &["run_id", "ordinal"],
        unique: NO_UNIQUE,
    },
    // encryption-at-rest key metadata
    CanonicalTable {
        name: "key_metadata",
        columns: KEY_METADATA_COLUMNS,
        primary_key: &["key_id"],
        unique: NO_UNIQUE,
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
            // Every PK / UNIQUE column must be a real column on the table.
            let names: Vec<&str> = t.columns.iter().map(|c| c.name).collect();
            for pk in t.primary_key {
                assert!(names.contains(pk), "{}: PK {pk} not a column", t.name);
            }
            for u in t.unique {
                for col in *u {
                    assert!(names.contains(col), "{}: UNIQUE {col} not a column", t.name);
                }
            }
        }
    }

    #[test]
    fn table_names_are_unique_in_registry() {
        let tables = canonical_tables();
        for (i, a) in tables.iter().enumerate() {
            for b in &tables[i + 1..] {
                assert_ne!(a.name, b.name, "duplicate table {}", a.name);
            }
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
    fn full_m1_capture_schema_is_registered() {
        // A regression guard: every M1 capture relation must be in the registry
        // (so it gets immutability triggers + the property test).
        for required in [
            "checkpoint",
            "session_in_checkpoint",
            "session",
            "session_event",
            "turn",
            "model_call",
            "tool_call",
            "file_revision",
            "tool_reads_file",
            "tool_edits_file",
            "subagent_session",
            "token_usage",
            "price_table",
            "cost_fact",
            "attribution",
            "attribution_hunk",
            "changed_hunk",
            "shadow_branch",
            "shadow_branch_event",
            "session_transition",
            "agent_hook_event",
            "archive_decision",
            "substrate_install",
            "substrate_block_event",
            "ingest_watermark",
            "verification_run",
            "verification_divergence",
        ] {
            assert!(is_canonical_table(required), "missing table {required}");
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

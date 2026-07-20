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
    /// A closed value set. When non-empty, the DDL emits
    /// `CHECK (<name> IN ('a','b',…))` so the engine rejects any other value
    /// (the recall graph's `kind` enums use this — semantic kinds are refused
    /// until a Phase-7 migration widens the set), and the registry-driven
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
    Column::text("session_id"),    // sha256(agent, native session id)
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

// Per-canonical-event idempotency index (P8 S2). One row per *logical* event;
// the content/identity-derived key is its identity, so a re-delivered hook is a
// true no-op (no double-count, the chain never gets a duplicate leaf). Distinct
// from `agent_hook_event` (one row per hook *receipt*) — a single hook fans out to
// several canonical events, each needing independent idempotent capture.
const CAPTURED_EVENT_COLUMNS: &[Column] = &[
    Column::text("idempotency_key"), // = sha256(agent ‖ session ‖ verb ‖ event_ordinal ‖ payload_sha256)
    Column::text("session_id"),
    Column::int("turn_ordinal"),
    Column::int("event_ordinal"),
    Column::text("kind"),  // canonical EventKind token
    Column::text("trust"), // canonical Trust token
    Column::text("payload_sha256"),
    Column::int("occurred_at_ms"),
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

// Agent enable/disable history (P8 S3). enable/disable are *events*, not a mutable
// "enabled" boolean (04 §4.3 rule 2) — so the history of which agents were wired
// when is auditable and 4NF-clean (one row per action).
const AGENT_ENABLEMENT_COLUMNS: &[Column] = &[
    Column::text("change_id"), // = sha256(agent ‖ action ‖ changed_at ‖ wired_hooks_hash)
    Column::text("agent"),
    Column::text("action"), // 'enable' | 'disable'
    Column::int("changed_at"),
    Column::text("wired_hooks_hash"), // sha256 over the wired hook config (provenance)
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

// Local-only identity (phase-04 step-03): one row per distinct observed
// identity state; a change appends a new snapshot superseding the prior one.
const DEVELOPER_IDENTITY_SNAPSHOT_COLUMNS: &[Column] = &[
    Column::text("snapshot_id"), // content-addressed over the identity fields
    Column::text("developer_id"), // salted blake3 (never the raw email)
    Column::text("git_author_name"), // display only
    Column::text("git_email_normalized"), // normalization basis of developer_id
    Column::text("machine_id"),  // salted blake3 (never the raw fingerprint)
    Column::int("observed_at"),
    Column::text_null("supersedes_id"),
    Column::int("schema_version"),
];

// Session→developer attribution with explicit evidence + confidence; a
// correction is a NEW binding row superseding the prior one (04 §4.3).
const SESSION_BINDING_COLUMNS: &[Column] = &[
    Column::text("binding_id"), // content-addressed over (session, developer, basis)
    Column::text("session_id"),
    Column::text("developer_id"),
    Column::text("machine_id"),
    Column::int("confidence_milli"), // confidence × 1000 (0..=1000)
    Column::text("basis"),           // BindingBasis::as_str token
    Column::int("recorded_at"),
    Column::text_null("supersedes_id"),
    Column::int("schema_version"),
];

// ----- Turn boundaries (P4 S2, pulled into M1) ------------------------------
//
// The working-tree state observed at a turn boundary: `start` closes the
// human's H_i window (edits since the previous turn ended), `end` closes the
// agent's A_i window (edits within the turn). Only hashes and hunk coordinates
// are durable — file bytes stay in the local git object store, never here.

const TURN_BOUNDARY_SNAPSHOT_COLUMNS: &[Column] = &[
    Column::text("snapshot_id"), // = sha256(session_id ‖ turn_ordinal ‖ boundary)
    Column::text("session_id"),
    Column::int("turn_ordinal"),
    Column::text("boundary"),      // 'start' | 'end'
    Column::text("actor"),         // whose window this boundary closes: 'human' | 'agent'
    Column::text("worktree_hash"), // content hash over the boundary's (path, sha256) set
    Column::int("captured_at"),
    Column::int("schema_version"),
];

// Per-file content hash at one boundary. FK: turn_boundary_snapshot.
const TURN_BOUNDARY_FILE_COLUMNS: &[Column] = &[
    Column::text("snapshot_id"),
    Column::text("path"),
    Column::text("blob_oid"), // git blob oid of the file bytes at this boundary
    Column::text("content_sha256"), // content hash of those bytes
    Column::int("schema_version"),
];

// One H_i/A_i diff hunk (coordinates only), the edit-stack input the LIFO
// attribution replay consumes at commit time.
const TURN_DIFF_HUNK_COLUMNS: &[Column] = &[
    Column::text("hunk_id"), // = sha256(session ‖ ordinal ‖ edit_kind ‖ path ‖ coords)
    Column::text("session_id"),
    Column::int("turn_ordinal"),
    Column::text("edit_kind"), // 'human' (H_i) | 'agent' (A_i)
    Column::text("path"),
    Column::int("old_start"),
    Column::int("old_len"),
    Column::int("new_start"),
    Column::int("new_len"),
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
        name: "captured_event",
        columns: CAPTURED_EVENT_COLUMNS,
        primary_key: &["idempotency_key"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "agent_enablement",
        columns: AGENT_ENABLEMENT_COLUMNS,
        primary_key: &["change_id"],
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
    // --- capture v3 additive (identity): past CAPTURE_V1_TABLE_COUNT ---------
    // Local-only developer identity (phase-04 step-03): hashed ids only, the
    // raw email/fingerprint never reach the projection; corrections supersede.
    CanonicalTable {
        name: "developer_identity_snapshot",
        columns: DEVELOPER_IDENTITY_SNAPSHOT_COLUMNS,
        primary_key: &["snapshot_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "session_binding",
        columns: SESSION_BINDING_COLUMNS,
        primary_key: &["binding_id"],
        unique: NO_UNIQUE,
    },
    // --- capture v4 additive (turn boundaries, P4 S2): past the identity set --
    CanonicalTable {
        name: "turn_boundary_snapshot",
        columns: TURN_BOUNDARY_SNAPSHOT_COLUMNS,
        primary_key: &["snapshot_id"],
        unique: &[&["session_id", "turn_ordinal", "boundary"]],
    },
    CanonicalTable {
        name: "turn_boundary_file",
        columns: TURN_BOUNDARY_FILE_COLUMNS,
        primary_key: &["snapshot_id", "path"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "turn_diff_hunk",
        columns: TURN_DIFF_HUNK_COLUMNS,
        primary_key: &["hunk_id"],
        unique: NO_UNIQUE,
    },
    // --- capture v5 additive (projection meta, ADR-0010): past the v4 set ------
    // Key/value projection facts; today exactly one key ('rebuild_epoch'). The
    // row is written once per projection lifetime — `rift_store::rebuild`
    // recreates the file and inserts old-epoch+1 — so the append-only guards
    // hold without any UPDATE. An absent row reads as epoch 0.
    CanonicalTable {
        name: "projection_meta",
        columns: PROJECTION_META_COLUMNS,
        primary_key: &["key"],
        unique: NO_UNIQUE,
    },
    // --- capture v6 additive (read tokens, M6-12): past the v5 set -------------
    // The token estimate for one READ's bytes, measured at read time — the only
    // moment the bytes are certainly in hand (a modified working file's
    // read-time content may exist nowhere else). Keyed by the tool call, not
    // the revision: fact and estimate are written by the same hook, so a
    // session's archived lines can never be mutated retroactively by another
    // session measuring the same revision later. `tokenizer` records
    // provenance ("skeptical buyer": every count names its measurer); only
    // real BPE measurements are persisted — the chars/4 fallback never lands.
    CanonicalTable {
        name: "file_read_tokens",
        columns: FILE_READ_TOKENS_COLUMNS,
        primary_key: &["tool_call_id"],
        unique: NO_UNIQUE,
    },
    // --- capture v7 additive (truthful System panel, M5.14) -----------------
    CanonicalTable {
        name: "capture_session_identity",
        columns: CAPTURE_SESSION_IDENTITY_COLUMNS,
        primary_key: &["session_id"],
        unique: &[&["agent", "native_session_id"]],
    },
    CanonicalTable {
        name: "main_context_observation",
        columns: MAIN_CONTEXT_OBSERVATION_COLUMNS,
        primary_key: &["observation_id"],
        unique: NO_UNIQUE,
    },
];

/// Columns for the v5 `projection_meta` key/value table (ADR-0010).
const PROJECTION_META_COLUMNS: &[Column] = &[
    Column::text("key"),  // e.g. 'rebuild_epoch'
    Column::int("value"), // the integer fact
    Column::int("schema_version"),
];

/// Columns for the v6 `file_read_tokens` table (M6-12). FK: tool_call.
const FILE_READ_TOKENS_COLUMNS: &[Column] = &[
    Column::text("tool_call_id"), // FK: tool_call (1:1, sparse)
    Column::int("input_tokens"),  // measured over the read-time bytes
    Column::text("tokenizer"),    // e.g. 'cl100k_base' (provenance)
    Column::int("schema_version"),
];

/// Columns for the v7 native-to-canonical capture-session identity relation.
const CAPTURE_SESSION_IDENTITY_COLUMNS: &[Column] = &[
    Column::text("session_id"),
    Column::text("agent"),
    Column::text("native_session_id"),
    Column::int("schema_version"),
];

/// Columns for prospective main-chain context snapshots. These remain
/// intentionally distinct from whole-turn `token_usage` spend rows.
const MAIN_CONTEXT_OBSERVATION_COLUMNS: &[Column] = &[
    Column::text("observation_id"),
    Column::text("session_id"),
    Column::text("model"),
    Column::int("observed_at"),
    Column::int("uncached_input"),
    Column::int("cached_input"),
    Column::int("cache_creation"),
    Column::int("output"),
    Column::text("counter_method"),
    Column::int("schema_version"),
];

/// Frozen v1 prefix of the capture registry: the tables shipped in the
/// `canonical_core` migration, whose SQL is checksum-locked once applied.
/// Append new tables AFTER this index and ship them in a NEW migration —
/// growing or reordering the prefix would change `canonical_core`'s generated
/// SQL and trip the checksum lock on every existing projection. Same contract
/// as [`RECALL_V1_TABLE_COUNT`]; `capture_core_v1_sql_is_byte_stable` (in
/// `crate::migrate`) pins the generated bytes.
pub(crate) const CAPTURE_V1_TABLE_COUNT: usize = 30;

/// The frozen v1 capture tables (the `canonical_core` migration source).
#[must_use]
// indexing_slicing: compile-time constant bound; `capture_registry_growth_
// lands_past_v1_freeze` pins the boundary, and any drift panics in every test.
#[allow(clippy::indexing_slicing)]
pub(crate) fn capture_v1_tables() -> &'static [CanonicalTable] {
    &TABLES[..CAPTURE_V1_TABLE_COUNT]
}

/// Boundary after the v3 (`capture_identity`) tables. Like the v1 freeze, this
/// slice end is locked once v3 has shipped: the identity migration's SQL is
/// generated from exactly `TABLES[CAPTURE_V1_TABLE_COUNT..CAPTURE_IDENTITY_TABLE_COUNT]`,
/// so later tables must append past it and ship in their own migration.
pub(crate) const CAPTURE_IDENTITY_TABLE_COUNT: usize = 32;

/// Boundary after the v4 (`capture_turn_boundary`) tables. Frozen once v4
/// shipped; later tables append past it and ship in their own migration.
pub(crate) const CAPTURE_TURN_BOUNDARY_TABLE_COUNT: usize = 35;

/// Boundary after the v5 (`capture_projection_meta`) table. Frozen once v5
/// shipped; later tables append past it and ship in their own migration.
pub(crate) const CAPTURE_PROJECTION_META_TABLE_COUNT: usize = 36;

/// Boundary after the v6 (`capture_read_tokens`) table — the current end of
/// the capture registry. The next additive capture migration starts here.
pub(crate) const CAPTURE_READ_TOKENS_TABLE_COUNT: usize = 37;

/// Boundary after the v7 M5.14 identity and context-observation relations.
pub(crate) const CAPTURE_SYSTEM_TABLE_COUNT: usize = 39;

/// The v3 `capture_identity` tables (frozen once shipped — see
/// [`CAPTURE_IDENTITY_TABLE_COUNT`]).
#[must_use]
#[allow(clippy::indexing_slicing)] // same compile-time bound as above
pub(crate) fn capture_identity_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_V1_TABLE_COUNT..CAPTURE_IDENTITY_TABLE_COUNT]
}

/// The v4 `capture_turn_boundary` tables (P4 S2, pulled into M1).
#[must_use]
#[allow(clippy::indexing_slicing)] // same compile-time bound as above
pub(crate) fn capture_turn_boundary_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_IDENTITY_TABLE_COUNT..CAPTURE_TURN_BOUNDARY_TABLE_COUNT]
}

/// The v5 `capture_projection_meta` table (ADR-0010's rebuild epoch).
#[must_use]
#[allow(clippy::indexing_slicing)] // same compile-time bound as above
pub(crate) fn capture_projection_meta_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_TURN_BOUNDARY_TABLE_COUNT..CAPTURE_PROJECTION_META_TABLE_COUNT]
}

/// The v6 `file_revision_tokens` table (M6-12's read-time token estimates).
#[must_use]
#[allow(clippy::indexing_slicing)] // same compile-time bound as above
pub(crate) fn capture_read_tokens_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_PROJECTION_META_TABLE_COUNT..CAPTURE_READ_TOKENS_TABLE_COUNT]
}

/// The v7 M5.14 session-identity and main-context observation tables.
#[must_use]
#[allow(clippy::indexing_slicing)]
pub(crate) fn capture_system_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_READ_TOKENS_TABLE_COUNT..CAPTURE_SYSTEM_TABLE_COUNT]
}

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

// =========================================================================
// Stage 3 — the recall projection (`recall.db`)
// =========================================================================
//
// A SEPARATE registry + database file from the M1 capture projection
// (`projection.db`): the recall graph is the read-side, rebuildable delta
// ledger, and `rift-memd rebuild` destroys only `recall.db`. It reuses this
// crate's migration + immutability machinery (registered canonical → guard
// triggers → the registry-driven property test) via [`recall_canonical_tables`].
//
// The model is the generic node/edge graph from `04-database-design §4.5`:
// `graph_node` + typed `graph_edge`, with the **payload of each node kind in
// its own typed `n_*` relation** (4NF). Mechanical facts only — the `kind`
// columns are `CHECK`-constrained to the v1 set, so a semantic kind
// (`similar_to`/`touches`/`frame`) is refused until a Phase-7 migration widens
// the set. Foreign keys are documented in comments, not emitted as SQL
// `REFERENCES` — same posture as the M1 capture schema (the single writer
// guarantees referential integrity; enforced FKs are a Stage-6 item).

/// The v1 **mechanical** node kinds. The full set is declared now (even for the
/// typed tables deferred to the follow-up) so the `CHECK` never needs widening;
/// semantic kinds are simply absent and therefore rejected.
pub const RECALL_NODE_KINDS: &[&str] = &[
    "Checkpoint",
    "Session",
    "Turn",
    "ModelCall",
    "TokenUsage",
    "CostEstimate",
    "ToolCall",
    "ToolResult",
    "FileRevision",
    "FileRead",
    "ChangedHunk",
    "TestRun",
    "ErrorSignature",
    "Commit",
    "Branch",
    "PR",
    "Summary",
    "Handle",
];

/// The v1 **mechanical** edge kinds (incl. the retention-lineage edges). A
/// semantic edge (`similar_to`/`touches`/`frame`) is not here, so the `CHECK`
/// on `graph_edge.kind` rejects it until Phase 7 adds it by migration.
pub const RECALL_EDGE_KINDS: &[&str] = &[
    "created",
    "linked_to",
    "called",
    "read",
    "produced",
    "changed",
    "in",
    "failed_with",
    "spent",
    "binds",
    "invalidated_by",
    "derived_from",
    "deletion_depends_on",
];

// --- generic graph ---------------------------------------------------------
const GRAPH_NODE_COLUMNS: &[Column] = &[
    Column::text("node_id"),                        // content hash or identity id
    Column::text_one_of("kind", RECALL_NODE_KINDS), // CHECK-constrained to v1
    Column::text("ref_id"),                         // FK into the kind's typed table (comment-FK)
    Column::int("created_at"),                      // transaction-time (epoch ms)
    Column::int("schema_version"),
];
// Typed edges are their OWN relation (4NF). `UNIQUE(from,to,kind)` makes
// re-ingest a set-union no-op (04 §4.4).
const GRAPH_EDGE_COLUMNS: &[Column] = &[
    Column::text("edge_id"),
    Column::text("from_node"), // → graph_node(node_id) (comment-FK)
    Column::text("to_node"),   // → graph_node(node_id) (comment-FK)
    Column::text_one_of("kind", RECALL_EDGE_KINDS),
    Column::int("created_at"),
];
const EDGE_UNIQUE: &[&[&str]] = &[&["from_node", "to_node", "kind"]];

// --- typed payload relations (the core subset that carries v1 data) --------
const N_CHECKPOINT_COLUMNS: &[Column] = &[
    Column::text("node_id"), // = checkpoint_id
    Column::text("dialect"), // 'rift' | 'entire'
    Column::int("ordinal"),  // deterministic walk position
    Column::text_null("prev_checkpoint"),
    Column::text_null("strategy"),
    Column::text_null("merkle_root"),
    Column::text_null("developer"), // owning developer (scope axis; NULL = current)
];
const N_SESSION_COLUMNS: &[Column] = &[
    Column::text("node_id"),        // = session id
    Column::text_null("developer"), // scope axis (NULL = current/unknown)
];
const N_TOOL_CALL_COLUMNS: &[Column] = &[
    Column::text("node_id"),       // = tool_use_id
    Column::text("checkpoint_id"), // denormalized for scope filtering
    Column::text("tool_use_id"),
    Column::text_null("tool_name"),
];
// A path@hash is ONE immutable fact: node_id = sha256(path ‖ 0x00 ‖ content_sha256)
// (mirrors the M1 `file_revision` keying), so the same path@content dedupes
// across checkpoints yet two paths with identical content stay distinct.
const N_FILE_REVISION_COLUMNS: &[Column] = &[
    Column::text("node_id"),
    Column::text("path"),
    Column::text("blob_oid"), // = content_sha256 in v1 (real git OID: follow-up)
    Column::text("content_sha256"), // the validity comparator (worktree hasher emits SHA-256)
    Column::int("size"),
];
// The irreducible (tool_call, file_revision, range) read triple (5NF). Carries
// the captured `input_tokens` + a deterministic `read_ordinal` so the metric
// can rank 2nd..Nth reads without a tokenizer.
const N_FILE_READ_COLUMNS: &[Column] = &[
    Column::text("node_id"),
    Column::text("checkpoint_id"), // denormalized for scope filtering
    Column::text("tool_call_id"),
    Column::text("file_revision"), // → n_file_revision(node_id) (comment-FK)
    Column::int("input_tokens"),
    Column::int("read_ordinal"), // shard.ordinal * 1e6 + index (deterministic)
    Column::int_null("byte_range_lo"),
    Column::int_null("byte_range_hi"),
];
const N_TOKEN_USAGE_COLUMNS: &[Column] = &[
    Column::text("node_id"),
    Column::text("checkpoint_id"),
    Column::text("call_id"),
    Column::int("uncached_in"),
    Column::int("cached_in"),
    Column::int("cache_write"),
    Column::int("output"),
];
const N_CHANGED_HUNK_COLUMNS: &[Column] = &[
    Column::text("node_id"),   // = sha256(old_rev ‖ new_rev ‖ ranges)
    Column::text("commit_id"), // = the checkpoint id grouping the change in v1
    Column::text("file_revision"),
    Column::int_null("old_start"),
    Column::int_null("old_len"),
    Column::int_null("new_start"),
    Column::int_null("new_len"),
    Column::int("is_binary"), // 1 = whole-file binary hunk (never line-diffed)
];

// --- validity / digest store (P3) ------------------------------------------
// summary_id = sha256(source ‖ extractor_ver ‖ policy_ver ‖ redaction_ver).
// NO `embedding` column in v1 (rusqlite has no native vector; Turso adds it —
// see the `rift-turso-rewrite-deferral` memory). Validity is COMPUTED, so there
// is deliberately no mutable `valid`/`status` column.
const SUMMARY_BINDING_COLUMNS: &[Column] = &[
    Column::text("summary_id"),
    Column::text("path"),
    Column::text("source_sha256"),
    Column::text("bound_blob_oid"),
    Column::int("extractor_ver"),
    Column::int("policy_ver"),
    Column::int("redaction_ver"),
    Column::text("digest_ref"), // → digest_blob(digest_ref)
    Column::int("created_at"),
];
// The content-addressed digest store (digest_ref = sha256(bytes)).
const DIGEST_BLOB_COLUMNS: &[Column] = &[
    Column::text("digest_ref"),
    Column::blob("bytes"),
    Column::int("created_at"),
];

// --- lineage (W12) + metric trend ------------------------------------------
const RETENTION_LINEAGE_COLUMNS: &[Column] = &[
    Column::text("node_id"),
    Column::text("source_artifact"),
    Column::text("retention_class"), // 'mechanical' | 'derived' | 'sensitive-derived'
    Column::text_null("derived_from"),
    Column::text_null("deletion_depends_on"),
    Column::int("created_at"),
];
const METRIC_SNAPSHOT_COLUMNS: &[Column] = &[
    Column::text("snapshot_id"), // ULID
    Column::text("scope_hash"),
    Column::int("total_input_tokens"),
    Column::int("repeated_read_tokens"),
    Column::int("avoidable_tokens"),
    Column::int("computed_at"),
];

// --- outcome labels (P4 S2, W8) --------------------------------------------
// Bitemporal, append-only beliefs about a subject's lifecycle, derived from the
// git lifecycle (reachability/revert/branch state), NEVER an agent assertion. A
// correction is a NEW row whose `supersedes` points at the prior belief — there
// is no mutable `effective`/`current` flag; the effective label is COMPUTED
// (latest non-superseded as-of a query time). `label_id` is content-addressed
// (`sha256(subject ‖ kind ‖ valid_from ‖ evidence)`) so re-derivation on rebuild
// is an idempotent set-union.
/// The closed set of outcome-label subject kinds (mirrors `LabelSubject`).
pub const OUTCOME_LABEL_SUBJECT_KINDS: &[&str] = &["session", "checkpoint", "file_revision"];
/// The closed set of outcome-label kinds (mirrors `rift_recall_types::OUTCOME_KINDS`).
pub const OUTCOME_LABEL_KINDS: &[&str] = &[
    "merged",
    "tests_passed",
    "reverted",
    "abandoned",
    "low_human_mod",
    "high_human_mod",
];
const OUTCOME_LABEL_COLUMNS: &[Column] = &[
    Column::text("label_id"), // = sha256(subject ‖ kind ‖ valid_from ‖ evidence)
    Column::text_one_of("subject_kind", OUTCOME_LABEL_SUBJECT_KINDS),
    Column::text("subject_id"),
    Column::text_one_of("kind", OUTCOME_LABEL_KINDS),
    Column::int("valid_from"), // valid-time: when it became true in the world
    Column::int("observed_at"), // transaction-time: when Rift learned it
    Column::text("evidence_ref"), // commit oid / CI id / attribution snapshot
    Column::text_null("supersedes"), // → outcome_label(label_id) (comment-FK); NULL = original
];

// --- memd serve audit (P5/P7 — the `memd.*` serving-decision trail) ---------
// Every recall serve decision (manifest/summary/exact/pack/invalidated/refused)
// lands here through the `rift hooks memd` verb: append-only, content-addressed
// on the event's idempotency key (a re-delivered handoff is a no-op). Neutral by
// construction — `handle`/`reason` name *what* and *why*, never the withheld
// bytes (05-security §5.5, W10). Exact retrieval is audited BEFORE bytes return.
const MEMD_AUDIT_EVENT_COLUMNS: &[Column] = &[
    Column::text("audit_id"), // = the event's content-addressed idempotency key
    Column::text("verb"),     // 'memd.manifest_served' | … | 'memd.refused'
    Column::text_null("handle"), // the target/handle (neutral; never withheld bytes)
    Column::text_null("reason"), // neutral reason tag (refusals) / exact-retrieval reason
    Column::int("tokens_returned"),
    Column::int("tokens_avoided"),
    Column::int("recorded_at"),
    Column::int("schema_version"),
];

// --- attributed MCP calls (per process/connection, per decoded invocation) --
// Unlike `memd_audit_event`, whose legacy ids are outcome-content addressed,
// this relation uses an invocation id. It therefore counts two identical calls
// twice while still making replay of one hook envelope idempotent. Only neutral
// routing/metric facts live here; tool arguments and transcript bytes never do.
const MEMD_CALL_EVENT_COLUMNS: &[Column] = &[
    Column::text("call_id"),
    Column::text("thread_id"),
    Column::text("tool_name"),
    Column::text("terminal_verb"),
    Column::int("tokens_returned"),
    Column::int("tokens_avoided"),
    Column::int("recorded_at"),
    Column::int("schema_version"),
];

// --- content→blob-oid map (ADR-0005) ---------------------------------------
// Handles bind content by `content_sha256` (the hash of the file bytes as read),
// but git addresses blobs by object id (`sha1("blob <len>\0" ‖ bytes)`). This
// append-only relation carries the bridge, computed by the worktree hasher in the
// same pass as `content_sha256`, so durable `file:` handle resolution and exact
// retrieval reach real git objects. PK = `content_sha256` (one blob per content);
// content-addressed → re-ingest is a set-union no-op. Resolution stays
// self-validating: after fetching by OID, the resolver re-hashes and checks the
// requested `content_sha256` before serving.
const BLOB_OID_MAP_COLUMNS: &[Column] = &[
    Column::text("content_sha256"), // the validity/handle comparator (PK)
    Column::text("blob_oid"),       // git blob object id (SHA-1 framing)
    Column::text("path"),           // first path these bytes were seen at (lineage)
    Column::int("first_seen"),      // transaction-time (epoch ms)
];

const RECALL_TABLES: &[CanonicalTable] = &[
    CanonicalTable {
        name: "graph_node",
        columns: GRAPH_NODE_COLUMNS,
        primary_key: &["node_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "graph_edge",
        columns: GRAPH_EDGE_COLUMNS,
        primary_key: &["edge_id"],
        unique: EDGE_UNIQUE,
    },
    CanonicalTable {
        name: "n_checkpoint",
        columns: N_CHECKPOINT_COLUMNS,
        primary_key: &["node_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "n_session",
        columns: N_SESSION_COLUMNS,
        primary_key: &["node_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "n_tool_call",
        columns: N_TOOL_CALL_COLUMNS,
        primary_key: &["node_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "n_file_revision",
        columns: N_FILE_REVISION_COLUMNS,
        primary_key: &["node_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "n_file_read",
        columns: N_FILE_READ_COLUMNS,
        primary_key: &["node_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "n_token_usage",
        columns: N_TOKEN_USAGE_COLUMNS,
        primary_key: &["node_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "n_changed_hunk",
        columns: N_CHANGED_HUNK_COLUMNS,
        primary_key: &["node_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "summary_binding",
        columns: SUMMARY_BINDING_COLUMNS,
        primary_key: &["summary_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "digest_blob",
        columns: DIGEST_BLOB_COLUMNS,
        primary_key: &["digest_ref"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "retention_lineage",
        columns: RETENTION_LINEAGE_COLUMNS,
        primary_key: &["node_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "metric_snapshot",
        columns: METRIC_SNAPSHOT_COLUMNS,
        primary_key: &["snapshot_id"],
        unique: NO_UNIQUE,
    },
    // --- v2 additive (P4 S2): [RECALL_V1_TABLE_COUNT, RECALL_V2_TABLE_COUNT) --
    CanonicalTable {
        name: "outcome_label",
        columns: OUTCOME_LABEL_COLUMNS,
        primary_key: &["label_id"],
        unique: NO_UNIQUE,
    },
    // --- v3 additive (Layer-2 custody): [RECALL_V2_TABLE_COUNT, RECALL_V3_TABLE_COUNT).
    // recall.db carries its own wrapped-DEK custody rows (same shape as the
    // capture projection's table) so each projection is self-contained.
    CanonicalTable {
        name: "key_metadata",
        columns: KEY_METADATA_COLUMNS,
        primary_key: &["key_id"],
        unique: NO_UNIQUE,
    },
    // --- v4 additive (memd serve audit): past RECALL_V3_TABLE_COUNT ------------
    CanonicalTable {
        name: "memd_audit_event",
        columns: MEMD_AUDIT_EVENT_COLUMNS,
        primary_key: &["audit_id"],
        unique: NO_UNIQUE,
    },
    // --- v5 additive (ADR-0005 blob-oid map): past RECALL_V4_TABLE_COUNT -------
    CanonicalTable {
        name: "n_blob_oid_map",
        columns: BLOB_OID_MAP_COLUMNS,
        primary_key: &["content_sha256"],
        unique: NO_UNIQUE,
    },
    // --- v6 additive (durable ingest watermark): past RECALL_V5_TABLE_COUNT ----
    // The recall ingest path appends one row per run (branch + last checkpoint
    // ingested), so incremental ingest is durable evidence, not just an
    // in-memory `Watermark` (PRD-M2 audit reconciliation). Same column shape as
    // the capture projection's table; separate DB, separate rows.
    CanonicalTable {
        name: "ingest_watermark",
        columns: INGEST_WATERMARK_COLUMNS,
        primary_key: &["watermark_id"],
        unique: NO_UNIQUE,
    },
    // --- v7 additive (per-thread MCP call attribution) -----------------------
    CanonicalTable {
        name: "memd_call_event",
        columns: MEMD_CALL_EVENT_COLUMNS,
        primary_key: &["call_id"],
        unique: NO_UNIQUE,
    },
    // --- v8 additive (ADR-0042 eval experiment records) ----------------------
    CanonicalTable {
        name: "eval_run",
        columns: EVAL_RUN_COLUMNS,
        primary_key: &["run_id"],
        unique: NO_UNIQUE,
    },
];

/// The number of v1 (`recall_core` migration) recall tables. Tables after this
/// index were introduced by later **additive** migrations, so the v1 migration
/// builds only from the prefix and its checksum stays stable when the registry
/// grows (the additive-only, checksum-locked migration contract).
pub(crate) const RECALL_V1_TABLE_COUNT: usize = 13;

/// One past the last v2 (`recall_outcome_labels` migration) table.
pub(crate) const RECALL_V2_TABLE_COUNT: usize = 14;

/// One past the last v3 (`recall_key_custody` migration) table. Frozen once v3
/// shipped: the custody migration's SQL is generated from exactly the v2..v3
/// slice, so later tables append past it and ship in their own migration.
pub(crate) const RECALL_V3_TABLE_COUNT: usize = 15;

/// One past the last v4 (`recall_memd_audit` migration) table.
pub(crate) const RECALL_V4_TABLE_COUNT: usize = 16;

/// One past the last v5 (`recall_blob_oid_map` migration) table.
pub(crate) const RECALL_V5_TABLE_COUNT: usize = 17;

/// One past the last v6 (`recall_ingest_watermark` migration) table.
pub(crate) const RECALL_V6_TABLE_COUNT: usize = 18;

/// One past the last v7 (`recall_memd_calls` migration) table. Frozen once v7
/// shipped: later tables append past it and ship in their own migration.
pub(crate) const RECALL_V7_TABLE_COUNT: usize = 19;

/// The v1 recall tables (the `recall_core` migration's source of truth).
#[allow(clippy::indexing_slicing)] // compile-time constant bound (see capture_v1_tables)
pub(crate) fn recall_v1_tables() -> &'static [CanonicalTable] {
    &RECALL_TABLES[..RECALL_V1_TABLE_COUNT]
}

/// The v2-additive recall tables (the `recall_outcome_labels` migration).
#[allow(clippy::indexing_slicing)] // compile-time constant bound (see capture_v1_tables)
pub(crate) fn recall_v2_outcome_tables() -> &'static [CanonicalTable] {
    &RECALL_TABLES[RECALL_V1_TABLE_COUNT..RECALL_V2_TABLE_COUNT]
}

/// The v3-additive recall tables (the `recall_key_custody` migration).
#[allow(clippy::indexing_slicing)] // compile-time constant bound (see capture_v1_tables)
pub(crate) fn recall_v3_custody_tables() -> &'static [CanonicalTable] {
    &RECALL_TABLES[RECALL_V2_TABLE_COUNT..RECALL_V3_TABLE_COUNT]
}

/// The v4-additive recall tables (the `recall_memd_audit` migration): the
/// append-only `memd.*` serve-decision trail.
#[allow(clippy::indexing_slicing)] // compile-time constant bound (see capture_v1_tables)
pub(crate) fn recall_v4_audit_tables() -> &'static [CanonicalTable] {
    &RECALL_TABLES[RECALL_V3_TABLE_COUNT..RECALL_V4_TABLE_COUNT]
}

/// The v5-additive recall tables (the `recall_blob_oid_map` migration): the
/// ADR-0005 content→blob-oid bridge. Frozen once v5 shipped (the next additive
/// migration starts past [`RECALL_V5_TABLE_COUNT`]).
#[allow(clippy::indexing_slicing)] // compile-time constant bound (see capture_v1_tables)
pub(crate) fn recall_v5_blob_map_tables() -> &'static [CanonicalTable] {
    &RECALL_TABLES[RECALL_V4_TABLE_COUNT..RECALL_V5_TABLE_COUNT]
}

/// The v6-additive recall tables (the `recall_ingest_watermark` migration): the
/// durable incremental-ingest watermark. Frozen once v6 shipped.
#[allow(clippy::indexing_slicing)] // compile-time constant bound (see capture_v1_tables)
pub(crate) fn recall_v6_watermark_tables() -> &'static [CanonicalTable] {
    &RECALL_TABLES[RECALL_V5_TABLE_COUNT..RECALL_V6_TABLE_COUNT]
}

/// The v7-additive recall tables: one neutral terminal fact per decoded MCP
/// invocation, attributed to the serving process/connection. Frozen once v7
/// shipped (the next additive migration starts past [`RECALL_V7_TABLE_COUNT`]).
#[allow(clippy::indexing_slicing)] // compile-time constant bound (see capture_v1_tables)
pub(crate) fn recall_v7_call_tables() -> &'static [CanonicalTable] {
    &RECALL_TABLES[RECALL_V6_TABLE_COUNT..RECALL_V7_TABLE_COUNT]
}

/// The v8-additive recall tables (ADR-0042): the append-only bitemporal eval
/// experiment records the M8 gate reads history from.
#[allow(clippy::indexing_slicing)] // compile-time constant bound (see capture_v1_tables)
pub(crate) fn recall_v8_eval_tables() -> &'static [CanonicalTable] {
    &RECALL_TABLES[RECALL_V7_TABLE_COUNT..]
}

/// Secondary indexes for the recall graph (appended after the tables in the
/// recall migration). Additive; tuned against the metric/scope/hunk queries.
pub const RECALL_INDEX_SQL: &str = "\
CREATE INDEX IF NOT EXISTS idx_node_kind ON graph_node(kind);\n\
CREATE INDEX IF NOT EXISTS idx_edge_from ON graph_edge(from_node, kind);\n\
CREATE INDEX IF NOT EXISTS idx_edge_to   ON graph_edge(to_node, kind);\n\
CREATE INDEX IF NOT EXISTS idx_fread_rev  ON n_file_read(file_revision);\n\
CREATE INDEX IF NOT EXISTS idx_fread_ckpt ON n_file_read(checkpoint_id);\n";

/// Secondary index for the outcome-label store (the v2 additive migration).
/// Looks up a subject's belief history for effective-label resolution.
pub const RECALL_OUTCOME_INDEX_SQL: &str =
    "CREATE INDEX IF NOT EXISTS idx_outcome_subject ON outcome_label(subject_kind, subject_id);\n";

/// Indexes for event-anchored call windows and per-thread grouping in Canopy.
pub const RECALL_CALL_EVENT_INDEX_SQL: &str = "\
CREATE INDEX IF NOT EXISTS idx_memd_call_recorded ON memd_call_event(recorded_at);\n\
CREATE INDEX IF NOT EXISTS idx_memd_call_thread_recorded ON memd_call_event(thread_id, recorded_at);\n";

// --- eval experiment records (ADR-0042, M6-07) ------------------------------
// Append-only bitemporal verdicts of the Lost-in-Delta harness: what the code
// concluded, when it was true, and when we learned it. NOT rebuildable from
// the substrate (it captures what-code-concluded-when) — exactly why the rows
// are bitemporal and append-only, mirroring `verification_run`'s posture. The
// run id is content-addressed (`sha256(task_set ‖ condition ‖ code_version)`)
// so re-running an identical experiment is an idempotent no-op. Percentages
// are stored as **basis points** (pct × 100, integer) so the record stays
// byte-deterministic across platforms.

/// The closed set of eval conditions (ADR-0019: deterministic conditions gate;
/// real-model runs are recorded but never gate).
pub const EVAL_RUN_CONDITIONS: &[&str] =
    &["served", "synthetic", "seeded-regression", "real-model"];

/// The closed verdict vocabulary (mirrors `rift_recall_eval::Verdict`).
pub const EVAL_RUN_VERDICTS: &[&str] = &["go", "no-go"];

const EVAL_RUN_COLUMNS: &[Column] = &[
    Column::text("run_id"), // = sha256(task_set ‖ condition ‖ code_version)
    Column::text("task_set"),
    Column::text_one_of("condition", EVAL_RUN_CONDITIONS),
    Column::text("code_version"),
    Column::int("thresholds_version"), // the GaThresholds the verdict used
    Column::int("runs"),               // paired runs scored
    Column::int("p10_token_savings_bp"),
    Column::int("p50_token_savings_bp"), // the gated median, basis points
    Column::int("p90_token_savings_bp"),
    Column::int("p50_cost_savings_bp"),
    Column::int("baseline_tokens"),          // Σ full-read baseline
    Column::int("delta_tokens"),             // Σ delta-served spend
    Column::int("emitted_avoided_tokens"),   // Σ serving-side claims
    Column::int("reconcile_over_estimates"), // claims flagged over ±5%
    Column::int("max_latency_steps"),
    Column::text_one_of("verdict", EVAL_RUN_VERDICTS),
    Column::text_null("failing_dimensions"), // comma-joined, NULL on go
    Column::int("valid_from"),               // valid-time: when the verdict held
    Column::int_null("valid_to"),            // NULL = still the current belief
    Column::int("recorded_at"),              // transaction-time: when recorded
    Column::int("schema_version"),
];

/// The registry of recall (`recall.db`) canonical tables (v1 core + additive).
/// The immutability property test iterates this **in addition to**
/// [`canonical_tables`], so every recall table gets its append-only guard for free.
#[must_use]
pub fn recall_canonical_tables() -> &'static [CanonicalTable] {
    RECALL_TABLES
}

/// Look up a recall canonical table by name.
#[must_use]
pub fn recall_canonical_table(name: &str) -> Option<&'static CanonicalTable> {
    RECALL_TABLES.iter().find(|t| t.name == name)
}

// --- Canopy UI-state (Stage 4 P4 S1 / P5 S2) -------------------------------
// Machine-global, append-only UI facts: which projects the dashboard watches and
// each project's feature-toggle intent. A SEPARATE registry + db file
// (`ui-state.db`) from the per-repo capture projection, so these never touch
// capture data — yet they still obey the immutability discipline (04 §4.3): an
// `op`/`desired` change is a NEW event row (never an in-place UPDATE), and the
// current state is the fold of the latest-valid event per key. `op`/`feature` are
// plain `TEXT` validated at the Rust writer (the M1 "CHECK deferred" posture), so
// the store stays decoupled from Canopy's enum spelling.

const CANOPY_PROJECT_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("project_id"),      // stable per registered project
    Column::text("op"),              // 'added' | 'removed' | 'relabeled' | 'reordered'
    Column::text_null("repo_id"),    // set on 'added'
    Column::text_null("path"),       // set on 'added'
    Column::text_null("label"),      // set on 'added'/'relabeled'
    Column::int_null("order_key"),   // set on 'reordered'
    Column::text_null("supersedes"), // prior event_id this corrects (04 §4.3)
    Column::int("created_at"),       // transaction-time (epoch ms)
];

const CANOPY_TOGGLE_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("project_id"),      // FK: a canopy_project_event.project_id (comment-FK)
    Column::text("feature"), // 'fff' | 'codegraph' | 'codegraph-typescript' | … | 'rift-total-recall'
    Column::int("desired"),  // 1 = on, 0 = off (the user's intent at this time)
    Column::text_null("supersedes"), // prior event_id for this (project,feature)
    Column::int("created_at"), // transaction-time (epoch ms)
];

// Per-project dashboard arrangement (ADR-0035): the panel editor's layout, saved
// per project_id as a superseding event. `layout` is the encoded LayoutModel;
// NULL is a reset to the default (a superseding event, never a delete).
const CANOPY_LAYOUT_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("project_id"),      // FK: a canopy_project_event.project_id (comment-FK)
    Column::text_null("layout"),     // encoded LayoutModel; NULL = reset-to-default
    Column::text_null("supersedes"), // prior layout event_id for this project
    Column::int("created_at"),       // transaction-time (epoch ms)
];

// The profile plane (ADR-0036): named, machine-global operator views. A profile
// scopes what the dashboard shows — its own membership over the global catalogue
// (`canopy_project_event` stays the registration source of truth), its own
// per-(profile, project) arrangements, and (ADR-0038) its own theme slots. All
// per-key, time-varying, superseding-event state like its siblings above.
const CANOPY_PROFILE_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("profile_id"),      // stable per profile (ULID)
    Column::text("op"),              // 'created' | 'renamed' | 'removed' | 'selected'
    Column::text_null("name"),       // set on 'created'/'renamed'
    Column::text_null("supersedes"), // prior event_id this corrects (04 §4.3)
    Column::int("created_at"),       // transaction-time (epoch ms)
];

const CANOPY_PROFILE_PROJECT_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("profile_id"),      // FK: a canopy_profile_event.profile_id (comment-FK)
    Column::text("project_id"),      // FK: a canopy_project_event.project_id (comment-FK)
    Column::text("op"),              // 'added' | 'removed' | 'reordered'
    Column::int_null("order_key"),   // set on 'added'/'reordered'
    Column::text_null("supersedes"), // prior event_id for this (profile,project)
    Column::int("created_at"),       // transaction-time (epoch ms)
];

// Per-(profile, project) arrangement (ADR-0036, superseding ADR-0035's
// per-project table for writes): the `LayoutModel` codec is reused unchanged;
// NULL is a reset to the default. `canopy_layout_event` is frozen for writes and
// stays readable as the default profile's fallback.
const CANOPY_PROFILE_LAYOUT_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("profile_id"),      // FK: a canopy_profile_event.profile_id (comment-FK)
    Column::text("project_id"),      // FK: a canopy_project_event.project_id (comment-FK)
    Column::text_null("layout"),     // encoded LayoutModel; NULL = reset-to-default
    Column::text_null("supersedes"), // prior layout event_id for this (profile,project)
    Column::int("created_at"),       // transaction-time (epoch ms)
];

// Per-(profile, slot) surface-theme overrides (ADR-0038): `colour` is `#RRGGBB`
// hex with NULL = reset-to-default; latest-wins per (profile, slot). Slots are
// the five roles (background/border/button/graph/text) validated at the writer.
const CANOPY_PROFILE_THEME_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("profile_id"),      // FK: a canopy_profile_event.profile_id (comment-FK)
    Column::text("slot"),            // 'background' | 'border' | 'button' | 'graph' | 'text'
    Column::text_null("colour"),     // '#RRGGBB'; NULL = reset-to-default
    Column::text_null("supersedes"), // prior event_id for this (profile,slot)
    Column::int("created_at"),       // transaction-time (epoch ms)
];

// The startup pin (ADR-0045): a single latest-wins fold — the profile launch
// opens on, NULL = follow last-selected (today's behaviour). Launch only ever
// READS this table; pinning/unpinning are ordinary superseding events.
const CANOPY_PROFILE_STARTUP_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text_null("profile_id"), // FK comment-FK; NULL = follow last-selected
    Column::text_null("supersedes"), // prior startup event_id
    Column::int("created_at"),       // transaction-time (epoch ms)
];

const CANOPY_TABLES: &[CanonicalTable] = &[
    CanonicalTable {
        name: "canopy_project_event",
        columns: CANOPY_PROJECT_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "canopy_toggle_event",
        columns: CANOPY_TOGGLE_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "canopy_layout_event",
        columns: CANOPY_LAYOUT_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "canopy_profile_event",
        columns: CANOPY_PROFILE_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "canopy_profile_project_event",
        columns: CANOPY_PROFILE_PROJECT_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "canopy_profile_layout_event",
        columns: CANOPY_PROFILE_LAYOUT_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "canopy_profile_theme_event",
        columns: CANOPY_PROFILE_THEME_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "canopy_profile_startup_event",
        columns: CANOPY_PROFILE_STARTUP_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
];

/// The registry of canopy UI-state (`ui-state.db`) canonical tables. Machine-
/// global and append-only, iterated by the immutability property test **in
/// addition to** [`canonical_tables`]/[`recall_canonical_tables`], so every
/// canopy table gets its append-only guard for free.
#[must_use]
pub fn canopy_canonical_tables() -> &'static [CanonicalTable] {
    CANOPY_TABLES
}

/// Look up a canopy canonical table by name.
#[must_use]
pub fn canopy_canonical_table(name: &str) -> Option<&'static CanonicalTable> {
    CANOPY_TABLES.iter().find(|t| t.name == name)
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
            "captured_event",
            "agent_enablement",
            "archive_decision",
            "substrate_install",
            "substrate_block_event",
            "ingest_watermark",
            "verification_run",
            "verification_divergence",
            // turn-boundary attribution (P4 S2, pulled into M1)
            "turn_boundary_snapshot",
            "turn_boundary_file",
            "turn_diff_hunk",
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

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
//! * **`CHECK` constraints** (confidence range, category enum, non-negative
//!   counts) are enforced at the Rust writer layer (clamped scores, typed
//!   enums, `u32` counts); SQL-level `CHECK` is a later hardening item. (Kept
//!   out of the DDL so the registry-driven immutability test can insert a generic
//!   sample row into every table.)
//! * **Floats** never reach the store: confidence is an integer in milli-units
//!   (`confidence_milli ∈ 0..=1000`), money is integer micro-units — consistent
//!   with the no-float money posture.

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

// --- The registry of canonical tables -------------------------------------
// All hashes/oids are stored as lowercase hex `TEXT` (matching the integrity
// store's `Sha256Id::to_hex`/`parse_hex`); timestamps are epoch-ms `INTEGER`.

// ----- Integrity / chain: checkpoints, sessions-in-checkpoint, witnesses ---

const CHECKPOINT_COLUMNS: &[Column] = &[
    Column::text("checkpoint_id"), // Sha256Id (content-addressed = sha256(metadata))
    Column::text("strategy"),      // manual-commit, on-pr, scheduled, policy-driven; or the
    // `import` marker (2026-08-29). Unconstrained TEXT — comment only, no migration.
    Column::int("created_at"),      // transaction-time (epoch ms)
    Column::text("merkle_root"),    // sha256 over this checkpoint's event leaves
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

// ----- Sessions / turns / calls / files ------------------------------------

const SESSION_COLUMNS: &[Column] = &[
    Column::text("session_id"),    // sha256(agent, native session id)
    Column::text("agent"),         // 'claude-code' | …
    Column::text("model_primary"), // headline model
    // The **family** id since 2026-08-31: the identity recorded
    // in the repository's common dir, so a session worked in a linked worktree
    // is filed under the repository it belongs to and not under that checkout's
    // path hash. Rows written before that keep the per-path capture id they
    // were written with, and nothing rewrites them: the mint ADOPTS the main
    // work tree's historical id, so both values name the same database and no
    // reader changes meaning. That is why this needs no migration.
    Column::text("repo_id"),
    Column::int("started_at"), // valid-time (epoch ms)
    Column::int("schema_version"),
];

// Lifecycle as EVENTS, not a mutable status column. FK: session.
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

// A path@blob_oid is ONE immutable, content-addressed fact.
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

// ----- Tokens / price / cost -----------------------------------------------

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

// Bitemporal cost estimate: never UPDATE; reconciliations
// supersede. (Named `cost_fact`; the earlier draft called it `cost_estimate`.)
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

// ----- Attribution: who wrote which lines ----------------------------------

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

// The committed-diff hunks for `C`. FK: file_revision.
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

// ----- Shadow-branch lifecycle ---------------------------------------------

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

// ----- Hook / lifecycle audit ----------------------------------------------

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

// Per-canonical-event idempotency index. One row per *logical* event;
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

// Agent enable/disable history. enable/disable are *events*, not a mutable
// "enabled" boolean — so the history of which agents were wired
// when is auditable and 4NF-clean (one row per action).
const AGENT_ENABLEMENT_COLUMNS: &[Column] = &[
    Column::text("change_id"), // = sha256(agent ‖ action ‖ changed_at ‖ wired_hooks_hash)
    Column::text("agent"),
    Column::text("action"), // 'enable' | 'disable'
    Column::int("changed_at"),
    Column::text("wired_hooks_hash"), // sha256 over the wired hook config (provenance)
    Column::int("schema_version"),
];

// Checkpoint-strategy decision audit.
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

// ----- Retired wrapper-backend audit ---------------------------------------

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

// ----- Ingest watermark ----------------------------------------------------

const INGEST_WATERMARK_COLUMNS: &[Column] = &[
    Column::text("watermark_id"),
    Column::text("branch"),     // 'rift/checkpoints/v1'
    Column::text("branch_tip"), // git oid at time of ingest (hex)
    Column::int("ingested_at"),
    Column::int("schema_version"),
];

// ----- Verification log ----------------------------------------------------

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

// Encryption-at-rest key metadata: insert-only; rotation is a
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

// Local-only identity: one row per distinct observed
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
// correction is a NEW binding row superseding the prior one.
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

// ----- Turn boundaries: the human and agent edit windows --------------------
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
    // Retired wrapper-backend audit. SEALED FOREVER, not deleted (owner
    // decision of 2026-08-11, refining the deletion authorization): the
    // wrapper backend was removed and this table's only writer went with it —
    // the API, row type and guard legs are gone. The table stays because the
    // historical
    // migrations are checksum-frozen on every deployed projection and
    // `is_additive` forbids DROP: the guards that make the ledger
    // append-only make its SHAPE append-only too. An empty fossil table is
    // the honest cost of that trade, and the byte-stability pin below this
    // registry is what reddened when physical deletion was attempted.
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
    // Local-only developer identity: hashed ids only, the
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
    // --- capture v4 additive (turn boundaries): past the identity set --------
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
    // --- capture v5 additive (projection meta): past the v4 set ---------------
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
    // --- capture v6 additive (read tokens): past the v5 set -------------------
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
    // --- capture v7 additive (truthful System panel) -------------------------
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
    // --- capture v9 additive (model provenance) ----------------------------
    CanonicalTable {
        name: "model_observation",
        columns: MODEL_OBSERVATION_COLUMNS,
        primary_key: &["observation_id"],
        unique: NO_UNIQUE,
    },
    // --- capture v11 additive (the checkpoint↔commit linkage) ---------------
    // WHY A RELATION AND NOT A COLUMN ON `checkpoint`, twice over:
    //
    // 1. `checkpoint` is table #1 of the FROZEN v1 prefix, and `canonical_core`
    //    (migration v2) renders its SQL from that prefix. Widening
    //    `CHECKPOINT_COLUMNS` rewrites an APPLIED migration's checksum-locked
    //    bytes on every deployed projection — the collision this program
    //    already paid for once. A new table is a new version, never an edit to
    //    an applied one.
    // 2. Even without the freeze this is the right shape. The fact is SPARSE:
    //    a shard need not name a commit, and a legacy-dialect shard often does
    //    not. A nullable column spells "named nothing" and "we never looked"
    //    with the same NULL; a missing ROW says exactly one thing — this
    //    checkpoint named no commit.
    //
    // The row is a PROJECTION of `metadata.json`'s `commit` field, not a new
    // canonical fact: the shard already carries it, `ingest_from_substrate`
    // rebuilds it from the substrate, and nothing here can outlive the archive
    // it was derived from.
    CanonicalTable {
        name: "checkpoint_commit",
        columns: CHECKPOINT_COMMIT_COLUMNS,
        primary_key: &["checkpoint_id"],
        unique: NO_UNIQUE,
    },
    // --- capture v12 additive (soak witness) ------------------------------
    // WHAT A `verification_run` ROW CANNOT SAY, measured before this table
    // existed: its `run_id` is `leaf_hash("{exit_code}:{checkpoints}:{now}")`
    // — no archive input at all — so two runs over identical state and two
    // over different states are the same row shape, and the two tables share
    // no reference in either direction. A +/-5 s time-window join over the
    // owner's own projection left 30 of 35 archive decisions with no run and
    // 33 of 39 runs with no decision, with one of the <=5 pairs ambiguous.
    // That is not a correspondence; it is the preimage guessing that made 39
    // recorded runs unreadable.
    //
    // So the linkage becomes a ROW, not a column: `verification_run` is table
    // #28, inside the frozen `CAPTURE_V1_TABLE_COUNT` prefix whose SQL
    // checksum every deployed projection locks, and widening it would red
    // `capture_core_v1_sql_is_byte_stable` and break every existing install.
    // v11's `checkpoint_commit` exists for exactly this reason.
    //
    // The primary key is COMPOSITE because doctor's stale-session sweep runs
    // ONE verify for N finalized sessions: one run legitimately witnesses
    // many checkpoints.
    CanonicalTable {
        name: "verification_witness",
        columns: VERIFICATION_WITNESS_COLUMNS,
        primary_key: &["run_id", "checkpoint_id"],
        unique: NO_UNIQUE,
    },
    // --- capture v13 additive (the legacy-import claim relation) -------------
    // A durable home for a pre-Rift shard the legacy import path read: it is a
    // CLAIM about an imported checkpoint, never a chain anchor. A `checkpoint`
    // row is not an option — its id is a `Sha256Id` and a pre-Rift id never
    // parses as one; minting a Rift id for it would put a row in the projection
    // git cannot produce, and `rift verify` would read the surplus as tamper on
    // every subsequent run (append-only `verification_divergence` never heals).
    // So this is a SEPARATE relation, exactly as `checkpoint_commit` and
    // `verification_witness` are separate from `checkpoint` for the same
    // checksum-freeze reason: `checkpoint` sits in the frozen v1 prefix, and a
    // new fact about an old shape is a new table, never a widened one.
    CanonicalTable {
        name: "imported_checkpoint",
        columns: IMPORTED_CHECKPOINT_COLUMNS,
        primary_key: &["import_id"],
        unique: NO_UNIQUE,
    },
    // --- capture v14 additive (worktree provenance) --------------------------
    // Worktree provenance. `session_worktree` is the append-only observation
    // relation: one row per OBSERVED STATE (session, worktree, branch, head,
    // subagent), content-addressed with every field NUL-terminated and absence
    // encoded, so a redelivered hook is `INSERT OR IGNORE` and a session's
    // history is the ordered set of its rows. It is projection-only — the
    // `work_tree` column is a local address, and paths never enter shards.
    // `checkpoint_worktree` is the rebuildable projection of the shard
    // metadata's `worktree` member (the ARCHIVING worktree, first-archiver
    // semantics); a shard without the member projects NO row, which is what
    // renders `unknown`. NEW tables, never columns: `session` and `checkpoint`
    // sit in the frozen v1 prefix (the subagent discriminator stays, and
    // shards stay sealed — the projection may not invent rows for them).
    CanonicalTable {
        name: "session_worktree",
        columns: SESSION_WORKTREE_COLUMNS,
        primary_key: &["observation_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "checkpoint_worktree",
        columns: CHECKPOINT_WORKTREE_COLUMNS,
        primary_key: &["checkpoint_id"],
        unique: NO_UNIQUE,
    },
    // --- capture v15 additive (measured tool results) ------------------------
    // WHY A RELATION AND NOT A COLUMN ON `file_read_tokens`: that table is
    // keyed by `tool_call_id` and reached through `tool_reads_file`, so only a
    // call that RESOLVED TO A FILE can have a row at all. A search returns
    // repository content and resolves to no file — it has no `tool_reads_file`
    // link and frequently no `tool_call` row of its own — so measuring one
    // under that key is not a widening, it is a different question.
    //
    // The row is the measurement of one tool RESULT: what it put in front of
    // the model, counted with the real tokenizer by the detached worker, with
    // the channel it arrived on and the basis of the count both named. A
    // figure whose basis is not published is a figure a reader has to guess
    // the provenance of, which is the failure this plane already paid for once
    // (`docs/guide/08-cost-and-savings.md` §8.7).
    //
    // `session_id` rather than nothing: the savings denominator is per family,
    // and `session.agent` is the only per-family axis the projection has. A
    // result that cannot be attributed is filed under the projection's own
    // `(unattributed)` bucket by the reader, never folded into a family that
    // may not have made it.
    CanonicalTable {
        name: "tool_result_tokens",
        columns: TOOL_RESULT_TOKENS_COLUMNS,
        primary_key: &["tool_result_id"],
        unique: NO_UNIQUE,
    },
];

/// Columns for the v11 `checkpoint_commit` linkage.
///
/// `commit` is a SQL keyword, so the column is `commit_oid` — which is also one
/// of the two spellings `CheckpointMetadata` already accepts for the field
/// (`#[serde(alias = "commit_oid")]`), so the artifact and the projection name
/// the value identically.
const CHECKPOINT_COMMIT_COLUMNS: &[Column] = &[
    Column::text("checkpoint_id"), // FK: checkpoint (1:0..1)
    Column::text("commit_oid"),    // the git commit this checkpoint captured (hex)
    Column::int("schema_version"),
];

/// Columns for the v12 `verification_witness` soak linkage.
///
/// `chain_head` is NULLABLE deliberately: an unreadable chain head is ABSENT,
/// not the empty string. Reading absence as emptiness is a defect this program
/// has already found twice, and a soak counter is the last place to repeat it.
const VERIFICATION_WITNESS_COLUMNS: &[Column] = &[
    Column::text("run_id"),          // FK: verification_run
    Column::text("checkpoint_id"),   // the checkpoint this run actually witnessed
    Column::text_null("chain_head"), // chain position at witness time, if readable
    Column::int("schema_version"),
];

/// Columns for the v13 `imported_checkpoint` durable-claim relation.
/// `import_id` is content-addressed over `(source_id,
/// source_layout)` so re-running `rift import --from-legacy` over an unchanged
/// shard is a set-union no-op, exactly like every other bridge-derived row.
///
/// `source_root_claim` is NULLABLE: the legacy header may claim no root at all
/// (`RootCheck::Unclaimed`), and NULL is what "the header named nothing" means
/// — never the empty string. `root_check` is CHECK-constrained to the closed
/// vocabulary [`IMPORTED_CHECKPOINT_ROOT_CHECKS`] so an unrecognised outcome
/// cannot silently reach storage.
const IMPORTED_CHECKPOINT_COLUMNS: &[Column] = &[
    Column::text("import_id"), // content-addressed: tagged(source_id, source_layout)
    Column::text("source_id"), // the pre-Rift checkpoint id, as spelled on disk
    Column::text_one_of("source_layout", IMPORTED_CHECKPOINT_LAYOUTS),
    Column::text_null("source_root_claim"), // the header's claimed merkle root, if any
    Column::text_one_of("root_check", IMPORTED_CHECKPOINT_ROOT_CHECKS),
    Column::int("imported_at"), // transaction-time (epoch ms; caller-supplied, never wall-clock here)
    Column::int("schema_version"),
];

/// The closed `source_layout` vocabulary — `rift_shadow::legacy_import::Layout`'s
/// stable tokens, mirrored here so the CHECK constraint and the writer's enum
/// cannot drift into two spellings of "the same layout" (`rift-store` does not
/// depend on `rift-shadow`, so the tokens are copied, not imported; a mismatch
/// would surface immediately as an insert failure against the CHECK).
pub const IMPORTED_CHECKPOINT_LAYOUTS: &[&str] = &["sharded-branch", "per-checkpoint-refs"];

/// The closed `root_check` vocabulary — `rift_shadow::legacy_import::RootCheck`'s
/// stable tokens, mirrored for the same reason as
/// [`IMPORTED_CHECKPOINT_LAYOUTS`].
pub const IMPORTED_CHECKPOINT_ROOT_CHECKS: &[&str] =
    &["self-consistent", "mismatch", "unclaimed", "not-derivable"];

/// Columns for the v14 `session_worktree` observation relation.
///
/// `observation_id` is content-addressed over the observed STATE — every field
/// NUL-terminated and absence encoded (`f(None) = "0\0"`), never the time —
/// so `(branch="a", head=None)` and `(branch=None, head="a")` cannot collide
/// and a repeated observation dedups. `work_tree` is an absolute path: a LOCAL
/// address, which is why this relation is projection-only and no shard carries
/// it. `branch`/`head_oid` are NULLABLE: detached or unreadable HEAD is
/// ABSENT, never the empty string. `basis` is CHECK-constrained to the closed
/// vocabulary [`SESSION_WORKTREE_BASES`].
const SESSION_WORKTREE_COLUMNS: &[Column] = &[
    Column::text("observation_id"), // content-addressed over the observed state
    Column::text("session_id"),     // FK: session (1:N observations)
    Column::text("worktree_token"), // RepoPaths::worktree_token() (12 hex)
    Column::text("worktree_name"),  // admin name, or 'main'
    Column::int("is_linked"),       // 0/1
    Column::text("work_tree"),      // absolute path — local address, not content
    Column::text_null("branch"),    // HEAD's symbolic name at observation
    Column::text_null("head_oid"),  // HEAD oid at observation
    Column::text_null("subagent_id"), // the subagent_session id when a subagent's hook observed
    Column::text_one_of("basis", SESSION_WORKTREE_BASES),
    Column::int("first_observed_at"), // epoch ms of the first insert
    Column::int("schema_version"),
];

/// The closed `basis` vocabulary for `SESSION_WORKTREE_COLUMNS`: where the
/// observed worktree came from. `payload_cwd` = the agent
/// payload's `cwd` (Claude Code, Codex); `workspace_path` = Antigravity's
/// `workspacePaths[0]` (its payload has no `cwd`); `process_cwd` = the hook
/// process's own cwd; `commit`/`attach`/`import` = the non-hook writers.
pub const SESSION_WORKTREE_BASES: &[&str] = &[
    "payload_cwd",
    "workspace_path",
    "process_cwd",
    "commit",
    "attach",
    "import",
];

/// Columns for the v14 `checkpoint_worktree` projection: the
/// shard metadata's `worktree` member, projected by the artifact bridge. A
/// shard without the member (every shard sealed before it existed) projects
/// NO row — readers
/// render that as `unknown`, never `main`. `branch` NULLABLE as above.
const CHECKPOINT_WORKTREE_COLUMNS: &[Column] = &[
    Column::text("checkpoint_id"),  // FK: checkpoint (1:0..1)
    Column::text("worktree_token"), // the ARCHIVING worktree (first-archiver semantics)
    Column::text("worktree_name"),
    Column::int("is_linked"), // 0/1
    Column::text_null("branch"),
    Column::int("schema_version"),
];

/// Columns for the v15 `tool_result_tokens` relation.
///
/// `tool_result_id` is content-addressed over the fact — `(session, turn,
/// tool_name, payload_sha256)` — so a redelivered hook is `INSERT OR IGNORE`
/// and one executed call is one row, whatever id the client does or does not
/// carry (Claude Code publishes `tool_use_id`; Codex publishes none).
///
/// `channel` and `basis` are CHECK-constrained to the closed vocabularies
/// [`TOOL_RESULT_CHANNELS`] and [`TOOL_RESULT_BASES`], so an unrecognised
/// spelling cannot reach storage and be summed as though it meant something.
/// `output_tokens` is `0` on an unmeasured row, which is exactly what it
/// contributes to a denominator; what makes the row worth writing is that the
/// call is COUNTED as one nobody measured rather than silently absent.
const TOOL_RESULT_TOKENS_COLUMNS: &[Column] = &[
    Column::text("tool_result_id"), // content-addressed: tagged(session, turn, tool, payload)
    Column::text("session_id"),     // FK: session — the only per-family axis there is
    Column::text("tool_name"),      // the family's own spelling ('Grep', 'Bash', 'rg')
    Column::text_one_of("channel", TOOL_RESULT_CHANNELS),
    Column::int("output_tokens"), // measured over the bytes the tool put in front of the model
    Column::text("tokenizer"),    // e.g. 'cl100k_base' (provenance, never a bare number)
    Column::text_one_of("basis", TOOL_RESULT_BASES),
    Column::int("recorded_at"), // epoch ms; the DECISION's clock, not the worker's
    Column::int("schema_version"),
];

/// The closed `channel` vocabulary: which kind of call put repository content
/// in front of the model.
///
/// `read_tool` is a structured read tool, `shell_read` a command line that
/// read a file, `search` a call that searched names or contents. They are
/// separate because the savings denominator is published per channel and a
/// reader must be able to take one out.
pub const TOOL_RESULT_CHANNELS: &[&str] = &["read_tool", "shell_read", "search"];

/// The closed `basis` vocabulary: how the count was arrived at.
///
/// `reexecuted` — the worker ran the same search read-only against a tree
/// whose key had not moved and counted the real output. `in_process` — the
/// worker matched it with Rift's own index or content matcher and counted the
/// rendering, which is a reconstruction of the tool's answer rather than the
/// tool's own bytes. `unmeasured` — nothing the allow-list covers, or a tree
/// that moved under the job: the row counts the call and contributes no
/// tokens, because a call nobody measured is a fact and a figure nobody
/// measured is not.
pub const TOOL_RESULT_BASES: &[&str] = &["reexecuted", "in_process", "unmeasured"];

/// Columns for the v5 `projection_meta` key/value table (the rebuild epoch).
const PROJECTION_META_COLUMNS: &[Column] = &[
    Column::text("key"),  // e.g. 'rebuild_epoch'
    Column::int("value"), // the integer fact
    Column::int("schema_version"),
];

/// Columns for the v6 `file_read_tokens` table. FK: tool_call.
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

/// Columns for later, provenance-aware model observations. This relation
/// avoids mutating an immutable session row created before its model is known.
const MODEL_OBSERVATION_COLUMNS: &[Column] = &[
    Column::text("observation_id"),
    Column::text("session_id"),
    Column::text("model"),
    Column::text("source"),
    Column::int("observed_at"),
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

/// Boundary after the v7 identity and context-observation relations.
pub(crate) const CAPTURE_SYSTEM_TABLE_COUNT: usize = 39;

/// Boundary after the v9 model-observation relation.
pub(crate) const CAPTURE_MODEL_OBSERVATION_TABLE_COUNT: usize = 40;

/// Boundary after the v11 `checkpoint_commit` relation.
pub(crate) const CAPTURE_CHECKPOINT_COMMIT_TABLE_COUNT: usize = 41;

/// Boundary after the v12 `verification_witness` relation.
pub(crate) const CAPTURE_VERIFICATION_WITNESS_TABLE_COUNT: usize = 42;

/// Boundary after the v13 `imported_checkpoint` relation.
pub(crate) const CAPTURE_IMPORTED_CHECKPOINT_TABLE_COUNT: usize = 43;

/// Boundary after the v14 worktree-provenance relations. Frozen once v14
/// shipped; later tables append past it and ship in their own migration.
pub(crate) const CAPTURE_WORKTREE_TABLE_COUNT: usize = 45;

/// Boundary after the v15 `tool_result_tokens` relation — the current end of
/// the capture registry. The next additive capture migration starts here.
pub(crate) const CAPTURE_TOOL_RESULT_TABLE_COUNT: usize = 46;

/// The v3 `capture_identity` tables (frozen once shipped — see
/// [`CAPTURE_IDENTITY_TABLE_COUNT`]).
#[must_use]
#[allow(clippy::indexing_slicing)] // same compile-time bound as above
pub(crate) fn capture_identity_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_V1_TABLE_COUNT..CAPTURE_IDENTITY_TABLE_COUNT]
}

/// The v4 `capture_turn_boundary` tables.
#[must_use]
#[allow(clippy::indexing_slicing)] // same compile-time bound as above
pub(crate) fn capture_turn_boundary_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_IDENTITY_TABLE_COUNT..CAPTURE_TURN_BOUNDARY_TABLE_COUNT]
}

/// The v5 `capture_projection_meta` table (the rebuild epoch).
#[must_use]
#[allow(clippy::indexing_slicing)] // same compile-time bound as above
pub(crate) fn capture_projection_meta_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_TURN_BOUNDARY_TABLE_COUNT..CAPTURE_PROJECTION_META_TABLE_COUNT]
}

/// The v6 `file_revision_tokens` table (read-time token estimates).
#[must_use]
#[allow(clippy::indexing_slicing)] // same compile-time bound as above
pub(crate) fn capture_read_tokens_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_PROJECTION_META_TABLE_COUNT..CAPTURE_READ_TOKENS_TABLE_COUNT]
}

/// The v7 session-identity and main-context observation tables.
#[must_use]
#[allow(clippy::indexing_slicing)]
pub(crate) fn capture_system_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_READ_TOKENS_TABLE_COUNT..CAPTURE_SYSTEM_TABLE_COUNT]
}

/// The v9 append-only model-observation table.
#[must_use]
#[allow(clippy::indexing_slicing)]
pub(crate) fn capture_model_observation_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_SYSTEM_TABLE_COUNT..CAPTURE_MODEL_OBSERVATION_TABLE_COUNT]
}

/// The v11 append-only `checkpoint_commit` linkage table.
#[must_use]
#[allow(clippy::indexing_slicing)]
pub(crate) fn capture_checkpoint_commit_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_MODEL_OBSERVATION_TABLE_COUNT..CAPTURE_CHECKPOINT_COMMIT_TABLE_COUNT]
}

/// The v12 append-only `verification_witness` soak linkage.
#[must_use]
#[allow(clippy::indexing_slicing)]
pub(crate) fn capture_verification_witness_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_CHECKPOINT_COMMIT_TABLE_COUNT..CAPTURE_VERIFICATION_WITNESS_TABLE_COUNT]
}

/// The v13 append-only `imported_checkpoint` durable-claim relation.
#[must_use]
#[allow(clippy::indexing_slicing)]
pub(crate) fn capture_imported_checkpoint_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_VERIFICATION_WITNESS_TABLE_COUNT..CAPTURE_IMPORTED_CHECKPOINT_TABLE_COUNT]
}

/// The v14 `session_worktree` + `checkpoint_worktree` provenance relations
/// (frozen once shipped, like every slice before it).
#[must_use]
#[allow(clippy::indexing_slicing)] // same compile-time bound as above
pub(crate) fn capture_worktree_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_IMPORTED_CHECKPOINT_TABLE_COUNT..CAPTURE_WORKTREE_TABLE_COUNT]
}

/// The v15 append-only `tool_result_tokens` relation.
#[must_use]
#[allow(clippy::indexing_slicing)] // same compile-time bound as above
pub(crate) fn capture_tool_result_tables() -> &'static [CanonicalTable] {
    &TABLES[CAPTURE_WORKTREE_TABLE_COUNT..CAPTURE_TOOL_RESULT_TABLE_COUNT]
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
// The recall projection (`recall.db`)
// =========================================================================
//
// A SEPARATE registry + database file from the capture projection
// (`projection.db`): the recall graph is the read-side, rebuildable delta
// ledger, and `rift-memd rebuild` destroys only `recall.db`. It reuses this
// crate's migration + immutability machinery (registered canonical → guard
// triggers → the registry-driven property test) via [`recall_canonical_tables`].
//
// The model is the generic node/edge graph the database design settles on:
// `graph_node` + typed `graph_edge`, with the **payload of each node kind in
// its own typed `n_*` relation** (4NF). Mechanical facts only — the `kind`
// columns are `CHECK`-constrained to the v1 set, so a semantic kind
// (`similar_to`/`touches`/`frame`) is refused until a later migration widens
// the set. Foreign keys are documented in comments, not emitted as SQL
// `REFERENCES` — same posture as the capture schema (the single writer
// guarantees referential integrity; enforced FKs are a later hardening item).

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
/// on `graph_edge.kind` rejects it until a later migration adds it.
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
// re-ingest a set-union no-op.
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
    Column::text("dialect"), // native or frozen pre-Rift persisted token
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
// (mirrors the capture projection's `file_revision` keying), so the same
// path@content dedupes
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

// --- validity / digest store -----------------------------------------------
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

// --- retention lineage + metric trend --------------------------------------
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

// --- outcome labels --------------------------------------------------------
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

// --- serve audit (the `memd.*` serving-decision trail) ---------------------
// Every recall serve decision (manifest/summary/exact/pack/invalidated/refused)
// lands here through the `rift hooks memd` verb: append-only, content-addressed
// on the event's idempotency key (a re-delivered handoff is a no-op). Neutral by
// construction — `handle`/`reason` name *what* and *why*, never the withheld
// bytes. Exact retrieval is audited BEFORE bytes return.
const MEMD_AUDIT_EVENT_COLUMNS: &[Column] = &[
    Column::text("audit_id"), // = the event's content-addressed idempotency key
    // 'memd.manifest_served' | 'memd.manifest_injected' | … | 'memd.refused'.
    // The two manifest verbs are a pull/push split: `served` answers a
    // request, `injected` is pushed at a turn boundary and is SPEND, never
    // credit. Rows written before the split carry `served` for both and no
    // in-row fact separates them — the relation holds none, by construction.
    Column::text("verb"),
    Column::text_null("handle"), // the target/handle (neutral; never withheld bytes)
    Column::text_null("reason"), // neutral reason tag (refusals) / exact-retrieval reason
    Column::int("tokens_returned"),
    Column::int("tokens_avoided"),
    Column::int("recorded_at"),
    Column::int("schema_version"),
];

// --- serve-audit citation columns ------------------------------------------
// `memd_audit_event.handle` records a citation resolution as one
// composed string — `pack:<key>/evidence:<n>[=file:<path>@sha256:<hash>]` —
// because `memd_audit_event` is a FROZEN v4 relation: widening
// `MEMD_AUDIT_EVENT_COLUMNS` would change `recall_memd_audit_schema_sql`'s
// (already-applied) generated bytes and poison the checksum lock on every
// deployed `recall.db` (the same hazard `checkpoint_commit`/
// `verification_witness` exist to avoid on the capture side). So the two
// structured facts a query wants — "every resolution of ordinal N in pack
// X" without parsing a column — live in a SEPARATE, sparse companion
// relation instead: one row per audit event that is a citation resolution
// (`retrieve_by_citation`'s route, whether it serves bytes or refuses),
// keyed on the SAME `audit_id`. A row's mere PRESENCE is the discriminator
// the audit-citation request asked for — never the columns' NULLness, which
// this ledger has twice recorded as a "which era wrote this" defect.
//
// `pack_key` is a content address (`is_content_address`-checked at parse by
// `rift_recall_pack::CitationTarget`); `ordinal` is the pack-local number a
// caller sent back. Both are populated from the EXISTING `handle`/`target`
// string at write time (`bins/rift/src/hooks.rs::append_memd_audit`, via
// `rift_recall_serve::pack::CitationTarget::parse`) — the composed string
// itself is UNCHANGED (the citation tests that assert its exact shape keep
// passing), and parsing happens exactly once, at the one place a row is
// ever written, never at query time.
const MEMD_AUDIT_CITATION_COLUMNS: &[Column] = &[
    Column::text("audit_id"), // FK: memd_audit_event(audit_id), 1:0..1
    Column::text("pack_key"),
    Column::int("ordinal"),
    Column::int("schema_version"),
];

// --- FROZEN NAMES (read this before "tidying" any spelling below) ----------
// Everything from here to the ServeChannel banner below names relations,
// columns,
// indexes and migrations that SHIPPED. Their spellings are load-bearing in
// three independent ways: the registry row IS the table's name in deployed
// `recall.db` files; the migration bodies are checksum-locked, so editing one
// byte poisons every projection already on disk; and the tables are
// append-only, so their historical rows can never be rewritten under a new
// name. They survive as read-compat history for a transport that no longer
// exists — not as live vocabulary. The live serving axis is `ServeChannel`
// (`serve_call_event`/`serve_session_event`), declared after the banner.
// ---------------------------------------------------------------------------

// --- attributed calls (per process/connection, per decoded invocation) ------
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

/// Append-only client connection lifecycle facts. The live/connected claim is
/// derived from a separate short-lived lease; these rows are history only.
const MCP_CONNECTION_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),
    Column::text("thread_id"),
    Column::text("client"),
    Column::text_null("peer_name"),
    Column::text_null("peer_version"),
    Column::text_one_of("phase", &["opened", "closed"]),
    Column::int("occurred_at"),
    Column::int("schema_version"),
];

// --- The ServeChannel axis, superseding memd_call_event / ------------------
// --- mcp_connection_event ---------------------------------------------------
//
// `memd_call_event`/`mcp_connection_event` are built around a transport
// -specific axis (`thread_id`/`client`: one persistent connection multiplexing
// many calls). Neither hook injection nor a skill CLI invocation has that: a
// skill-cli call is one process making one call, and a hook injection is one
// hook event — there is no connection to open, close, or multiplex over.
// `ServeChannel` (`rift_recall_events::ServeChannel`) is the axis that
// replaces it. Both old tables REMAIN, read-compat only: every Canopy surface
// now reads `serve_call_event` (the System-panel scalar and, since the Calls
// widget was re-originated onto the channel axis, that card too), and no
// writer targets them. Nothing here deletes a row or a table.

/// The closed `rift_recall_events::ServeChannel` vocabulary, mirrored here
/// (this crate does not depend on `rift-recall-events`) so the CHECK
/// constraint and the enum's tokens cannot drift into two spellings of "the
/// same channel".
pub const SERVE_CHANNELS: &[&str] = &["hook_inject", "skill_cli"];

/// One neutral fact per served invocation, channel-discriminated. No
/// `thread_id`: see the section note above for why neither channel has one.
const SERVE_CALL_EVENT_COLUMNS: &[Column] = &[
    Column::text("call_id"), // caller-chosen, per-invocation (idempotency key)
    Column::text_one_of("channel", SERVE_CHANNELS),
    Column::text("tool_name"),
    Column::text("terminal_verb"), // the memd.* decision verb
    Column::int("tokens_returned"),
    Column::int("tokens_avoided"),
    Column::int("recorded_at"),
    Column::int("schema_version"),
];

/// One row per channel, EVER — the coarsest granularity that is still an
/// honest fact today. The historical per-connection open/closed
/// pair has no analogue here (neither channel has a connection); refining to
/// per-agent-session granularity is future work for the hook-injection
/// feature itself (its `ContextInjectionDecided` session state), declared
/// rather than approximated.
const SERVE_SESSION_EVENT_COLUMNS: &[Column] = &[
    Column::text_one_of("channel", SERVE_CHANNELS),
    Column::int("first_served_at"),
    Column::int("schema_version"),
];

// --- content→blob-oid map --------------------------------------------------
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
    // --- v2 additive (outcome labels): [RECALL_V1_.., RECALL_V2_TABLE_COUNT) --
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
    // --- v5 additive (the blob-oid map): past RECALL_V4_TABLE_COUNT -----------
    CanonicalTable {
        name: "n_blob_oid_map",
        columns: BLOB_OID_MAP_COLUMNS,
        primary_key: &["content_sha256"],
        unique: NO_UNIQUE,
    },
    // --- v6 additive (durable ingest watermark): past RECALL_V5_TABLE_COUNT ----
    // The recall ingest path appends one row per run (branch + last checkpoint
    // ingested), so incremental ingest is durable evidence, not just an
    // in-memory `Watermark` (audit reconciliation). Same column shape as
    // the capture projection's table; separate DB, separate rows.
    CanonicalTable {
        name: "ingest_watermark",
        columns: INGEST_WATERMARK_COLUMNS,
        primary_key: &["watermark_id"],
        unique: NO_UNIQUE,
    },
    // --- v7 additive (per-thread call attribution; frozen name) --------------
    CanonicalTable {
        name: "memd_call_event",
        columns: MEMD_CALL_EVENT_COLUMNS,
        primary_key: &["call_id"],
        unique: NO_UNIQUE,
    },
    // --- v8 additive (eval experiment records) -------------------------------
    CanonicalTable {
        name: "eval_run",
        columns: EVAL_RUN_COLUMNS,
        primary_key: &["run_id"],
        unique: NO_UNIQUE,
    },
    // --- v9 additive (truthful connection history; frozen name) ------------
    CanonicalTable {
        name: "mcp_connection_event",
        columns: MCP_CONNECTION_EVENT_COLUMNS,
        primary_key: &["event_id"],
        unique: NO_UNIQUE,
    },
    // --- v10 additive (audit citation columns) -------------------------------
    CanonicalTable {
        name: "memd_audit_citation",
        columns: MEMD_AUDIT_CITATION_COLUMNS,
        primary_key: &["audit_id"],
        unique: NO_UNIQUE,
    },
    // --- v11 additive (the ServeChannel axis) --------------------------------
    CanonicalTable {
        name: "serve_call_event",
        columns: SERVE_CALL_EVENT_COLUMNS,
        primary_key: &["call_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "serve_session_event",
        columns: SERVE_SESSION_EVENT_COLUMNS,
        primary_key: &["channel"],
        unique: NO_UNIQUE,
    },
    // --- v12 additive (worktree scope): past RECALL_V11_TABLE_COUNT ----------
    // Branch containment needs recall-side facts: recall.db NEVER joins a
    // capture table (a standing design rule — the two projections rebuild
    // independently), so the anchor commit and the archiving worktree are
    // mirrored here by the recall ingest from each shard's own metadata.
    // Rebuildable; a memberless shard writes no n_checkpoint_worktree row
    // (unknown renders as unknown).
    CanonicalTable {
        name: "n_checkpoint_commit",
        columns: N_CHECKPOINT_COMMIT_COLUMNS,
        primary_key: &["node_id"],
        unique: NO_UNIQUE,
    },
    CanonicalTable {
        name: "n_checkpoint_worktree",
        columns: N_CHECKPOINT_WORKTREE_COLUMNS,
        primary_key: &["node_id"],
        unique: NO_UNIQUE,
    },
    // A cross-branch grant's free text: `memd_audit_event.reason` is a frozen
    // NEUTRAL TAG by contract (the served event carries the fixed tag
    // `cross-branch-grant`), so the user-supplied text — untrusted free text,
    // <= 200 bytes enforced by the writer — lands here, stored and NEVER
    // re-served.
    CanonicalTable {
        name: "serve_grant_event",
        columns: SERVE_GRANT_EVENT_COLUMNS,
        primary_key: &["grant_id"],
        unique: NO_UNIQUE,
    },
];

/// Columns for the v12 `n_checkpoint_commit` relation: the
/// checkpoint's anchor commit, mirrored from shard `metadata.commit` by the
/// recall ingest so the containment predicate never joins a capture table.
const N_CHECKPOINT_COMMIT_COLUMNS: &[Column] = &[
    Column::text("node_id"),    // FK: n_checkpoint (1:0..1)
    Column::text("commit_oid"), // the anchor commit (hex), from metadata.commit
    Column::int("schema_version"),
];

/// Columns for the v12 `n_checkpoint_worktree` relation: the ARCHIVING
/// worktree (the first-archiver member), mirrored from
/// the shard's `worktree` member. No row when the member is absent.
const N_CHECKPOINT_WORKTREE_COLUMNS: &[Column] = &[
    Column::text("node_id"),        // FK: n_checkpoint (1:0..1)
    Column::text("worktree_token"), // 12 lowercase hex
    Column::text("worktree_name"),
    Column::int("is_linked"),    // 0/1
    Column::text_null("branch"), // NULL when the member carried none
    Column::int("schema_version"),
];

/// Columns for the v12 `serve_grant_event` relation: the
/// cross-branch grant's user-supplied reason text, kept OUT of the frozen
/// neutral-tag `memd_audit_event.reason` column. `reason_text` <= 200 bytes,
/// enforced by the writer; stored, audited, never re-served.
const SERVE_GRANT_EVENT_COLUMNS: &[Column] = &[
    Column::text("grant_id"),    // content-addressed by the writer
    Column::text("audit_id"),    // FK: memd_audit_event (the serve it widened)
    Column::text("kind"),        // the grant axis (e.g. 'branches')
    Column::text("reason_text"), // untrusted free text, <= 200 bytes
    Column::int("recorded_at"),  // epoch ms, caller-supplied
    Column::int("schema_version"),
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

/// One past the last v8 (`recall_eval_runs`) table. Frozen once shipped.
pub(crate) const RECALL_V8_TABLE_COUNT: usize = 20;

/// One past the v9 connection lifecycle table.
pub(crate) const RECALL_V9_TABLE_COUNT: usize = 21;

/// One past the v10 `memd_audit_citation` relation.
pub(crate) const RECALL_V10_TABLE_COUNT: usize = 22;

/// One past the v11 `serve_call_event`/`serve_session_event` relations — the
/// current end of the recall registry when they shipped. The next additive
/// recall migration starts here.
pub(crate) const RECALL_V11_TABLE_COUNT: usize = 24;

/// One past the v12 `n_checkpoint_commit`/`n_checkpoint_worktree`/
/// `serve_grant_event` relations — the current end of the recall registry.
/// The next additive recall migration starts here.
pub(crate) const RECALL_V12_TABLE_COUNT: usize = 27;

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
/// content→blob-oid bridge. Frozen once v5 shipped (the next additive
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

/// The v7-additive recall tables: one neutral terminal fact per decoded
/// invocation, attributed to the serving process/connection. Frozen once v7
/// shipped (the next additive migration starts past [`RECALL_V7_TABLE_COUNT`]).
#[allow(clippy::indexing_slicing)] // compile-time constant bound (see capture_v1_tables)
pub(crate) fn recall_v7_call_tables() -> &'static [CanonicalTable] {
    &RECALL_TABLES[RECALL_V6_TABLE_COUNT..RECALL_V7_TABLE_COUNT]
}

/// The v8-additive recall tables: the append-only bitemporal eval experiment
/// records the evaluation harness reads history from.
#[allow(clippy::indexing_slicing)] // compile-time constant bound (see capture_v1_tables)
pub(crate) fn recall_v8_eval_tables() -> &'static [CanonicalTable] {
    &RECALL_TABLES[RECALL_V7_TABLE_COUNT..RECALL_V8_TABLE_COUNT]
}

/// The v9-additive connection lifecycle table.
#[allow(clippy::indexing_slicing)]
pub(crate) fn recall_v9_connection_tables() -> &'static [CanonicalTable] {
    &RECALL_TABLES[RECALL_V8_TABLE_COUNT..RECALL_V9_TABLE_COUNT]
}

/// The v10-additive `memd_audit_citation` relation.
#[allow(clippy::indexing_slicing)]
pub(crate) fn recall_v10_audit_citation_tables() -> &'static [CanonicalTable] {
    &RECALL_TABLES[RECALL_V9_TABLE_COUNT..RECALL_V10_TABLE_COUNT]
}

/// The v11-additive `serve_call_event`/`serve_session_event` relations.
#[allow(clippy::indexing_slicing)]
pub(crate) fn recall_v11_serve_channel_tables() -> &'static [CanonicalTable] {
    &RECALL_TABLES[RECALL_V10_TABLE_COUNT..RECALL_V11_TABLE_COUNT]
}

/// The v12-additive worktree-scope relations (frozen once shipped, like
/// every slice before it).
#[must_use]
#[allow(clippy::indexing_slicing)] // same compile-time bound as above
pub(crate) fn recall_v12_worktree_scope_tables() -> &'static [CanonicalTable] {
    &RECALL_TABLES[RECALL_V11_TABLE_COUNT..RECALL_V12_TABLE_COUNT]
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

/// Index for per-client connection history/status queries.
pub const RECALL_CONNECTION_EVENT_INDEX_SQL: &str = "\
CREATE INDEX IF NOT EXISTS idx_mcp_connection_client_time ON mcp_connection_event(client, occurred_at);\n";

/// Index for "every resolution of ordinal N in pack X" — the exact query
/// the audit-citation request named as the reason two proper columns beat a
/// composed string.
pub const RECALL_AUDIT_CITATION_INDEX_SQL: &str = "\
CREATE INDEX IF NOT EXISTS idx_memd_audit_citation_pack_ordinal ON memd_audit_citation(pack_key, ordinal);\n";

/// Indexes for channel-scoped time windows over the ServeChannel tables —
/// the same query shape `RECALL_CALL_EVENT_INDEX_SQL` serves for the
/// historical table it supersedes.
pub const RECALL_SERVE_CHANNEL_INDEX_SQL: &str = "\
CREATE INDEX IF NOT EXISTS idx_serve_call_recorded ON serve_call_event(recorded_at);\n\
CREATE INDEX IF NOT EXISTS idx_serve_call_channel_recorded ON serve_call_event(channel, recorded_at);\n";

// --- eval experiment records -----------------------------------------------
// Append-only bitemporal verdicts of the Lost-in-Delta harness: what the code
// concluded, when it was true, and when we learned it. NOT rebuildable from
// the substrate (it captures what-code-concluded-when) — exactly why the rows
// are bitemporal and append-only, mirroring `verification_run`'s posture. The
// run id is content-addressed (`sha256(task_set ‖ condition ‖ code_version)`)
// so re-running an identical experiment is an idempotent no-op. Percentages
// are stored as **basis points** (pct × 100, integer) so the record stays
// byte-deterministic across platforms.

/// The closed set of eval conditions, frozen by the migration that created the
/// table. `real-model` was written for a paid sweep that no longer exists and
/// no row ever carried; the vocabulary is on an append-only relation, so it
/// stays rather than becoming a migration on a shipped one.
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

// --- Canopy UI-state -------------------------------------------------------
// Machine-global, append-only UI facts: which projects the dashboard watches and
// each project's feature-toggle intent. A SEPARATE registry + db file
// (`ui-state.db`) from the per-repo capture projection, so these never touch
// capture data — yet they still obey the immutability discipline: an
// `op`/`desired` change is a NEW event row (never an in-place UPDATE), and the
// current state is the fold of the latest-valid event per key. `op`/`feature` are
// plain `TEXT` validated at the Rust writer (the same "CHECK deferred" posture
// the capture schema takes), so
// the store stays decoupled from Canopy's enum spelling.

const CANOPY_PROJECT_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("project_id"),      // stable per registered project
    Column::text("op"),              // 'added' | 'removed' | 'relabeled' | 'reordered'
    Column::text_null("repo_id"),    // set on 'added'
    Column::text_null("path"),       // set on 'added'
    Column::text_null("label"),      // set on 'added'/'relabeled'
    Column::int_null("order_key"),   // set on 'reordered'
    Column::text_null("supersedes"), // prior event_id this corrects
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

// Per-project dashboard arrangement: the panel editor's layout, saved
// per project_id as a superseding event. `layout` is the encoded LayoutModel;
// NULL is a reset to the default (a superseding event, never a delete).
const CANOPY_LAYOUT_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("project_id"),      // FK: a canopy_project_event.project_id (comment-FK)
    Column::text_null("layout"),     // encoded LayoutModel; NULL = reset-to-default
    Column::text_null("supersedes"), // prior layout event_id for this project
    Column::int("created_at"),       // transaction-time (epoch ms)
];

// The profile plane: named, machine-global operator views. A profile
// scopes what the dashboard shows — its own membership over the global catalogue
// (`canopy_project_event` stays the registration source of truth), its own
// per-(profile, project) arrangements, and its own theme slots. All
// per-key, time-varying, superseding-event state like its siblings above.
const CANOPY_PROFILE_EVENT_COLUMNS: &[Column] = &[
    Column::text("event_id"),        // ULID
    Column::text("profile_id"),      // stable per profile (ULID)
    Column::text("op"),              // 'created' | 'renamed' | 'removed' | 'selected'
    Column::text_null("name"),       // set on 'created'/'renamed'
    Column::text_null("supersedes"), // prior event_id this corrects
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

// Per-(profile, project) arrangement, superseding the per-project table for
// writes: the `LayoutModel` codec is reused unchanged;
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

// Per-(profile, slot) surface-theme overrides: `colour` is `#RRGGBB`
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

// The startup pin: a single latest-wins fold — the profile launch
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
    fn full_capture_schema_is_registered() {
        // A regression guard: every capture relation must be in the registry
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
            "verification_witness",
            // turn-boundary attribution
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

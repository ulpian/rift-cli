//! The capture projection (`projection.db`): the registry of every relation the
//! session ledger writes, its frozen migration slices and their accessors.
//! Its own module because the capture, recall and canopy registries are three
//! independent databases that only share the [`CanonicalTable`] machinery.

use super::{CanonicalTable, Column, NO_UNIQUE};

mod boundary_gaps;
pub(crate) use boundary_gaps::{BOUNDARY_GAP_CONTEXT_INDEX_SQL, BOUNDARY_GAP_TABLES};

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
// `<sessions>/<id>.json` run-state file is a cache of this; these rows are truth.
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
    // 'recorded' | 'recorded-no-event' | 'deferred' | 'deferred-no-session'
    // | 'skipped-unknown-verb'
    Column::text("outcome"),
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

pub(super) const INGEST_WATERMARK_COLUMNS: &[Column] = &[
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
pub(super) const KEY_METADATA_COLUMNS: &[Column] = &[
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
    // Key/value projection facts: 'rebuild_epoch' and
    // 'session_identity_backfill_covered'. Each row is written at most once
    // per projection lifetime — `rift_store::rebuild` recreates the file and
    // inserts old-epoch+1; the open that first sees full identity coverage
    // inserts the marker — so the append-only guards hold without any
    // UPDATE. An absent epoch row reads as epoch 0.
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
    // --- capture v17 additive (observations a leading `cd` decided) ----------
    // The same observation as `session_worktree`, under a basis that table's
    // CHECK cannot admit: a read whose leading `cd` named another member of
    // the family is filed under that member, because the payload's directory
    // is where the agent's shell sat, not where it read. A CHECK changes only
    // by rebuilding its table, and a migration may not DROP or RENAME — the
    // guards that make the ledger append-only make its SHAPE append-only too —
    // so the wider vocabulary is a NEW relation beside the old one, never a
    // widened copy of it. Every reader reads the union of the two. Like its
    // twin it is projection-only: `work_tree` is a local address.
    CanonicalTable {
        name: "session_worktree_cd",
        columns: SESSION_WORKTREE_CD_COLUMNS,
        primary_key: &["observation_id"],
        unique: NO_UNIQUE,
    },
    // v19: sparse knowledge, never inferred from legacy blob contents.
    CanonicalTable {
        name: "turn_boundary_file_kind",
        columns: TURN_BOUNDARY_FILE_KIND_COLUMNS,
        primary_key: &["snapshot_id", "path"],
        unique: NO_UNIQUE,
    }, // v20: collection audit only, never event-time or authorship proof.
    BOUNDARY_OBSERVATION_TABLE,
    boundary_gaps::BATCH,
    boundary_gaps::TICKET,
];

pub(crate) const BOUNDARY_OBSERVATION_TABLE: CanonicalTable = CanonicalTable {
    name: "turn_boundary_observation",
    columns: TURN_BOUNDARY_OBSERVATION_COLUMNS,
    primary_key: &["snapshot_id"],
    unique: NO_UNIQUE,
};

const TURN_BOUNDARY_OBSERVATION_COLUMNS: &[Column] = &[
    Column::text("snapshot_id"),
    Column::text("worktree_hash"),
    Column::int("collection_started_at"),
    Column::int("collection_finished_at"),
    Column::int("schema_version"),
];

const TURN_BOUNDARY_FILE_KIND_COLUMNS: &[Column] = &[
    Column::text("snapshot_id"),
    Column::text("path"),
    Column::text_one_of("kind", &["regular", "link", "absent", "unknown"]),
    Column::int("schema_version"),
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

/// Columns for the v17 `session_worktree_cd` relation: exactly
/// [`SESSION_WORKTREE_COLUMNS`] but for the `basis` CHECK, which admits
/// [`SESSION_WORKTREE_CD_BASES`].
///
/// The same columns in the same order, so a reader can take the union of the
/// two relations with one `SELECT *` apiece and order it as it always has.
const SESSION_WORKTREE_CD_COLUMNS: &[Column] = &[
    Column::text("observation_id"), // content-addressed over the state AND the basis
    Column::text("session_id"),     // FK: session (1:N observations)
    Column::text("worktree_token"), // RepoPaths::worktree_token() (12 hex)
    Column::text("worktree_name"),  // admin name, or 'main'
    Column::int("is_linked"),       // 0/1
    Column::text("work_tree"),      // absolute path — local address, not content
    Column::text_null("branch"),    // HEAD's symbolic name at observation
    Column::text_null("head_oid"),  // HEAD oid at observation
    Column::text_null("subagent_id"), // the subagent_session id when a subagent's hook observed
    Column::text_one_of("basis", SESSION_WORKTREE_CD_BASES),
    Column::int("first_observed_at"), // epoch ms of the first insert
    Column::int("schema_version"),
];

/// The closed `basis` vocabulary for [`SESSION_WORKTREE_CD_COLUMNS`]: every
/// word of [`SESSION_WORKTREE_BASES`], and `shell_cd` — a shell read the
/// grammar recognised, filed under the member its leading `cd` named.
///
/// A superset rather than `shell_cd` alone so the relation describes what an
/// observation may say rather than which writer happens to use it; the writer
/// still sends every old word to the old relation, so no observation is ever
/// in both.
const SESSION_WORKTREE_CD_BASES: &[&str] = &[
    "payload_cwd",
    "workspace_path",
    "process_cwd",
    "commit",
    "attach",
    "import",
    "shell_cd",
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
/// as [`super::recall::RECALL_V1_TABLE_COUNT`]; `capture_core_v1_sql_is_byte_stable` (in
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

/// Boundary after the v15 `tool_result_tokens` relation. The v17
/// `session_worktree_cd` relation sits past it, the current end of the
/// registry, and its migration reads it by name.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::is_canonical_table;

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
    fn boundary_collection_attempt_has_an_append_only_registry_entry() {
        let table =
            canonical_table("turn_boundary_observation").expect("audit relation registered");
        assert_eq!(table.primary_key, &["snapshot_id"]);
        assert_eq!(
            table.columns.iter().map(|c| c.name).collect::<Vec<_>>(),
            vec![
                "snapshot_id",
                "worktree_hash",
                "collection_started_at",
                "collection_finished_at",
                "schema_version",
            ]
        );
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
}

#[cfg(test)]
mod worktree_cd_tests {
    use super::*;

    /// The wider relation can say everything the old one can, and the one
    /// word it adds is the only difference: a writer that routes by the old
    /// vocabulary therefore never has an observation neither relation admits.
    #[test]
    fn the_cd_vocabulary_is_the_old_one_plus_shell_cd() {
        let (old, wide) = (SESSION_WORKTREE_BASES, SESSION_WORKTREE_CD_BASES);
        assert!(old.iter().all(|basis| wide.contains(basis)), "{wide:?}");
        let added: Vec<&str> = wide.iter().copied().filter(|b| !old.contains(b)).collect();
        assert_eq!(added, ["shell_cd"]);
    }

    /// The two relations differ only in the `basis` CHECK, so their union is
    /// a plain `SELECT *` on each side.
    #[test]
    fn the_cd_relation_has_the_old_relations_columns_in_order() {
        let names = |columns: &[Column]| columns.iter().map(|c| c.name).collect::<Vec<_>>();
        assert_eq!(
            names(SESSION_WORKTREE_CD_COLUMNS),
            names(SESSION_WORKTREE_COLUMNS)
        );
        assert_eq!(
            TABLES.get(CAPTURE_TOOL_RESULT_TABLE_COUNT).map(|t| t.name),
            Some("session_worktree_cd"),
            "the v17 relation stays at its shipped registry position"
        );
    }
}

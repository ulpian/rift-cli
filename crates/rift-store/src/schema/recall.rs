//! The recall projection (`recall.db`): the node/edge graph registry, its
//! frozen migration slices, the secondary-index SQL and the eval-run vocabulary.
//! Its own module because `rift-memd rebuild` destroys only this database, so
//! its registry must never share an array with the capture one.

use super::capture::{INGEST_WATERMARK_COLUMNS, KEY_METADATA_COLUMNS};
use super::{CanonicalTable, Column, NO_UNIQUE};

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
// string at write time (`rift_recall_ledger::audit::append_memd_audit`, via
// `rift_recall_pack::CitationTarget::parse`) — the composed string
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
    // --- v13 additive (serve attribution): past RECALL_V12_TABLE_COUNT --------
    CanonicalTable {
        name: "memd_audit_session",
        columns: MEMD_AUDIT_SESSION_COLUMNS,
        primary_key: &["serve_key"],
        unique: NO_UNIQUE,
    },
    // --- v14 additive (pull claims) ---------------------------------------------
    CanonicalTable {
        name: "memd_audit_pull_claim",
        columns: MEMD_AUDIT_PULL_CLAIM_COLUMNS,
        primary_key: &["audit_id"],
        unique: NO_UNIQUE,
    },
    // --- v15 additive (window rows) ---------------------------------------------
    CanonicalTable {
        name: "n_window_row",
        columns: N_WINDOW_ROW_COLUMNS,
        primary_key: &["row_id"],
        unique: NO_UNIQUE,
    },
    // --- v16 additive (outline token counts) ----------------------------------
    CanonicalTable {
        name: "summary_token_count",
        columns: SUMMARY_TOKEN_COUNT_COLUMNS,
        primary_key: &["summary_id", "ordinal"],
        unique: NO_UNIQUE,
    },
    // --- v17 additive (watermark positions) ------------------------------------
    CanonicalTable {
        name: "ingest_watermark_position",
        columns: INGEST_WATERMARK_POSITION_COLUMNS,
        primary_key: &["watermark_id"],
        unique: NO_UNIQUE,
    },
    // --- v18 additive (observed heads) -------------------------------------------
    CanonicalTable {
        name: "n_checkpoint_observed_head",
        columns: N_CHECKPOINT_OBSERVED_HEAD_COLUMNS,
        primary_key: &["node_id"],
        unique: NO_UNIQUE,
    },
];

/// The two relations a serve key can live in: a schema-1 decision row, or a
/// schema-4 call row. Some producers write only one half of a decision, so
/// the attribution names which half its key addresses.
const SERVE_KEY_RELATIONS: &[&str] = &["memd_audit_event", "serve_call_event"];

/// Columns for the v13 `memd_audit_session` relation: which session, and
/// which context window inside it, made one serve.
///
/// A sparse companion on the serve's own key, for the reason
/// `memd_audit_citation` is one: both serve relations are frozen, and
/// widening either would move a checksum every deployed `recall.db` holds.
/// No row is the fact "no session made this" — a person at a terminal — and
/// it is never inferred from timing. One row per serve key rather than one
/// per decision, because a decision's two halves carry two keys and three
/// producers write only one of them; no foreign key is declared, since the
/// key lives in whichever relation `relation` names. The session's
/// checkpoint freezes its serves by joining here, and a rebuild writes the
/// rows back from the frozen lines, which carry both identifiers.
const MEMD_AUDIT_SESSION_COLUMNS: &[Column] = &[
    Column::text("serve_key"), // memd_audit_event.audit_id or serve_call_event.call_id
    Column::text_one_of("relation", SERVE_KEY_RELATIONS),
    Column::text("session_id"),      // the agent's own session id
    Column::text_null("window_key"), // the window journal's one-way key; NULL when unknown
    Column::int("schema_version"),
];

/// The rules a pull's claim can be booked under: the two a stated hash leads
/// to, spelled as the serving envelope spells them.
///
/// The three other words a response can carry describe a call that stated
/// nothing or a verb that credits nothing, and such a call writes no claim
/// row, so they are not admitted here.
const PULL_CLAIM_BASES: &[&str] = &["stated_recorded", "unverified"];

/// Columns for the v14 `memd_audit_pull_claim` relation: how many held
/// revisions a pull's caller named, and how many of them this family had
/// recorded reading.
///
/// A sparse companion on the audit row's own key, for the reason
/// `memd_audit_citation` is one: `memd_audit_event` is frozen, and widening
/// it would move a checksum every deployed `recall.db` holds. A pull that
/// stated no hash writes no row, so a row's presence says a claim was made,
/// and a row of zeros never stands for "nothing was claimed". Counts and the
/// rule's word only: never a hash, a path or a byte. The writer refuses a
/// row whose stated count is not the sum of the other two.
const MEMD_AUDIT_PULL_CLAIM_COLUMNS: &[Column] = &[
    Column::text("audit_id"), // FK: memd_audit_event(audit_id), 1:0..1
    Column::int("stated_count"),
    Column::int("recorded_count"),
    Column::int("unrecorded_count"),
    Column::text_one_of("basis", PULL_CLAIM_BASES),
    Column::int("schema_version"),
];

/// The kinds a frozen window row can be: the five a run of lines takes, the
/// expected hash of a whole-file write, an edit and an eviction, spelled as
/// `window_journal.jsonl` spells them.
const WINDOW_ROW_KINDS: &[&str] = &[
    "read",
    "deny",
    "observe",
    "hold",
    "prevented",
    "expect",
    "edit",
    "evict",
];

/// Columns for the v15 `n_window_row` relation: one row per line a
/// checkpoint's `window_journal.jsonl` froze.
///
/// Keyed by the SHA-256 of the line's canonical bytes rather than by any of
/// its members, because the artifact is cumulative per session: every later
/// checkpoint of a session carries the earlier rows again, and the same line
/// must land once however many shards carry it and whichever order a rebuild
/// reads them in. The per-kind members are nullable columns of one relation,
/// not one relation per kind, since every reader asks about a window or a
/// revision across kinds. `file_revision` is the `n_file_revision` node id of
/// the revision a row names, which is how the row joins the read graph;
/// `checkpoint_id` is the first checkpoint whose ingest wrote it.
const N_WINDOW_ROW_COLUMNS: &[Column] = &[
    Column::text("row_id"),     // sha256 of the line's canonical bytes
    Column::text("window_key"), // the window journal's one-way key
    Column::text_null("subagent"),
    Column::text_one_of("kind", WINDOW_ROW_KINDS),
    Column::int("turn"),
    Column::int("at"),                   // epoch ms
    Column::text_null("path"),           // `/`-spelled repository path
    Column::text_null("content_sha256"), // a slice's or expect's hash, an edit's hash_new
    Column::text_null("hash_old"),
    Column::int_null("range_lo"),
    Column::int_null("range_hi"),
    Column::int_null("total_lines"),
    Column::text_null("sites"), // the canonical JSON array of `[lo, hi]` runs
    Column::int_null("shift"),
    Column::int_null("full"), // 0/1
    Column::text_null("reason"),
    Column::text_null("file_revision"), // FK: n_file_revision (comment-FK)
    Column::text("checkpoint_id"),
    Column::int("schema_version"),
];

/// The two bases a token count can rest on, spelled as `rift_tokens` spells
/// them (this crate does not depend on it): the bundled BPE, or the
/// conservative estimate it falls back to.
const TOKEN_COUNT_BASES: &[&str] = &["cl100k_base", "chars/4+1"];

/// Columns for the v16 `summary_token_count` relation: the raw token count of
/// each definition an outline names, and of the whole file, under the
/// outline's own summary key.
///
/// A sparse companion on `summary_binding`'s key rather than new columns:
/// that relation is in the frozen v1 body, and widening it would move a
/// checksum every deployed `recall.db` holds. Nor do the counts ride inside
/// the digest: the digest is the extractor's output alone, sealed under the
/// column cipher, while a count is the tokenizer's, read by queries that must
/// not decrypt anything. Ordinal 0 is the whole file; ordinal `n` is the
/// outline's `n`-th definition. Identifiers, line numbers and counts only:
/// never a name, which stays in the sealed digest.
const SUMMARY_TOKEN_COUNT_COLUMNS: &[Column] = &[
    Column::text("summary_id"), // FK: summary_binding(summary_id)
    Column::int("ordinal"),     // 0 = the whole file
    Column::int("line_lo"),
    Column::int("line_hi"),
    Column::int("tokens"), // raw, never calibrated
    Column::text_one_of("tokenizer", TOKEN_COUNT_BASES),
    Column::int("schema_version"),
];

/// Columns for the v17 `ingest_watermark_position` relation: where in its
/// dialect's walk the tip an `ingest_watermark` row names stood when the row
/// was written.
///
/// A sparse companion keyed on the watermark row's own id rather than a new
/// column: `ingest_watermark` shares one column list with the capture
/// projection and was created by the frozen v6 body, so widening it would
/// move a checksum every deployed `recall.db` holds. With the position kept,
/// index warm resumes from a whole stream cursor, and the cursor's
/// id-at-position check catches a stream rewritten under the index even when
/// the tip itself survived the rewrite. No row is the fact "written before
/// positions were kept", and the reader falls back to locating the tip.
const INGEST_WATERMARK_POSITION_COLUMNS: &[Column] = &[
    Column::text("watermark_id"), // FK: ingest_watermark(watermark_id)
    Column::int("chain_seq"),     // 1-based position of the tip; never 0
    Column::int("schema_version"),
];

/// Columns for the v18 `n_checkpoint_observed_head` relation: the `HEAD` a
/// turn-end seal saw, mirrored from shard `metadata.observed_head`.
///
/// Its own relation rather than a column on `n_checkpoint_commit`: an
/// observed head is not an anchor. The anchor stays one checkpoint per commit,
/// and a second claimant on one commit is the cursor regression slice 4 fixed.
/// The serving scope reads this only to admit an anchorless seal to the member
/// that wrote it, on its branch, while the seen head is still in its history.
const N_CHECKPOINT_OBSERVED_HEAD_COLUMNS: &[Column] = &[
    Column::text("node_id"),       // FK: n_checkpoint (1:0..1)
    Column::text("observed_head"), // hex oid, from metadata.observed_head
    Column::int("schema_version"),
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
/// `serve_grant_event` relations. The v13 `memd_audit_session` relation
/// starts here.
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
/// [`super::canonical_tables`], so every recall table gets its append-only guard for free.
#[must_use]
pub fn recall_canonical_tables() -> &'static [CanonicalTable] {
    RECALL_TABLES
}

/// Look up a recall canonical table by name.
#[must_use]
pub fn recall_canonical_table(name: &str) -> Option<&'static CanonicalTable> {
    RECALL_TABLES.iter().find(|t| t.name == name)
}

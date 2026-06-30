//! Typed, append-only writers for the capture-projection relations.
//!
//! These mirror the DDL in [`crate::schema`] and use [`CanonicalStore::insert_or_ignore`]
//! so re-ingesting the same content-addressed fact is a **no-op** (idempotent
//! set-union, db-design §4.4/§4.7). They never UPDATE/DELETE — corrections are new
//! superseding rows handled by the event bridge.
//!
//! Only the content-addressed diff-capture relations (`file_revision`,
//! `changed_hunk`) live here today; the session/turn/tool projection writers land
//! with the event bridge (P1 S4).

use rift_core::Result;

use crate::{CanonicalStore, Value};

/// A `file_revision` row: a `path@blob_oid` is one immutable, content-addressed
/// fact (P2 S3 / db-design §4.4).
#[derive(Debug, Clone)]
pub struct FileRevisionRow {
    /// `= sha256(path ‖ 0x00 ‖ blob_oid)` (hex).
    pub file_revision_id: String,
    /// File path.
    pub path: String,
    /// Git blob oid (hex).
    pub blob_oid: String,
    /// Byte-compatible content hash (hex).
    pub content_sha256: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `file_revision` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_file_revision(
    store: &impl CanonicalStore,
    r: &FileRevisionRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "file_revision",
            &[
                "file_revision_id",
                "path",
                "blob_oid",
                "content_sha256",
                "schema_version",
            ],
            &[
                Value::text(r.file_revision_id.clone()),
                Value::text(r.path.clone()),
                Value::text(r.blob_oid.clone()),
                Value::text(r.content_sha256.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `changed_hunk` row: a committed-diff hunk for `C` (P4 S1).
#[derive(Debug, Clone)]
pub struct ChangedHunkRow {
    /// `= hash(commit_id, file_revision_id, old_start, new_start)` (hex).
    pub hunk_id: String,
    /// The commit whose diff this hunk belongs to.
    pub commit_id: String,
    /// The file revision this hunk lands in (hex).
    pub file_revision_id: String,
    /// Old/new half-open line ranges.
    pub old_start: i64,
    /// Old length.
    pub old_len: i64,
    /// New start.
    pub new_start: i64,
    /// New length.
    pub new_len: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `changed_hunk` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_changed_hunk(store: &impl CanonicalStore, r: &ChangedHunkRow) -> Result<bool> {
    store
        .insert_or_ignore(
            "changed_hunk",
            &[
                "hunk_id",
                "commit_id",
                "file_revision_id",
                "old_start",
                "old_len",
                "new_start",
                "new_len",
                "schema_version",
            ],
            &[
                Value::text(r.hunk_id.clone()),
                Value::text(r.commit_id.clone()),
                Value::text(r.file_revision_id.clone()),
                Value::Int(r.old_start),
                Value::Int(r.old_len),
                Value::Int(r.new_start),
                Value::Int(r.new_len),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `token_usage` row: the four token classes, 1:1 with a `model_call` but a
/// separate relation (P2 S4).
#[derive(Debug, Clone)]
pub struct TokenUsageRow {
    /// The model call these counts are for.
    pub call_id: String,
    /// Uncached input tokens.
    pub uncached_input: i64,
    /// Cached input tokens.
    pub cached_input: i64,
    /// Cache-creation tokens.
    pub cache_creation: i64,
    /// Output tokens.
    pub output: i64,
    /// How the counts were derived (`provider_reported` | `tiktoken` | `estimator:<model>`).
    pub counter_method: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record `token_usage` (idempotent on `call_id`).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_token_usage(store: &impl CanonicalStore, r: &TokenUsageRow) -> Result<bool> {
    store
        .insert_or_ignore(
            "token_usage",
            &[
                "call_id",
                "uncached_input",
                "cached_input",
                "cache_creation",
                "output",
                "counter_method",
                "schema_version",
            ],
            &[
                Value::text(r.call_id.clone()),
                Value::Int(r.uncached_input),
                Value::Int(r.cached_input),
                Value::Int(r.cache_creation),
                Value::Int(r.output),
                Value::text(r.counter_method.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `price_table` row: a versioned (bitemporal) per-`model×class` price (P2 S4).
#[derive(Debug, Clone)]
pub struct PriceTableRow {
    /// Surrogate id (ULID).
    pub price_id: String,
    /// Model the price applies to.
    pub model: String,
    /// Token class (`uncached_input` | `cached_input` | `cache_creation` | `output`).
    pub token_class: String,
    /// Price in micro-units of currency per **million** tokens.
    pub unit_price_micros: i64,
    /// Currency code.
    pub currency: String,
    /// Valid-time start (provider effective date, epoch ms).
    pub valid_from: i64,
    /// Valid-time end (`None` = current).
    pub valid_to: Option<i64>,
    /// Transaction-time (epoch ms).
    pub recorded_at: i64,
    /// The price row this supersedes, if any.
    pub supersedes_id: Option<String>,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `price_table` row (idempotent on `price_id`; price changes supersede).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_price(store: &impl CanonicalStore, r: &PriceTableRow) -> Result<bool> {
    store
        .insert_or_ignore(
            "price_table",
            &[
                "price_id",
                "model",
                "token_class",
                "unit_price_micros",
                "currency",
                "valid_from",
                "valid_to",
                "recorded_at",
                "supersedes_id",
                "schema_version",
            ],
            &[
                Value::text(r.price_id.clone()),
                Value::text(r.model.clone()),
                Value::text(r.token_class.clone()),
                Value::Int(r.unit_price_micros),
                Value::text(r.currency.clone()),
                Value::Int(r.valid_from),
                r.valid_to.map_or(Value::Null, Value::Int),
                Value::Int(r.recorded_at),
                r.supersedes_id.clone().map_or(Value::Null, Value::text),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A per-file `attribution` rollup row, keyed by `(checkpoint, file_revision)`
/// (P2 S5). `confidence_milli ∈ 0..=1000` (writer-clamped; the float `[0,1]`
/// score scaled to integer milli-units so no float reaches the store).
#[derive(Debug, Clone)]
pub struct AttributionRow {
    /// Checkpoint this attribution is seen in.
    pub checkpoint_id: String,
    /// File revision attributed.
    pub file_revision_id: String,
    /// Agent-added line count.
    pub agent_added: i64,
    /// Human-added line count.
    pub human_added: i64,
    /// Agent lines a human later modified.
    pub human_modified_agent: i64,
    /// Agent lines a human deleted before commit.
    pub agent_removed_by_human: i64,
    /// Confidence in milli-units (0..=1000).
    pub confidence_milli: i64,
    /// Attribution algorithm version (provenance/reproducibility).
    pub method_version: String,
    /// Prior `(ckpt,rev)` this corrects, if any.
    pub supersedes_pk: Option<String>,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a per-file `attribution` rollup (idempotent on its composite PK).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_attribution(store: &impl CanonicalStore, r: &AttributionRow) -> Result<bool> {
    store
        .insert_or_ignore(
            "attribution",
            &[
                "checkpoint_id",
                "file_revision_id",
                "agent_added",
                "human_added",
                "human_modified_agent",
                "agent_removed_by_human",
                "confidence_milli",
                "method_version",
                "supersedes_pk",
                "schema_version",
            ],
            &[
                Value::text(r.checkpoint_id.clone()),
                Value::text(r.file_revision_id.clone()),
                Value::Int(r.agent_added),
                Value::Int(r.human_added),
                Value::Int(r.human_modified_agent),
                Value::Int(r.agent_removed_by_human),
                Value::Int(r.confidence_milli),
                Value::text(r.method_version.clone()),
                r.supersedes_pk.clone().map_or(Value::Null, Value::text),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// Clamp a `[0,1]` confidence to integer milli-units (`0..=1000`) for storage —
/// the no-float boundary for the attribution score.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn confidence_to_milli(confidence: f64) -> i64 {
    // c ∈ [0,1] ⇒ c*1000 ∈ [0,1000] ⇒ round ∈ 0..=1000, always fits i64.
    let c = confidence.clamp(0.0, 1.0);
    (c * 1000.0).round() as i64
}

// ----- hook lifecycle & wrapper audit writers (P3 / P1) ---------------------
//
// The DDL + immutability guards for these tables already live in [`crate::schema`]
// (registered in `canonical_tables()`); these are the typed writers. Ids are
// content-addressed by the caller (orchestration / bin layer), so a replayed hook
// or a re-provision of the same verified build is an idempotent no-op.

/// A `substrate_install` row: one verified capture-backend provision (P1 S1/B2).
/// `install_id` is content-addressed over `substrate_sha256`, so re-provisioning
/// the same build is a no-op and a post-upgrade hash is a new row (never an UPDATE).
#[derive(Debug, Clone)]
pub struct SubstrateInstallRow {
    /// `= sha256(substrate_sha256)` (hex).
    pub install_id: String,
    /// The verified SHA-256 of the installed build (hex; content-addressed identity).
    pub substrate_sha256: String,
    /// The Rift release this pin shipped with.
    pub rift_release: String,
    /// The private install root (off `PATH`).
    pub install_root: String,
    /// Transaction-time (epoch ms).
    pub verified_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `substrate_install` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_substrate_install(
    store: &impl CanonicalStore,
    r: &SubstrateInstallRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "substrate_install",
            &[
                "install_id",
                "substrate_sha256",
                "rift_release",
                "install_root",
                "verified_at",
                "schema_version",
            ],
            &[
                Value::text(r.install_id.clone()),
                Value::text(r.substrate_sha256.clone()),
                Value::text(r.rift_release.clone()),
                Value::text(r.install_root.clone()),
                Value::Int(r.verified_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `substrate_block_event` row: a default-deny refusal of a wrapped invocation
/// (P1 S3). `verb` is the refused root token only — never key/transcript bytes,
/// never the upstream brand.
#[derive(Debug, Clone)]
pub struct SubstrateBlockEventRow {
    /// `= sha256(verb ‖ reason ‖ occurred_at)` (hex).
    pub block_id: String,
    /// The refused root token.
    pub verb: String,
    /// Why it was refused (`forbidden` | `not-allowlisted` | `networky-arg`).
    pub reason: String,
    /// Transaction-time (epoch ms).
    pub occurred_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `substrate_block_event` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_substrate_block_event(
    store: &impl CanonicalStore,
    r: &SubstrateBlockEventRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "substrate_block_event",
            &[
                "block_id",
                "verb",
                "reason",
                "occurred_at",
                "schema_version",
            ],
            &[
                Value::text(r.block_id.clone()),
                Value::text(r.verb.clone()),
                Value::text(r.reason.clone()),
                Value::Int(r.occurred_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `session_transition` row: one append-only state-machine transition (P3 S3),
/// the durable source of truth for a session's phase. `transition_id` is
/// content-addressed so a replayed hook event is a no-op.
#[derive(Debug, Clone)]
pub struct SessionTransitionRow {
    /// `= sha256(session_id ‖ from_state ‖ to_state ‖ trigger_kind ‖ occurred_at)` (hex).
    pub transition_id: String,
    /// The session this transition belongs to.
    pub session_id: String,
    /// The state transitioned from.
    pub from_state: String,
    /// The state transitioned to.
    pub to_state: String,
    /// The trigger that drove it (`trigger` is a SQL keyword → `trigger_kind`).
    pub trigger_kind: String,
    /// Transaction-time (epoch ms).
    pub occurred_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `session_transition` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_session_transition(
    store: &impl CanonicalStore,
    r: &SessionTransitionRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "session_transition",
            &[
                "transition_id",
                "session_id",
                "from_state",
                "to_state",
                "trigger_kind",
                "occurred_at",
                "schema_version",
            ],
            &[
                Value::text(r.transition_id.clone()),
                Value::text(r.session_id.clone()),
                Value::text(r.from_state.clone()),
                Value::text(r.to_state.clone()),
                Value::text(r.trigger_kind.clone()),
                Value::Int(r.occurred_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// An `agent_hook_event` row: the receipt of one `rift hooks <agent> <verb>`
/// invocation (P3 S2). `session_id` may be `None` before `session.start`.
#[derive(Debug, Clone)]
pub struct AgentHookEventRow {
    /// `= sha256(agent ‖ verb ‖ session ‖ ordinal ‖ payload_hash)` (hex).
    pub hook_event_id: String,
    /// The agent that fired.
    pub agent: String,
    /// The agent-native verb.
    pub verb: String,
    /// The session (may be `None` pre-session-start).
    pub session_id: Option<String>,
    /// Transaction-time (epoch ms).
    pub received_at: i64,
    /// Outcome (`recorded` | `deferred` | `skipped-unknown-verb`).
    pub outcome: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record an `agent_hook_event` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_agent_hook_event(
    store: &impl CanonicalStore,
    r: &AgentHookEventRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "agent_hook_event",
            &[
                "hook_event_id",
                "agent",
                "verb",
                "session_id",
                "received_at",
                "outcome",
                "schema_version",
            ],
            &[
                Value::text(r.hook_event_id.clone()),
                Value::text(r.agent.clone()),
                Value::text(r.verb.clone()),
                r.session_id.clone().map_or(Value::Null, Value::text),
                Value::Int(r.received_at),
                Value::text(r.outcome.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `captured_event` row: the per-canonical-event idempotency index (P8 S2).
/// One row per *logical* event; `idempotency_key` is content/identity-derived, so
/// re-delivering the same hook is a true no-op (no double-count). The actual event
/// content lives in the canonical git artifacts (redacted) + the Phase-2/3
/// relations; this index is the dedup guard that keeps those append-only stores
/// from getting a duplicate.
#[derive(Debug, Clone)]
pub struct CapturedEventRow {
    /// `= sha256(agent ‖ session ‖ verb ‖ event_ordinal ‖ payload_sha256)` (hex).
    pub idempotency_key: String,
    /// The session this event belongs to.
    pub session_id: String,
    /// Turn ordinal within the session.
    pub turn_ordinal: i64,
    /// Event ordinal within the turn.
    pub event_ordinal: i64,
    /// Canonical `EventKind` token.
    pub kind: String,
    /// Canonical `Trust` token.
    pub trust: String,
    /// Content address of the (redacted) payload (hex).
    pub payload_sha256: String,
    /// Valid-time of the event (epoch ms).
    pub occurred_at_ms: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `captured_event` (idempotent on its content/identity-derived key).
/// Returns `true` if this was a new logical event, `false` if it was a duplicate
/// delivery (a true no-op — the dedup guarantee).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_captured_event(
    store: &impl CanonicalStore,
    r: &CapturedEventRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "captured_event",
            &[
                "idempotency_key",
                "session_id",
                "turn_ordinal",
                "event_ordinal",
                "kind",
                "trust",
                "payload_sha256",
                "occurred_at_ms",
                "schema_version",
            ],
            &[
                Value::text(r.idempotency_key.clone()),
                Value::text(r.session_id.clone()),
                Value::Int(r.turn_ordinal),
                Value::Int(r.event_ordinal),
                Value::text(r.kind.clone()),
                Value::text(r.trust.clone()),
                Value::text(r.payload_sha256.clone()),
                Value::Int(r.occurred_at_ms),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// An `agent_enablement` row: one enable/disable *event* for an agent (P8 S3).
/// enable/disable are events, not a mutable boolean, so the wiring history is
/// auditable. `change_id` is content-addressed so a re-run of the same action is a
/// no-op.
#[derive(Debug, Clone)]
pub struct AgentEnablementRow {
    /// `= sha256(agent ‖ action ‖ changed_at ‖ wired_hooks_hash)` (hex).
    pub change_id: String,
    /// The agent token.
    pub agent: String,
    /// The action (`enable` | `disable`).
    pub action: String,
    /// Transaction-time (epoch ms).
    pub changed_at: i64,
    /// SHA-256 over the wired hook config (provenance; hex).
    pub wired_hooks_hash: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record an `agent_enablement` event (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_agent_enablement(
    store: &impl CanonicalStore,
    r: &AgentEnablementRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "agent_enablement",
            &[
                "change_id",
                "agent",
                "action",
                "changed_at",
                "wired_hooks_hash",
                "schema_version",
            ],
            &[
                Value::text(r.change_id.clone()),
                Value::text(r.agent.clone()),
                Value::text(r.action.clone()),
                Value::Int(r.changed_at),
                Value::text(r.wired_hooks_hash.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// An `archive_decision` row: the audit of one checkpoint-strategy decision
/// (P3 S4). `archived` is a recorded outcome, never a mutated status.
#[derive(Debug, Clone)]
pub struct ArchiveDecisionRow {
    /// `= sha256(session_id ‖ strategy ‖ reason ‖ decided_at ‖ checkpoint_id?)` (hex).
    pub decision_id: String,
    /// The session this decision was about (may be `None`).
    pub session_id: Option<String>,
    /// Which strategy decided (`manual-commit` | `on-pr` | `scheduled` | `policy-driven`).
    pub strategy: String,
    /// Why it fired (`commit` | `pr` | `schedule` | `sensitive-path`).
    pub reason: String,
    /// Whether a checkpoint was produced.
    pub archived: bool,
    /// The resulting checkpoint id, if any (hex).
    pub checkpoint_id: Option<String>,
    /// Transaction-time (epoch ms).
    pub decided_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record an `archive_decision` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_archive_decision(
    store: &impl CanonicalStore,
    r: &ArchiveDecisionRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "archive_decision",
            &[
                "decision_id",
                "session_id",
                "strategy",
                "reason",
                "archived",
                "checkpoint_id",
                "decided_at",
                "schema_version",
            ],
            &[
                Value::text(r.decision_id.clone()),
                r.session_id.clone().map_or(Value::Null, Value::text),
                Value::text(r.strategy.clone()),
                Value::text(r.reason.clone()),
                Value::Int(i64::from(r.archived)),
                r.checkpoint_id.clone().map_or(Value::Null, Value::text),
                Value::Int(r.decided_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `verification_run` row: one `rift verify` / dual-run soak record (P5 S4 / P7 S2).
#[derive(Debug, Clone)]
pub struct VerificationRunRow {
    /// Surrogate run id.
    pub run_id: String,
    /// The branch verified (`rift/checkpoints/v1`).
    pub branch: String,
    /// The run's exit code (0 = clean).
    pub exit_code: i64,
    /// Transaction-time (epoch ms).
    pub checked_at: i64,
    /// SHA-256 over the run outcome (hex).
    pub outcome_sha256: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `verification_run` (idempotent on `run_id`).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_verification_run(
    store: &impl CanonicalStore,
    r: &VerificationRunRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "verification_run",
            &[
                "run_id",
                "branch",
                "exit_code",
                "checked_at",
                "outcome_sha256",
                "schema_version",
            ],
            &[
                Value::text(r.run_id.clone()),
                Value::text(r.branch.clone()),
                Value::Int(r.exit_code),
                Value::Int(r.checked_at),
                Value::text(r.outcome_sha256.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `verification_divergence` row: one parity divergence in a run (P5 S4 / P7 S2).
#[derive(Debug, Clone)]
pub struct VerificationDivergenceRow {
    /// The run this divergence belongs to.
    pub run_id: String,
    /// Order within the run.
    pub ordinal: i64,
    /// The checkpoint involved (may be empty for capability-level divergences).
    pub checkpoint_id: String,
    /// The layer that diverged (`Leaf` | `CheckpointRoot` | `git_write` | …).
    pub layer: String,
    /// The comparison site (`GitVsRecomputed` | `DualRunWrapperVsNative` | …).
    pub site: String,
    /// A human-readable detail (identifiers/hashes only — never transcript bytes).
    pub detail: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `verification_divergence` (idempotent on `(run_id, ordinal)`).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_verification_divergence(
    store: &impl CanonicalStore,
    r: &VerificationDivergenceRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "verification_divergence",
            &[
                "run_id",
                "ordinal",
                "checkpoint_id",
                "layer",
                "site",
                "detail",
                "schema_version",
            ],
            &[
                Value::text(r.run_id.clone()),
                Value::Int(r.ordinal),
                Value::text(r.checkpoint_id.clone()),
                Value::text(r.layer.clone()),
                Value::text(r.site.clone()),
                Value::text(r.detail.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteStore;

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    fn file_rev(id: &str) -> FileRevisionRow {
        FileRevisionRow {
            file_revision_id: id.into(),
            path: "src/lib.rs".into(),
            // distinct oid per id so UNIQUE(path, blob_oid) holds (in reality the
            // id is *derived* from (path, blob_oid), so they co-vary)
            blob_oid: format!("oid-{id}"),
            content_sha256: "def456".into(),
            schema_version: 1,
        }
    }

    #[tokio::test]
    async fn file_revision_insert_is_idempotent() {
        let s = store();
        assert!(
            record_file_revision(&s, &file_rev("rev1")).await.unwrap(),
            "first insert"
        );
        assert!(
            !record_file_revision(&s, &file_rev("rev1")).await.unwrap(),
            "re-insert of same id is a no-op (set-union)"
        );
        assert_eq!(s.count("file_revision").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn changed_hunk_round_trips_and_is_idempotent() {
        let s = store();
        record_file_revision(&s, &file_rev("rev1")).await.unwrap();
        let h = ChangedHunkRow {
            hunk_id: "hunk1".into(),
            commit_id: "commitA".into(),
            file_revision_id: "rev1".into(),
            old_start: 1,
            old_len: 1,
            new_start: 1,
            new_len: 2,
            schema_version: 1,
        };
        assert!(record_changed_hunk(&s, &h).await.unwrap());
        assert!(!record_changed_hunk(&s, &h).await.unwrap(), "idempotent");
        assert_eq!(s.count("changed_hunk").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn distinct_keys_accumulate() {
        let s = store();
        record_file_revision(&s, &file_rev("rev1")).await.unwrap();
        record_file_revision(&s, &file_rev("rev2")).await.unwrap();
        assert_eq!(s.count("file_revision").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn token_usage_is_idempotent_per_call() {
        let s = store();
        let row = TokenUsageRow {
            call_id: "call1".into(),
            uncached_input: 100,
            cached_input: 20,
            cache_creation: 5,
            output: 50,
            counter_method: "estimator:heuristic".into(),
            schema_version: 1,
        };
        assert!(record_token_usage(&s, &row).await.unwrap());
        assert!(
            !record_token_usage(&s, &row).await.unwrap(),
            "1:1 per call_id"
        );
        assert_eq!(s.count("token_usage").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn price_and_attribution_round_trip() {
        let s = store();
        record_price(
            &s,
            &PriceTableRow {
                price_id: "p1".into(),
                model: "claude-opus".into(),
                token_class: "output".into(),
                unit_price_micros: 15_000_000,
                currency: "USD".into(),
                valid_from: 0,
                valid_to: None,
                recorded_at: 1,
                supersedes_id: None,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        assert_eq!(s.count("price_table").await.unwrap(), 1);

        record_attribution(
            &s,
            &AttributionRow {
                checkpoint_id: "ckpt1".into(),
                file_revision_id: "rev1".into(),
                agent_added: 10,
                human_added: 3,
                human_modified_agent: 2,
                agent_removed_by_human: 1,
                confidence_milli: confidence_to_milli(0.61),
                method_version: "attr@1".into(),
                supersedes_pk: None,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        assert_eq!(s.count("attribution").await.unwrap(), 1);
    }

    #[test]
    fn confidence_milli_clamps_and_rounds() {
        assert_eq!(confidence_to_milli(0.0), 0);
        assert_eq!(confidence_to_milli(1.0), 1000);
        assert_eq!(confidence_to_milli(0.611), 611);
        assert_eq!(confidence_to_milli(1.5), 1000, "clamped above");
        assert_eq!(confidence_to_milli(-0.5), 0, "clamped below");
    }

    #[tokio::test]
    async fn substrate_install_is_idempotent_per_hash() {
        let s = store();
        let row = SubstrateInstallRow {
            install_id: "install-A".into(), // = sha256(substrate_sha256) at the call site
            substrate_sha256: "aa".repeat(32),
            rift_release: "0.1.0".into(),
            install_root: "/data/rift/substrate".into(),
            verified_at: 100,
            schema_version: 1,
        };
        assert!(record_substrate_install(&s, &row).await.unwrap());
        assert!(
            !record_substrate_install(&s, &row).await.unwrap(),
            "same verified hash → idempotent no-op"
        );
        // A different verified build (post-upgrade) is a new row, never an UPDATE.
        let upgraded = SubstrateInstallRow {
            install_id: "install-B".into(),
            substrate_sha256: "bb".repeat(32),
            ..row
        };
        assert!(record_substrate_install(&s, &upgraded).await.unwrap());
        assert_eq!(s.count("substrate_install").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn block_event_and_transition_round_trip_idempotent() {
        let s = store();
        let blk = SubstrateBlockEventRow {
            block_id: "blk1".into(),
            verb: "auth".into(),
            reason: "forbidden".into(),
            occurred_at: 1,
            schema_version: 1,
        };
        assert!(record_substrate_block_event(&s, &blk).await.unwrap());
        assert!(!record_substrate_block_event(&s, &blk).await.unwrap());

        let tr = SessionTransitionRow {
            transition_id: "t1".into(),
            session_id: "01JSESSION".into(),
            from_state: "idle".into(),
            to_state: "active".into(),
            trigger_kind: "start".into(),
            occurred_at: 2,
            schema_version: 1,
        };
        assert!(record_session_transition(&s, &tr).await.unwrap());
        assert!(
            !record_session_transition(&s, &tr).await.unwrap(),
            "a replayed hook event is a no-op"
        );
        assert_eq!(s.count("substrate_block_event").await.unwrap(), 1);
        assert_eq!(s.count("session_transition").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn agent_hook_event_allows_null_session() {
        let s = store();
        let pre = AgentHookEventRow {
            hook_event_id: "h1".into(),
            agent: "claude-code".into(),
            verb: "SessionStart".into(),
            session_id: None, // pre-session-start
            received_at: 1,
            outcome: "recorded".into(),
            schema_version: 1,
        };
        assert!(record_agent_hook_event(&s, &pre).await.unwrap());
        assert!(!record_agent_hook_event(&s, &pre).await.unwrap());
        assert_eq!(s.count("agent_hook_event").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn archive_decision_records_outcome_and_is_idempotent() {
        let s = store();
        let archived = ArchiveDecisionRow {
            decision_id: "d1".into(),
            session_id: Some("01JSESSION".into()),
            strategy: "manual-commit".into(),
            reason: "commit".into(),
            archived: true,
            checkpoint_id: Some("ckpt1".into()),
            decided_at: 5,
            schema_version: 1,
        };
        assert!(record_archive_decision(&s, &archived).await.unwrap());
        assert!(
            !record_archive_decision(&s, &archived).await.unwrap(),
            "a retried hook at the same decided_at is idempotent"
        );
        // A no-op decision (didn't archive) is still a recorded fact.
        let no_archive = ArchiveDecisionRow {
            decision_id: "d2".into(),
            archived: false,
            checkpoint_id: None,
            decided_at: 6,
            ..archived
        };
        assert!(record_archive_decision(&s, &no_archive).await.unwrap());
        assert_eq!(s.count("archive_decision").await.unwrap(), 2);
    }
}

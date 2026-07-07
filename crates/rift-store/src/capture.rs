//! Typed, append-only writers for the capture-projection relations.
//!
//! These mirror the DDL in [`crate::schema`] and use [`CanonicalStore::insert_or_ignore`]
//! so re-ingesting the same content-addressed fact is a **no-op** (idempotent
//! set-union, db-design §4.4/§4.7). They never UPDATE/DELETE — corrections are new
//! superseding rows handled by the event bridge.
//!
//! Covers the full normalized capture model (M1): the diff-capture relations,
//! the session/turn/model-call/tool-call decomposition (P2 S3), token usage,
//! the turn-boundary relations (P4 S2), the ingest watermark (P1 S4), and the
//! hook/wrapper audit relations. Ids are content-addressed by the caller
//! (`rift-shadow`'s shared id-derivation module), never here.

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

/// The four effective per-MTok prices for one model, resolved bitemporally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectivePrices {
    /// micro-currency per 1M uncached input tokens.
    pub uncached_input: i64,
    /// micro-currency per 1M cached input tokens.
    pub cached_input: i64,
    /// micro-currency per 1M cache-creation tokens.
    pub cache_creation: i64,
    /// micro-currency per 1M output tokens.
    pub output: i64,
}

/// Resolve one class's effective price: in force at `valid_at` per the belief
/// held at `as_of` (bitemporal, P6 S3): latest `valid_from ≤ valid_at`, then
/// latest `recorded_at ≤ as_of`, deterministic tie-break on `price_id`.
async fn effective_class_price(
    store: &impl CanonicalStore,
    model: &str,
    token_class: &str,
    valid_at: i64,
    as_of: i64,
) -> Result<Option<i64>> {
    store
        .query_opt_i64(
            "SELECT unit_price_micros FROM price_table \
             WHERE model = ?1 AND token_class = ?2 \
               AND valid_from <= ?3 AND (valid_to IS NULL OR valid_to > ?3) \
               AND recorded_at <= ?4 \
             ORDER BY valid_from DESC, recorded_at DESC, price_id DESC LIMIT 1",
            &[
                Value::text(model.to_owned()),
                Value::text(token_class.to_owned()),
                Value::Int(valid_at),
                Value::Int(as_of),
            ],
        )
        .await
}

/// Resolve all four classes for `model`, or `None` if any class has no price on
/// file — a partial catalog never produces a partially-priced cost fact.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn effective_prices(
    store: &impl CanonicalStore,
    model: &str,
    valid_at: i64,
    as_of: i64,
) -> Result<Option<EffectivePrices>> {
    let (Some(uncached_input), Some(cached_input), Some(cache_creation), Some(output)) = (
        effective_class_price(store, model, "uncached_input", valid_at, as_of).await?,
        effective_class_price(store, model, "cached_input", valid_at, as_of).await?,
        effective_class_price(store, model, "cache_creation", valid_at, as_of).await?,
        effective_class_price(store, model, "output", valid_at, as_of).await?,
    ) else {
        return Ok(None);
    };
    Ok(Some(EffectivePrices {
        uncached_input,
        cached_input,
        cache_creation,
        output,
    }))
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

// ----- normalized session / turn / call writers (P1 S4 · P2 S3) -------------
//
// The decomposed model both event sources project into: agent hooks live, the
// artifact bridge authoritatively (ADR-0002). Every id is content-addressed by
// `rift-shadow`'s shared id-derivation module, so hook-rows ∪ bridge-rows is a
// set-union no-op by construction.

/// A `session` row (P2 S3): one agent session.
#[derive(Debug, Clone)]
pub struct SessionRow {
    /// Content-addressed session id (derived from the agent's native id).
    pub session_id: String,
    /// The agent (`claude-code` | …).
    pub agent: String,
    /// The headline model (may be empty until a model call reports one).
    pub model_primary: String,
    /// The repo this session ran in.
    pub repo_id: String,
    /// Valid-time (epoch ms).
    pub started_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `session` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_session(store: &impl CanonicalStore, r: &SessionRow) -> Result<bool> {
    store
        .insert_or_ignore(
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
                Value::text(r.session_id.clone()),
                Value::text(r.agent.clone()),
                Value::text(r.model_primary.clone()),
                Value::text(r.repo_id.clone()),
                Value::Int(r.started_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `session_event` row: lifecycle as events, never a mutable status column.
#[derive(Debug, Clone)]
pub struct SessionEventRow {
    /// Content-addressed event id.
    pub event_id: String,
    /// The session.
    pub session_id: String,
    /// `start` | `end` | `compaction`.
    pub kind: String,
    /// Valid-time (epoch ms).
    pub occurred_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `session_event` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_session_event(
    store: &impl CanonicalStore,
    r: &SessionEventRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "session_event",
            &[
                "event_id",
                "session_id",
                "kind",
                "occurred_at",
                "schema_version",
            ],
            &[
                Value::text(r.event_id.clone()),
                Value::text(r.session_id.clone()),
                Value::text(r.kind.clone()),
                Value::Int(r.occurred_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `turn` row: one prompt→response cycle within a session.
#[derive(Debug, Clone)]
pub struct TurnRow {
    /// Content-addressed turn id (`session ‖ ordinal`).
    pub turn_id: String,
    /// The session.
    pub session_id: String,
    /// Order within the session.
    pub ordinal: i64,
    /// Valid-time (epoch ms).
    pub started_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `turn` (idempotent on its content-addressed key; the
/// `UNIQUE(session_id, ordinal)` constraint also absorbs replays).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_turn(store: &impl CanonicalStore, r: &TurnRow) -> Result<bool> {
    store
        .insert_or_ignore(
            "turn",
            &[
                "turn_id",
                "session_id",
                "ordinal",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(r.turn_id.clone()),
                Value::text(r.session_id.clone()),
                Value::Int(r.ordinal),
                Value::Int(r.started_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `model_call` row: one LLM invocation within a turn.
#[derive(Debug, Clone)]
pub struct ModelCallRow {
    /// Content-addressed call id.
    pub call_id: String,
    /// The turn.
    pub turn_id: String,
    /// The model invoked (may differ from `session.model_primary`).
    pub model: String,
    /// Valid-time (epoch ms).
    pub started_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `model_call` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_model_call(store: &impl CanonicalStore, r: &ModelCallRow) -> Result<bool> {
    store
        .insert_or_ignore(
            "model_call",
            &[
                "call_id",
                "turn_id",
                "model",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(r.call_id.clone()),
                Value::text(r.turn_id.clone()),
                Value::text(r.model.clone()),
                Value::Int(r.started_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `tool_call` row: one tool invocation within a turn.
#[derive(Debug, Clone)]
pub struct ToolCallRow {
    /// Content-addressed tool-call id.
    pub tool_call_id: String,
    /// The turn.
    pub turn_id: String,
    /// The tool, agent-namespaced.
    pub tool_name: String,
    /// Valid-time (epoch ms).
    pub started_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `tool_call` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_tool_call(store: &impl CanonicalStore, r: &ToolCallRow) -> Result<bool> {
    store
        .insert_or_ignore(
            "tool_call",
            &[
                "tool_call_id",
                "turn_id",
                "tool_name",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(r.tool_call_id.clone()),
                Value::text(r.turn_id.clone()),
                Value::text(r.tool_name.clone()),
                Value::Int(r.started_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// Record a `tool_reads_file` link (M:N, its own relation — 4NF).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_tool_reads_file(
    store: &impl CanonicalStore,
    tool_call_id: &str,
    file_revision_id: &str,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "tool_reads_file",
            &["tool_call_id", "file_revision_id"],
            &[
                Value::text(tool_call_id.to_owned()),
                Value::text(file_revision_id.to_owned()),
            ],
        )
        .await
}

/// Record a `tool_edits_file` link (distinct from reads — they vary independently).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_tool_edits_file(
    store: &impl CanonicalStore,
    tool_call_id: &str,
    file_revision_id: &str,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "tool_edits_file",
            &["tool_call_id", "file_revision_id"],
            &[
                Value::text(tool_call_id.to_owned()),
                Value::text(file_revision_id.to_owned()),
            ],
        )
        .await
}

/// A `subagent_session` row: a child session linked to its parent turn.
#[derive(Debug, Clone)]
pub struct SubagentSessionRow {
    /// The child session (FK: session).
    pub child_session_id: String,
    /// The parent turn (FK: turn).
    pub parent_turn_id: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `subagent_session` (idempotent on the child id).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_subagent_session(
    store: &impl CanonicalStore,
    r: &SubagentSessionRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "subagent_session",
            &["child_session_id", "parent_turn_id", "schema_version"],
            &[
                Value::text(r.child_session_id.clone()),
                Value::text(r.parent_turn_id.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `session_in_checkpoint` row: the M:N checkpoint↔session link.
#[derive(Debug, Clone)]
pub struct SessionInCheckpointRow {
    /// The checkpoint.
    pub checkpoint_id: String,
    /// The session.
    pub session_id: String,
    /// Order within the checkpoint.
    pub ordinal: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `session_in_checkpoint` (idempotent on the composite key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_session_in_checkpoint(
    store: &impl CanonicalStore,
    r: &SessionInCheckpointRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "session_in_checkpoint",
            &["checkpoint_id", "session_id", "ordinal", "schema_version"],
            &[
                Value::text(r.checkpoint_id.clone()),
                Value::text(r.session_id.clone()),
                Value::Int(r.ordinal),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// An `attribution_hunk` row: the per-hunk detail the per-file rollup sums.
#[derive(Debug, Clone)]
pub struct AttributionHunkRow {
    /// The anchoring checkpoint.
    pub checkpoint_id: String,
    /// The file revision.
    pub file_revision_id: String,
    /// Hunk order within the file's diff.
    pub hunk_ordinal: i64,
    /// New-side start line.
    pub new_start: i64,
    /// New-side length.
    pub new_len: i64,
    /// One of `agent_added` | `human_added` | `human_modified_agent` |
    /// `agent_removed_by_human` | `unknown`.
    pub category: String,
    /// Lines in this hunk attributed to `category`.
    pub line_count: i64,
    /// Confidence × 1000 (0..=1000).
    pub confidence_milli: i64,
    /// The attribution method version that produced this row.
    pub method_version: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record an `attribution_hunk` (idempotent on the composite key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_attribution_hunk(
    store: &impl CanonicalStore,
    r: &AttributionHunkRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "attribution_hunk",
            &[
                "checkpoint_id",
                "file_revision_id",
                "hunk_ordinal",
                "new_start",
                "new_len",
                "category",
                "line_count",
                "confidence_milli",
                "method_version",
                "schema_version",
            ],
            &[
                Value::text(r.checkpoint_id.clone()),
                Value::text(r.file_revision_id.clone()),
                Value::Int(r.hunk_ordinal),
                Value::Int(r.new_start),
                Value::Int(r.new_len),
                Value::text(r.category.clone()),
                Value::Int(r.line_count),
                Value::Int(r.confidence_milli),
                Value::text(r.method_version.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

// ----- ingest watermark (P1 S4) ---------------------------------------------

/// An `ingest_watermark` row: the append-only record of the last checkpoint
/// commit ingested per branch. Re-ingest from the watermark is a no-op on
/// already-seen content; the latest row per branch is the high-water mark.
#[derive(Debug, Clone)]
pub struct IngestWatermarkRow {
    /// `= sha256(branch ‖ branch_tip)` (hex) — re-recording the same tip is a no-op.
    pub watermark_id: String,
    /// The ingested branch (`rift/checkpoints/v1`).
    pub branch: String,
    /// The branch tip (git oid, hex) at ingest time.
    pub branch_tip: String,
    /// Transaction-time (epoch ms).
    pub ingested_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record an `ingest_watermark` advance (idempotent on `(branch, tip)`).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_ingest_watermark(
    store: &impl CanonicalStore,
    r: &IngestWatermarkRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "ingest_watermark",
            &[
                "watermark_id",
                "branch",
                "branch_tip",
                "ingested_at",
                "schema_version",
            ],
            &[
                Value::text(r.watermark_id.clone()),
                Value::text(r.branch.clone()),
                Value::text(r.branch_tip.clone()),
                Value::Int(r.ingested_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// When the given session last actually archived (`archive_decision` with
/// `archived = true`), if ever — the scheduled strategy's reference point.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn latest_archive_ms(
    store: &impl CanonicalStore,
    session_id: &str,
) -> Result<Option<i64>> {
    store
        .query_opt_i64(
            "SELECT decided_at FROM archive_decision \
             WHERE session_id = ?1 AND archived = 1 \
             ORDER BY decided_at DESC, decision_id DESC LIMIT 1",
            &[Value::text(session_id.to_owned())],
        )
        .await
}

/// One captured read fact for a session: the tool call that read `path` whose
/// bytes hashed to `content_sha256` at read time (M5-06) — exactly the triple
/// the checkpoint shard's `tool_calls.jsonl` line carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionReadFact {
    /// The tool-call row id anchoring the read.
    pub tool_call_id: String,
    /// The tool name (`Read`-shaped).
    pub tool_name: String,
    /// Repo-relative path.
    pub path: String,
    /// SHA-256 (hex) of the bytes at read time.
    pub content_sha256: String,
}

/// Every read fact captured for `session_row_id` (the projected session id),
/// oldest first — the condense-time source of the shard's `tool_calls.jsonl`.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn session_read_facts(
    store: &impl CanonicalStore,
    session_row_id: &str,
) -> Result<Vec<SessionReadFact>> {
    let rows = store
        .query(
            "SELECT tc.tool_call_id, tc.tool_name, fr.path, fr.content_sha256 \
             FROM tool_reads_file trf \
             JOIN tool_call tc ON tc.tool_call_id = trf.tool_call_id \
             JOIN turn t ON t.turn_id = tc.turn_id \
             JOIN file_revision fr ON fr.file_revision_id = trf.file_revision_id \
             WHERE t.session_id = ?1 \
             ORDER BY tc.started_at ASC, tc.tool_call_id ASC",
            &[Value::text(session_row_id.to_owned())],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(SessionReadFact {
            tool_call_id: r.get_text(0)?,
            tool_name: r.get_text(1)?,
            path: r.get_text(2)?,
            content_sha256: r.get_text(3)?,
        });
    }
    Ok(out)
}

/// The most recently recorded branch tip for `branch`, if any — where the
/// bridge resumes from. Append-only reads: latest `ingested_at` wins (ties
/// broken by `watermark_id` for determinism).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn latest_ingest_watermark(
    store: &impl CanonicalStore,
    branch: &str,
) -> Result<Option<String>> {
    let rows = store
        .query(
            "SELECT branch_tip FROM ingest_watermark WHERE branch = ?1 \
             ORDER BY ingested_at DESC, watermark_id DESC LIMIT 1",
            &[Value::text(branch.to_owned())],
        )
        .await?;
    match rows.first() {
        Some(r) => Ok(Some(r.get_text(0)?)),
        None => Ok(None),
    }
}

// ----- turn boundaries (P4 S2, pulled into M1) -------------------------------

/// A `turn_boundary_snapshot` row: the working-tree observation that closes an
/// H_i (`start`, human) or A_i (`end`, agent) edit window.
#[derive(Debug, Clone)]
pub struct TurnBoundarySnapshotRow {
    /// `= sha256(session_id ‖ turn_ordinal ‖ boundary)` (hex).
    pub snapshot_id: String,
    /// The session.
    pub session_id: String,
    /// The turn ordinal this boundary belongs to.
    pub turn_ordinal: i64,
    /// `start` | `end`.
    pub boundary: String,
    /// Whose edit window this boundary closes: `human` | `agent`.
    pub actor: String,
    /// Content hash over the boundary's `(path, content_sha256)` set.
    pub worktree_hash: String,
    /// Valid-time (epoch ms).
    pub captured_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `turn_boundary_snapshot` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_turn_boundary_snapshot(
    store: &impl CanonicalStore,
    r: &TurnBoundarySnapshotRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "turn_boundary_snapshot",
            &[
                "snapshot_id",
                "session_id",
                "turn_ordinal",
                "boundary",
                "actor",
                "worktree_hash",
                "captured_at",
                "schema_version",
            ],
            &[
                Value::text(r.snapshot_id.clone()),
                Value::text(r.session_id.clone()),
                Value::Int(r.turn_ordinal),
                Value::text(r.boundary.clone()),
                Value::text(r.actor.clone()),
                Value::text(r.worktree_hash.clone()),
                Value::Int(r.captured_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `turn_boundary_file` row: one file's content hash at a boundary. Only
/// hashes/oids are durable — the bytes stay in the local git object store.
#[derive(Debug, Clone)]
pub struct TurnBoundaryFileRow {
    /// The boundary snapshot (FK).
    pub snapshot_id: String,
    /// File path.
    pub path: String,
    /// Git blob oid of the file bytes at this boundary (hex).
    pub blob_oid: String,
    /// Content hash of those bytes (hex).
    pub content_sha256: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `turn_boundary_file` (idempotent on `(snapshot, path)`).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_turn_boundary_file(
    store: &impl CanonicalStore,
    r: &TurnBoundaryFileRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "turn_boundary_file",
            &[
                "snapshot_id",
                "path",
                "blob_oid",
                "content_sha256",
                "schema_version",
            ],
            &[
                Value::text(r.snapshot_id.clone()),
                Value::text(r.path.clone()),
                Value::text(r.blob_oid.clone()),
                Value::text(r.content_sha256.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `turn_diff_hunk` row: one H_i/A_i hunk (coordinates only) — the edit-stack
/// input the LIFO attribution replay consumes at commit time.
#[derive(Debug, Clone)]
pub struct TurnDiffHunkRow {
    /// `= sha256(session ‖ ordinal ‖ edit_kind ‖ path ‖ coords)` (hex).
    pub hunk_id: String,
    /// The session.
    pub session_id: String,
    /// The turn ordinal.
    pub turn_ordinal: i64,
    /// `human` (H_i) | `agent` (A_i).
    pub edit_kind: String,
    /// File path (boundary-time; renames resolve at commit).
    pub path: String,
    /// Old-side start line.
    pub old_start: i64,
    /// Old-side length.
    pub old_len: i64,
    /// New-side start line.
    pub new_start: i64,
    /// New-side length.
    pub new_len: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `turn_diff_hunk` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_turn_diff_hunk(
    store: &impl CanonicalStore,
    r: &TurnDiffHunkRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "turn_diff_hunk",
            &[
                "hunk_id",
                "session_id",
                "turn_ordinal",
                "edit_kind",
                "path",
                "old_start",
                "old_len",
                "new_start",
                "new_len",
                "schema_version",
            ],
            &[
                Value::text(r.hunk_id.clone()),
                Value::text(r.session_id.clone()),
                Value::Int(r.turn_ordinal),
                Value::text(r.edit_kind.clone()),
                Value::text(r.path.clone()),
                Value::Int(r.old_start),
                Value::Int(r.old_len),
                Value::Int(r.new_start),
                Value::Int(r.new_len),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// The per-file rows of the session's **latest** boundary snapshot (the state
/// the next boundary diffs against). Ordering: highest turn ordinal wins, and
/// within a turn `end` supersedes `start`.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn latest_turn_boundary_files(
    store: &impl CanonicalStore,
    session_id: &str,
) -> Result<Vec<TurnBoundaryFileRow>> {
    let rows = store
        .query(
            "SELECT f.snapshot_id, f.path, f.blob_oid, f.content_sha256, f.schema_version \
             FROM turn_boundary_file f WHERE f.snapshot_id = (\
               SELECT s.snapshot_id FROM turn_boundary_snapshot s \
               WHERE s.session_id = ?1 \
               ORDER BY s.turn_ordinal DESC, \
                        CASE s.boundary WHEN 'end' THEN 1 ELSE 0 END DESC, \
                        s.snapshot_id DESC LIMIT 1) \
             ORDER BY f.path ASC",
            &[Value::text(session_id.to_owned())],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(TurnBoundaryFileRow {
            snapshot_id: r.get_text(0)?,
            path: r.get_text(1)?,
            blob_oid: r.get_text(2)?,
            content_sha256: r.get_text(3)?,
            schema_version: r.get_i64(4)?,
        });
    }
    Ok(out)
}

/// How many `start` / `end` boundary snapshots exist for a session — the
/// capture-coverage signal attribution confidence keys off.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn turn_boundary_counts(
    store: &impl CanonicalStore,
    session_id: &str,
) -> Result<(i64, i64)> {
    let starts = store
        .query_opt_i64(
            "SELECT COUNT(*) FROM turn_boundary_snapshot \
             WHERE session_id = ?1 AND boundary = 'start'",
            &[Value::text(session_id.to_owned())],
        )
        .await?
        .unwrap_or(0);
    let ends = store
        .query_opt_i64(
            "SELECT COUNT(*) FROM turn_boundary_snapshot \
             WHERE session_id = ?1 AND boundary = 'end'",
            &[Value::text(session_id.to_owned())],
        )
        .await?
        .unwrap_or(0);
    Ok((starts, ends))
}

/// All `turn_diff_hunk` rows for one session, in replay order: by turn ordinal,
/// then H (human) before A (agent) within a turn — the LIFO edit-stack order
/// `[H_0, A_0, H_1, A_1, …]` — then by path/position for determinism.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn turn_diff_hunks_for_session(
    store: &impl CanonicalStore,
    session_id: &str,
) -> Result<Vec<TurnDiffHunkRow>> {
    let rows = store
        .query(
            "SELECT hunk_id, session_id, turn_ordinal, edit_kind, path, \
                    old_start, old_len, new_start, new_len, schema_version \
             FROM turn_diff_hunk WHERE session_id = ?1 \
             ORDER BY turn_ordinal ASC, \
                      CASE edit_kind WHEN 'human' THEN 0 ELSE 1 END ASC, \
                      path ASC, new_start ASC, hunk_id ASC",
            &[Value::text(session_id.to_owned())],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(TurnDiffHunkRow {
            hunk_id: r.get_text(0)?,
            session_id: r.get_text(1)?,
            turn_ordinal: r.get_i64(2)?,
            edit_kind: r.get_text(3)?,
            path: r.get_text(4)?,
            old_start: r.get_i64(5)?,
            old_len: r.get_i64(6)?,
            new_start: r.get_i64(7)?,
            new_len: r.get_i64(8)?,
            schema_version: r.get_i64(9)?,
        });
    }
    Ok(out)
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

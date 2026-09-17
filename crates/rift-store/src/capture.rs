//! Typed, append-only writers for the capture-projection relations.
//!
//! These mirror the DDL in [`crate::schema`] and use [`CanonicalStore::insert_or_ignore`]
//! so re-ingesting the same content-addressed fact is a **no-op** (idempotent
//! set-union, db-design §4.4/§4.7). They never UPDATE/DELETE — corrections are new
//! superseding rows handled by the event bridge.
//!
//! Covers the full normalized capture model: the diff-capture relations,
//! the session/turn/model-call/tool-call decomposition, token usage,
//! the turn-boundary relations, the ingest watermark, and the hook and
//! retired-wrapper audit relations. Ids are content-addressed by the caller
//! (`rift-shadow`'s shared id-derivation module), never here.

use rift_core::Result;

use crate::{CanonicalStore, Value};

/// Explicit mapping between one adapter-native session identity and the
/// canonical capture-session row id shared by projection and Canopy.
#[derive(Debug, Clone)]
pub struct CaptureSessionIdentityRow {
    /// Canonical capture-session row id.
    pub session_id: String,
    /// Adapter name such as `claude-code`.
    pub agent: String,
    /// Session id emitted by the adapter.
    pub native_session_id: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a native-to-canonical capture-session identity (idempotent).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_capture_session_identity(
    store: &impl CanonicalStore,
    r: &CaptureSessionIdentityRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "capture_session_identity",
            &["session_id", "agent", "native_session_id", "schema_version"],
            &[
                Value::text(r.session_id.clone()),
                Value::text(r.agent.clone()),
                Value::text(r.native_session_id.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A prospective main-chain context snapshot. Unlike [`TokenUsageRow`], this
/// is one model context at one observable boundary, never whole-turn spend.
#[derive(Debug, Clone)]
pub struct MainContextObservationRow {
    /// Content-addressed logical observation id.
    pub observation_id: String,
    /// Canonical capture-session row id.
    pub session_id: String,
    /// Provider model identifier.
    pub model: String,
    /// Observation valid-time in epoch milliseconds.
    pub observed_at: i64,
    /// Uncached input tokens currently in the context.
    pub uncached_input: i64,
    /// Cached-read input tokens currently in the context.
    pub cached_input: i64,
    /// Cache-creation tokens currently in the context.
    pub cache_creation: i64,
    /// Output tokens currently in the context.
    pub output: i64,
    /// Counter provenance, for example `provider_reported`.
    pub counter_method: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a main-context observation (idempotent on its logical hook boundary).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_main_context_observation(
    store: &impl CanonicalStore,
    r: &MainContextObservationRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "main_context_observation",
            &[
                "observation_id",
                "session_id",
                "model",
                "observed_at",
                "uncached_input",
                "cached_input",
                "cache_creation",
                "output",
                "counter_method",
                "schema_version",
            ],
            &[
                Value::text(r.observation_id.clone()),
                Value::text(r.session_id.clone()),
                Value::text(r.model.clone()),
                Value::Int(r.observed_at),
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

/// One immutable observation that a capture session used or was configured for
/// a named model.
#[derive(Debug, Clone)]
pub struct ModelObservationRow {
    /// Content-addressed observation id.
    pub observation_id: String,
    /// Canonical capture-session row id.
    pub session_id: String,
    /// Model identifier.
    pub model: String,
    /// `runtime_hook` or `configured_client`.
    pub source: String,
    /// Observation valid-time in epoch milliseconds.
    pub observed_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a provenance-aware model observation.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_model_observation(
    store: &impl CanonicalStore,
    row: &ModelObservationRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "model_observation",
            &[
                "observation_id",
                "session_id",
                "model",
                "source",
                "observed_at",
                "schema_version",
            ],
            &[
                Value::text(row.observation_id.clone()),
                Value::text(row.session_id.clone()),
                Value::text(row.model.clone()),
                Value::text(row.source.clone()),
                Value::Int(row.observed_at),
                Value::Int(row.schema_version),
            ],
        )
        .await
}

/// A `file_revision` row: a `path@blob_oid` is one immutable, content-addressed
/// fact (db-design §4.4).
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

/// A `changed_hunk` row: a committed-diff hunk for `C`.
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
/// separate relation, so the two evolve independently.
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

/// A `price_table` row: a versioned (bitemporal) per-`model×class` price.
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
/// held at `as_of` (bitemporal): latest `valid_from ≤ valid_at`, then
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

/// A per-file `attribution` rollup row, keyed by `(checkpoint, file_revision)`.
/// `confidence_milli ∈ 0..=1000` (writer-clamped; the float `[0,1]`
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

// ----- hook lifecycle & retired-wrapper audit writers -----------------------
//
// The DDL + immutability guards for these tables already live in [`crate::schema`]
// (registered in `canonical_tables()`); these are the typed writers. Ids are
// content-addressed by the caller (orchestration / bin layer), so a replayed hook
// or a re-provision of the same verified build is an idempotent no-op.

/// A `substrate_block_event` row: a default-deny refusal of a wrapped invocation.
/// `verb` is the refused root token only — never key/transcript bytes,
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

/// A `session_transition` row: one append-only state-machine transition, the
/// durable source of truth for a session's current state. `transition_id` is
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
/// invocation. `session_id` may be `None` before `session.start`.
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
    /// Outcome (`recorded` | `deferred` | `deferred-no-session` |
    /// `skipped-unknown-verb`). `deferred-no-session` is the visible receipt
    /// for a hook carrying no usable session id: the payload is
    /// not ingested, but the loss is counted rather than silent.
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

/// A `captured_event` row: the per-canonical-event idempotency index.
/// One row per *logical* event; `idempotency_key` is content/identity-derived, so
/// re-delivering the same hook is a true no-op (no double-count). The actual event
/// content lives in the canonical git artifacts (redacted) + the decomposed
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

/// An `agent_enablement` row: one enable/disable *event* for an agent.
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

/// An `archive_decision` row: the audit of one checkpoint-strategy decision.
/// `archived` is a recorded outcome, never a mutated status.
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

/// A `verification_run` row: one `rift verify` / dual-run soak record.
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

/// A `verification_divergence` row: one parity divergence in a run.
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

/// A `verification_witness` row (v12): the condensation a verification run
/// actually witnessed.
///
/// **Why this row exists at all.** `verification_run` records that a verify
/// happened; it does not record what it was a verify *of*. `record_verification`
/// mints its `run_id` from `leaf_hash("{exit_code}:{checkpoints}:{now}")`, which
/// takes no archive input, so a run and the archive decision beside it share no
/// key in either direction. An owner decision of 2026-08-18 makes the soak's
/// population CONDENSATIONS rather than verify invocations, and this row is the
/// only place that distinction is written down: a run with a witness accrued
/// from real work, a run without one is a bare `rift verify`.
///
/// One run may carry SEVERAL witnesses — doctor's stale-session sweep runs one
/// verify after finalizing N sessions — which is why the primary key is
/// composite.
#[derive(Debug, Clone)]
pub struct VerificationWitnessRow {
    /// The run that witnessed (FK: `verification_run`).
    pub run_id: String,
    /// The checkpoint it witnessed (FK: `checkpoint`).
    pub checkpoint_id: String,
    /// The `refs/rift/chain/v1` head at witness time, when it could be read.
    ///
    /// `None` means UNREADABLE, never "empty" — the column is nullable for that
    /// reason and this field is `Option` for the same one. A soak counter is the
    /// last place to let absence read as a value.
    pub chain_head: Option<String>,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `verification_witness` (idempotent on `(run_id, checkpoint_id)`).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_verification_witness(
    store: &impl CanonicalStore,
    r: &VerificationWitnessRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "verification_witness",
            &["run_id", "checkpoint_id", "chain_head", "schema_version"],
            &[
                Value::text(r.run_id.clone()),
                Value::text(r.checkpoint_id.clone()),
                r.chain_head.clone().map_or(Value::Null, Value::text),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

// ----- normalized session / turn / call writers -----------------------------
//
// The decomposed model both event sources project into: agent hooks live, the
// artifact bridge authoritatively. Every id is content-addressed by
// `rift-shadow`'s shared id-derivation module, so hook-rows ∪ bridge-rows is a
// set-union no-op by construction.

/// A `session` row: one agent session.
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

/// Record a `tool_result_tokens` measurement: what one tool result put in
/// front of the model, on whichever channel it arrived.
///
/// The row that makes the savings denominator **repository content**, not
/// read-shaped calls: a search returns repository content too, resolves to no
/// file, and therefore has no `file_read_tokens` key at all. Keyed by the
/// result's own content-addressed id, so a redelivered job writes no second
/// row.
///
/// `basis` names how the figure was arrived at
/// ([`crate::schema::TOOL_RESULT_BASES`]); an `unmeasured` row carries zero
/// tokens and exists to be counted rather than summed.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
#[allow(clippy::too_many_arguments)] // one row, one argument per column it fills
pub async fn record_tool_result_tokens(
    store: &impl CanonicalStore,
    tool_result_id: &str,
    session_id: &str,
    tool_name: &str,
    channel: &str,
    output_tokens: u32,
    tokenizer: &str,
    basis: &str,
    recorded_at: i64,
    schema_version: i64,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "tool_result_tokens",
            &[
                "tool_result_id",
                "session_id",
                "tool_name",
                "channel",
                "output_tokens",
                "tokenizer",
                "basis",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text(tool_result_id.to_owned()),
                Value::text(session_id.to_owned()),
                Value::text(tool_name.to_owned()),
                Value::text(channel.to_owned()),
                Value::Int(i64::from(output_tokens)),
                Value::text(tokenizer.to_owned()),
                Value::text(basis.to_owned()),
                Value::Int(recorded_at),
                Value::Int(schema_version),
            ],
        )
        .await
}

/// Record a `file_read_tokens` measurement: the token count of one
/// read's bytes, measured at read time — the only moment a modified working
/// file's content is certainly in hand. Keyed by the read's own tool call, so
/// it lands atomically with the fact it describes and is idempotent on replay.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_file_read_tokens(
    store: &impl CanonicalStore,
    tool_call_id: &str,
    input_tokens: u32,
    tokenizer: &str,
    schema_version: i64,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "file_read_tokens",
            &[
                "tool_call_id",
                "input_tokens",
                "tokenizer",
                "schema_version",
            ],
            &[
                Value::text(tool_call_id.to_owned()),
                Value::Int(i64::from(input_tokens)),
                Value::text(tokenizer.to_owned()),
                Value::Int(schema_version),
            ],
        )
        .await
}

/// The capture projection's whole `content_sha256 → blob_oid` mapping (the
/// historical token resolution: hash → oid → object-store bytes). One query —
/// resolvers consult it per read, and `content_sha256` is unindexed, so
/// per-hash lookups would table-scan per read. First row wins per hash; the
/// mapping is content-addressed, so every row for a hash names the same oid.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn content_blob_oid_map(
    store: &impl CanonicalStore,
) -> Result<std::collections::BTreeMap<String, String>> {
    let rows = store
        .query(
            "SELECT content_sha256, blob_oid FROM file_revision \
             ORDER BY file_revision_id ASC",
            &[],
        )
        .await?;
    let mut out = std::collections::BTreeMap::new();
    for r in &rows {
        out.entry(r.get_text(0)?).or_insert(r.get_text(1)?);
    }
    Ok(out)
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

// ----- ingest watermark -----------------------------------------------------

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
/// bytes hashed to `content_sha256` at read time — exactly the triple
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
    /// Token estimate over the read-time bytes, when one was captured
    /// (facts recorded before the v6 migration have none).
    pub input_tokens: Option<u32>,
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
            "SELECT tc.tool_call_id, tc.tool_name, fr.path, fr.content_sha256, frt.input_tokens \
             FROM tool_reads_file trf \
             JOIN tool_call tc ON tc.tool_call_id = trf.tool_call_id \
             JOIN turn t ON t.turn_id = tc.turn_id \
             JOIN file_revision fr ON fr.file_revision_id = trf.file_revision_id \
             LEFT JOIN file_read_tokens frt ON frt.tool_call_id = trf.tool_call_id \
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
            input_tokens: r.get_opt_i64(4)?.and_then(|t| u32::try_from(t).ok()),
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

// ----- turn boundaries -------------------------------------------------------

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

/// **THE ordered selection of a session's last turn-boundary snapshot — one
/// rule, one home.** Highest turn ordinal wins; within a turn `end` supersedes
/// `start`; the snapshot id breaks a tie the unique key
/// `(session_id, turn_ordinal, boundary)` makes unreachable anyway.
///
/// This exists as a function because the rule used to be written out twice —
/// here and in `rift-shadow`'s `turn_boundary.rs` — in two `ORDER BY` clauses
/// differing only by a table alias, across a crate boundary. The property that
/// matters is that the WINDOW'S ACTOR and the ANCHOR BYTES come from the same
/// snapshot, and while the duplicate clauses agreed they made that property a
/// textual coincidence: nothing structural stopped one from being edited. It
/// now holds because there is only one selection to edit, and callers that need
/// both facts take them from a single returned row (see
/// [`turn_boundary_files_for_snapshot`]).
///
/// `None` when the session has no boundary at all.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn latest_turn_boundary_snapshot(
    store: &impl CanonicalStore,
    session_id: &str,
) -> Result<Option<TurnBoundarySnapshotRow>> {
    let rows = store
        .query(
            "SELECT snapshot_id, session_id, turn_ordinal, boundary, actor, worktree_hash, \
                    captured_at, schema_version \
             FROM turn_boundary_snapshot WHERE session_id = ?1 \
             ORDER BY turn_ordinal DESC, \
                      CASE boundary WHEN 'end' THEN 1 ELSE 0 END DESC, \
                      snapshot_id DESC LIMIT 1",
            &[Value::text(session_id.to_owned())],
        )
        .await?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(TurnBoundarySnapshotRow {
        snapshot_id: r.get_text(0)?,
        session_id: r.get_text(1)?,
        turn_ordinal: r.get_i64(2)?,
        boundary: r.get_text(3)?,
        actor: r.get_text(4)?,
        worktree_hash: r.get_text(5)?,
        captured_at: r.get_i64(6)?,
        schema_version: r.get_i64(7)?,
    }))
}

/// The per-file rows of ONE boundary snapshot, in path order. Paired with
/// [`latest_turn_boundary_snapshot`] this lets a caller take the actor and the
/// anchor bytes from a snapshot it has already identified, rather than running
/// the selection twice and hoping.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn turn_boundary_files_for_snapshot(
    store: &impl CanonicalStore,
    snapshot_id: &str,
) -> Result<Vec<TurnBoundaryFileRow>> {
    let rows = store
        .query(
            "SELECT snapshot_id, path, blob_oid, content_sha256, schema_version \
             FROM turn_boundary_file WHERE snapshot_id = ?1 ORDER BY path ASC",
            &[Value::text(snapshot_id.to_owned())],
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

/// The per-file rows of the session's **latest** boundary snapshot (the state
/// the next boundary diffs against) — the composition of the two functions
/// above, and no ordering rule of its own.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn latest_turn_boundary_files(
    store: &impl CanonicalStore,
    session_id: &str,
) -> Result<Vec<TurnBoundaryFileRow>> {
    let Some(snapshot) = latest_turn_boundary_snapshot(store, session_id).await? else {
        return Ok(Vec::new());
    };
    turn_boundary_files_for_snapshot(store, &snapshot.snapshot_id).await
}

/// Every boundary at which a session observed `path`, oldest first:
/// `(turn_ordinal, boundary, blob_oid)` from `turn_boundary_file` joined to its
/// `turn_boundary_snapshot`, ordered by turn ascending and `start` before
/// `end` within a turn — the same order the session's hunk stack replays in.
/// Read-only; the content-addressed `blob_oid` is what lets a commit's parent
/// blob be matched to the boundary that last observed it.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn turn_boundary_files_for_session_path(
    store: &impl CanonicalStore,
    session_id: &str,
    path: &str,
) -> Result<Vec<(i64, String, String)>> {
    let rows = store
        .query(
            "SELECT s.turn_ordinal, s.boundary, f.blob_oid \
             FROM turn_boundary_file f \
             JOIN turn_boundary_snapshot s ON s.snapshot_id = f.snapshot_id \
             WHERE s.session_id = ?1 AND f.path = ?2 \
             ORDER BY s.turn_ordinal ASC, \
                      CASE s.boundary WHEN 'end' THEN 1 ELSE 0 END ASC, \
                      s.snapshot_id ASC",
            &[
                Value::text(session_id.to_owned()),
                Value::text(path.to_owned()),
            ],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push((r.get_i64(0)?, r.get_text(1)?, r.get_text(2)?));
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

/// A `session_worktree` row: one OBSERVATION of the worktree a session was
/// seen in.
///
/// Append-only and content-addressed over the observed state, so a session's
/// history is the ordered set of its rows and nothing is ever updated. The
/// caller derives `observation_id`
/// (`rift_shadow::ids::session_worktree_observation_id`) — as every other
/// content-addressed row here does — and screens `basis` against
/// [`crate::schema::SESSION_WORKTREE_BASES`] before this DAO sees it (the CHECK
/// constraint refuses anything else regardless).
#[derive(Debug, Clone)]
pub struct SessionWorktreeRow {
    /// Content-addressed over `(session, token, branch, head, subagent)`.
    pub observation_id: String,
    /// The capture session row id (FK: session).
    pub session_id: String,
    /// `RepoPaths::worktree_token()` — 12 hex.
    pub worktree_token: String,
    /// The linked worktree's administrative name, or `main`.
    pub worktree_name: String,
    /// Whether this is a linked worktree (0/1).
    pub is_linked: i64,
    /// The checkout's absolute path — a LOCAL ADDRESS. It is why this relation
    /// is projection-only: paths never enter shards.
    pub work_tree: String,
    /// `HEAD`'s symbolic name at observation; `None` when detached/unreadable.
    pub branch: Option<String>,
    /// `HEAD`'s oid at observation; `None` when unborn/unresolvable.
    pub head_oid: Option<String>,
    /// The subagent session id when a subagent's hook made the observation —
    /// recorded under the PARENT's session id, which is what the payload
    /// carries inside a subagent.
    pub subagent_id: Option<String>,
    /// Where the observed directory came from — one of
    /// [`crate::schema::SESSION_WORKTREE_BASES`].
    pub basis: String,
    /// Epoch ms of the FIRST insert. A repeat observation keeps this value,
    /// because the id dedups before the timestamp is ever compared — which is
    /// exactly why the id must not include the time.
    pub first_observed_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `session_worktree` observation (insert-only, idempotent on the
/// state's own id): `true` when this state had not been observed before.
///
/// `basis` must be one of [`crate::schema::SESSION_WORKTREE_BASES`]. The CHECK
/// constraint enforces that, and — because this is `INSERT OR IGNORE`, which
/// SQLite applies to CHECK violations as well as to key conflicts — an unknown
/// token DROPS THE ROW and returns `false` rather than raising. Said plainly
/// rather than left to be discovered: every caller in this workspace passes a
/// `&'static str` from the closed set, which is what keeps the silent arm
/// unreachable.
///
/// # Errors
/// [`rift_core::RiftError::Storage`]/[`rift_core::RiftError::Immutability`] on
/// a non-conflict failure.
pub async fn record_session_worktree(
    store: &impl CanonicalStore,
    r: &SessionWorktreeRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "session_worktree",
            &[
                "observation_id",
                "session_id",
                "worktree_token",
                "worktree_name",
                "is_linked",
                "work_tree",
                "branch",
                "head_oid",
                "subagent_id",
                "basis",
                "first_observed_at",
                "schema_version",
            ],
            &[
                Value::text(r.observation_id.clone()),
                Value::text(r.session_id.clone()),
                Value::text(r.worktree_token.clone()),
                Value::text(r.worktree_name.clone()),
                Value::Int(r.is_linked),
                Value::text(r.work_tree.clone()),
                r.branch.clone().map_or(Value::Null, Value::text),
                r.head_oid.clone().map_or(Value::Null, Value::text),
                r.subagent_id.clone().map_or(Value::Null, Value::text),
                Value::text(r.basis.clone()),
                Value::Int(r.first_observed_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// Record a `checkpoint_worktree` projection row (insert-only, idempotent):
/// the worktree the shard's `metadata.json` names as its archiver.
///
/// A PROJECTION, not a new fact: the bridge re-derives it from the shard on
/// every rebuild, so re-recording an existing checkpoint is a no-op (`false`),
/// never an `UPDATE`. A shard WITHOUT the member calls this not at all — the
/// absent row is what every reader renders as `unknown`; a sealed shard is
/// never given an invented one.
///
/// # Errors
/// [`rift_core::RiftError::Storage`]/[`rift_core::RiftError::Immutability`] on
/// a non-conflict failure.
pub async fn record_checkpoint_worktree_if_new(
    store: &impl CanonicalStore,
    checkpoint_id: &str,
    worktree_token: &str,
    worktree_name: &str,
    is_linked: bool,
    branch: Option<&str>,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "checkpoint_worktree",
            &[
                "checkpoint_id",
                "worktree_token",
                "worktree_name",
                "is_linked",
                "branch",
                "schema_version",
            ],
            &[
                Value::text(checkpoint_id.to_owned()),
                Value::text(worktree_token.to_owned()),
                Value::text(worktree_name.to_owned()),
                Value::Int(i64::from(is_linked)),
                branch.map_or(Value::Null, |b| Value::text(b.to_owned())),
                Value::Int(1),
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

    // --- relations whose READER and WRITER must agree -----------------------
    //
    // Each pair below is a writer's column list and a reader's `SELECT` list,
    // written apart and kept in step by nothing but attention. The invariant is
    // not "this line executes" — it is that the value which goes in comes back
    // out of the field it went into, and that the reader's stated ordering rule
    // is the one it implements. A transposed pair of same-typed columns is
    // invisible to every other test in this crate.

    #[tokio::test]
    async fn edits_and_reads_are_independent_links_over_the_same_call() {
        // Two relations, deliberately not one (4NF: a tool may read a file it
        // never edits and edit one it never read). Recording one must not
        // satisfy the other.
        let s = store();
        assert!(record_tool_edits_file(&s, "call1", "rev1").await.unwrap());
        assert!(
            !record_tool_edits_file(&s, "call1", "rev1").await.unwrap(),
            "re-linking the same (call, revision) is a no-op"
        );
        assert!(record_tool_edits_file(&s, "call1", "rev2").await.unwrap());
        assert_eq!(s.count("tool_edits_file").await.unwrap(), 2);
        assert_eq!(
            s.count("tool_reads_file").await.unwrap(),
            0,
            "an edit link must not populate the read relation"
        );
    }

    fn attribution_hunk(ordinal: i64, category: &str) -> AttributionHunkRow {
        AttributionHunkRow {
            checkpoint_id: "ckpt1".into(),
            file_revision_id: "rev1".into(),
            hunk_ordinal: ordinal,
            new_start: 10 + ordinal,
            new_len: 3,
            category: category.into(),
            line_count: 3,
            confidence_milli: 900,
            method_version: "v1".into(),
            schema_version: 1,
        }
    }

    #[tokio::test]
    async fn attribution_hunks_accumulate_per_ordinal_and_re_record_as_no_ops() {
        let s = store();
        assert!(
            record_attribution_hunk(&s, &attribution_hunk(0, "agent_added"))
                .await
                .unwrap()
        );
        assert!(
            !record_attribution_hunk(&s, &attribution_hunk(0, "agent_added"))
                .await
                .unwrap(),
            "a replayed attribution pass must not double-count a hunk"
        );
        assert!(
            record_attribution_hunk(&s, &attribution_hunk(1, "human_added"))
                .await
                .unwrap()
        );
        assert_eq!(s.count("attribution_hunk").await.unwrap(), 2);
    }

    fn watermark(id: &str, branch: &str, tip: &str, at: i64) -> IngestWatermarkRow {
        IngestWatermarkRow {
            watermark_id: id.into(),
            branch: branch.into(),
            branch_tip: tip.into(),
            ingested_at: at,
            schema_version: 1,
        }
    }

    #[tokio::test]
    async fn the_ingest_watermark_is_the_latest_tip_of_its_own_branch() {
        let s = store();
        assert_eq!(
            latest_ingest_watermark(&s, "rift/checkpoints/v1")
                .await
                .unwrap(),
            None
        );

        // Append out of order: the reader must pick by `ingested_at`, not by
        // insertion order — the bridge re-ingests and re-appends freely.
        record_ingest_watermark(&s, &watermark("w2", "rift/checkpoints/v1", "tipB", 20))
            .await
            .unwrap();
        record_ingest_watermark(&s, &watermark("w1", "rift/checkpoints/v1", "tipA", 10))
            .await
            .unwrap();
        record_ingest_watermark(
            &s,
            &watermark("w3", concat!("ent", "ire", "/checkpoints/v1"), "tipZ", 30),
        )
        .await
        .unwrap();

        assert_eq!(
            latest_ingest_watermark(&s, "rift/checkpoints/v1")
                .await
                .unwrap()
                .as_deref(),
            Some("tipB"),
            "the newest tip of THIS branch"
        );
        assert_eq!(
            latest_ingest_watermark(&s, concat!("ent", "ire", "/checkpoints/v1"))
                .await
                .unwrap()
                .as_deref(),
            Some("tipZ"),
            "a second branch resumes from its own watermark, not the newest row"
        );
        assert_eq!(
            latest_ingest_watermark(&s, "no/such/branch").await.unwrap(),
            None
        );
        // Re-recording an identical (branch, tip) is a set-union no-op.
        assert!(
            !record_ingest_watermark(&s, &watermark("w2", "rift/checkpoints/v1", "tipB", 20))
                .await
                .unwrap()
        );
    }

    fn snapshot(id: &str, turn: i64, boundary: &str) -> TurnBoundarySnapshotRow {
        TurnBoundarySnapshotRow {
            snapshot_id: id.into(),
            session_id: "sess1".into(),
            turn_ordinal: turn,
            boundary: boundary.into(),
            actor: if boundary == "start" {
                "human"
            } else {
                "agent"
            }
            .into(),
            worktree_hash: format!("wt-{id}"),
            captured_at: turn,
            schema_version: 1,
        }
    }

    fn boundary_file(snapshot_id: &str, path: &str) -> TurnBoundaryFileRow {
        TurnBoundaryFileRow {
            snapshot_id: snapshot_id.into(),
            path: path.into(),
            blob_oid: format!("oid-{snapshot_id}-{path}"),
            content_sha256: format!("sha-{snapshot_id}-{path}"),
            schema_version: 1,
        }
    }

    #[tokio::test]
    async fn the_latest_boundary_is_the_newest_turn_and_end_beats_start_inside_it() {
        // The documented rule, which is the one attribution diffs against:
        // highest turn ordinal wins, and within a turn `end` supersedes `start`.
        // Both halves are asserted, and THE IDS ARE CHOSEN ADVERSARIALLY: the
        // reader's last tiebreak is `snapshot_id DESC`, so the `start` of the
        // newest turn is named `s9` and its `end` `s2`. A first draft of this
        // test used s1/s2 and stayed green when the `end`-beats-`start` clause
        // was deleted — the fallback sort happened to pick the right row, and
        // the test proved nothing about the clause it names.
        let s = store();
        assert!(latest_turn_boundary_files(&s, "sess1")
            .await
            .unwrap()
            .is_empty());

        for (id, turn, boundary) in [("s0", 0, "start"), ("s9", 1, "start"), ("s2", 1, "end")] {
            assert!(
                record_turn_boundary_snapshot(&s, &snapshot(id, turn, boundary))
                    .await
                    .unwrap()
            );
            assert!(record_turn_boundary_file(&s, &boundary_file(id, "b.rs"))
                .await
                .unwrap());
            assert!(record_turn_boundary_file(&s, &boundary_file(id, "a.rs"))
                .await
                .unwrap());
        }
        assert!(
            !record_turn_boundary_snapshot(&s, &snapshot("s2", 1, "end"))
                .await
                .unwrap(),
            "a retried boundary capture is a no-op"
        );

        let files = latest_turn_boundary_files(&s, "sess1").await.unwrap();
        assert_eq!(files.len(), 2);
        // Reader/writer agreement, field by field — a transposed pair of
        // same-typed text columns is invisible to a count.
        assert_eq!(
            files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
            vec!["a.rs".to_string(), "b.rs".to_string()],
            "files come back path-ordered"
        );
        for f in &files {
            assert_eq!(f.snapshot_id, "s2", "the `end` of the newest turn");
            assert_eq!(f.blob_oid, format!("oid-s2-{}", f.path));
            assert_eq!(f.content_sha256, format!("sha-s2-{}", f.path));
            assert_eq!(f.schema_version, 1);
        }
        assert!(latest_turn_boundary_files(&s, "other")
            .await
            .unwrap()
            .is_empty());

        // The coverage signal attribution confidence keys off.
        assert_eq!(turn_boundary_counts(&s, "sess1").await.unwrap(), (2, 1));
        assert_eq!(turn_boundary_counts(&s, "other").await.unwrap(), (0, 0));
    }

    /// **ONE ordering rule, ONE home.** The row that names the open window's
    /// actor and the rows that supply the anchor bytes must be the SAME
    /// snapshot — the property `rift-shadow`'s replay depends on, which used to
    /// rest on two `ORDER BY` clauses in two crates happening to match.
    ///
    /// Same adversarial ids as the test above (the newest turn's `start` sorts
    /// AFTER its `end` by id, so a missing `end`-beats-`start` clause picks the
    /// wrong row rather than falling back to the right one by luck).
    #[tokio::test]
    async fn the_ordered_selection_has_one_home_and_both_readers_name_the_same_snapshot() {
        let s = store();
        assert!(latest_turn_boundary_snapshot(&s, "sess1")
            .await
            .unwrap()
            .is_none());

        for (id, turn, boundary) in [("s0", 0, "start"), ("s9", 1, "start"), ("s2", 1, "end")] {
            record_turn_boundary_snapshot(&s, &snapshot(id, turn, boundary))
                .await
                .unwrap();
            record_turn_boundary_file(&s, &boundary_file(id, "a.rs"))
                .await
                .unwrap();
        }

        let latest = latest_turn_boundary_snapshot(&s, "sess1")
            .await
            .unwrap()
            .expect("a boundary exists");
        assert_eq!(latest.snapshot_id, "s2", "the `end` of the newest turn");
        assert_eq!(
            latest.boundary, "end",
            "the token the open-window derivation reads"
        );
        assert_eq!(latest.turn_ordinal, 1);
        assert_eq!(latest.session_id, "sess1");

        // The files reader draws from that SAME row — not from a rule restated.
        let files = latest_turn_boundary_files(&s, "sess1").await.unwrap();
        assert!(!files.is_empty());
        for f in &files {
            assert_eq!(
                f.snapshot_id, latest.snapshot_id,
                "anchor bytes and window actor must come from one snapshot"
            );
        }
        // And the by-id reader is the same selection with the choice made
        // explicit, so a caller can hold the row and its files together.
        let by_id: Vec<(String, String)> =
            turn_boundary_files_for_snapshot(&s, &latest.snapshot_id)
                .await
                .unwrap()
                .into_iter()
                .map(|f| (f.path, f.blob_oid))
                .collect();
        assert_eq!(
            by_id,
            files
                .iter()
                .map(|f| (f.path.clone(), f.blob_oid.clone()))
                .collect::<Vec<_>>()
        );
        assert!(turn_boundary_files_for_snapshot(&s, "s0")
            .await
            .unwrap()
            .iter()
            .all(|f| f.snapshot_id == "s0"));
    }

    fn diff_hunk(id: &str, turn: i64, kind: &str, path: &str, new_start: i64) -> TurnDiffHunkRow {
        TurnDiffHunkRow {
            hunk_id: id.into(),
            session_id: "sess1".into(),
            turn_ordinal: turn,
            edit_kind: kind.into(),
            path: path.into(),
            old_start: new_start,
            old_len: 1,
            new_start,
            new_len: 2,
            schema_version: 1,
        }
    }

    #[tokio::test]
    async fn diff_hunks_come_back_in_lifo_edit_stack_order() {
        // `[H_0, A_0, H_1, A_1, …]` — the order the attribution replay consumes.
        // Inserted deliberately scrambled so insertion order cannot pass for the
        // sort.
        let s = store();
        assert!(turn_diff_hunks_for_session(&s, "sess1")
            .await
            .unwrap()
            .is_empty());
        for h in [
            diff_hunk("h4", 1, "agent", "a.rs", 1),
            diff_hunk("h1", 0, "agent", "a.rs", 5),
            diff_hunk("h3", 1, "human", "a.rs", 1),
            diff_hunk("h0", 0, "human", "b.rs", 1),
            diff_hunk("h2", 0, "agent", "a.rs", 1),
        ] {
            assert!(record_turn_diff_hunk(&s, &h).await.unwrap());
        }
        assert!(
            !record_turn_diff_hunk(&s, &diff_hunk("h0", 0, "human", "b.rs", 1))
                .await
                .unwrap(),
            "a replayed turn must not duplicate its hunks"
        );
        // Another session's hunks must not leak into this replay.
        let mut other = diff_hunk("h9", 0, "human", "z.rs", 1);
        other.session_id = "sess2".into();
        assert!(record_turn_diff_hunk(&s, &other).await.unwrap());

        let hunks = turn_diff_hunks_for_session(&s, "sess1").await.unwrap();
        assert_eq!(
            hunks
                .iter()
                .map(|h| (h.turn_ordinal, h.edit_kind.clone(), h.new_start))
                .collect::<Vec<_>>(),
            vec![
                (0, "human".to_string(), 1),
                (0, "agent".to_string(), 1),
                (0, "agent".to_string(), 5),
                (1, "human".to_string(), 1),
                (1, "agent".to_string(), 1),
            ],
            "turn ascending, human before agent inside a turn, then position"
        );
        let first = hunks.first().unwrap();
        assert_eq!(first.hunk_id, "h0");
        assert_eq!(first.session_id, "sess1");
        assert_eq!(first.path, "b.rs");
        assert_eq!((first.old_start, first.old_len), (1, 1));
        assert_eq!((first.new_start, first.new_len), (1, 2));
        assert_eq!(first.schema_version, 1);
    }

    fn witness(run: &str, checkpoint: &str, head: Option<&str>) -> VerificationWitnessRow {
        VerificationWitnessRow {
            run_id: run.into(),
            checkpoint_id: checkpoint.into(),
            chain_head: head.map(std::borrow::ToOwned::to_owned),
            schema_version: 1,
        }
    }

    /// One run may witness several checkpoints — doctor's sweep runs one verify
    /// after finalizing N sessions — and re-recording either pair is a no-op,
    /// because a drain that is retried must not double-count a condensation.
    #[tokio::test]
    async fn a_run_witnesses_many_checkpoints_and_re_recording_one_is_a_no_op() {
        let s = store();
        assert!(
            record_verification_witness(&s, &witness("run-1", "ck-a", Some("head1")))
                .await
                .unwrap()
        );
        assert!(
            record_verification_witness(&s, &witness("run-1", "ck-b", Some("head1")))
                .await
                .unwrap()
        );
        assert!(
            !record_verification_witness(&s, &witness("run-1", "ck-a", Some("head1")))
                .await
                .unwrap(),
            "a retried drain must not double-count the condensation it already witnessed"
        );
        assert_eq!(s.count("verification_witness").await.unwrap(), 2);
    }

    /// An unreadable chain head is stored as NULL, never as the empty string —
    /// the zero-vs-absent distinction this program has already lost twice, and
    /// the soak is the last counter that should lose it a third time.
    #[tokio::test]
    async fn an_unreadable_chain_head_is_null_and_not_an_empty_string() {
        let s = store();
        record_verification_witness(&s, &witness("run-2", "ck-c", None))
            .await
            .unwrap();
        let rows = s
            .query(
                "SELECT chain_head IS NULL, COALESCE(chain_head, 'ABSENT') FROM \
                 verification_witness WHERE run_id = ?1",
                &[Value::text("run-2")],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_i64(0).unwrap(), 1, "chain_head must be NULL");
        assert_eq!(rows[0].get_text(1).unwrap(), "ABSENT");
    }
    fn observation(id: &str, branch: Option<&str>) -> SessionWorktreeRow {
        SessionWorktreeRow {
            observation_id: id.into(),
            session_id: "sess".into(),
            worktree_token: "0123456789ab".into(),
            worktree_name: "lane-a".into(),
            is_linked: 1,
            work_tree: "/Users/x/rift-worktrees/lane-a".into(),
            branch: branch.map(str::to_owned),
            head_oid: None,
            subagent_id: None,
            basis: "payload_cwd".into(),
            first_observed_at: 10,
            schema_version: 1,
        }
    }

    /// The two v14 provenance DAOs: insert-only, idempotent
    /// on their own key, NULL where the fact is absent, and closed on `basis`.
    #[tokio::test]
    async fn the_worktree_provenance_rows_are_insert_only_and_honest_about_absence() {
        let s = store();

        assert!(
            record_session_worktree(&s, &observation("o1", Some("wt/a")))
                .await
                .unwrap()
        );
        // The SAME id is a no-op, never an UPDATE — what makes a redelivered
        // hook, a sweep and a rebuild collapse to one row.
        assert!(
            !record_session_worktree(&s, &observation("o1", Some("other")))
                .await
                .unwrap()
        );
        // A different observed state is a different row: this is history.
        assert!(record_session_worktree(&s, &observation("o2", None))
            .await
            .unwrap());

        let rows = s
            .query(
                "SELECT branch, head_oid, subagent_id FROM session_worktree ORDER BY observation_id",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get_opt_text(0).unwrap().as_deref(), Some("wt/a"));
        assert_eq!(
            rows[1].get_opt_text(0).unwrap(),
            None,
            "a detached HEAD is NULL, never the empty string"
        );
        assert_eq!(rows[0].get_opt_text(1).unwrap(), None);
        assert_eq!(rows[0].get_opt_text(2).unwrap(), None);

        // `basis` is a closed vocabulary at the SQL layer, not only at the
        // caller's: a token outside `SESSION_WORKTREE_BASES` never lands.
        // `INSERT OR IGNORE` makes that a DROPPED ROW rather than an error —
        // stated as an assertion because a silent drop is worth knowing about,
        // and unreachable from the product path only because every caller's
        // basis is a `&'static str` from the closed set (pinned in
        // `bins/rift`'s `the_basis_tokens_the_hook_writes_are_the_ones_the_
        // schema_admits`).
        let mut bogus = observation("o3", None);
        bogus.basis = "guessed".into();
        assert!(
            !record_session_worktree(&s, &bogus).await.unwrap(),
            "an unknown basis writes nothing"
        );
        assert_eq!(
            s.query(
                "SELECT observation_id FROM session_worktree WHERE basis = 'guessed'",
                &[]
            )
            .await
            .unwrap()
            .len(),
            0
        );

        // The checkpoint twin: one row per checkpoint, re-derived by every
        // rebuild, and a shard that names no worktree simply never calls it.
        assert!(record_checkpoint_worktree_if_new(
            &s,
            "cp1",
            "0123456789ab",
            "lane-a",
            true,
            Some("wt/a")
        )
        .await
        .unwrap());
        assert!(
            !record_checkpoint_worktree_if_new(&s, "cp1", "ffffffffffff", "other", false, None)
                .await
                .unwrap(),
            "a re-ingest of the same checkpoint is a no-op, not a correction"
        );
        let kept = s
            .query(
                "SELECT worktree_name, is_linked, branch FROM checkpoint_worktree",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].get_text(0).unwrap(), "lane-a");
        assert_eq!(kept[0].get_i64(1).unwrap(), 1);
        assert_eq!(kept[0].get_opt_text(2).unwrap().as_deref(), Some("wt/a"));
    }
}

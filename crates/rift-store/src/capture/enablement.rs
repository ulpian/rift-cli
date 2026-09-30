//! The hook-lifecycle and audit relations: refused wrapped invocations, hook
//! receipts, the captured-event dedup index, agent enable/disable events and
//! archive-strategy decisions.
//!
//! The DDL + immutability guards for these tables already live in
//! [`crate::schema`] (registered in `canonical_tables()`); these are the typed
//! writers. Ids are content-addressed by the caller (orchestration / bin
//! layer), so a replayed hook or a re-provision of the same verified build is
//! an idempotent no-op.

use rift_core::Result;

use crate::{CanonicalStore, Value};

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
    /// `recorded-no-event` is a verb the adapter knows that carries nothing
    /// to capture, recorded so the hook still counts as having arrived.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::fixtures::store;

    #[tokio::test]
    async fn a_replayed_block_event_is_a_no_op() {
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
        assert_eq!(s.count("substrate_block_event").await.unwrap(), 1);
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

//! The session relations: a session's identity mapping, its lifecycle events
//! and state-machine transitions, its subagents and the checkpoints it is in.
//! One module because every row here keys on `session_id` alone.
//!
//! The `session` row is the decomposed model both event sources project into:
//! agent hooks live, the artifact bridge authoritatively. Every id is
//! content-addressed by `rift-shadow`'s shared id-derivation module, so
//! hook-rows ∪ bridge-rows is a set-union no-op by construction.

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::fixtures::store;

    #[tokio::test]
    async fn a_replayed_session_transition_is_a_no_op() {
        let s = store();
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
        assert_eq!(s.count("session_transition").await.unwrap(), 1);
    }
}

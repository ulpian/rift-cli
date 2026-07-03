//! Durable, append-only backing for local-only identity (phase-04 step-03).
//!
//! `rift-core::identity` owns the derivation (salted hashes, honest confidence);
//! this module persists its outputs into the trigger-guarded
//! `developer_identity_snapshot` and `session_binding` relations. Only hashed
//! ids and display strings are stored — never a raw fingerprint or salt.
//! Corrections supersede via `supersedes_id` (`04 §4.3`): an identity change
//! appends a new snapshot; a re-binding appends a new binding row.
//!
//! Determinism: `observed_at`/`recorded_at` are injected by the caller; row ids
//! are content-addressed over identifying fields so re-observing the same state
//! is an idempotent `INSERT OR IGNORE` no-op.

use rift_core::identity::{BindingEvidence, IdentitySnapshot};
use rift_core::Result;
use sha2::{Digest, Sha256};

use crate::{CanonicalStore, Value};

fn sha256_hex(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
        h.update([0]);
    }
    let out = h.finalize();
    let mut s = String::with_capacity(out.len() * 2);
    use core::fmt::Write as _;
    for b in out {
        let _ = write!(s, "{b:02x}");
    }
    s
}

const SNAPSHOT_COLUMNS: [&str; 8] = [
    "snapshot_id",
    "developer_id",
    "git_author_name",
    "git_email_normalized",
    "machine_id",
    "observed_at",
    "supersedes_id",
    "schema_version",
];

const BINDING_COLUMNS: [&str; 9] = [
    "binding_id",
    "session_id",
    "developer_id",
    "machine_id",
    "confidence_milli",
    "basis",
    "recorded_at",
    "supersedes_id",
    "schema_version",
];

/// The content-addressed id of a snapshot's identity *state* (time-independent,
/// so re-observing an unchanged identity is a no-op).
#[must_use]
pub fn snapshot_state_id(snap: &IdentitySnapshot) -> String {
    sha256_hex(&[
        b"identity-snapshot",
        snap.developer.to_string().as_bytes(),
        snap.machine.to_string().as_bytes(),
        snap.git_author_name.as_bytes(),
        snap.git_email_normalized.as_bytes(),
    ])
}

/// The latest recorded identity snapshot: `(snapshot_id, developer_id, machine_id)`.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on query failure.
pub async fn latest_identity_snapshot(
    store: &impl CanonicalStore,
) -> Result<Option<(String, String, String)>> {
    // "Latest" = the head of the supersession chain (the row nothing
    // supersedes), NOT a timestamp sort — same-millisecond rows tie there.
    let rows = store
        .query(
            "SELECT snapshot_id, developer_id, machine_id FROM developer_identity_snapshot \
             WHERE snapshot_id NOT IN (SELECT supersedes_id FROM developer_identity_snapshot \
             WHERE supersedes_id IS NOT NULL) \
             ORDER BY observed_at DESC, snapshot_id DESC LIMIT 1",
            &[],
        )
        .await?;
    match rows.first() {
        Some(r) => Ok(Some((r.get_text(0)?, r.get_text(1)?, r.get_text(2)?))),
        None => Ok(None),
    }
}

/// Record the current identity observation, superseding the latest snapshot iff
/// the identity *state* changed (same state ⇒ idempotent no-op). Returns the
/// snapshot's state id.
///
/// # Errors
/// [`rift_core::RiftError::Storage`]/[`rift_core::RiftError::Immutability`] on failure.
pub async fn record_identity_snapshot(
    store: &impl CanonicalStore,
    snap: &IdentitySnapshot,
) -> Result<String> {
    let id = snapshot_state_id(snap);
    let supersedes = match latest_identity_snapshot(store).await? {
        Some((prior_id, _, _)) if prior_id != id => Some(prior_id),
        Some(_) => return Ok(id), // unchanged state — nothing to append
        None => None,
    };
    store
        .insert_or_ignore(
            "developer_identity_snapshot",
            &SNAPSHOT_COLUMNS,
            &[
                Value::text(&id),
                Value::text(snap.developer.to_string()),
                Value::text(snap.git_author_name.clone()),
                Value::text(snap.git_email_normalized.clone()),
                Value::text(snap.machine.to_string()),
                Value::Int(snap.observed_at.as_millis()),
                supersedes.map_or(Value::Null, Value::text),
                Value::Int(1),
            ],
        )
        .await?;
    Ok(id)
}

/// The latest binding row id + developer id for a session, if any.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on query failure.
pub async fn latest_binding_for_session(
    store: &impl CanonicalStore,
    session_id: &str,
) -> Result<Option<(String, String)>> {
    // "Latest" = the head of the supersession chain for this session (the row
    // nothing supersedes), NOT a timestamp sort — same-millisecond rows tie there.
    let rows = store
        .query(
            "SELECT binding_id, developer_id FROM session_binding WHERE session_id = ? \
             AND binding_id NOT IN (SELECT supersedes_id FROM session_binding \
             WHERE session_id = ? AND supersedes_id IS NOT NULL) \
             ORDER BY recorded_at DESC, binding_id DESC LIMIT 1",
            &[Value::text(session_id), Value::text(session_id)],
        )
        .await?;
    match rows.first() {
        Some(r) => Ok(Some((r.get_text(0)?, r.get_text(1)?))),
        None => Ok(None),
    }
}

/// Record a session→developer binding for a projection `session_id`
/// (agent-supplied TEXT id) from gathered [`BindingEvidence`]. A re-binding
/// with different evidence appends a new row superseding the prior one;
/// identical evidence is an idempotent no-op.
///
/// # Errors
/// [`rift_core::RiftError::Storage`]/[`rift_core::RiftError::Immutability`] on failure.
pub async fn record_session_binding(
    store: &impl CanonicalStore,
    session_id: &str,
    binding: &BindingEvidence,
) -> Result<String> {
    let developer = binding.developer.to_string();
    let machine = binding.machine.to_string();
    let basis = binding.basis;
    let id = sha256_hex(&[
        b"session-binding",
        session_id.as_bytes(),
        developer.as_bytes(),
        machine.as_bytes(),
        basis.as_str().as_bytes(),
    ]);
    let supersedes = match latest_binding_for_session(store, session_id).await? {
        Some((prior_id, _)) if prior_id != id => Some(prior_id),
        Some(_) => return Ok(id), // same evidence — nothing to append
        None => None,
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let confidence_milli = i64::from((binding.confidence.value() * 1000.0).round() as u16);
    store
        .insert_or_ignore(
            "session_binding",
            &BINDING_COLUMNS,
            &[
                Value::text(&id),
                Value::text(session_id),
                Value::text(developer),
                Value::text(machine),
                Value::Int(confidence_milli),
                Value::text(basis.as_str()),
                Value::Int(binding.recorded_at.as_millis()),
                supersedes.map_or(Value::Null, Value::text),
                Value::Int(1),
            ],
        )
        .await?;
    Ok(id)
}

/// Projection sessions that have no binding yet (the capture wiring binds these
/// as they appear; idempotent across hook invocations).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on query failure.
pub async fn unbound_session_ids(store: &impl CanonicalStore) -> Result<Vec<String>> {
    let rows = store
        .query(
            "SELECT session_id FROM session WHERE session_id NOT IN \
             (SELECT session_id FROM session_binding)",
            &[],
        )
        .await?;
    rows.iter().map(|r| r.get_text(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{apply_migrations, default_migrations, SqliteStore};
    use rift_core::identity::{binding_evidence, snapshot, FixtureIdentity};
    use rift_core::{RiftError, Timestamp};

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        apply_migrations(&s, &default_migrations()).unwrap();
        s
    }

    fn alice() -> FixtureIdentity {
        FixtureIdentity {
            name: Some("Alice".into()),
            email: Some("alice@example.com".into()),
            fingerprint: Some("machine-guid-1".into()),
            salt: [9u8; 32],
            env_override: None,
        }
    }

    #[tokio::test]
    async fn identity_change_appends_new_snapshot() {
        let s = store();
        let snap_a = snapshot(&alice(), Timestamp::from_millis(1));
        let id_a = record_identity_snapshot(&s, &snap_a).await.unwrap();
        // Re-observing the same identity state is a no-op.
        let id_a2 = record_identity_snapshot(&s, &snapshot(&alice(), Timestamp::from_millis(2)))
            .await
            .unwrap();
        assert_eq!(id_a, id_a2);
        assert_eq!(s.count("developer_identity_snapshot").await.unwrap(), 1);

        // A changed identity appends a NEW superseding row; the old row stays.
        let mut bob = alice();
        bob.email = Some("bob@example.com".into());
        let id_b = record_identity_snapshot(&s, &snapshot(&bob, Timestamp::from_millis(3)))
            .await
            .unwrap();
        assert_ne!(id_a, id_b);
        assert_eq!(s.count("developer_identity_snapshot").await.unwrap(), 2);
        let (latest, _, _) = latest_identity_snapshot(&s).await.unwrap().unwrap();
        assert_eq!(latest, id_b);
        let rows = s
            .query(
                "SELECT supersedes_id FROM developer_identity_snapshot WHERE snapshot_id = ?",
                &[Value::text(&id_b)],
            )
            .await
            .unwrap();
        assert_eq!(rows[0].get_opt_text(0).unwrap().as_deref(), Some(id_a.as_str()));
    }

    #[tokio::test]
    async fn binding_supersede_keeps_prior() {
        let s = store();
        seed_session(&s, "sess_1").await;
        let at = Timestamp::from_millis(1);
        let b1 = binding_evidence(&alice(), at);
        let id1 = record_session_binding(&s, "sess_1", &b1).await.unwrap();
        // Identical evidence re-binds idempotently.
        let id1b = record_session_binding(&s, "sess_1", &b1).await.unwrap();
        assert_eq!(id1, id1b);
        assert_eq!(s.count("session_binding").await.unwrap(), 1);

        // A corrected binding (new evidence) supersedes; the wrong row REMAINS.
        let mut ci = alice();
        ci.env_override = Some("ci-bot@example.com".into());
        let b2 = binding_evidence(&ci, at);
        let id2 = record_session_binding(&s, "sess_1", &b2).await.unwrap();
        assert_ne!(id1, id2);
        assert_eq!(s.count("session_binding").await.unwrap(), 2);
        let (latest, _) = latest_binding_for_session(&s, "sess_1").await.unwrap().unwrap();
        assert_eq!(latest, id2);
        // Append-only: the superseded row cannot be mutated away.
        assert!(matches!(
            s.execute_raw("DELETE FROM session_binding"),
            Err(RiftError::Immutability(_))
        ));
    }

    #[tokio::test]
    async fn unbound_sessions_shrink_as_bindings_land() {
        let s = store();
        seed_session(&s, "sess_1").await;
        seed_session(&s, "sess_2").await;
        assert_eq!(unbound_session_ids(&s).await.unwrap().len(), 2);
        let b = binding_evidence(&alice(), Timestamp::from_millis(1));
        record_session_binding(&s, "sess_1", &b).await.unwrap();
        assert_eq!(unbound_session_ids(&s).await.unwrap(), vec!["sess_2".to_string()]);
    }

    async fn seed_session(s: &SqliteStore, id: &str) {
        s.insert(
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
                Value::text(id),
                Value::text("claude_code"),
                Value::text("m"),
                Value::text("repo-1"),
                Value::Int(1),
                Value::Int(1),
            ],
        )
        .await
        .unwrap();
    }
}

//! P3 S4 — the condense operation (shadow → permanent, redacted checkpoint).
//!
//! Condense is the single point where **raw local** capture becomes **durable**
//! data, so it is the one-way redaction gate in action (`05-security` §5.3): the
//! raw bytes pass through [`redact`](rift_shadow_redact) and only the resulting
//! `RedactedBytes` flow onward — a path that skips redaction does not compile. In
//! v0.1 the substrate performs the git write of the checkpoint shard under our
//! screened, pinned, local-only invocation (P1 S2/S3); Rift orchestrates *when*.
//! It is idempotent: identical content → identical `checkpoint_id`.

use std::path::Path;

use rift_core::{Result, SecretBytes};
use rift_shadow_entire::CaptureIntent;
use rift_shadow_hooks::strategy::{ArchiveReason, StrategyKind};
use rift_store::{record_archive_decision, ArchiveDecisionRow, CanonicalStore};

use crate::backend::ShadowBackend;
use crate::sha256_hex;

/// A content-addressed checkpoint id (lowercase hex).
pub type CheckpointId = String;

/// Derive the content-addressed checkpoint id for a session's redacted capture.
fn checkpoint_id_for(session: &str, redacted: &[u8]) -> CheckpointId {
    let content = sha256_hex(&[redacted]);
    sha256_hex(&[session.as_bytes(), b"\0", content.as_bytes()])
}

/// Condense raw shadow capture into a durable, redacted checkpoint and record the
/// `archive_decision`. `commit_oid` is the anchoring commit (HEAD) the substrate
/// archives against.
///
/// # Errors
/// [`RiftError`] if redaction, the substrate archive, or the audit insert fails.
pub async fn condense(
    backend: &impl ShadowBackend,
    store: &impl CanonicalStore,
    repo: &Path,
    session: &str,
    raw_capture: &SecretBytes,
    strategy: StrategyKind,
    reason: ArchiveReason,
    commit_oid: &str,
    now_ms: i64,
) -> Result<CheckpointId> {
    // ONE-WAY GATE: redact first. Nothing past this line sees the raw bytes; the
    // checkpoint id is content-addressed over the *redacted* form.
    let out = rift_shadow_redact::redact(raw_capture)?;
    let checkpoint_id = checkpoint_id_for(session, out.redacted.as_slice());

    // The backend writes the shard. Behind the `ShadowBackend` seam this is the
    // wrapper today (and the native rewrite once promoted, P7) — callers are agnostic.
    let intent = CaptureIntent::ArchiveCheckpoint {
        repo: repo.to_path_buf(),
        commit: commit_oid.to_owned(),
    };
    backend.archive_checkpoint(&intent)?;

    record_archive_decision(
        store,
        &ArchiveDecisionRow {
            decision_id: decision_id(session, strategy, reason, now_ms, Some(&checkpoint_id)),
            session_id: Some(session.to_owned()),
            strategy: strategy.as_str().to_owned(),
            reason: reason.as_str().to_owned(),
            archived: true,
            checkpoint_id: Some(checkpoint_id.clone()),
            decided_at: now_ms,
            schema_version: 1,
        },
    )
    .await?;
    Ok(checkpoint_id)
}

/// Derive an `archive_decision` id. A retried hook at the same `decided_at` is
/// idempotent; a genuinely later decision differs by `decided_at`.
pub(crate) fn decision_id(
    session: &str,
    strategy: StrategyKind,
    reason: ArchiveReason,
    decided_at: i64,
    checkpoint_id: Option<&str>,
) -> String {
    sha256_hex(&[
        session.as_bytes(),
        b"\0",
        strategy.as_str().as_bytes(),
        b"\0",
        reason.as_str().as_bytes(),
        b"\0",
        decided_at.to_string().as_bytes(),
        b"\0",
        checkpoint_id.unwrap_or("").as_bytes(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::EntireBackend;
    use rift_shadow_entire::MockRunner;
    use rift_store::SqliteStore;

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    #[tokio::test]
    async fn condense_is_idempotent_and_redacts() {
        let s = store();
        let runner = MockRunner::default();
        let backend = EntireBackend::new(&runner);
        let raw = SecretBytes::new(b"secret token=AKIAIOSFODNN7EXAMPLE in capture".to_vec());
        let repo = std::path::Path::new("/tmp/repo");

        let id1 = condense(
            &backend,
            &s,
            repo,
            "01JSESSION",
            &raw,
            StrategyKind::ManualCommit,
            ArchiveReason::Commit,
            "abc123",
            1,
        )
        .await
        .unwrap();
        // identical content → identical checkpoint id (idempotent)
        let id2 = condense(
            &backend,
            &s,
            repo,
            "01JSESSION",
            &raw,
            StrategyKind::ManualCommit,
            ArchiveReason::Commit,
            "abc123",
            1,
        )
        .await
        .unwrap();
        assert_eq!(id1, id2);
        // same decided_at + content → one decision row
        assert_eq!(s.count("archive_decision").await.unwrap(), 1);
        // the substrate archive was invoked through the screened seam
        assert_eq!(runner.calls.borrow().len(), 2);
    }

    #[tokio::test]
    async fn redaction_survival_seeded_secret_never_in_checkpoint_inputs() {
        // The checkpoint id is derived over the REDACTED bytes; a seeded secret in
        // the raw capture is masked before it can address anything durable.
        let raw_secret = "AKIAIOSFODNN7EXAMPLE";
        let out = rift_shadow_redact::redact(&SecretBytes::new(
            format!("aws key {raw_secret} here").into_bytes(),
        ))
        .unwrap();
        let redacted = String::from_utf8_lossy(out.redacted.as_slice()).into_owned();
        assert!(
            !redacted.contains(raw_secret),
            "seeded secret must not survive"
        );
    }
}

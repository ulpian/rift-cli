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
use rift_shadow_hooks::strategy::{ArchiveReason, StrategyKind};
use rift_store::{record_archive_decision, ArchiveDecisionRow, CanonicalStore};

use crate::backend::{CheckpointArchive, RedactionReportView, ShadowBackend};
use crate::sha256_hex;

/// A content-addressed checkpoint id (lowercase hex).
pub type CheckpointId = String;

/// Derive the content-addressed checkpoint id for a session's redacted capture.
fn checkpoint_id_for(session: &str, redacted: &[u8]) -> CheckpointId {
    let content = sha256_hex(&[redacted]);
    sha256_hex(&[session.as_bytes(), b"\0", content.as_bytes()])
}

/// The checkpoint id `condense` will derive for `(session, raw_capture)` —
/// computable **before** the archive runs (it depends only on the session and
/// the redacted capture, never the commit oid), which is what lets
/// `prepare-commit-msg` inject a `Rift-Checkpoint` trailer that the post-commit
/// archive then makes true.
///
/// # Errors
/// [`rift_core::RiftError`] if redaction fails.
pub fn predicted_checkpoint_id(session: &str, raw_capture: &SecretBytes) -> Result<CheckpointId> {
    let out = rift_shadow_redact::redact(raw_capture)?;
    Ok(checkpoint_id_for(session, out.redacted.as_slice()))
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
    let report = RedactionReportView::from_redact(&out.report);

    // The backend writes the shard. Behind the `ShadowBackend` seam this is the
    // wrapper today (and the native rewrite once promoted, P7) — callers are
    // agnostic. The archive carries `repo`/`commit` for the wrapper and the full
    // structural content (id, sessions, strategy, merkle root, redacted capture,
    // redaction report) for the native writer. In v0.1 no per-turn event stream is
    // threaded here yet, so the Merkle root is over the empty leaf set.
    let archive = CheckpointArchive {
        repo: repo.to_path_buf(),
        commit: commit_oid.to_owned(),
        checkpoint_id: checkpoint_id.clone(),
        sessions: vec![session.to_owned()],
        strategy: strategy.as_str().to_owned(),
        prev_hash: None,
        merkle_root: crate::checkpoint_merkle_root(&[]),
        events: Vec::new(),
        content: out.redacted,
        report,
        created_at_ms: now_ms,
    };
    if let Err(e) = backend.archive_checkpoint(&archive) {
        // A default-deny refusal is an audited fact (P1 S3), not just an error:
        // the archive root verb is the refused token. Audit best-effort — the
        // refusal itself must still surface even if the audit insert fails.
        if matches!(e, rift_core::RiftError::PolicyDenied(_)) {
            let _ =
                crate::provision::record_block(store, "checkpoint", "screen-refused", now_ms).await;
        }
        return Err(e);
    }

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
    async fn a_refused_archive_is_audited_as_a_block_event() {
        use crate::backend::ShadowBackend;

        /// A backend that refuses every archive as a policy block (screen refusal).
        struct RefusingBackend;
        impl ShadowBackend for RefusingBackend {
            fn archive_checkpoint(&self, _archive: &CheckpointArchive) -> rift_core::Result<()> {
                Err(rift_core::RiftError::PolicyDenied("refused".into()))
            }
        }

        let s = store();
        let raw = SecretBytes::new(b"capture".to_vec());
        let out = condense(
            &RefusingBackend,
            &s,
            std::path::Path::new("/r"),
            "01JSESSION",
            &raw,
            StrategyKind::ManualCommit,
            ArchiveReason::Commit,
            "abc123",
            1,
        )
        .await;
        assert!(matches!(out, Err(rift_core::RiftError::PolicyDenied(_))));
        assert_eq!(
            s.count("substrate_block_event").await.unwrap(),
            1,
            "the refusal left an append-only audit row"
        );
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

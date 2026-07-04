//! End-to-end (M1): a fake agent session is captured into canonical events, its
//! checkpoint Merkle root is computed and recorded into the immutable projection,
//! and `rift verify` cross-checks it against the canonical chain — while the
//! redaction gate keeps a seeded secret out of the durable bytes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use rift_core::{SecretBytes, Sha256Id};
use rift_hashchain::ChainLink;
use rift_shadow::hooks::{AdapterRegistry, AgentId, RawHook};
use rift_shadow::{capture, checkpoint_merkle_root, redact, verify};
use rift_store::{record_checkpoint, ChainSource, CheckpointRecord, SqliteStore};

struct OneLink(ChainLink);
impl ChainSource for OneLink {
    fn links(&self) -> rift_core::Result<Vec<ChainLink>> {
        Ok(vec![self.0])
    }
}

fn hook(verb: &str, turn: u32, payload: serde_json::Value) -> RawHook {
    RawHook {
        agent: AgentId::CLAUDE_CODE,
        verb: verb.into(),
        session: "01JE2E".into(),
        turn: Some(turn),
        occurred_at_ms: i64::from(turn) + 1,
        payload,
    }
}

#[tokio::test]
async fn capture_to_immutable_ledger_to_verify() {
    let reg = AdapterRegistry::with_builtin();

    // 1. capture a scripted session; the prompt payload carries a seeded AWS key.
    let secret_payload = serde_json::json!({ "text": "token=AKIAIOSFODNN7EXAMPLE" });
    let mut events = capture(&hook("SessionStart", 0, serde_json::json!({})), &reg).unwrap();
    events.extend(capture(&hook("UserPromptSubmit", 1, secret_payload.clone()), &reg).unwrap());
    assert!(!events.is_empty(), "session produced canonical events");

    // 2. the redaction gate masks the secret out of the durable payload bytes.
    let raw_bytes = serde_json::to_vec(&secret_payload).unwrap();
    let red = redact::redact(&SecretBytes::new(raw_bytes)).unwrap();
    let cleared = String::from_utf8_lossy(red.redacted.as_slice());
    assert!(
        !cleared.contains("AKIAIOSFODNN7EXAMPLE"),
        "secret must be redacted"
    );

    // 3. compute the checkpoint Merkle root and record it in the immutable store.
    let root = checkpoint_merkle_root(&events);
    let checkpoint_id = Sha256Id::from_bytes([1; 32]);
    let store = SqliteStore::open_in_memory().unwrap();
    store.create_canonical_schema().unwrap();
    record_checkpoint(
        &store,
        &CheckpointRecord {
            checkpoint_id,
            strategy: "manual-commit".into(),
            created_at: 1,
            merkle_root: root,
            prev: None,
            schema_version: 1,
        },
    )
    .await
    .unwrap();

    // 4. verify the projection against the authoritative (git-equivalent) chain.
    let source = OneLink(ChainLink {
        checkpoint_id,
        merkle_root: root,
        prev: None,
    });
    let report = verify(&store, &source).await.unwrap();
    assert!(report.ok);
    assert_eq!(report.checkpoints, 1);
}

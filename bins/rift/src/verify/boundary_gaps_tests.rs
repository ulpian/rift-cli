use super::{passes::shard_passes, verdict::Verdict};
use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate, SubstrateReader};
use rift_shadow::backend::{shard_merkle_root, ShardLeafBytes, ROOT_BOUND_ID_MIN_CANON_VERSION};

fn checked_gap_bytes(native: &str) -> Vec<u8> {
    use rift_core::boundary_gap::*;
    let ticket = GapTicket::new(GapTicketFields {
        source_key: SourceKey::parse_hex(&"11".repeat(32)).unwrap(),
        context_digest: ContextDigest::parse_hex(&"22".repeat(32)).unwrap(),
        ticket_digest: TicketDigest::parse_hex(&"33".repeat(32)).unwrap(),
        session_key: SessionKey::parse_hex(&"44".repeat(32)).unwrap(),
        turn_ordinal: 1,
        boundary: BoundaryEdge::End,
        head_oid: None,
        captured_at: 10,
        queued_at: 20,
        attempts: 0,
    })
    .unwrap();
    let batch = GapBatch::new(
        rift_core::RepoId::new("repo").unwrap(),
        MemberKey::parse_hex(&"55".repeat(32)).unwrap(),
        EvidenceDigest::parse_hex(&"66".repeat(32)).unwrap(),
        30,
        vec![ticket],
    )
    .unwrap();
    let carrier =
        SessionKey::parse_hex(&rift_core::capture_session_row_id("claude-code", native)).unwrap();
    let bytes = encode_canonical(&[GapEnvelope::new(carrier, batch)]).unwrap();
    assert_eq!(decode_checked(&bytes).unwrap().len(), 1);
    bytes
}

fn preserve_fixture_identity(sub: &MemorySubstrate, id: &str) {
    let artifacts = [
        "content.redacted",
        "full.jsonl",
        "sessions.jsonl",
        "redaction_report.json",
        "metadata.json",
        "turn_boundary_gaps.jsonl",
    ]
    .into_iter()
    .map(|name| (name, sub.read_artifact(id, name).unwrap()))
    .collect::<Vec<_>>();
    eprintln!(
        "GAP_AUTH_FIXTURE {}",
        serde_json::json!({"checkpoint_id": id, "artifacts": artifacts})
    );
}

pub(super) fn native_control() -> (MemorySubstrate, String) {
    let content = b"safe native content";
    let full = br#"{"session_id":"native-gap-carrier","turn":0,"event":0,"kind":"session_start","verb":"SessionStart","occurred_at_ms":10,"payload":{}}"#;
    let report = super::tests::fixture_report();
    let sessions = br#"{"agent":"claude-code","schema_version":1,"session":"native-gap-carrier"}"#;
    let leaves = ShardLeafBytes {
        boundary_gaps: None,
        content_redacted: content,
        full_jsonl: Some(full),
        redaction_report: report.as_bytes(),
        serve_audit: None,
        sessions: Some(sessions),
        usage: None,
        window_journal: None,
        session_worktrees: None,
        tool_results: None,
        tool_calls: None,
        transcript_compact: None,
        transcript_full: None,
        tasks: &[],
        spills: &[],
        originals: &[],
        session_metas: &[],
        digests: rift_shadow::backend::DigestLeafBytes::default(),
    };
    let root = shard_merkle_root(&leaves).to_hex();
    let id = rift_shadow::condense::checkpoint_ids_for_shard(
        &["native-gap-carrier".to_owned()],
        ROOT_BOUND_ID_MIN_CANON_VERSION,
        &leaves,
    )
    .remove(0);
    let metadata = format!(
        r#"{{"canon_version":3,"strategy":"manual-commit","merkle_root":"{root}","sessions":["native-gap-carrier"]}}"#
    );
    let mut sub = MemorySubstrate::local()
        .with_shard(CheckpointShard {
            checkpoint_id: id.clone(),
            dialect: Dialect::Rift,
            prev: None,
            ordinal: 1,
        })
        .with_artifact(&id, "content.redacted", content)
        .with_artifact(&id, "full.jsonl", full)
        .with_artifact(&id, "sessions.jsonl", sessions)
        .with_artifact(&id, "redaction_report.json", report.as_bytes())
        .with_artifact(&id, "metadata.json", metadata.as_bytes());
    for name in [
        "content.redacted",
        "full.jsonl",
        "sessions.jsonl",
        "redaction_report.json",
        "metadata.json",
    ] {
        let bytes = sub.read_artifact(&id, name).unwrap().unwrap();
        sub = sub.with_native_artifact(&id, name, &bytes);
    }
    (sub, id)
}

#[test]
fn an_unbound_gap_artifact_cannot_keep_a_native_shards_old_root_and_id() {
    let (control, id) = native_control();
    let valid = shard_passes(&control).unwrap();
    assert_eq!((valid.checked, valid.worst), (1, Verdict::Ok));
    preserve_fixture_identity(&control, &id);
    let metadata = control.read_artifact(&id, "metadata.json").unwrap();
    let gap = checked_gap_bytes("native-gap-carrier");
    let forged = control.with_artifact(&id, "turn_boundary_gaps.jsonl", &gap);
    let forged = forged.with_native_artifact(
        &id,
        "turn_boundary_gaps.jsonl",
        &checked_gap_bytes("native-gap-carrier"),
    );
    assert_eq!(
        forged.read_artifact(&id, "metadata.json").unwrap(),
        metadata
    );
    assert_eq!(forged.checkpoints().unwrap()[0].checkpoint_id, id);
    preserve_fixture_identity(&forged, &id);
    let found = shard_passes(&forged).unwrap();
    assert_eq!(found.checked, 1);
    assert!(
        matches!(found.worst, Verdict::Tamper(_)),
        "a new audit leaf needs a new authenticated root and checkpoint ID: {:?}",
        found.worst
    );
}

fn bound_gap_control() -> (MemorySubstrate, String) {
    let (control, old_id) = native_control();
    let header = rift_shadow::backend::native_read::shard_header(&control, &old_id).unwrap();
    let mut native =
        rift_shadow::backend::native_read::NativeArtifacts::read(&control, &old_id, &header)
            .unwrap();
    native.boundary_gaps = Some(checked_gap_bytes("native-gap-carrier"));
    let report = control
        .read_artifact(&old_id, "redaction_report.json")
        .unwrap()
        .unwrap();
    let tasks = native.task_leaves();
    let sessions = native.session_meta_leaves();
    let spills = rift_shadow::backend::native_read::manifests::spill_leaves(&native.spills);
    let originals =
        rift_shadow::backend::native_read::manifests::original_leaves(&native.originals);
    let leaves = native.leaves(
        native.content.as_deref().unwrap(),
        &report,
        &tasks,
        &sessions,
        &spills,
        &originals,
    );
    let root = rift_shadow::backend::shard_merkle_root(&leaves).to_hex();
    let id =
        rift_shadow::condense::checkpoint_ids_for_shard(&header.sessions, 3, &leaves).remove(0);
    let mut metadata: serde_json::Value =
        serde_json::from_slice(header.metadata_bytes.as_deref().unwrap()).unwrap();
    metadata["merkle_root"] = serde_json::Value::String(root);
    let mut out = MemorySubstrate::local().with_shard(CheckpointShard {
        checkpoint_id: id.clone(),
        dialect: Dialect::Rift,
        prev: None,
        ordinal: 1,
    });
    for name in [
        "content.redacted",
        "full.jsonl",
        "sessions.jsonl",
        "redaction_report.json",
    ] {
        out = out.with_artifact(
            &id,
            name,
            &control.read_artifact(&old_id, name).unwrap().unwrap(),
        );
    }
    out = out
        .with_artifact(
            &id,
            "metadata.json",
            &serde_json::to_vec(&metadata).unwrap(),
        )
        .with_artifact(
            &id,
            "turn_boundary_gaps.jsonl",
            native.boundary_gaps.as_deref().unwrap(),
        );
    for name in [
        "metadata.json",
        "redaction_report.json",
        "content.redacted",
        "full.jsonl",
        "sessions.jsonl",
        "turn_boundary_gaps.jsonl",
    ] {
        let bytes = out.read_artifact(&id, name).unwrap().unwrap();
        out = out.with_native_artifact(&id, name, &bytes);
    }
    (out, id)
}

#[test]
fn a_bound_gap_with_an_explicit_native_carrier_verifies() {
    let (sub, _) = bound_gap_control();
    let found = shard_passes(&sub).unwrap();
    assert_eq!((found.checked, found.worst), (1, Verdict::Ok));
}

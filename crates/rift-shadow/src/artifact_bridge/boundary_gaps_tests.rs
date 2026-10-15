use super::ingest_from_substrate;
use crate::backend::{shard_merkle_root, ShardLeafBytes, ROOT_BOUND_ID_MIN_CANON_VERSION};
use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate, SubstrateReader};
use rift_store::{CanonicalStore, SqliteStore};

struct ChangingBridgeSource {
    inner: MemorySubstrate,
    reads: std::cell::RefCell<std::collections::BTreeMap<(String, String), usize>>,
}

impl SubstrateReader for ChangingBridgeSource {
    fn read_native_artifact(&self, id: &str, path: &str) -> rift_core::Result<Option<Vec<u8>>> {
        let mut reads = self.reads.borrow_mut();
        let count = reads
            .entry((id.to_owned(), format!("native:{path}")))
            .or_default();
        *count += 1;
        if *count > 1 {
            return Ok(Some(b"changed after native authentication".to_vec()));
        }
        self.inner.read_native_artifact(id, path)
    }

    fn is_local_only(&self) -> bool {
        true
    }
    fn checkpoints(&self) -> rift_core::Result<Vec<CheckpointShard>> {
        self.inner.checkpoints()
    }
    fn blob(&self, id: &str) -> rift_core::Result<Option<Vec<u8>>> {
        self.inner.blob(id)
    }
    fn read_artifact(&self, id: &str, path: &str) -> rift_core::Result<Option<Vec<u8>>> {
        let mut reads = self.reads.borrow_mut();
        let count = reads.entry((id.to_owned(), path.to_owned())).or_default();
        *count += 1;
        if *count > 1 {
            return Ok(Some(b"changed after authentication".to_vec()));
        }
        self.inner.read_artifact(id, path)
    }
}

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

fn native_control() -> (MemorySubstrate, String) {
    let content = b"safe native content";
    let full = br#"{"session_id":"native-gap-carrier","turn":0,"event":0,"kind":"session_start","verb":"SessionStart","occurred_at_ms":10,"payload":{}}"#;
    let report = super::tests::valid_report();
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
        digests: crate::backend::DigestLeafBytes::default(),
    };
    let root = shard_merkle_root(&leaves).to_hex();
    let id = crate::condense::checkpoint_ids_for_shard(
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

fn fresh_store() -> SqliteStore {
    let store = SqliteStore::open_in_memory().unwrap();
    store.create_canonical_schema().unwrap();
    store
}

#[tokio::test]
async fn an_unbound_gap_artifact_is_refused_before_projection_or_checkpoint_sealing() {
    let (control, id) = native_control();
    let control_store = fresh_store();
    let valid = ingest_from_substrate(&control_store, &control, "repo", "claude-code", 100)
        .await
        .unwrap();
    assert_eq!(
        (
            valid.shards_projected,
            valid.refused.len(),
            valid.parse_issues
        ),
        (1, 0, 0)
    );
    assert_eq!(control_store.count("checkpoint").await.unwrap(), 1);
    preserve_fixture_identity(&control, &id);
    let metadata = control.read_artifact(&id, "metadata.json").unwrap();
    let forged = control.with_artifact(
        &id,
        "turn_boundary_gaps.jsonl",
        &checked_gap_bytes("native-gap-carrier"),
    );
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
    let forged_store = fresh_store();
    let report = ingest_from_substrate(&forged_store, &forged, "repo", "claude-code", 100)
        .await
        .unwrap();
    let checkpoint_count = forged_store.count("checkpoint").await.unwrap();
    let gap_count = forged_store.count("turn_boundary_gap_batch").await.unwrap();
    let ticket_count = forged_store
        .count("turn_boundary_gap_ticket")
        .await
        .unwrap();
    assert_eq!(
        (
            report.shards_projected,
            report.refused.len(),
            checkpoint_count,
            gap_count,
            ticket_count
        ),
        (0, 1, 0, 0, 0),
        "unbound gap bytes cannot be ignored while sealing their carrier"
    );
}

async fn native_facts(store: &SqliteStore) -> Vec<(&'static str, Vec<rift_store::Row>)> {
    let mut facts = Vec::new();
    for table in [
        "checkpoint",
        "session",
        "turn",
        "captured_event",
        "session_in_checkpoint",
        "capture_session_identity",
    ] {
        facts.push((
            table,
            store
                .query(&format!("SELECT * FROM {table} ORDER BY 1"), &[])
                .await
                .unwrap(),
        ));
    }
    facts
}

#[tokio::test]
async fn an_existing_checkpoint_cannot_hide_an_unbound_gap_artifact_on_resume() {
    let (control, id) = native_control();
    let store = fresh_store();
    let valid = ingest_from_substrate(&store, &control, "repo", "claude-code", 100)
        .await
        .unwrap();
    assert_eq!(
        (
            valid.shards_projected,
            valid.refused.len(),
            valid.parse_issues
        ),
        (1, 0, 0)
    );
    assert_eq!(store.count("checkpoint").await.unwrap(), 1);
    let before = native_facts(&store).await;
    let metadata = control.read_artifact(&id, "metadata.json").unwrap();
    let forged = control.with_artifact(
        &id,
        "turn_boundary_gaps.jsonl",
        &checked_gap_bytes("native-gap-carrier"),
    );
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
    let report = ingest_from_substrate(&store, &forged, "repo", "claude-code", 100)
        .await
        .unwrap();
    let after = native_facts(&store).await;
    let batches = store.count("turn_boundary_gap_batch").await.unwrap();
    let tickets = store.count("turn_boundary_gap_ticket").await.unwrap();
    assert_eq!(
        before, after,
        "refusal preserves every existing row of the named native relations"
    );
    assert_eq!((report.shards_projected, report.shards_skipped, report.refused.len(), batches, tickets), (0, 0, 1, 0, 0), "resume must authenticate a present gap before treating its checkpoint as already projected");
}

fn bound_gap_control() -> (MemorySubstrate, String) {
    let (control, old_id) = native_control();
    let header = crate::backend::native_read::shard_header(&control, &old_id).unwrap();
    let mut native =
        crate::backend::native_read::NativeArtifacts::read(&control, &old_id, &header).unwrap();
    native.boundary_gaps = Some(checked_gap_bytes("native-gap-carrier"));
    let report = control
        .read_artifact(&old_id, "redaction_report.json")
        .unwrap()
        .unwrap();
    let tasks = native.task_leaves();
    let sessions = native.session_meta_leaves();
    let spills = crate::backend::native_read::manifests::spill_leaves(&native.spills);
    let originals = crate::backend::native_read::manifests::original_leaves(&native.originals);
    let leaves = native.leaves(
        native.content.as_deref().unwrap(),
        &report,
        &tasks,
        &sessions,
        &spills,
        &originals,
    );
    let root = crate::backend::shard_merkle_root(&leaves).to_hex();
    let id = crate::condense::checkpoint_ids_for_shard(&header.sessions, 3, &leaves).remove(0);
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

#[tokio::test]
async fn a_bound_audit_projects_subject_digests_without_subject_native_facts_and_replays_exactly() {
    let (sub, id) = bound_gap_control();
    let store = fresh_store();
    let report = ingest_from_substrate(&store, &sub, "destination-repo", "claude-code", 100)
        .await
        .unwrap();
    assert_eq!(
        (
            report.shards_projected,
            report.refused.len(),
            report.parse_issues
        ),
        (1, 0, 0)
    );
    assert_eq!(store.count("checkpoint").await.unwrap(), 1);
    assert_eq!(store.count("turn_boundary_gap_batch").await.unwrap(), 1);
    assert_eq!(store.count("turn_boundary_gap_ticket").await.unwrap(), 1);
    assert_eq!(store.count("session").await.unwrap(), 1);
    assert_eq!(store.count("capture_session_identity").await.unwrap(), 1);
    let gap = rift_core::boundary_gap::decode_checked(
        &sub.read_artifact(&id, "turn_boundary_gaps.jsonl")
            .unwrap()
            .unwrap(),
    )
    .unwrap()
    .remove(0);
    assert_eq!(
        rift_store::boundary_gap_batch(&store, gap.batch().gap_id())
            .await
            .unwrap()
            .as_ref(),
        Some(gap.batch())
    );
    let native_before = native_facts(&store).await;
    let gaps_before = store
        .query(
            "SELECT * FROM turn_boundary_gap_ticket ORDER BY source_key",
            &[],
        )
        .await
        .unwrap();
    let replay = ingest_from_substrate(&store, &sub, "destination-repo", "claude-code", 101)
        .await
        .unwrap();
    assert!(replay.refused.is_empty());
    assert_eq!(native_facts(&store).await, native_before);
    assert_eq!(
        store
            .query(
                "SELECT * FROM turn_boundary_gap_ticket ORDER BY source_key",
                &[]
            )
            .await
            .unwrap(),
        gaps_before
    );
}

#[tokio::test]
async fn gap_bridge_projects_only_the_first_authenticated_artifact_responses() {
    let (sub, _) = bound_gap_control();
    let control = fresh_store();
    let expected = ingest_from_substrate(&control, &sub, "repo", "claude-code", 100)
        .await
        .unwrap();
    assert_eq!((expected.shards_projected, expected.refused.len()), (1, 0));
    let source = ChangingBridgeSource {
        inner: sub,
        reads: std::cell::RefCell::default(),
    };
    let store = fresh_store();
    let actual = ingest_from_substrate(&store, &source, "repo", "claude-code", 100)
        .await
        .unwrap();
    assert_eq!((actual.shards_projected, actual.refused.len()), (1, 0));
    assert_eq!(native_facts(&store).await, native_facts(&control).await);
    for table in ["turn_boundary_gap_batch", "turn_boundary_gap_ticket"] {
        let sql = format!("SELECT * FROM {table} ORDER BY 1");
        assert_eq!(
            store.query(&sql, &[]).await.unwrap(),
            control.query(&sql, &[]).await.unwrap()
        );
    }
    assert!(source.reads.borrow().values().all(|count| *count == 1));
}

fn bound_artifacts() -> std::collections::BTreeMap<String, Vec<u8>> {
    let (source, id) = bound_gap_control();
    [
        "metadata.json",
        "content.redacted",
        "full.jsonl",
        "sessions.jsonl",
        "redaction_report.json",
        "turn_boundary_gaps.jsonl",
    ]
    .into_iter()
    .map(|name| {
        (
            name.to_owned(),
            source.read_artifact(&id, name).unwrap().unwrap(),
        )
    })
    .collect()
}

fn mapped_source(
    id: &str,
    artifacts: &std::collections::BTreeMap<String, Vec<u8>>,
) -> MemorySubstrate {
    let mut source = MemorySubstrate::local().with_shard(CheckpointShard {
        checkpoint_id: id.to_owned(),
        dialect: Dialect::Rift,
        prev: None,
        ordinal: 1,
    });
    for (name, bytes) in artifacts {
        source = source
            .with_artifact(id, name, bytes)
            .with_native_artifact(id, name, bytes);
    }
    source
}

fn rebound_source(
    mut artifacts: std::collections::BTreeMap<String, Vec<u8>>,
) -> (MemorySubstrate, String) {
    use crate::backend::native_read::{shard_header, NativeArtifacts};
    let temporary = mapped_source("fixture", &artifacts);
    let header = shard_header(&temporary, "fixture").unwrap();
    let native = NativeArtifacts::read(&temporary, "fixture", &header).unwrap();
    let tasks = native.task_leaves();
    let sessions = native.session_meta_leaves();
    let spills = crate::backend::native_read::manifests::spill_leaves(&native.spills);
    let originals = crate::backend::native_read::manifests::original_leaves(&native.originals);
    let leaves = native.leaves(
        native.content.as_deref().unwrap(),
        artifacts.get("redaction_report.json").unwrap(),
        &tasks,
        &sessions,
        &spills,
        &originals,
    );
    let root = crate::backend::shard_merkle_root(&leaves).to_hex();
    let id = crate::condense::checkpoint_ids_for_shard(&header.sessions, 3, &leaves).remove(0);
    let mut metadata: serde_json::Value =
        serde_json::from_slice(artifacts.get("metadata.json").unwrap()).unwrap();
    metadata["merkle_root"] = root.into();
    artifacts.insert(
        "metadata.json".into(),
        serde_json::to_vec(&metadata).unwrap(),
    );
    (mapped_source(&id, &artifacts), id)
}

async fn all_canonical_facts(store: &SqliteStore) -> Vec<(&'static str, Vec<rift_store::Row>)> {
    let mut facts = vec![];
    for table in rift_store::schema::canonical_tables() {
        facts.push((
            table.name,
            store
                .query(&format!("SELECT * FROM {} ORDER BY 1", table.name), &[])
                .await
                .unwrap(),
        ));
    }
    facts
}

#[tokio::test]
async fn otherwise_root_and_path_bound_semantic_errors_refuse_before_any_canonical_fact() {
    let (control, _) = rebound_source(bound_artifacts());
    let store = fresh_store();
    assert_eq!(
        ingest_from_substrate(&store, &control, "repo", "claude-code", 100)
            .await
            .unwrap()
            .shards_projected,
        1
    );
    let valid_map = br#"{"agent":"claude-code","schema_version":1,"session":"native-gap-carrier"}"#;
    let bad_maps = [
        [valid_map.as_slice(), b"\n", valid_map.as_slice()].concat(),
        br#"{"agent":"codex","schema_version":1,"session":"native-gap-carrier"}"#.to_vec(),
        br#"{"agent":"claude-code","schema_version":1,"session":"outside-header"}"#.to_vec(),
        br#"{"agent":"claude-code","schema_version":2,"session":"native-gap-carrier"}"#.to_vec(),
        br#"{"agent":"claude-code","agent":"claude-code","schema_version":1,"session":"native-gap-carrier"}"#.to_vec(),
    ];
    for (index, map) in bad_maps.into_iter().enumerate() {
        let mut artifacts = bound_artifacts();
        artifacts.insert("sessions.jsonl".into(), map);
        let (source, _) = rebound_source(artifacts);
        let store = fresh_store();
        let before = all_canonical_facts(&store).await;
        let report = ingest_from_substrate(&store, &source, "repo", "claude-code", 100)
            .await
            .unwrap();
        assert_eq!(
            (report.shards_projected, report.refused.len()),
            (0, 1),
            "mapping case {index}"
        );
        assert_eq!(all_canonical_facts(&store).await, before);
    }
    for bytes in [
        checked_gap_bytes("outside-header"),
        b"{\"schema_version\":2}\n".to_vec(),
        [
            checked_gap_bytes("native-gap-carrier"),
            checked_gap_bytes("native-gap-carrier"),
        ]
        .concat(),
    ] {
        let mut artifacts = bound_artifacts();
        artifacts.insert("turn_boundary_gaps.jsonl".into(), bytes);
        let (source, _) = rebound_source(artifacts);
        let store = fresh_store();
        let before = all_canonical_facts(&store).await;
        let report = ingest_from_substrate(&store, &source, "repo", "claude-code", 100)
            .await
            .unwrap();
        assert_eq!((report.shards_projected, report.refused.len()), (0, 1));
        assert_eq!(all_canonical_facts(&store).await, before);
    }
}

#[tokio::test]
async fn strict_required_metadata_refuses_even_when_leaf_root_and_path_are_unchanged() {
    let (control, id) = bound_gap_control();
    let mut baseline = bound_artifacts();
    let raw = String::from_utf8(baseline.remove("metadata.json").unwrap()).unwrap();
    let metadata = [
        raw.replace("\"canon_version\":3", "\"canon_version\":0"),
        raw.replace("\"canon_version\":3", "\"canon_version\":1"),
        raw.replace("\"canon_version\":3", "\"canon_version\":2"),
        raw.replace("\"canon_version\":3", "\"canon_version\":4"),
        raw.replace("\"canon_version\":3", "\"canon_version\":4294967299"),
        raw.replace(
            "\"canon_version\":3",
            "\"canon_version\":3,\"canon_version\":3",
        ),
        raw.replace("\"canon_version\":3,", ""),
    ];
    assert_eq!(
        ingest_from_substrate(&fresh_store(), &control, "repo", "claude-code", 100)
            .await
            .unwrap()
            .shards_projected,
        1
    );
    let mut compatible = baseline.clone();
    compatible.insert(
        "metadata.json".into(),
        raw.replace(
            "\"canon_version\":3",
            "\"canon_version\":3,\"unrelated_extension\":{\"nested\":[1,2]}",
        )
        .into_bytes(),
    );
    let compatible = mapped_source(&id, &compatible);
    let admitted = ingest_from_substrate(&fresh_store(), &compatible, "repo", "claude-code", 100)
        .await
        .unwrap();
    assert_eq!((admitted.shards_projected, admitted.refused.len()), (1, 0));
    for raw in metadata {
        let mut artifacts = baseline.clone();
        artifacts.insert("metadata.json".into(), raw.into_bytes());
        let source = mapped_source(&id, &artifacts);
        let store = fresh_store();
        let before = all_canonical_facts(&store).await;
        let report = ingest_from_substrate(&store, &source, "repo", "claude-code", 100)
            .await
            .unwrap();
        assert_eq!((report.shards_projected, report.refused.len()), (0, 1));
        assert_eq!(all_canonical_facts(&store).await, before);
    }
}

#[tokio::test]
async fn a_changed_leaf_cannot_be_authorized_by_replacing_only_the_recorded_root() {
    let (_, old_id) = bound_gap_control();
    let mut artifacts = bound_artifacts();
    artifacts.insert("content.redacted".into(), b"changed safe content".to_vec());
    let (rebound, new_id) = rebound_source(artifacts.clone());
    assert_ne!(new_id, old_id);
    assert_eq!(
        ingest_from_substrate(&fresh_store(), &rebound, "repo", "claude-code", 100)
            .await
            .unwrap()
            .shards_projected,
        1
    );
    artifacts.insert(
        "metadata.json".into(),
        rebound
            .read_artifact(&new_id, "metadata.json")
            .unwrap()
            .unwrap(),
    );
    let forged = mapped_source(&old_id, &artifacts);
    let store = fresh_store();
    let before = all_canonical_facts(&store).await;
    let report = ingest_from_substrate(&store, &forged, "repo", "claude-code", 100)
        .await
        .unwrap();
    assert_eq!((report.shards_projected, report.refused.len()), (0, 1));
    assert_eq!(all_canonical_facts(&store).await, before);
}

fn two_subject_envelopes() -> Vec<rift_core::boundary_gap::GapEnvelope> {
    use rift_core::boundary_gap::*;
    let first = decode_checked(&checked_gap_bytes("native-gap-carrier"))
        .unwrap()
        .remove(0);
    let ticket = GapTicket::new(GapTicketFields {
        source_key: SourceKey::parse_hex(&"77".repeat(32)).unwrap(),
        context_digest: ContextDigest::parse_hex(&"88".repeat(32)).unwrap(),
        ticket_digest: TicketDigest::parse_hex(&"99".repeat(32)).unwrap(),
        session_key: SessionKey::parse_hex(&"aa".repeat(32)).unwrap(),
        turn_ordinal: 2,
        boundary: BoundaryEdge::Start,
        head_oid: None,
        captured_at: 15,
        queued_at: 25,
        attempts: 1,
    })
    .unwrap();
    let batch = first.batch();
    let second = GapBatch::new(
        batch.repo_id().clone(),
        batch.member_key(),
        batch.evidence_digest(),
        batch.recorded_at(),
        vec![ticket],
    )
    .unwrap();
    decode_checked(
        &encode_canonical(&[
            first.clone(),
            GapEnvelope::new(first.carrier_session_key(), second),
        ])
        .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn a_known_later_batch_conflict_leaves_every_canonical_relation_unchanged() {
    use rift_core::boundary_gap::*;
    let envelopes = two_subject_envelopes();
    let mut artifacts = bound_artifacts();
    artifacts.insert(
        "turn_boundary_gaps.jsonl".into(),
        encode_canonical(&envelopes).unwrap(),
    );
    let (source, _) = rebound_source(artifacts);
    let control = fresh_store();
    assert_eq!(
        ingest_from_substrate(&control, &source, "repo", "claude-code", 100)
            .await
            .unwrap()
            .shards_projected,
        1
    );
    assert_eq!(control.count("turn_boundary_gap_batch").await.unwrap(), 2);
    assert_eq!(control.count("turn_boundary_gap_ticket").await.unwrap(), 2);
    assert_eq!(control.count("session").await.unwrap(), 1);
    let later = envelopes.last().unwrap().batch();
    let conflict = GapBatch::new(
        later.repo_id().clone(),
        later.member_key(),
        later.evidence_digest(),
        later.recorded_at() + 1,
        later.tickets().to_vec(),
    )
    .unwrap();
    assert_eq!(conflict.gap_id(), later.gap_id());
    let store = fresh_store();
    rift_store::record_boundary_gap_batch(&store, &conflict)
        .await
        .unwrap();
    let before = all_canonical_facts(&store).await;
    let report = ingest_from_substrate(&store, &source, "repo", "claude-code", 100)
        .await
        .unwrap();
    assert_eq!((report.shards_projected, report.refused.len()), (0, 1));
    assert_eq!(all_canonical_facts(&store).await, before);
    assert!(
        rift_store::boundary_gap_batch(&store, envelopes.first().unwrap().batch().gap_id())
            .await
            .unwrap()
            .is_none()
    );
}

async fn seed_parentless_member(store: &SqliteStore, batch: &rift_core::boundary_gap::GapBatch) {
    use rift_store::Value;
    let ticket = batch.tickets().first().unwrap();
    assert!(store
        .insert_or_ignore(
            "turn_boundary_gap_ticket",
            &[
                "gap_id",
                "source_key",
                "context_sha256",
                "ticket_sha256",
                "session_key",
                "turn_ordinal",
                "boundary",
                "head_oid",
                "captured_at",
                "queued_at",
                "attempts",
                "schema_version"
            ],
            &[
                Value::text(batch.gap_id().to_hex()),
                Value::text(ticket.source_key().to_hex()),
                Value::text(ticket.context_digest().to_hex()),
                Value::text(ticket.ticket_digest().to_hex()),
                Value::text(ticket.session_key().to_hex()),
                Value::Int(i64::from(ticket.turn_ordinal())),
                Value::text(ticket.boundary().as_str()),
                ticket.head_oid().map_or(Value::Null, Value::text),
                Value::Int(ticket.captured_at()),
                Value::Int(ticket.queued_at()),
                Value::Int(i64::from(ticket.attempts())),
                Value::Int(1)
            ]
        )
        .await
        .unwrap());
}

#[tokio::test]
async fn an_exact_parentless_subset_resumes_without_minting_subject_sessions() {
    use rift_core::boundary_gap::*;
    let envelopes = two_subject_envelopes();
    let first = envelopes.first().unwrap();
    let original = first.batch();
    let tickets = envelopes
        .iter()
        .flat_map(|e| e.batch().tickets().iter().cloned())
        .collect();
    let batch = GapBatch::new(
        original.repo_id().clone(),
        original.member_key(),
        original.evidence_digest(),
        original.recorded_at(),
        tickets,
    )
    .unwrap();
    let mut artifacts = bound_artifacts();
    artifacts.insert(
        "turn_boundary_gaps.jsonl".into(),
        encode_canonical(&[GapEnvelope::new(first.carrier_session_key(), batch.clone())]).unwrap(),
    );
    let (source, _) = rebound_source(artifacts);
    let store = fresh_store();
    seed_parentless_member(&store, &batch).await;
    assert_eq!(store.count("turn_boundary_gap_batch").await.unwrap(), 0);
    let before = store
        .query(
            "SELECT * FROM turn_boundary_gap_ticket ORDER BY source_key",
            &[],
        )
        .await
        .unwrap();
    let report = ingest_from_substrate(&store, &source, "repo", "claude-code", 100)
        .await
        .unwrap();
    assert_eq!((report.shards_projected, report.refused.len()), (1, 0));
    assert_eq!(
        rift_store::boundary_gap_batch(&store, batch.gap_id())
            .await
            .unwrap(),
        Some(batch)
    );
    let after = store
        .query(
            "SELECT * FROM turn_boundary_gap_ticket ORDER BY source_key",
            &[],
        )
        .await
        .unwrap();
    assert!(before.iter().all(|row| after.contains(row)));
    assert_eq!(after.len(), 2);
    for table in [
        "session",
        "capture_session_identity",
        "session_in_checkpoint",
    ] {
        assert_eq!(store.count(table).await.unwrap(), 1);
    }
    for table in [
        "turn_diff_hunk",
        "attribution",
        "attribution_hunk",
        "token_usage",
    ] {
        assert_eq!(store.count(table).await.unwrap(), 0);
    }
}

#[tokio::test]
async fn changed_native_gap_bytes_refuse_instead_of_authenticating_a_different_observation() {
    let (source, id) = bound_gap_control();
    let control = fresh_store();
    assert_eq!(
        ingest_from_substrate(&control, &source, "repo", "claude-code", 100)
            .await
            .unwrap()
            .shards_projected,
        1
    );
    let changed = source.with_artifact(
        &id,
        "turn_boundary_gaps.jsonl",
        &checked_gap_bytes("other-observation"),
    );
    let store = fresh_store();
    let before = all_canonical_facts(&store).await;
    let report = ingest_from_substrate(&store, &changed, "repo", "claude-code", 100)
        .await
        .unwrap();
    assert_eq!((report.shards_projected, report.refused.len()), (0, 1));
    assert_eq!(all_canonical_facts(&store).await, before);
}

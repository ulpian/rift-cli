use super::*;
use crate::backend::{fixtures::archive, NativeBackend, ShadowBackend};
use rift_core::boundary_gap::*;

fn frozen(native: &str) -> FrozenBoundaryGaps {
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
        rift_core::RepoId::new("source-repo").unwrap(),
        MemberKey::parse_hex(&"55".repeat(32)).unwrap(),
        EvidenceDigest::parse_hex(&"66".repeat(32)).unwrap(),
        30,
        vec![ticket],
    )
    .unwrap();
    FrozenBoundaryGaps::new(vec![GapEnvelope::new(
        SessionKey::parse_hex(&rift_core::capture_session_row_id("claude-code", native)).unwrap(),
        batch,
    )])
    .unwrap()
}

#[test]
fn absent_audit_preserves_bytes_and_present_audit_moves_root_and_path() {
    let mut a = archive();
    let ordinary = a.artifact_map();
    let ordinary_root = a.merkle_root();
    let ordinary_id = a.derived_checkpoint_id();
    assert!(!ordinary.contains_key(BOUNDARY_GAPS_NAME));
    a.session_agents
        .insert("native-1".into(), "claude-code".into());
    let mapped_root = a.merkle_root();
    let mapped_id = a.derived_checkpoint_id();
    a.boundary_gaps = Some(frozen("native-1"));
    assert_ne!(a.merkle_root(), mapped_root);
    assert_ne!(a.derived_checkpoint_id(), mapped_id);
    assert_eq!(
        a.artifact_map().get(BOUNDARY_GAPS_NAME).unwrap(),
        a.boundary_gaps.as_ref().unwrap().as_bytes()
    );
    a.boundary_gaps = None;
    a.session_agents.clear();
    assert_eq!(a.artifact_map(), ordinary);
    assert_eq!(a.merkle_root(), ordinary_root);
    assert_eq!(a.derived_checkpoint_id(), ordinary_id);
}

#[test]
fn invalid_carriers_refuse_before_any_git_object_write() {
    for case in 0..4 {
        let mut a = archive();
        a.boundary_gaps = Some(frozen("native-1"));
        a.session_agents
            .insert("native-1".into(), "claude-code".into());
        match case {
            0 => a.boundary_gaps = Some(frozen("other-native")),
            1 => {
                a.session_agents
                    .insert("extra-native".into(), "claude-code".into());
            }
            2 => a.session_agents.clear(),
            _ => {
                a.session_agents
                    .insert("native-1".into(), "unknown-agent".into());
            }
        }
        a.checkpoint_id = a.derived_checkpoint_id().unwrap();
        let backend = NativeBackend::in_memory();
        assert!(backend.archive_checkpoint(&a).is_err());
        assert_eq!(
            (
                backend.writer().durable_count(),
                backend.writer().structural_count(),
                backend.writer().tree_count()
            ),
            (0, 0, 0)
        );
    }
}

#[test]
fn explicit_mapping_rejects_duplicate_fields_records_and_unknown_schema() {
    let valid = br#"{"agent":"claude-code","schema_version":1,"session":"native-1"}"#;
    assert_eq!(strict_session_agents(valid).unwrap().len(), 1);
    let repeated = [valid.as_slice(), b"\n", valid.as_slice()].concat();
    assert!(strict_session_agents(&repeated).is_err());
    for bytes in [
        br#"{"agent":"claude-code","agent":"claude-code","schema_version":1,"session":"native-1"}"#
            .as_slice(),
        br#"{"agent":"claude-code","schema_version":2,"session":"native-1"}"#,
        br#"{"agent":"claude-code","schema_version":1,"session":"native-1","unknown":"private"}"#,
    ] {
        assert!(strict_session_agents(bytes).is_err());
    }
}

#[test]
fn a_valid_mapped_carrier_writes_but_a_changed_path_id_writes_nothing() {
    let mut a = archive();
    a.boundary_gaps = Some(frozen("native-1"));
    a.session_agents
        .insert("native-1".into(), "claude-code".into());
    a.checkpoint_id = a.derived_checkpoint_id().unwrap();
    let valid = NativeBackend::in_memory();
    valid.archive_checkpoint(&a).unwrap();
    assert!(valid.writer().durable_count() > 0);
    assert!(valid.writer().structural_count() > 0);
    assert!(valid.writer().tree_count() > 0);
    a.checkpoint_id = "ff".repeat(32);
    let invalid = NativeBackend::in_memory();
    assert!(invalid.archive_checkpoint(&a).is_err());
    assert_eq!(
        (
            invalid.writer().durable_count(),
            invalid.writer().structural_count(),
            invalid.writer().tree_count()
        ),
        (0, 0, 0)
    );
}

#[test]
fn empty_and_repeated_native_membership_cannot_establish_a_carrier() {
    let map = BTreeMap::from([("native-1".into(), "claude-code".into())]);
    let gaps = frozen("native-1");
    assert!(validate_carriers(&[], &map, gaps.envelopes()).is_err());
    assert!(validate_carriers(
        &["native-1".into(), "native-1".into()],
        &map,
        gaps.envelopes()
    )
    .is_err());
}

fn empty_spill_source(
    a: &crate::backend::CheckpointArchive,
    omit: bool,
) -> rift_recall_substrate::MemorySubstrate {
    use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};
    let mut source = MemorySubstrate::local().with_shard(CheckpointShard {
        checkpoint_id: a.checkpoint_id.clone(),
        dialect: Dialect::Rift,
        prev: None,
        ordinal: 1,
    });
    for (name, bytes) in a.artifact_map() {
        if omit && name == "tool-results/empty.txt" {
            continue;
        }
        source = source
            .with_artifact(&a.checkpoint_id, &name, &bytes)
            .with_native_artifact(&a.checkpoint_id, &name, &bytes);
    }
    source
}

fn legacy_spill_binding(
    source: &impl rift_recall_substrate::SubstrateReader,
    id: &str,
) -> (String, Vec<String>) {
    use crate::backend::native_read::{shard_header, NativeArtifacts};
    let header = shard_header(source, id).unwrap();
    let native = NativeArtifacts::read(source, id, &header).unwrap();
    let report = source
        .read_artifact(id, "redaction_report.json")
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
    (
        crate::backend::shard_merkle_root(&leaves).to_hex(),
        crate::condense::checkpoint_ids_for_shard(&header.sessions, 3, &leaves),
    )
}

#[tokio::test]
async fn deleting_a_sealed_empty_spill_preserves_legacy_hashes_but_refuses_gap_projection() {
    use crate::backend::{ArchivedSpill, SpillState};
    use rift_recall_substrate::SubstrateReader;
    use rift_store::{CanonicalStore, SqliteStore};
    let mut a = crate::fixture_archive("two_turn");
    let native = a.sessions.first().unwrap().clone();
    a.session_agents
        .insert(native.clone(), "claude-code".into());
    a.boundary_gaps = Some(frozen(&native));
    let mut transcript =
        crate::backend::fixtures::transcript_of(br#"{"type":"user","message":"safe"}"#);
    transcript.spills.push(ArchivedSpill {
        tool_use_id: "empty".into(),
        state: SpillState::Sealed,
        body: Some(crate::backend::fixtures::redacted_of(b"")),
        source_bytes: None,
    });
    a.transcript = Some(transcript);
    a.checkpoint_id = a.derived_checkpoint_id().unwrap();
    let present = empty_spill_source(&a, false);
    let missing = empty_spill_source(&a, true);
    assert_eq!(
        present
            .read_artifact(&a.checkpoint_id, "tool-results/empty.txt")
            .unwrap(),
        Some(vec![])
    );
    assert_eq!(
        missing
            .read_artifact(&a.checkpoint_id, "tool-results/empty.txt")
            .unwrap(),
        None
    );
    let binding = legacy_spill_binding(&present, &a.checkpoint_id);
    assert_eq!(binding, legacy_spill_binding(&missing, &a.checkpoint_id));
    assert_eq!(
        binding,
        (a.merkle_root().to_hex(), vec![a.checkpoint_id.clone()])
    );
    for (source, projected, refused) in [(present, 1, 0), (missing, 0, 1)] {
        let store = SqliteStore::open_in_memory().unwrap();
        store.create_canonical_schema().unwrap();
        let report = crate::ingest_from_substrate(&store, &source, "repo", "claude-code", 100)
            .await
            .unwrap();
        assert_eq!(
            (report.shards_projected, report.refused.len()),
            (projected, refused)
        );
        for table in [
            "checkpoint",
            "turn_boundary_gap_batch",
            "turn_boundary_gap_ticket",
        ] {
            assert_eq!(
                store.count(table).await.unwrap(),
                i64::try_from(projected).unwrap()
            );
        }
    }
}

fn maximal_source(
    id: &str,
    artifacts: &BTreeMap<String, Vec<u8>>,
) -> rift_recall_substrate::MemorySubstrate {
    use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};
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

#[test]
fn every_leaf_in_the_maximal_native_assembly_is_covered_by_gap_content_binding() {
    use crate::backend::native_read::load_native_boundary_gaps;
    use rift_recall_substrate::SubstrateReader;
    // This is a complete content-binding matrix, separate from the CLI's
    // original-result rendering diagnostics over this historical rich fixture.
    let mut a = crate::shard_fixture::every_artifact_archive().unwrap();
    a.boundary_gaps = Some(frozen(a.sessions.first().unwrap()));
    a.checkpoint_id = a.derived_checkpoint_id().unwrap();
    let artifacts = a.artifact_map();
    let source = maximal_source(&a.checkpoint_id, &artifacts);
    let shard = source.checkpoints().unwrap().remove(0);
    let gap = a.boundary_gaps.as_ref().unwrap().as_bytes();
    assert!(load_native_boundary_gaps(&source, &shard, gap).is_ok());
    let names: Vec<_> = artifacts
        .keys()
        .filter(|name| !["metadata.json", "content_hash.txt"].contains(&name.as_str()))
        .cloned()
        .collect();
    for prefix in [
        "tasks/",
        "sessions/",
        "tool-results/",
        "tool-originals/",
        "digest.",
    ] {
        assert!(
            names.iter().any(|name| name.starts_with(prefix)),
            "matrix includes {prefix}"
        );
    }
    for name in names {
        for remove in [false, true] {
            let mut changed = artifacts.clone();
            if remove {
                changed.remove(&name);
            } else {
                changed.get_mut(&name).unwrap().push(b'!');
            }
            let source = maximal_source(&a.checkpoint_id, &changed);
            assert_eq!(
                source
                    .read_native_artifact(&a.checkpoint_id, "metadata.json")
                    .unwrap()
                    .as_ref(),
                artifacts.get("metadata.json")
            );
            assert!(
                load_native_boundary_gaps(&source, &shard, gap).is_err(),
                "changed leaf {name}, removed={remove}"
            );
        }
    }
}

use rift_recall_substrate::MemorySubstrate;
use rift_shadow::backend::{ArchivedTask, TRANSCRIPT_COMPACT_NAME, TRANSCRIPT_FULL_NAME};

use super::*;
use crate::verify::artifacts::shard_header;
use crate::verify::tests::shard;

const ID: &str = "cp-fallback";
const TASK: &str = "toolu_task";

/// A rule that matches inside its own placeholder, so the fixpoint never
/// settles on a line carrying the word.
fn wedged() -> rift_shadow::redact::RulePackSet {
    rift_shadow::redact::RulePackSet::builtin()
        .with_vendor_pattern("osc", "REDACTED", "acme@1")
        .unwrap()
}

/// A subagent pair sealed through the real gate with one stuck line, its
/// metadata written by the real writer: `(metadata, full, compact)`.
fn wedged_task() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let raw = rift_core::SecretBytes::new(
        b"{\"type\":\"user\",\"text\":\"hello\"}\n{\"type\":\"user\",\"text\":\"start REDACTED end\"}\n"
            .to_vec(),
    );
    let (pair, _) = rift_shadow::archived_transcript(&raw, 0, &wedged()).unwrap();
    let (full, compact) = (
        pair.full.as_slice().to_vec(),
        pair.compact.as_slice().to_vec(),
    );
    let meta = ArchivedTask {
        tool_use_id: TASK.into(),
        agent_id: "a1".into(),
        started_at_ms: 1,
        ended_at_ms: 2,
        input_tokens: None,
        output_tokens: None,
        transcript: Some(pair),
        provenance: None,
    }
    .metadata_json();
    (meta, full, compact)
}

/// A shard whose one task carries the wedged pair; `full` is stored in place
/// of the task's sealed transcript.
fn substrate(meta: &[u8], full: &[u8], compact: &[u8]) -> MemorySubstrate {
    let parent = format!(r#"{{"sessions":["s"],"tasks":["{TASK}"]}}"#);
    MemorySubstrate::local()
        .with_shard(shard(ID, None, 0))
        .with_artifact(ID, "redaction_report.json", b"{}")
        .with_artifact(ID, "metadata.json", parent.as_bytes())
        .with_artifact(ID, &format!("tasks/{TASK}/metadata.json"), meta)
        .with_artifact(ID, &format!("tasks/{TASK}/{TRANSCRIPT_FULL_NAME}"), full)
        .with_artifact(
            ID,
            &format!("tasks/{TASK}/{TRANSCRIPT_COMPACT_NAME}"),
            compact,
        )
}

fn verdicts(reader: &MemorySubstrate) -> Vec<Verdict> {
    let header = shard_header(reader, ID).unwrap();
    let native = NativeArtifacts::read(reader, ID, &header).unwrap();
    fallback_pass(
        ID,
        crate::verify::fallback::header_counts(&header),
        &native,
        reader
            .read_artifact(ID, "redaction_report.json")
            .unwrap()
            .as_deref(),
    )
}

/// The writer's count reaches the checkpoint total the doctor reads, and a
/// pair that holds its placeholder records passes.
#[test]
fn a_recorded_fallback_is_read_and_its_records_are_found() {
    let (meta, full, compact) = wedged_task();
    let reader = substrate(&meta, &full, &compact);
    let total = on_checkpoint(&reader, ID).unwrap();
    assert!(total.any());
    assert!(total.placeholders >= 1, "{total:?}");
    assert_eq!(verdicts(&reader), Vec::<Verdict>::new());
}

/// A transcript that no longer holds the records its metadata counts is a
/// redaction finding naming the document.
#[test]
fn a_recorded_placeholder_the_transcript_does_not_hold_is_a_redaction_finding() {
    let (meta, _, _) = wedged_task();
    let reader = substrate(&meta, b"{\"type\":\"user\"}\n", b"");
    let found = verdicts(&reader);
    assert_eq!(found.len(), 1, "{found:?}");
    let Verdict::Redaction(why) = &found[0] else {
        panic!("{found:?}");
    };
    assert!(why.contains("tasks/toolu_task/metadata.json"), "{why}");
}

/// More placeholders than fallback lines is no pair the writer produces.
#[test]
fn more_placeholders_than_lines_is_a_structure_finding() {
    let recorded = Recorded {
        lines: 1,
        placeholders: 2,
    };
    assert!(matches!(
        judge("cp", "", recorded, 2),
        Some(Verdict::Structure(_))
    ));
    assert_eq!(judge("cp", "", Recorded::default(), 0), None);
}

/// A shard with no metadata, or with none of the keys, records nothing.
#[test]
fn absent_keys_are_zero_but_absent_documents_are_unreadable() {
    let reader = MemorySubstrate::local().with_shard(shard(ID, None, 0));
    assert!(on_checkpoint(&reader, ID).is_err());
    let doc = serde_json::json!({ "sessions": ["s"] });
    assert!(!Recorded::of(Some(&doc)).unwrap().any());
}

#[test]
fn authoritative_report_cannot_be_hidden_by_removing_parent_counts() {
    let reader = MemorySubstrate::local()
        .with_shard(shard(ID, None, 0))
        .with_artifact(ID, "metadata.json", br#"{"sessions":["s"]}"#)
        .with_artifact(
            ID,
            "redaction_report.json",
            br#"{"redaction_flat_fallback":2,"redaction_fallback_placeholders":2}"#,
        );
    assert!(
        on_checkpoint(&reader, ID).is_err(),
        "doctor must surface an inconsistent mirror"
    );
    assert!(verdicts(&reader)
        .iter()
        .any(|v| matches!(v, Verdict::Structure(_))));
}

#[test]
fn count_pairs_are_strict_and_accept_the_zero_placeholder_subset() {
    assert_eq!(Recorded::of(None), Ok(Recorded::default()));
    let valid =
        serde_json::json!({"redaction_flat_fallback":3,"redaction_fallback_placeholders":0});
    assert_eq!(
        Recorded::of(Some(&valid)),
        Ok(Recorded {
            lines: 3,
            placeholders: 0
        })
    );
    for value in [
        serde_json::json!(null),
        serde_json::json!("1"),
        serde_json::json!(-1),
        serde_json::json!(1.0),
        serde_json::json!(4_294_967_296_u64),
    ] {
        for key in ["redaction_flat_fallback", "redaction_fallback_placeholders"] {
            let mut bad = valid.clone();
            bad[key] = value.clone();
            assert!(Recorded::of(Some(&bad)).is_err(), "{bad}");
        }
    }
    for bad in [
        serde_json::json!({"redaction_flat_fallback":1}),
        serde_json::json!({"redaction_fallback_placeholders":1}),
        serde_json::json!({"redaction_flat_fallback":0,"redaction_fallback_placeholders":0}),
        serde_json::json!({"redaction_flat_fallback":1,"redaction_fallback_placeholders":2}),
    ] {
        assert!(Recorded::of(Some(&bad)).is_err(), "{bad}");
    }
}

#[test]
fn literal_placeholder_surplus_is_allowed() {
    assert_eq!(judge(ID, "", Recorded::default(), 2), None);
    assert_eq!(
        judge(
            ID,
            "",
            Recorded {
                lines: 1,
                placeholders: 1
            },
            3
        ),
        None
    );
}

fn bound_parent() -> (MemorySubstrate, String, Vec<u8>, Vec<u8>) {
    bound_pair(false)
}

fn bound_pair(with_task: bool) -> (MemorySubstrate, String, Vec<u8>, Vec<u8>) {
    let mut archive = rift_shadow::shard_fixture::every_artifact_archive().unwrap();
    // The maximal golden's synthetic task-original line is not a delivered
    // result fixture. This proof needs only the writer's real parent pair.
    archive.tasks.clear();
    let raw = rift_core::SecretBytes::from("WEDGE ssn=\n123456789");
    let packs = rift_shadow::redact::RulePackSet::builtin()
        .with_vendor_pattern("osc", "WEDGE|REDACTED:vendor", "acme@1")
        .unwrap();
    archive.transcript = Some(rift_shadow::archived_transcript(&raw, 0, &packs).unwrap().0);
    if with_task {
        archive.tasks.push(ArchivedTask {
            tool_use_id: TASK.into(),
            agent_id: "a1".into(),
            started_at_ms: 1,
            ended_at_ms: 2,
            input_tokens: None,
            output_tokens: None,
            transcript: archive.transcript.clone(),
            provenance: None,
        });
    }
    let id = archive.derived_checkpoint_id().unwrap();
    archive.checkpoint_id = id.clone();
    let meta = archive.metadata_json();
    let report = archive.redaction_report_json();
    let mut reader = MemorySubstrate::local().with_shard(shard(&id, None, 0));
    for (path, bytes) in archive.artifact_map() {
        reader = reader.with_artifact(&id, &path, &bytes);
    }
    (reader, id, meta, report)
}

#[test]
fn parent_mirror_edits_are_rejected_without_changing_bound_bytes() {
    let (reader, id, bytes, _) = bound_parent();
    assert_eq!(
        crate::verify::passes::shard_passes(&reader).unwrap().worst,
        Verdict::Ok
    );
    let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for value in [
        serde_json::json!(0),
        serde_json::json!(null),
        serde_json::json!("2"),
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!(4_294_967_296_u64),
    ] {
        for key in ["redaction_flat_fallback", "redaction_fallback_placeholders"] {
            let mut meta = original.clone();
            meta[key] = value.clone();
            let edited = bound_parent().0.with_artifact(
                &id,
                "metadata.json",
                &serde_json::to_vec(&meta).unwrap(),
            );
            assert!(
                matches!(
                    crate::verify::passes::shard_passes(&edited).unwrap().worst,
                    Verdict::Structure(_)
                ),
                "{meta}"
            );
            assert!(on_checkpoint(&edited, &id).is_err(), "{meta}");
        }
    }
    for keys in [
        vec!["redaction_flat_fallback"],
        vec!["redaction_fallback_placeholders"],
        vec!["redaction_flat_fallback", "redaction_fallback_placeholders"],
    ] {
        let mut meta = original.clone();
        for key in keys {
            meta.as_object_mut().unwrap().remove(key);
        }
        let edited = bound_parent().0.with_artifact(
            &id,
            "metadata.json",
            &serde_json::to_vec(&meta).unwrap(),
        );
        assert!(
            matches!(
                crate::verify::passes::shard_passes(&edited).unwrap().worst,
                Verdict::Structure(_)
            ),
            "{meta}"
        );
    }
}

#[test]
fn fallback_report_edits_are_bound_by_root_and_checkpoint_identity() {
    let (reader, id, meta, report) = bound_parent();
    assert_eq!(
        crate::verify::passes::shard_passes(&reader).unwrap().worst,
        Verdict::Ok
    );
    let mut edited: serde_json::Value = serde_json::from_slice(&report).unwrap();
    edited["redaction_flat_fallback"] = 99.into();
    let edited_report = serde_json::to_vec(&edited).unwrap();
    let changed = reader.with_artifact(&id, "redaction_report.json", &edited_report);
    assert!(matches!(
        crate::verify::passes::shard_passes(&changed).unwrap().worst,
        Verdict::Tamper(why) if why.contains("Merkle root")
    ));
    let header = shard_header(&changed, &id).unwrap();
    let native = NativeArtifacts::read(&changed, &id, &header).unwrap();
    let tasks = native.task_leaves();
    let sessions = native.session_meta_leaves();
    let spills = crate::verify::spills::leaves(&native.spills);
    let originals = crate::verify::originals::leaves(&native.originals);
    let root = rift_shadow::backend::shard_merkle_root(&native.leaves(
        native.content.as_deref().unwrap(),
        &edited_report,
        &tasks,
        &sessions,
        &spills,
        &originals,
    ));
    let mut metadata: serde_json::Value = serde_json::from_slice(&meta).unwrap();
    metadata["merkle_root"] = root.to_hex().into();
    metadata["redaction_flat_fallback"] = 99.into();
    let recomputed = changed.with_artifact(
        &id,
        "metadata.json",
        &serde_json::to_vec(&metadata).unwrap(),
    );
    assert!(matches!(
        crate::verify::passes::shard_passes(&recomputed)
            .unwrap()
            .worst,
        Verdict::Tamper(why) if why.contains("checkpoint id")
    ));
}

#[test]
fn doctor_does_not_silently_skip_missing_documents_or_malformed_task_ids() {
    for metadata in [
        b"[]".as_slice(),
        b"null",
        b"{\"tasks\":false}",
        b"{\"tasks\":[3]}",
        b"{\"tasks\":[\"../bad\"]}",
        b"{\"tasks\":[\"toolu_missing\"]}",
        b"{\"tasks\":[\"toolu_duplicate\",\"toolu_duplicate\"]}",
    ] {
        let reader = MemorySubstrate::local()
            .with_shard(shard(ID, None, 0))
            .with_artifact(ID, "metadata.json", metadata)
            .with_artifact(ID, "redaction_report.json", b"{}");
        assert!(on_checkpoint(&reader, ID).is_err(), "{metadata:?}");
    }
}

#[test]
fn missing_parent_metadata_cannot_hide_a_positive_bound_report() {
    let reader = MemorySubstrate::local()
        .with_shard(shard(ID, None, 0))
        .with_artifact(
            ID,
            "redaction_report.json",
            br#"{"redaction_flat_fallback":2,"redaction_fallback_placeholders":2}"#,
        );
    assert!(on_checkpoint(&reader, ID).is_err());
    let reader = MemorySubstrate::local()
        .with_shard(shard(ID, None, 0))
        .with_artifact(ID, "metadata.json", b"{}");
    assert!(on_checkpoint(&reader, ID).is_err());
}

#[test]
fn doctor_sums_parent_and_task_pair_once_each() {
    let (meta, full, compact) = wedged_task();
    let reader = substrate(&meta, &full, &compact);
    let task = on_checkpoint(&reader, ID).unwrap();
    let parent = format!(
        r#"{{"sessions":["s"],"tasks":["{TASK}"],"redaction_flat_fallback":2,"redaction_fallback_placeholders":1}}"#
    );
    let reader = reader
        .with_artifact(ID, "metadata.json", parent.as_bytes())
        .with_artifact(
            ID,
            "redaction_report.json",
            br#"{"redaction_flat_fallback":2,"redaction_fallback_placeholders":1}"#,
        );
    assert_eq!(
        on_checkpoint(&reader, ID).unwrap(),
        task.plus(Recorded {
            lines: 2,
            placeholders: 1
        })
    );
}

#[test]
fn task_fallback_counts_are_typed_and_bound_by_their_metadata_leaf() {
    let (reader, id, _, _) = bound_pair(true);
    assert_eq!(
        crate::verify::passes::shard_passes(&reader).unwrap().worst,
        Verdict::Ok
    );
    let path = format!("tasks/{TASK}/metadata.json");
    let bytes = reader.read_artifact(&id, &path).unwrap().unwrap();
    let mut task: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    task["redaction_flat_fallback"] = "invalid".into();
    let changed = reader.with_artifact(&id, &path, &serde_json::to_vec(&task).unwrap());
    let header = shard_header(&changed, &id).unwrap();
    let native = NativeArtifacts::read(&changed, &id, &header).unwrap();
    let report = changed
        .read_artifact(&id, "redaction_report.json")
        .unwrap()
        .unwrap();
    assert!(fallback_pass(
        &id,
        crate::verify::fallback::header_counts(&header),
        &native,
        Some(&report)
    )
    .iter()
    .any(|v| matches!(v, Verdict::Structure(_))));
    assert!(
        matches!(crate::verify::passes::shard_passes(&changed).unwrap().worst,
        Verdict::Tamper(why) if why.contains("Merkle root"))
    );
}

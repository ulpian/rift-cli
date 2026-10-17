use std::cell::RefCell;

use rift_recall_substrate::{CheckpointShard, MemorySubstrate, SubstrateReader};
use rift_shadow::backend::{shard_merkle_leaves, shard_merkle_root};

use super::artifacts::{shard_header, NativeArtifacts};

const ID: &str = "native-reader-invariance";

struct RecordedReader {
    inner: MemorySubstrate,
    reads: RefCell<Vec<(String, Option<Vec<u8>>)>>,
}

impl SubstrateReader for RecordedReader {
    fn is_local_only(&self) -> bool {
        true
    }
    fn checkpoints(&self) -> rift_core::Result<Vec<CheckpointShard>> {
        self.inner.checkpoints()
    }
    fn blob(&self, id: &str) -> rift_core::Result<Option<Vec<u8>>> {
        self.inner.blob(id)
    }
    fn read_artifact(&self, id: &str, name: &str) -> rift_core::Result<Option<Vec<u8>>> {
        let bytes = self.inner.read_artifact(id, name)?;
        self.reads
            .borrow_mut()
            .push((name.to_owned(), bytes.clone()));
        Ok(bytes)
    }
}

fn maximal_reader() -> RecordedReader {
    let originals = serde_json::json!([
        {"tool_use_id":"rich","sha256":"11".repeat(32),"bytes":9,"form":"fold","replaced":[],"line":1,"rewritten":"spooled_not_delivered"},
        {"tool_use_id":"state-only","original":"original_unsealed","line":2},
        {"tool_use_id":"missing","sha256":"22".repeat(32),"bytes":7,"line":3}
    ]);
    let manifest = serde_json::json!({
        "canon_version":3,"sessions":["carrier-a","carrier-b"],"tasks":["task-a"],
        "task_tokens":{"input_tokens":2,"output_tokens":3},
        "spills":[{"tool_use_id":"spill","sha256":"33".repeat(32),"bytes":10},{"tool_use_id":"missing-spill","bytes":4}],
        "originals": originals
    });
    let mut sub = MemorySubstrate::local().with_artifact(
        ID,
        "metadata.json",
        &serde_json::to_vec(&manifest).unwrap(),
    );
    for name in [
        "content.redacted",
        "redaction_report.json",
        "full.jsonl",
        "tool_calls.jsonl",
        "tool_results.jsonl",
        "serve_audit.jsonl",
        "sessions.jsonl",
        "usage.jsonl",
        "window_journal.jsonl",
        "session_worktrees.jsonl",
        "transcript.compact.jsonl",
        "transcript.jsonl",
        "sessions/0/metadata.json",
        "sessions/1/metadata.json",
        "digest.failures.json",
        "digest.intent.json",
        "digest.log.json",
        "digest.shape.json",
        "tool-results/spill.txt",
        "tool-originals/rich.json",
        "tasks/task-a/transcript.compact.jsonl",
        "tasks/task-a/transcript.jsonl",
        "tasks/task-a/tool-results/spill.txt",
        "tasks/task-a/tool-originals/rich.json",
    ] {
        sub = sub.with_artifact(ID, name, name.as_bytes());
    }
    sub = sub.with_artifact(
        ID,
        "tasks/task-a/metadata.json",
        &serde_json::to_vec(&manifest).unwrap(),
    );
    RecordedReader {
        inner: sub,
        reads: RefCell::new(Vec::new()),
    }
}

fn metadata_presence() -> (String, String) {
    let absent = crate::verify::fallback::header_counts(
        &shard_header(&MemorySubstrate::local(), ID).unwrap(),
    );
    let malformed = crate::verify::fallback::header_counts(
        &shard_header(
            &MemorySubstrate::local().with_artifact(ID, "metadata.json", b"{"),
            ID,
        )
        .unwrap(),
    );
    assert!(absent.is_ok());
    assert!(malformed.is_err());
    (format!("{absent:?}"), format!("{malformed:?}"))
}

fn valid_binding_control() -> serde_json::Value {
    let (sub, id) = super::boundary_gaps_tests::native_control();
    let found = super::passes::shard_passes(&sub).unwrap();
    assert_eq!(
        (found.checked, found.worst),
        (1, super::verdict::Verdict::Ok)
    );
    serde_json::json!({"checkpoint_id":id, "metadata":sub.read_artifact(&id, "metadata.json").unwrap()})
}

#[test]
fn maximal_native_reader_leaf_root_id_and_metadata_presence_are_invariant() {
    let reader = maximal_reader();
    let header = shard_header(&reader, ID).unwrap();
    let native = NativeArtifacts::read(&reader, ID, &header).unwrap();
    assert_eq!(
        (
            native.tasks.len(),
            native.session_metas.len(),
            native.spills.len(),
            native.originals.len()
        ),
        (1, 2, 4, 6)
    );
    assert!(native
        .originals
        .iter()
        .any(|o| o.claim.tool_use_id == "state-only" && o.body.is_none()));
    assert!(native
        .originals
        .iter()
        .any(|o| o.claim.tool_use_id == "missing" && o.body.is_none()));
    let tasks = native.task_leaves();
    let sessions = native.session_meta_leaves();
    let spills = super::spills::leaves(&native.spills);
    let originals = super::originals::leaves(&native.originals);
    assert_eq!(
        originals.len(),
        4,
        "state-only entries never invent body leaves"
    );
    let report = reader
        .read_artifact(ID, "redaction_report.json")
        .unwrap()
        .unwrap();
    let leaves = native.leaves(
        native.content.as_deref().unwrap(),
        &report,
        &tasks,
        &sessions,
        &spills,
        &originals,
    );
    let hashes = shard_merkle_leaves(&leaves)
        .into_iter()
        .map(|h| h.to_hex())
        .collect::<Vec<_>>();
    let candidates = rift_shadow::condense::checkpoint_ids_for_shard(&header.sessions, 3, &leaves);
    let (absent, malformed) = metadata_presence();
    assert_eq!(
        reader
            .reads
            .borrow()
            .iter()
            .filter(|(name, _)| name == "metadata.json")
            .count(),
        1
    );
    let findings = super::originals::original_passes(ID, &native);
    let output = serde_json::json!({
        "reads": reader.reads.borrow().clone(), "leaf_hashes": hashes,
        "root": shard_merkle_root(&leaves).to_hex(), "checkpoint_candidates": candidates,
        "absent_fallback":absent, "malformed_fallback":malformed,
        "valid_binding_control":valid_binding_control(),
        "original_findings": format!("{:?}", findings.verdicts), "held_unchecked":findings.held_unchecked,
        "spill_findings":format!("{:?}", super::spills::spill_digest_pass(ID, &native.spills))
    });
    eprintln!("NATIVE_READ_BASELINE {output}");
}

use rift_recall_substrate::MemorySubstrate;
use rift_shadow::backend::{
    shard_merkle_root, ArchivedSpill, ArchivedTask, DigestLeafBytes, ShardLeafBytes, SpillState,
    TaskLeafBytes, ROOT_BOUND_ID_MIN_CANON_VERSION,
};

use super::*;
use crate::verify::passes::shard_passes;
use crate::verify::tests::{fixture_report, shard, FIXTURE_SESSION};

const OWN: &str = "toolu_own";
const TASK: &str = "toolu_task";
const INNER: &str = "toolu_inner";

/// A spill body sealed through the gate.
fn sealed(raw: &[u8]) -> rift_core::RedactedBytes {
    let raw = rift_core::SecretBytes::new(raw.to_vec());
    rift_shadow::sealed_spill(&raw, &rift_shadow::redact::RulePackSet::builtin()).unwrap()
}

/// Which stored spill body a fixture tampers with.
#[derive(Clone, Copy, PartialEq)]
enum Target {
    Own,
    Inner,
}

/// The subagent subtree of [`spill_shard`]: its metadata, transcript pair
/// and the one spill its transcript sealed.
struct Subtree {
    meta: Vec<u8>,
    full: Vec<u8>,
    compact: Vec<u8>,
    inner: Vec<u8>,
}

fn subtree() -> Subtree {
    let raw = rift_core::SecretBytes::new(b"{\"type\":\"user\"}\n".to_vec());
    let packs = rift_shadow::redact::RulePackSet::builtin();
    let (mut pair, _) = rift_shadow::archived_transcript(&raw, 0, &packs).unwrap();
    pair.spills = vec![ArchivedSpill {
        tool_use_id: INNER.into(),
        state: SpillState::Sealed,
        body: Some(sealed(b"inner spill\n")),
        source_bytes: None,
    }];
    let inner = pair.spills[0].body.clone().unwrap().as_slice().to_vec();
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
    Subtree {
        meta,
        full,
        compact,
        inner,
    }
}

/// A shard as the archive writes one whose session transcript sealed a
/// 60 K-character spill and whose one subagent sealed another; `tamper` is
/// applied to the `target` body as stored.
fn spill_shard(target: Target, tamper: impl Fn(&mut Vec<u8>)) -> (MemorySubstrate, String) {
    let content = b"parent capture".as_slice();
    let report = fixture_report();
    let own = sealed(&"o".repeat(60_000).into_bytes());
    let Subtree {
        meta: task_meta,
        full,
        compact,
        inner,
    } = subtree();
    let task_leaves = [TaskLeafBytes {
        tool_use_id: TASK,
        metadata: &task_meta,
        transcript_compact: Some(&compact),
        transcript_full: Some(&full),
    }];
    let spill_leaves = [
        SpillLeafBytes {
            task: None,
            tool_use_id: OWN,
            body: own.as_slice(),
        },
        SpillLeafBytes {
            task: Some(TASK),
            tool_use_id: INNER,
            body: &inner,
        },
    ];
    let leaves = ShardLeafBytes {
        boundary_gaps: None,
        content_redacted: content,
        redaction_report: report.as_bytes(),
        tasks: &task_leaves,
        spills: &spill_leaves,
        digests: DigestLeafBytes::default(),
        ..ShardLeafBytes::default()
    };
    let root = shard_merkle_root(&leaves).to_hex();
    let id = rift_shadow::condense::checkpoint_ids_for_shard(
        &[FIXTURE_SESSION.to_owned()],
        ROOT_BOUND_ID_MIN_CANON_VERSION,
        &leaves,
    )
    .remove(0);
    let (mut stored_own, mut stored_inner) = (own.as_slice().to_vec(), inner.clone());
    match target {
        Target::Own => tamper(&mut stored_own),
        Target::Inner => tamper(&mut stored_inner),
    }
    let manifest = format!(
        r#"[{{"bytes":{},"sha256":"{}","tool_use_id":"{OWN}"}},{{"spill":"spill_missing","tool_use_id":"toolu_gone"}}]"#,
        own.as_slice().len(),
        rift_shadow::sha256_hex(&[own.as_slice()]),
    );
    let sub = MemorySubstrate::local()
        .with_shard(shard(&id, None, 1))
        .with_artifact(&id, "redaction_report.json", report.as_bytes())
        .with_artifact(&id, "content.redacted", content)
        .with_artifact(&id, &format!("tool-results/{OWN}.txt"), &stored_own)
        .with_artifact(&id, &format!("tasks/{TASK}/metadata.json"), &task_meta)
        .with_artifact(&id, &format!("tasks/{TASK}/transcript.compact.jsonl"), &compact)
        .with_artifact(&id, &format!("tasks/{TASK}/transcript.jsonl"), &full)
        .with_artifact(&id, &format!("tasks/{TASK}/tool-results/{INNER}.txt"), &stored_inner)
        .with_artifact(
            &id,
            "metadata.json",
            format!(
                r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"strategy":"manual-commit","merkle_root":"{root}","sessions":["{FIXTURE_SESSION}"],"spills":{manifest},"tasks":["{TASK}"]}}"#
            )
            .as_bytes(),
        );
    (sub, id)
}

/// An honest shard carrying spills beside both transcripts verifies; a
/// recorded loss owes nothing.
#[test]
fn an_honest_shard_with_spills_verifies() {
    let (sub, _) = spill_shard(Target::Own, |_| ());
    let out = shard_passes(&sub).unwrap();
    assert!(matches!(out.worst, Verdict::Ok), "{:?}", out.worst);
}

/// One byte changed in a sealed spill fails verify, and the finding names
/// the file — the session's own and a subagent's alike.
#[test]
fn a_one_byte_tamper_in_a_spill_names_the_file() {
    for (target, path) in [
        (Target::Own, format!("tool-results/{OWN}.txt")),
        (
            Target::Inner,
            format!("tasks/{TASK}/tool-results/{INNER}.txt"),
        ),
    ] {
        let (sub, _) = spill_shard(target, |b| b[0] ^= 1);
        let out = shard_passes(&sub).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains(&path)),
            "{path}: {:?}",
            out.worst
        );
    }
}

/// A spill dropped from the shard while its manifest still claims it is
/// tamper, named by its path.
#[test]
fn a_claimed_spill_missing_from_the_shard_is_tamper() {
    let (sub, id) = spill_shard(Target::Own, |_| ());
    let stripped = MemorySubstrate::local()
        .with_shard(shard(&id, None, 1))
        .with_artifact(&id, "redaction_report.json", fixture_report().as_bytes())
        .with_artifact(&id, "content.redacted", b"parent capture")
        .with_artifact(
            &id,
            "metadata.json",
            &sub.read_artifact(&id, "metadata.json").unwrap().unwrap(),
        );
    let out = shard_passes(&stripped).unwrap();
    assert!(
        matches!(&out.worst, Verdict::Tamper(m) if m.contains(&format!("tool-results/{OWN}.txt"))),
        "{:?}",
        out.worst
    );
}

/// Only entries carrying a digest or a size claim a body.
#[test]
fn a_lost_spill_claims_no_body() {
    let doc = serde_json::json!({ "spills": [
        { "tool_use_id": "a", "bytes": 3, "sha256": "x" },
        { "tool_use_id": "b", "spill": "spill_missing" },
        { "bytes": 1 },
    ]});
    let got: Vec<String> = claims(Some(&doc))
        .into_iter()
        .map(|c| c.tool_use_id)
        .collect();
    assert_eq!(got, ["a"]);
    assert!(claims(None).is_empty());
}

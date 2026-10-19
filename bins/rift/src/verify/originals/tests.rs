use rift_recall_substrate::MemorySubstrate;
use rift_shadow::backend::rewrite::{
    original_record_json, render_folded, LineRange, ResultText, RewriteForm, Rewritten, Stream,
};
use rift_shadow::backend::{
    shard_merkle_root, ArchivedOriginal, ArchivedTask, ArchivedTranscript, DigestLeafBytes,
    OriginalState, ShardLeafBytes, TaskLeafBytes, ROOT_BOUND_ID_MIN_CANON_VERSION,
};

use super::*;
use crate::verify::passes::shard_passes;
use crate::verify::tests::{fixture_report, shard, FIXTURE_SESSION};

const FOLDED: &str = "toolu_folded";
const HELD: &str = "toolu_held";
const TASK: &str = "toolu_task";
const INNER: &str = "toolu_inner";
const OUTPUT: &str = "keep 0\ncut 1\ncut 2\nkeep 3";
const CONTENT: &[u8] = b"parent capture";

const CUT: [LineRange; 1] = [LineRange {
    stream: Stream::Stdout,
    start: 1,
    end: 3,
}];

/// A result line as the client writes one, delivering `stdout`.
fn result_line(call: &str, stdout: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": call, "content": stdout }]},
        "toolUseResult": { "stdout": stdout, "stderr": "", "interrupted": false },
    })
    .to_string()
}

/// A fold line as a rewriter might word one; the words are the test's own.
fn fold_line(n: u32) -> String {
    format!("[rift fold: {n} lines elided]")
}

fn folded() -> String {
    render_folded(
        ResultText {
            stdout: OUTPUT,
            stderr: "",
        },
        &CUT,
        fold_line,
    )
    .unwrap()
    .0
}

fn held() -> String {
    "keep 0\n[rift held: src/lib.rs@abc lines 2-3]\nkeep 3".to_owned()
}

fn original(id: &str, form: RewriteForm, line: u64) -> ArchivedOriginal {
    let packs = rift_shadow::redact::RulePackSet::builtin();
    let raw = rift_core::SecretBytes::new(original_record_json(OUTPUT, "", false));
    ArchivedOriginal {
        tool_use_id: id.into(),
        agent_id: None,
        form: Some(form),
        replaced: CUT.to_vec(),
        line: Some(line),
        rewritten: Some(Rewritten::Delivered(form)),
        state: OriginalState::Sealed,
        body: Some(
            rift_shadow::transcript::sealed_original(&raw, &packs)
                .unwrap()
                .into(),
        ),
    }
}

fn sealed(raw: &str, originals: Vec<ArchivedOriginal>) -> ArchivedTranscript {
    let packs = rift_shadow::redact::RulePackSet::builtin();
    let raw = rift_core::SecretBytes::new(raw.as_bytes().to_vec());
    let (mut pair, _) = rift_shadow::archived_transcript(&raw, 0, &packs).unwrap();
    pair.originals = originals;
    pair
}

/// A pair's metadata document, as the writer records one on a task.
fn metadata_of(tool_use_id: &str, pair: &ArchivedTranscript) -> Vec<u8> {
    ArchivedTask {
        tool_use_id: tool_use_id.into(),
        agent_id: "a1".into(),
        started_at_ms: 1,
        ended_at_ms: 2,
        input_tokens: None,
        output_tokens: None,
        transcript: Some(pair.clone()),
        provenance: None,
    }
    .metadata_json()
}

/// What a fixture shard is built from: the delivered text of the folded
/// call, an extra session line, and an edit to apply to the stored session
/// original.
struct Spec {
    delivered: String,
    extra: Option<String>,
    unlisted: bool,
    tamper: fn(&mut Vec<u8>),
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            delivered: folded(),
            extra: None,
            unlisted: false,
            tamper: |_| (),
        }
    }
}

/// The two transcripts of a fixture shard, each with its originals.
struct Parts {
    own: ArchivedTranscript,
    inner: ArchivedTranscript,
}

/// The session transcript records a folded and a held call, its subagent a
/// folded one, each original sealed beside its transcript.
fn parts(spec: &Spec) -> Parts {
    let mut lines = vec![
        result_line(FOLDED, &spec.delivered),
        result_line(HELD, &held()),
    ];
    lines.extend(spec.extra.clone());
    let mut own_originals = vec![
        original(FOLDED, RewriteForm::FoldedOutput, 0),
        original(HELD, RewriteForm::HeldOutputSubstitution, 1),
    ];
    if spec.unlisted {
        own_originals.clear();
    }
    Parts {
        own: sealed(&(lines.join("\n") + "\n"), own_originals),
        inner: sealed(
            &format!("{}\n", result_line(INNER, &folded())),
            vec![original(INNER, RewriteForm::FoldedOutput, 0)],
        ),
    }
}

/// Each sealed original of a transcript: its call and its bytes.
fn bodies(pair: &ArchivedTranscript) -> Vec<(String, Vec<u8>)> {
    pair.originals
        .iter()
        .map(|o| {
            (
                o.tool_use_id.clone(),
                o.body.clone().unwrap().as_slice().to_vec(),
            )
        })
        .collect()
}

/// One sealed original's leaf view.
fn leaf<'a>(task: Option<&'a str>, (id, body): &'a (String, Vec<u8>)) -> OriginalLeafBytes<'a> {
    OriginalLeafBytes {
        task,
        tool_use_id: id,
        body,
    }
}

/// A shard as the archive writes one over [`parts`]; `spec.tamper` edits the
/// first stored session original.
fn originals_shard(spec: &Spec) -> MemorySubstrate {
    let Parts { own, inner } = parts(spec);
    let task_meta = metadata_of(TASK, &inner);
    let (own_bodies, inner_bodies) = (bodies(&own), bodies(&inner));
    let report = fixture_report();
    let task_leaves = [TaskLeafBytes {
        tool_use_id: TASK,
        metadata: &task_meta,
        transcript_compact: Some(inner.compact.as_slice()),
        transcript_full: Some(inner.full.as_slice()),
    }];
    let mut original_leaves: Vec<OriginalLeafBytes<'_>> =
        own_bodies.iter().map(|b| leaf(None, b)).collect();
    original_leaves.extend(inner_bodies.iter().map(|b| leaf(Some(TASK), b)));
    let leaves = ShardLeafBytes {
        boundary_gaps: None,
        content_redacted: CONTENT,
        redaction_report: report.as_bytes(),
        transcript_compact: Some(own.compact.as_slice()),
        transcript_full: Some(own.full.as_slice()),
        tasks: &task_leaves,
        originals: &original_leaves,
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
    let own_doc: serde_json::Value = serde_json::from_slice(&metadata_of("x", &own)).unwrap();
    let manifest = own_doc
        .get("originals")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let metadata = format!(
        r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"strategy":"manual-commit","merkle_root":"{root}","sessions":["{FIXTURE_SESSION}"],"originals":{manifest},"tasks":["{TASK}"]}}"#
    );
    let mut sub = MemorySubstrate::local()
        .with_shard(shard(&id, None, 1))
        .with_artifact(&id, "redaction_report.json", report.as_bytes())
        .with_artifact(&id, "content.redacted", CONTENT)
        .with_artifact(&id, "metadata.json", metadata.as_bytes())
        .with_artifact(&id, "transcript.compact.jsonl", own.compact.as_slice())
        .with_artifact(&id, "transcript.jsonl", own.full.as_slice())
        .with_artifact(&id, &format!("tasks/{TASK}/metadata.json"), &task_meta)
        .with_artifact(
            &id,
            &format!("tasks/{TASK}/transcript.compact.jsonl"),
            inner.compact.as_slice(),
        )
        .with_artifact(
            &id,
            &format!("tasks/{TASK}/transcript.jsonl"),
            inner.full.as_slice(),
        );
    for (call, body) in &inner_bodies {
        sub = sub.with_artifact(
            &id,
            &format!("tasks/{TASK}/tool-originals/{call}.json"),
            body,
        );
    }
    for (i, (call, mut stored)) in own_bodies.into_iter().enumerate() {
        if i == 0 {
            (spec.tamper)(&mut stored);
        }
        sub = sub.with_artifact(&id, &format!("tool-originals/{call}.json"), &stored);
    }
    sub
}

/// An honest shard verifies; its held substitution is counted unchecked,
/// never passed.
#[test]
fn an_honest_shard_with_originals_verifies_and_counts_held_ranges() {
    let out = shard_passes(&originals_shard(&Spec::default())).unwrap();
    assert!(matches!(out.worst, Verdict::Ok), "{:?}", out.worst);
    assert_eq!(out.held_unchecked, 1);
}

/// One byte changed in a sealed original fails verify, named by its path.
#[test]
fn a_one_byte_tamper_in_an_original_names_the_file() {
    let spec = Spec {
        tamper: |b| b[0] ^= 1,
        ..Spec::default()
    };
    let out = shard_passes(&originals_shard(&spec)).unwrap();
    assert!(
        matches!(&out.worst, Verdict::Tamper(m) if m.contains(&format!("tool-originals/{FOLDED}.json"))),
        "{:?}",
        out.worst
    );
}

/// A delivered text that is not the form's rendering of its original fails
/// verify, naming the original and the transcript line.
#[test]
fn a_delivered_text_that_is_not_the_rendering_fails() {
    let spec = Spec {
        delivered: folded().replace("keep 3", "kept 3"),
        ..Spec::default()
    };
    let out = shard_passes(&originals_shard(&spec)).unwrap();
    assert!(
        matches!(&out.worst, Verdict::Tamper(m)
            if m.contains(&format!("tool-originals/{FOLDED}.json")) && m.contains("transcript.jsonl line 0")),
        "{:?}",
        out.worst
    );
}

/// The fold line's words are the rewriter's: verify holds the line to its
/// shape and its count, and every kept line to the original.
#[test]
fn a_fold_verifies_by_shape_and_count_whatever_its_words() {
    let fold = |line: &str| format!("keep 0\n{line}\nkeep 3");
    for honest in [
        "[rift fold: 2 more lines like the one above]",
        "[rift fold: 2]",
        "[rift fold: 2 lines [cut] here]",
    ] {
        let spec = Spec {
            delivered: fold(honest),
            ..Spec::default()
        };
        let out = shard_passes(&originals_shard(&spec)).unwrap();
        assert!(
            matches!(out.worst, Verdict::Ok),
            "{honest}: {:?}",
            out.worst
        );
    }
    for forged in [
        fold("[rift fold: 3 more lines like the one above]"),
        fold("[rift fold: 1 line]"),
        fold("[rift fold:2 lines]"),
        fold("(rift fold: 2 lines]"),
        fold("[rift fold: 2 lines] "),
        fold("[rift fold: 2 lines"),
        fold("[rift fold: two lines]"),
        "keep 0\n[rift fold: 2 lines elided]\nkeep 3 altered".to_owned(),
    ] {
        let spec = Spec {
            delivered: forged.clone(),
            ..Spec::default()
        };
        let out = shard_passes(&originals_shard(&spec)).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m)
                if m.contains(&format!("tool-originals/{FOLDED}.json")) && m.contains("transcript.jsonl line 0")),
            "{forged}: {:?}",
            out.worst
        );
    }
}

/// A Rift marker delivered on a line no manifest entry names is a rewrite
/// whose original was never sealed.
#[test]
fn a_marker_with_no_sealed_original_is_original_missing() {
    let spec = Spec {
        extra: Some(result_line(
            "toolu_unsealed",
            &format!("a\n{}", fold_line(4)),
        )),
        ..Spec::default()
    };
    let out = shard_passes(&originals_shard(&spec)).unwrap();
    assert!(
        matches!(&out.worst, Verdict::Structure(m) if m.contains("line 2") && m.contains("original_missing")),
        "{:?}",
        out.worst
    );
    let unlisted = Spec {
        unlisted: true,
        ..Spec::default()
    };
    let out = shard_passes(&originals_shard(&unlisted)).unwrap();
    assert!(
        matches!(&out.worst, Verdict::Structure(m) if m.contains("original_missing")),
        "{:?}",
        out.worst
    );
}

/// Every entry naming a call is a claim; only those with a digest or size
/// claim a body; a word outside its vocabulary is refused.
#[test]
fn claims_read_every_entry_and_words_stay_in_their_vocabularies() {
    let doc = serde_json::json!({ "originals": [
        { "tool_use_id": "a", "bytes": 3, "sha256": "x", "line": 2, "form": "folded_output" },
        { "tool_use_id": "b", "original": "original_unsealed" },
        { "bytes": 1 },
    ]});
    let got = claims(Some(&doc));
    assert_eq!(got.len(), 2);
    assert!(got[0].claims_body() && !got[1].claims_body());
    assert_eq!(got[0].line, Some(2));
    assert!(claims(None).is_empty());
    let mut odd = got[1].clone();
    odd.state = Some("original_lost".into());
    let bytes = OriginalBytes {
        task: None,
        claim: odd,
        body: None,
    };
    assert!(
        matches!(digest_finding("cp", &bytes), Some(Verdict::Tamper(m)) if m.contains("vocabulary"))
    );
}

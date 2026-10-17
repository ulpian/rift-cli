//! The passes that bind a shard to its own path: the checkpoint id, and the
//! name and rollup a nested task subtree adds beyond its leaves.
//!
//! Separate because these are the checks a tamperer cannot satisfy by editing
//! a file — the shard's path and its subtree names are not in any artifact —
//! and they are the ones a review comes back to.

use super::artifacts::{NativeArtifacts, ShardHeader, TaskArtifactBytes};
use super::verdict::Verdict;

/// **The checkpoint-id pass** — the binding a tamperer cannot recompute.
///
/// The recorded Merkle root lives in a file the tamperer also controls, so
/// editing `content.redacted` and recomputing `metadata.json`'s `merkle_root`
/// passed every strict check. The shard's *path*
/// is not in any file, so re-deriving it is the one binding writing a file cannot
/// fix, and edited content stops addressing the shard it sits in.
///
/// **Which derivation applies is the shard's declared era.** From epoch 3 the id
/// is `sha256(session ‖ \0 ‖ sha256_hex(root))` over the root RE-DERIVED here
/// from the artifacts — so every content artifact, `full.jsonl` and the
/// transcript included, is bound by the path. Below that the id bound only
/// `content.redacted` and `tool_calls.jsonl`, and both of that era's derivations
/// are accepted: 102 of this repository's 103 shards match one and one matches
/// the other, so a checker that insisted on a single formula would call an honest
/// shard tampered.
///
/// Runs on **every shard of the native dialect, whatever era it declares** —
/// including one that declares none: a deleted `canon_version` parses as epoch
/// 0, whose accepted set is the pre-root rules, and no era's rule grants an id
/// to edited bytes. That closes the label-stripping escape
/// the same way the epoch-1 relabel was closed: the shard's path is evidence
/// of the rule that minted it.
pub(super) fn checkpoint_id_pass(
    id: &str,
    sessions: &[String],
    canon_version: u32,
    native: &NativeArtifacts,
    report: Option<&[u8]>,
) -> Option<Verdict> {
    let (Some(content), Some(report)) = (native.content.as_deref(), report) else {
        // Every native shard writes both; the absence of either is a removal of
        // bytes the id addresses, not an incompleteness.
        return Some(Verdict::Tamper(format!(
            "{id}: an artifact its checkpoint id addresses is missing"
        )));
    };
    let task_leaves = native.task_leaves();
    let session_meta_leaves = native.session_meta_leaves();
    let spill_leaves = super::spills::leaves(&native.spills);
    let original_leaves = super::originals::leaves(&native.originals);
    let candidates = rift_shadow::condense::checkpoint_ids_for_shard(
        sessions,
        canon_version,
        &native.leaves(
            content,
            report,
            &task_leaves,
            &session_meta_leaves,
            &spill_leaves,
            &original_leaves,
        ),
    );
    // An empty candidate set is `checkpoint_ids_for_shard`'s "no era rule
    // grants this shard an id at all" — a session set the rules refuse
    // (none, or NUL-carrying), or a multi-session shard declaring a
    // single-session era. No writer minted one, so falling through to the
    // mismatch verdict below is the honest outcome, not an accident.
    if candidates.iter().any(|c| c == id) {
        None
    } else {
        Some(Verdict::Tamper(format!(
            "{id}: the checkpoint id does not address this shard's own bytes"
        )))
    }
}

/// The task-subtree pass: the two claims a nested artifact
/// adds beyond its leaves.
///
/// **Name/bytes agreement** — the subtree name is the one byte sequence of a
/// task that is not itself a leaf; the metadata's `tool_use_id` is, so a
/// renamed subtree is exactly a name/bytes disagreement. No writer mints one:
/// both come from the same `ArchivedTask`.
///
/// **The rollup recompute** — the parent's recorded `task_tokens` must equal
/// the sum over the task subtrees' own (leaf-bound) metadata, including in
/// absence: a stripped rollup hides spend, an invented one claims spend no
/// task reported. The parent metadata is not a leaf, which is exactly why the
/// recompute exists (same treatment as the recorded Merkle root).
pub(super) fn task_subtree_pass(
    id: &str,
    header: &ShardHeader,
    native: &NativeArtifacts,
) -> Vec<Verdict> {
    let mut found = Vec::new();
    let mut task_token_sum: Option<(u64, u64)> = None;
    for t in &native.tasks {
        let doc = serde_json::from_slice::<serde_json::Value>(&t.metadata).ok();
        let claimed = doc.as_ref().and_then(|v| {
            v.get("tool_use_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
        if claimed.as_deref().is_some_and(|c| c != t.tool_use_id) {
            found.push(Verdict::Tamper(format!(
                "{id}: task subtree '{}' carries metadata naming a different tool-use id",
                t.tool_use_id
            )));
        }
        found.extend(task_transcript_digest(id, t, doc.as_ref()));
        let input = doc
            .as_ref()
            .and_then(|v| v.get("input_tokens"))
            .and_then(serde_json::Value::as_u64);
        let output = doc
            .as_ref()
            .and_then(|v| v.get("output_tokens"))
            .and_then(serde_json::Value::as_u64);
        if input.is_some() || output.is_some() {
            let (i, o) = task_token_sum.unwrap_or((0, 0));
            task_token_sum = Some((
                i.saturating_add(input.unwrap_or(0)),
                o.saturating_add(output.unwrap_or(0)),
            ));
        }
    }
    if header.task_tokens != task_token_sum {
        found.push(Verdict::Tamper(format!(
            "{id}: the recorded task-token rollup does not match the sum of the task subtrees"
        )));
    }
    found
}

/// A collected subagent transcript's own binding: the `bytes` and `sha256`
/// its metadata records must describe the `transcript.jsonl` beside it, and a
/// metadata that records them over a subtree with no transcript is a claim
/// about bytes the shard does not hold.
///
/// The Merkle root already refuses an edited subtree; this pass is what names
/// which subtree, so a tampered subagent transcript is reported as itself
/// rather than only as a checkpoint whose id no longer addresses it. A task
/// written before collection records neither member and owes nothing here.
fn task_transcript_digest(
    id: &str,
    t: &TaskArtifactBytes,
    doc: Option<&serde_json::Value>,
) -> Option<Verdict> {
    let recorded = doc
        .and_then(|v| v.get("sha256"))
        .and_then(serde_json::Value::as_str);
    let size = doc
        .and_then(|v| v.get("bytes"))
        .and_then(serde_json::Value::as_u64);
    if recorded.is_none() && size.is_none() {
        return None;
    }
    let held = t.transcript_full.as_deref().unwrap_or_default();
    let agrees = t.transcript_full.is_some()
        && recorded == Some(rift_shadow::sha256_hex(&[held]).as_str())
        && size == u64::try_from(held.len()).ok();
    (!agrees).then(|| {
        Verdict::Tamper(format!(
            "{id}: task subtree '{}' transcript.jsonl does not match its recorded sha256",
            t.tool_use_id
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_substrate::{MemorySubstrate, SubstrateReader};

    use crate::verify::merkle::{strict_merkle_pass, VACUOUS_MERKLE_CANON_VERSION};
    use crate::verify::passes::shard_passes;
    use crate::verify::tests::{
        era_shard, era_shard_for, fixture_report, native_id, root_bound_shard, shard,
        task_bound_shard, writer_root, FIXTURE_SESSION, VACUOUS_ERA_ROOT,
    };

    fn content_artifacts(content: Option<Vec<u8>>, full: Option<Vec<u8>>) -> NativeArtifacts {
        NativeArtifacts {
            recorded_merkle_leaf: None,
            boundary_gaps: None,
            session_meta_presence: std::collections::BTreeSet::new(),
            content,
            full,
            tool_calls: None,
            serve_audit: None,
            sessions: None,
            usage: None,
            window_journal: None,
            session_worktrees: None,
            tool_results: None,
            transcript_compact: None,
            transcript_full: None,
            tasks: Vec::new(),
            session_metas: Vec::new(),
            digest_failures: None,
            digest_intent: None,
            digest_log: None,
            digest_shape: None,
            spills: Vec::new(),
            originals: Vec::new(),
        }
    }

    /// **PLANTED — the attack the strict Merkle pass could not see.**
    ///
    /// The recorded root lives in `metadata.json`, which the tamperer is already
    /// editing, so editing `content.redacted` AND recomputing the root passed
    /// every check this repository had. The shard's PATH is the binding they
    /// cannot recompute: it is `sha256(session ‖ … ‖ sha256_hex(capture) ‖ …)`.
    ///
    /// The plant is the whole attack, not a proxy for it: the root here is the
    /// root of the EDITED bytes, so the strict pass is satisfied and only the
    /// checkpoint-id pass can red this.
    #[test]
    fn editing_the_capture_and_recomputing_the_root_is_still_tamper() {
        let id = native_id(b"the redacted capture");
        let edited = b"the redacted capture, edited".as_slice();
        let sub = era_shard(
            MemorySubstrate::local(),
            &id,
            2,
            &writer_root(edited), // the tamperer did their arithmetic
            edited,
        );
        // The strict Merkle pass is genuinely satisfied — assert that, or the
        // test could be passing for the old reason and prove nothing new.
        assert!(
            strict_merkle_pass(
                &sub,
                &id,
                &content_artifacts(sub.read_artifact(&id, "content.redacted").unwrap(), None),
                sub.read_artifact(&id, "redaction_report.json")
                    .unwrap()
                    .as_deref(),
            )
            .unwrap()
            .is_none(),
            "the plant must get past the root check, or it is not this attack"
        );
        match shard_passes(&sub).unwrap().worst {
            Verdict::Tamper(d) => {
                assert!(d.contains(&id), "the finding names the shard: {d}");
                assert!(d.contains("does not address"), "and says what broke: {d}");
            }
            other => panic!("a recomputed-root forgery must be TAMPER, got {other:?}"),
        }
    }

    /// **PLANTED — relabelling the same forgery into the vacuous era.**
    ///
    /// The follow-up to the path-binding plant above: an edited shard relabelled
    /// `canon_version: 1` with the era's constant escaped the strict pass
    /// fully and merely moved a counter. It no longer does, because the
    /// checkpoint-id pass runs on every native era — that gate is the reason it
    /// is not gated on [`STRICT_MERKLE_MIN_CANON_VERSION`] like the root pass.
    #[test]
    fn relabelling_a_forged_shard_into_the_vacuous_era_does_not_hide_it() {
        let id = native_id(b"the redacted capture");
        let sub = era_shard(
            MemorySubstrate::local(),
            &id,
            VACUOUS_MERKLE_CANON_VERSION,
            VACUOUS_ERA_ROOT,
            b"the redacted capture, edited",
        );
        let out = shard_passes(&sub).unwrap();
        assert_eq!(out.vacuous_era, 1, "the relabel did take effect…");
        match out.worst {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("…and it still did not help: expected TAMPER, got {other:?}"),
        }
    }

    /// **The stripped-label hole, CLOSED — the flip of the test that held it open.**
    ///
    /// Deleting `canon_version` outright used to move a native shard into the
    /// pre-Rift dialect's exemption, and every native pass declined (this test
    /// asserted `Verdict::Ok`, counted as `pre_rift`, so the hole was
    /// enumerated in code). The exemption is now the DIALECT's — the ref the
    /// shard was read from, which editing its bytes cannot change — so the
    /// same shard stays in the native passes at epoch 0, and no era's id rule
    /// grants a path to edited bytes. What unblocked the flip: the
    /// corrupt-shard fixture stopped being a made-up id with no era on the
    /// native ref and became a real epoch-1 shard minting its own id.
    #[test]
    fn deleting_the_canon_version_no_longer_escapes_the_native_passes() {
        let id = native_id(b"the redacted capture");
        let report = format!(
            r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        );
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", b"the redacted capture, edited")
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"strategy":"manual-commit","merkle_root":"{VACUOUS_ERA_ROOT}","sessions":["{FIXTURE_SESSION}"]}}"#
                )
                .as_bytes(),
            );
        let out = shard_passes(&sub).unwrap();
        match out.worst {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("a label-stripped native shard must fail its id pass, got {other:?}"),
        }
        assert_eq!(
            out.pre_rift, 0,
            "the exemption is the legacy DIALECT's; a stripped label does not buy it"
        );
    }

    /// **Its epoch-3 twin, CLOSED by the same gate.** Era 3 made that hole one
    /// epoch wider: an epoch-3 shard whose `canon_version` was
    /// DELETED landed in the pre-Rift exemption and escaped every native pass —
    /// with more to hide, because only the native passes bind `full.jsonl`.
    /// The shard here is byte-honest; only the label is stripped. Epoch 0's
    /// accepted set is the pre-root rules, a root-bound path satisfies neither,
    /// and the dialect gate no longer lets it decline the question.
    #[test]
    fn deleting_a_root_bound_shards_canon_version_is_still_tamper() {
        let content = b"the redacted capture".as_slice();
        let full = br#"{"kind":"session_start","turn":0}"#.as_slice();
        let (_, id) = root_bound_shard(content, full);
        let report = fixture_report();
        let root = rift_shadow::backend::shard_merkle_root(&rift_shadow::backend::ShardLeafBytes {
            boundary_gaps: None,
            content_redacted: content,
            full_jsonl: Some(full),
            redaction_report: report.as_bytes(),
            serve_audit: None,
            sessions: None,
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
        })
        .to_hex();
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, "full.jsonl", full)
            .with_artifact(
                &id,
                "metadata.json",
                // No canon_version — the label deleted outright.
                format!(
                    r#"{{"strategy":"manual-commit","merkle_root":"{root}","sessions":["{FIXTURE_SESSION}"]}}"#
                )
                .as_bytes(),
            );
        match shard_passes(&sub).unwrap().worst {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("a label-stripped root-bound shard must be TAMPER, got {other:?}"),
        }
    }

    /// **The task subtree is verified through the same three bindings as every
    /// leaf, plus the one thing a nested artifact adds: its NAME.** (1) An
    /// honest task-carrying shard passes every pass. (2) Hiding the task from
    /// the metadata list and recomputing the recorded root satisfies the
    /// strict Merkle pass — the tamperer controls both files — and reds on the
    /// id pass, because the path still addresses the task-carrying root.
    /// (3) Renaming the subtree (list + tree key) while keeping its bytes
    /// leaves root and id green — the leaves never moved — and reds on the
    /// name/bytes agreement check, which exists for exactly this edit.
    #[test]
    fn a_hidden_or_renamed_task_subtree_is_tamper() {
        let content = b"parent capture".as_slice();
        let (sub, id, task_meta) = task_bound_shard(content, "toolu_v");
        let out = shard_passes(&sub).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "honest: {:?}", out.worst);
        assert_eq!(out.checked, 1);

        // (2) The hide: metadata drops the task list and records the TASKLESS
        // root (the tamperer's arithmetic done for them).
        let taskless_root = writer_root(content);
        let hidden = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", fixture_report().as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, "tasks/toolu_v/metadata.json", &task_meta)
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":3,"strategy":"manual-commit","merkle_root":"{taskless_root}","sessions":["{FIXTURE_SESSION}"]}}"#
                )
                .as_bytes(),
            );
        let out = shard_passes(&hidden).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("does not address")),
            "a hidden task must fail the id pass: {:?}",
            out.worst
        );

        // (3) The rename: same bytes, listed and stored under another name.
        let (orig, _, _) = task_bound_shard(content, "toolu_v");
        let root = {
            // Recorded root/id are unchanged — re-list under the new name only.
            let meta = orig.read_artifact(&id, "metadata.json").unwrap().unwrap();
            let v: serde_json::Value = serde_json::from_slice(&meta).unwrap();
            v.get("merkle_root")
                .and_then(serde_json::Value::as_str)
                .unwrap()
                .to_owned()
        };
        let renamed = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", fixture_report().as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, "tasks/toolu_w/metadata.json", &task_meta)
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":3,"strategy":"manual-commit","merkle_root":"{root}","sessions":["{FIXTURE_SESSION}"],"tasks":["toolu_w"]}}"#
                )
                .as_bytes(),
            );
        let out = shard_passes(&renamed).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("different tool-use id")),
            "a renamed subtree is a name/bytes disagreement: {:?}",
            out.worst
        );
    }

    /// A task-carrying epoch-3 shard whose parent metadata declares `rollup`
    /// verbatim — the tamperer's own edit, since the rollup is the one claim
    /// in that file no leaf commits to. The task subtree reports 120/34.
    fn task_rollup_shard(rollup: &str) -> MemorySubstrate {
        use rift_shadow::backend::{
            shard_merkle_root, ShardLeafBytes, TaskLeafBytes, ROOT_BOUND_ID_MIN_CANON_VERSION,
        };
        let content = b"parent capture".as_slice();
        let report = fixture_report();
        let task_meta = rift_shadow::backend::ArchivedTask {
            tool_use_id: "toolu_t".to_owned(),
            agent_id: "explore".to_owned(),
            started_at_ms: 4,
            ended_at_ms: 8,
            input_tokens: Some(120),
            output_tokens: Some(34),
            transcript: None,
            provenance: None,
        }
        .metadata_json();
        let task_leaves = [TaskLeafBytes {
            tool_use_id: "toolu_t",
            metadata: &task_meta,
            transcript_compact: None,
            transcript_full: None,
        }];
        let leaves = ShardLeafBytes {
            boundary_gaps: None,
            content_redacted: content,
            full_jsonl: None,
            redaction_report: report.as_bytes(),
            serve_audit: None,
            sessions: None,
            usage: None,
            window_journal: None,
            session_worktrees: None,
            tool_results: None,
            tool_calls: None,
            transcript_compact: None,
            transcript_full: None,
            tasks: &task_leaves,
            spills: &[],
            originals: &[],
            session_metas: &[],
            digests: rift_shadow::backend::DigestLeafBytes::default(),
        };
        let root = shard_merkle_root(&leaves).to_hex();
        let id = rift_shadow::condense::checkpoint_ids_for_shard(
            &[FIXTURE_SESSION.to_owned()],
            ROOT_BOUND_ID_MIN_CANON_VERSION,
            &leaves,
        )
        .remove(0);
        MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, "tasks/toolu_t/metadata.json", &task_meta)
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"strategy":"manual-commit","merkle_root":"{root}","sessions":["{FIXTURE_SESSION}"],"tasks":["toolu_t"]{rollup}}}"#
                )
                .as_bytes(),
            )
    }

    /// The recorded rollup equals the sum of the task subtrees, so the shard
    /// passes — the baseline the two forgeries below are measured against.
    #[test]
    fn an_honest_task_token_rollup_passes() {
        let honest = task_rollup_shard(r#","task_tokens":{"input_tokens":120,"output_tokens":34}"#);
        let out = shard_passes(&honest).unwrap();
        assert!(
            matches!(out.worst, Verdict::Ok),
            "honest rollup: {:?}",
            out.worst
        );
    }

    /// **The rollup is tamper-evident though the parent metadata is not a
    /// leaf** — same treatment as the recorded Merkle root: verify re-sums the
    /// task subtrees' own (leaf-bound) metadata against the recorded
    /// `task_tokens`, so stripping the rollup (hiding spend) reds without a
    /// single leaf byte moving, and root + id stay green throughout, which is
    /// exactly why the recompute must exist.
    #[test]
    fn a_stripped_task_token_rollup_is_tamper() {
        let stripped = task_rollup_shard("");
        let out = shard_passes(&stripped).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("task-token rollup")),
            "a stripped rollup must red: {:?}",
            out.worst
        );
    }

    /// The other direction: the parent claims a sum the task subtrees do not
    /// report. Understating spend is the same finding as hiding it.
    #[test]
    fn an_understated_task_token_rollup_is_tamper() {
        let understated =
            task_rollup_shard(r#","task_tokens":{"input_tokens":1,"output_tokens":34}"#);
        let out = shard_passes(&understated).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("task-token rollup")),
            "an understated rollup must red: {:?}",
            out.worst
        );
    }

    /// One shard of the era that binds its id to its root, carrying a
    /// `digest.shape.json` leaf and a `tool_calls.jsonl` the digest describes.
    ///
    /// The returned builder takes the shape bytes and the root the metadata
    /// records, so a test plants an edit and decides, separately, whether the
    /// tamperer bothered to recompute the root.
    struct DigestShardPlant {
        id: String,
        content: &'static [u8],
        report: String,
        tool_calls: Vec<u8>,
        shape: Vec<u8>,
        root: String,
    }

    impl DigestShardPlant {
        /// The honest shard, and the pieces an edit is planted over.
        fn new() -> Self {
            use rift_shadow::backend::{
                shard_merkle_root, DigestLeafBytes, ShardLeafBytes, ROOT_BOUND_ID_MIN_CANON_VERSION,
            };
            let content = b"parent capture".as_slice();
            let report = fixture_report();
            let shape = rift_shadow::digest::shape_json(&[read_fact()]).unwrap();
            let tool_calls = rift_shadow::backend::tool_calls_jsonl_bytes(&[read_fact()]).unwrap();
            let leaves = ShardLeafBytes {
                boundary_gaps: None,
                content_redacted: content,
                full_jsonl: None,
                redaction_report: report.as_bytes(),
                serve_audit: None,
                sessions: None,
                usage: None,
                window_journal: None,
                session_worktrees: None,
                tool_results: None,
                tool_calls: Some(&tool_calls),
                transcript_compact: None,
                transcript_full: None,
                tasks: &[],
                spills: &[],
                originals: &[],
                session_metas: &[],
                digests: DigestLeafBytes {
                    shape: Some(&shape),
                    ..DigestLeafBytes::default()
                },
            };
            let root = shard_merkle_root(&leaves).to_hex();
            let id = rift_shadow::condense::checkpoint_ids_for_shard(
                &[FIXTURE_SESSION.to_owned()],
                ROOT_BOUND_ID_MIN_CANON_VERSION,
                &leaves,
            )
            .remove(0);
            Self {
                id,
                content,
                report,
                tool_calls,
                shape,
                root,
            }
        }

        /// The shard as it stands, with `shape_bytes` in the digest leaf and
        /// `recorded_root` in the metadata the tamperer also controls.
        fn with(&self, shape_bytes: &[u8], recorded_root: &str) -> MemorySubstrate {
            use rift_shadow::backend::ROOT_BOUND_ID_MIN_CANON_VERSION;
            let id = &self.id;
            MemorySubstrate::local()
                .with_shard(shard(id, None, 1))
                .with_artifact(id, "redaction_report.json", self.report.as_bytes())
                .with_artifact(id, "content.redacted", self.content)
                .with_artifact(id, "tool_calls.jsonl", &self.tool_calls)
                .with_artifact(id, rift_shadow::digest::DIGEST_SHAPE_NAME, shape_bytes)
                .with_artifact(
                    id,
                    "metadata.json",
                    format!(
                        r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"strategy":"manual-commit","merkle_root":"{recorded_root}","sessions":["{FIXTURE_SESSION}"]}}"#
                    )
                    .as_bytes(),
                )
        }

        /// The root a tamperer would record after replacing the digest leaf
        /// with `shape_bytes` — their own arithmetic, done for them.
        fn root_over(&self, shape_bytes: &[u8]) -> String {
            use rift_shadow::backend::{shard_merkle_root, DigestLeafBytes, ShardLeafBytes};
            shard_merkle_root(&ShardLeafBytes {
                boundary_gaps: None,
                content_redacted: self.content,
                full_jsonl: None,
                redaction_report: self.report.as_bytes(),
                serve_audit: None,
                sessions: None,
                usage: None,
                window_journal: None,
                session_worktrees: None,
                tool_results: None,
                tool_calls: Some(&self.tool_calls),
                transcript_compact: None,
                transcript_full: None,
                tasks: &[],
                spills: &[],
                originals: &[],
                session_metas: &[],
                digests: DigestLeafBytes {
                    shape: Some(shape_bytes),
                    ..DigestLeafBytes::default()
                },
            })
            .to_hex()
        }
    }

    /// The one read fact both digest artifacts of the plant describe.
    fn read_fact() -> rift_shadow::backend::ArchivedReadFact {
        rift_shadow::backend::ArchivedReadFact {
            tool_use_id: "t1".to_owned(),
            tool_name: "Read".to_owned(),
            path: "src/a.rs".to_owned(),
            content_sha256: "aa".to_owned(),
            input_tokens: None,
            at: None,
            turn: None,
            blob_oid: None,
            tokenizer: None,
        }
    }

    /// A digest artifact is an ordinary leaf, so an untouched shard carrying
    /// one passes — the baseline the two edits below are measured against.
    #[test]
    fn a_digest_carrying_shard_verifies() {
        let plant = DigestShardPlant::new();
        let out = shard_passes(&plant.with(&plant.shape, &plant.root)).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "honest: {:?}", out.worst);
    }

    /// **A stored digest artifact is a leaf like any other**: editing
    /// `digest.shape.json` in place — no recomputed root, the
    /// lazy tamper — reds the strict Merkle pass.
    #[test]
    fn an_edited_digest_artifact_is_tamper() {
        let plant = DigestShardPlant::new();
        let edited = br#"{"by_ext":{"rs":999},"files":[],"schema_version":1}"#;
        let out = shard_passes(&plant.with(edited, &plant.root)).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("does not match the root")),
            "an in-place digest edit reds the strict pass: {:?}",
            out.worst
        );
    }

    /// The same edit with the root recomputed: the strict pass is satisfied
    /// and the path stops addressing the shard, so the id pass reds instead.
    #[test]
    fn an_edited_digest_artifact_with_a_recomputed_root_is_still_tamper() {
        let plant = DigestShardPlant::new();
        let edited = br#"{"by_ext":{"rs":999},"files":[],"schema_version":1}"#;
        let recomputed = plant.root_over(edited);
        let out = shard_passes(&plant.with(edited, &recomputed)).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("does not address")),
            "a recomputed-root digest edit reds the id pass: {:?}",
            out.worst
        );
    }

    /// **What binding the id to the root actually buys, shown on the artifact
    /// that had no binding at all.**
    ///
    /// Until epoch 3 the id covered `content.redacted` and `tool_calls.jsonl`.
    /// `full.jsonl` was covered by the recorded Merkle root and by nothing else —
    /// and the recorded root lives in `metadata.json`, which the tamperer is
    /// already editing. So editing an event stream and recomputing the root was a
    /// consistent forgery **at the shard's own path**.
    ///
    /// The plant does the tamperer's arithmetic: the recorded root here is the
    /// root of the EDITED bytes, so the strict Merkle pass is satisfied and only
    /// the id pass can red this. That is asserted, not assumed.
    #[test]
    fn editing_a_root_bound_shards_event_stream_is_still_tamper() {
        use rift_shadow::backend::ROOT_BOUND_ID_MIN_CANON_VERSION;
        let content = b"the redacted capture".as_slice();
        let full = br#"{"kind":"session_start","turn":0}"#.as_slice();
        let (honest, id) = root_bound_shard(content, full);
        let out = shard_passes(&honest).unwrap();
        assert!(
            matches!(out.worst, Verdict::Ok),
            "the honest shard must verify first, or the tamper case proves nothing: {:?}",
            out.worst
        );
        assert_eq!(out.vacuous_era, 0);

        // Now edit the event stream and recompute the recorded root over the
        // edited bytes — everything inside the shard is consistent.
        let edited = br#"{"kind":"session_start","turn":0,"forged":true}"#.as_slice();
        let report = fixture_report();
        let forged_root =
            rift_shadow::backend::shard_merkle_root(&rift_shadow::backend::ShardLeafBytes {
                boundary_gaps: None,
                content_redacted: content,
                full_jsonl: Some(edited),
                redaction_report: report.as_bytes(),
                ..rift_shadow::backend::ShardLeafBytes::default()
            })
            .to_hex();
        let forged = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, "full.jsonl", edited)
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"strategy":"manual-commit","merkle_root":"{forged_root}","sessions":["{FIXTURE_SESSION}"]}}"#
                )
                .as_bytes(),
            );
        assert!(
            strict_merkle_pass(
                &forged,
                &id,
                &content_artifacts(
                    forged.read_artifact(&id, "content.redacted").unwrap(),
                    forged.read_artifact(&id, "full.jsonl").unwrap()
                ),
                forged
                    .read_artifact(&id, "redaction_report.json")
                    .unwrap()
                    .as_deref(),
            )
            .unwrap()
            .is_none(),
            "the plant must get past the root check, or it is not this attack"
        );
        match shard_passes(&forged).unwrap().worst {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("an edited event stream at a bound id must be TAMPER, got {other:?}"),
        }
    }

    /// **The transcript pair is verified like every other content artifact**:
    /// an honest transcript-carrying shard passes both
    /// native passes, and editing a transcript — with the tamperer's own
    /// recomputed root, the same shape as the planted forgery — still fails the id pass. The
    /// honest half is the half that reds if `NativeArtifacts` ever stops
    /// reading the pair: the re-derived root would silently lose two leaves
    /// and disagree with the record.
    #[test]
    fn a_transcript_carrying_shard_verifies_and_an_edited_transcript_is_tamper() {
        use rift_shadow::backend::{
            shard_merkle_root, ShardLeafBytes, ROOT_BOUND_ID_MIN_CANON_VERSION,
            TRANSCRIPT_COMPACT_NAME, TRANSCRIPT_FULL_NAME,
        };
        let content = b"the redacted capture".as_slice();
        let compact = br#"{"v":1,"role":"user"}"#.as_slice();
        let tfull = br#"{"type":"user","message":"hi"}"#.as_slice();
        let report = fixture_report();
        let leaves = ShardLeafBytes {
            boundary_gaps: None,
            content_redacted: content,
            full_jsonl: None,
            redaction_report: report.as_bytes(),
            serve_audit: None,
            sessions: None,
            usage: None,
            window_journal: None,
            session_worktrees: None,
            tool_results: None,
            tool_calls: None,
            transcript_compact: Some(compact),
            transcript_full: Some(tfull),
            tasks: &[],
            spills: &[],
            originals: &[],
            session_metas: &[],
            digests: rift_shadow::backend::DigestLeafBytes::default(),
        };
        let root = shard_merkle_root(&leaves).to_hex();
        let id = rift_shadow::condense::checkpoint_ids_for_shard(
            &[FIXTURE_SESSION.to_owned()],
            ROOT_BOUND_ID_MIN_CANON_VERSION,
            &leaves,
        )
        .remove(0);
        let shard_with = |tfull_bytes: &[u8], recorded_root: &str| {
            MemorySubstrate::local()
                .with_shard(shard(&id, None, 1))
                .with_artifact(&id, "redaction_report.json", report.as_bytes())
                .with_artifact(&id, "content.redacted", content)
                .with_artifact(&id, TRANSCRIPT_COMPACT_NAME, compact)
                .with_artifact(&id, TRANSCRIPT_FULL_NAME, tfull_bytes)
                .with_artifact(
                    &id,
                    "metadata.json",
                    format!(
                        r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"compact_transcript":"{TRANSCRIPT_COMPACT_NAME}","compact_transcript_start":0,"merkle_root":"{recorded_root}","sessions":["{FIXTURE_SESSION}"],"strategy":"manual-commit"}}"#
                    )
                    .as_bytes(),
                )
        };
        let out = shard_passes(&shard_with(tfull, &root)).unwrap();
        assert!(
            matches!(out.worst, Verdict::Ok),
            "the honest transcript shard must verify: {:?}",
            out.worst
        );

        // The tamperer edits the full transcript and recomputes the recorded
        // root over the edited bytes — the strict pass is satisfied; the path
        // is not.
        let edited = br#"{"type":"user","message":"hi, forged"}"#.as_slice();
        let forged_root = shard_merkle_root(&ShardLeafBytes {
            boundary_gaps: None,
            transcript_full: Some(edited),
            ..leaves
        })
        .to_hex();
        match shard_passes(&shard_with(edited, &forged_root))
            .unwrap()
            .worst
        {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("an edited transcript at a bound id must be TAMPER, got {other:?}"),
        }
    }

    /// **Relabelling DOWN does not buy the looser id rule** — measured, because
    /// the design note first said it would.
    ///
    /// Epochs ≤ 2 accept the pre-root derivations, so the obvious attack on an
    /// asymmetric era gate is to rewrite a root-bound shard's `canon_version` to
    /// 2 and edit whatever the older rule did not bind. It does not work, and the
    /// reason is structural rather than lucky: the two eras' rules produce
    /// DIFFERENT ids for the same bytes, and the shard's path can only ever
    /// satisfy one of them. A path minted by the root rule is evidence of the era
    /// it was minted in, so the label cannot be moved out from under it.
    ///
    /// What remains — and it is the forgery plant's known remainder, not this gate's — is a
    /// forger writing a whole new shard at the era-2 path its own bytes address.
    /// That is a new checkpoint, and membership is the chain's job.
    #[test]
    fn relabelling_a_root_bound_shard_into_the_capture_era_does_not_hide_it() {
        let content = b"the redacted capture".as_slice();
        let full = br#"{"kind":"session_start","turn":0}"#.as_slice();
        let (_, id) = root_bound_shard(content, full);
        let edited = br#"{"kind":"session_start","turn":0,"forged":true}"#.as_slice();
        let report = fixture_report();
        let relabelled_root =
            rift_shadow::backend::shard_merkle_root(&rift_shadow::backend::ShardLeafBytes {
                boundary_gaps: None,
                content_redacted: content,
                full_jsonl: Some(edited),
                redaction_report: report.as_bytes(),
                serve_audit: None,
                sessions: None,
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
            })
            .to_hex();
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, "full.jsonl", edited)
            .with_artifact(
                &id,
                "metadata.json",
                // canon_version 2: the era whose id rule ignores full.jsonl.
                format!(
                    r#"{{"canon_version":2,"strategy":"manual-commit","merkle_root":"{relabelled_root}","sessions":["{FIXTURE_SESSION}"]}}"#
                )
                .as_bytes(),
            );
        match shard_passes(&sub).unwrap().worst {
            Verdict::Tamper(d) => assert!(d.contains("does not address"), "{d}"),
            other => panic!("a relabelled root-bound shard must be TAMPER, got {other:?}"),
        }
    }

    /// A two-session shard written the way the archive writes one: the
    /// `sessions/<i>/metadata.json` leaves the writer lays down at N ≥ 2, at
    /// the id the multi-session derivation grants those bytes.
    ///
    /// `listed` is the session order the parent metadata declares, which is
    /// what a permutation test edits and nothing else.
    fn multi_session_shard(listed: &[&str]) -> MemorySubstrate {
        use rift_shadow::backend::{
            session_meta_json, shard_merkle_root, SessionMetaLeaf, ShardLeafBytes,
            ROOT_BOUND_ID_MIN_CANON_VERSION,
        };
        let content = b"the redacted capture".as_slice();
        let report = fixture_report();
        let sessions = [FIXTURE_SESSION.to_owned(), "01JSESSIONOTHER".to_owned()];
        // The writer's own layout: sessions/<i>/metadata.json leaves at N ≥ 2.
        let metas: Vec<(String, Vec<u8>)> = sessions
            .iter()
            .enumerate()
            .map(|(i, sid)| (i.to_string(), session_meta_json(i, sid)))
            .collect();
        let meta_leaves: Vec<SessionMetaLeaf<'_>> = metas
            .iter()
            .map(|(name, bytes)| SessionMetaLeaf {
                index_name: name,
                metadata: bytes,
            })
            .collect();
        let leaves = ShardLeafBytes {
            boundary_gaps: None,
            content_redacted: content,
            full_jsonl: None,
            redaction_report: report.as_bytes(),
            serve_audit: None,
            sessions: None,
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
            session_metas: &meta_leaves,
            digests: rift_shadow::backend::DigestLeafBytes::default(),
        };
        let root = shard_merkle_root(&leaves).to_hex();
        // The id is the DEFINED multi-session derivation —
        // the carve-out this test used to pin is retired.
        let id = rift_shadow::condense::checkpoint_ids_for_shard(
            &sessions,
            ROOT_BOUND_ID_MIN_CANON_VERSION,
            &leaves,
        )
        .remove(0);
        let listed = listed
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(",");
        let mut sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"strategy":"manual-commit","merkle_root":"{root}","sessions":[{listed}]}}"#
                )
                .as_bytes(),
            );
        for (name, bytes) in &metas {
            sub = sub.with_artifact(&id, &format!("sessions/{name}/metadata.json"), bytes);
        }
        sub
    }

    /// A multi-session checkpoint gets the id pass rather than a counted skip:
    /// the derivation is defined over N sessions, and what a checkpoint id
    /// should be over N of them is no longer a thing to guess inside a
    /// verifier.
    ///
    /// Measured before it was gated: every one of this repository's 103 shards
    /// names exactly one session, so this path has never run on real data.
    #[test]
    fn a_multi_session_shard_gets_the_id_pass_not_a_skip() {
        let sub = multi_session_shard(&[FIXTURE_SESSION, "01JSESSIONOTHER"]);
        let out = shard_passes(&sub).unwrap();
        assert!(
            matches!(out.worst, Verdict::Ok),
            "an honest multi-session shard passes the id pass: {:?}",
            out.worst
        );
    }

    /// …and the id really is session-order-bound: the same shard listing its
    /// sessions in the other order stops addressing its path.
    #[test]
    fn a_permuted_session_list_is_a_different_identity() {
        let swapped = multi_session_shard(&["01JSESSIONOTHER", FIXTURE_SESSION]);
        let out = shard_passes(&swapped).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("does not address")),
            "a permuted session list is a different identity: {:?}",
            out.worst
        );
    }

    /// **The zero-session arm, RETIRED from a counted skip into a finding.**
    /// A native shard whose header names no session — an
    /// emptied list or a deleted `sessions` field, two edits with one parse —
    /// used to turn the id pass off and be counted past. No era's rule grants
    /// an id to an empty session set (`from_root_sessions` refuses it; the
    /// pre-root rules were single-session by construction), so the empty
    /// candidate set now reds as tamper: the one thing a session-less label
    /// cannot explain is how the shard got its path.
    #[test]
    fn a_shard_naming_no_session_is_tamper_not_a_counted_skip() {
        let empty_list = b"a capture whose header lists no session".as_slice();
        let no_field = b"a capture whose header has no sessions field".as_slice();
        let sub = era_shard_for(
            MemorySubstrate::local(),
            &native_id(empty_list),
            2,
            &writer_root(empty_list),
            empty_list,
            &[],
        );
        // The field removed fully, not emptied — the shape `serde(default)`
        // turns into the same empty list, reached by a different edit.
        let id = native_id(no_field);
        let report = format!(
            r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        );
        let sub = sub
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", no_field)
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":2,"strategy":"manual-commit","merkle_root":"{}"}}"#,
                    writer_root(no_field)
                )
                .as_bytes(),
            );

        let out = shard_passes(&sub).unwrap();
        assert!(
            matches!(&out.worst, Verdict::Tamper(m) if m.contains("does not address")),
            "a session-less native shard cannot address its path: {:?}",
            out.worst
        );
    }

    /// An epoch-3 shard carrying one optional artifact — `serve_audit.jsonl`,
    /// `session_worktrees.jsonl`, `sessions.jsonl`, `tool_results.jsonl`,
    /// `usage.jsonl` or `window_journal.jsonl` — at the id
    /// its own artifacts address.
    fn artifact_bound_shard(content: &[u8], name: &str, bytes: &[u8]) -> (MemorySubstrate, String) {
        use rift_shadow::backend::ROOT_BOUND_ID_MIN_CANON_VERSION as EPOCH;
        use rift_shadow::backend::{shard_merkle_root, ShardLeafBytes};
        let report = fixture_report();
        let leaves = ShardLeafBytes {
            boundary_gaps: None,
            content_redacted: content,
            redaction_report: report.as_bytes(),
            serve_audit: (name == "serve_audit.jsonl").then_some(bytes),
            session_worktrees: (name == "session_worktrees.jsonl").then_some(bytes),
            tool_results: (name == "tool_results.jsonl").then_some(bytes),
            sessions: (name == "sessions.jsonl").then_some(bytes),
            usage: (name == "usage.jsonl").then_some(bytes),
            window_journal: (name == "window_journal.jsonl").then_some(bytes),
            ..ShardLeafBytes::default()
        };
        let root = shard_merkle_root(&leaves).to_hex();
        let id = rift_shadow::condense::checkpoint_ids_for_shard(
            &[FIXTURE_SESSION.to_owned()],
            EPOCH,
            &leaves,
        )
        .remove(0);
        let meta = format!(
            r#"{{"canon_version":{EPOCH},"strategy":"manual-commit","merkle_root":"{root}","sessions":["{FIXTURE_SESSION}"]}}"#
        );
        let sub = MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, name, bytes)
            .with_artifact(&id, "metadata.json", meta.as_bytes());
        (sub, id)
    }

    /// **The frozen serves are read back and bound like every leaf.** A shard
    /// carrying them verifies, one without them verifies as it always did, and
    /// editing a frozen line is tamper: the root the shard records was taken
    /// over the lines as written.
    #[test]
    fn a_frozen_serve_audit_is_read_back_and_the_root_holds_with_and_without_it() {
        let content = b"the redacted capture".as_slice();
        let (without, _) = root_bound_shard(content, br#"{"kind":"session_start","turn":0}"#);
        assert!(matches!(shard_passes(&without).unwrap().worst, Verdict::Ok));

        let serves = b"{\"idempotency_key\":\"k\",\"schema\":1}\n".as_slice();
        let (with, id) = artifact_bound_shard(content, "serve_audit.jsonl", serves);
        let out = shard_passes(&with).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "honest: {:?}", out.worst);
        assert_eq!(out.checked, 1);

        let edited =
            with.with_artifact(&id, "serve_audit.jsonl", b"{\"idempotency_key\":\"k2\"}\n");
        assert!(
            matches!(shard_passes(&edited).unwrap().worst, Verdict::Tamper(_)),
            "an edited frozen serve must not verify"
        );
    }

    /// **A pull's claim is bound like the rest of its line.** A shard whose
    /// frozen serve carries a claim verifies, and editing one of the claim's
    /// counts is tamper.
    #[test]
    fn a_frozen_serve_carrying_a_pull_claim_verifies_and_an_edited_claim_is_tamper() {
        let content = b"the redacted capture".as_slice();
        let line = |recorded: u32, unrecorded: u32| {
            format!(
                "{{\"idempotency_key\":\"k\",\"payload\":{{\"estimated_tokens_avoided\":9,\
                 \"estimated_tokens_returned\":3,\"kind\":\"manifest_served\",\"pull_claim\":\
                 {{\"basis\":\"stated_recorded\",\"recorded\":{recorded},\"stated\":4,\
                 \"unrecorded\":{unrecorded}}}}},\"recorded_at\":1,\"schema\":1,\
                 \"session_id\":\"s\",\"verb\":\"memd.manifest_served\"}}\n"
            )
        };
        let honest = line(1, 3);
        let (with, id) = artifact_bound_shard(content, "serve_audit.jsonl", honest.as_bytes());
        let out = shard_passes(&with).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "honest: {:?}", out.worst);
        assert_eq!(out.checked, 1);

        let edited = with.with_artifact(&id, "serve_audit.jsonl", line(4, 0).as_bytes());
        assert!(
            matches!(shard_passes(&edited).unwrap().worst, Verdict::Tamper(_)),
            "an edited claim must not verify"
        );
    }

    /// **The settled usage is read back and bound like every leaf.** A shard
    /// carrying `usage.jsonl` verifies, one without it verifies as it always
    /// did, and editing a frozen count is tamper.
    #[test]
    fn frozen_usage_is_read_back_and_the_root_holds_with_and_without_it() {
        let content = b"the redacted capture".as_slice();
        let (without, _) = root_bound_shard(content, br#"{"kind":"session_start","turn":0}"#);
        assert!(matches!(shard_passes(&without).unwrap().worst, Verdict::Ok));

        let usage = b"{\"call_id\":\"c\",\"output\":2,\"schema_version\":1}\n".as_slice();
        let (with, id) = artifact_bound_shard(content, "usage.jsonl", usage);
        let out = shard_passes(&with).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "honest: {:?}", out.worst);
        assert_eq!(out.checked, 1);

        let edited = with.with_artifact(
            &id,
            "usage.jsonl",
            b"{\"call_id\":\"c\",\"output\":0,\"schema_version\":1}\n",
        );
        assert!(
            matches!(shard_passes(&edited).unwrap().worst, Verdict::Tamper(_)),
            "an edited usage line must not verify"
        );
    }

    /// **The sessions' agents are read back and bound like every leaf.** A
    /// shard carrying `sessions.jsonl` verifies, and editing the agent a
    /// session was frozen under is tamper.
    #[test]
    fn frozen_session_agents_are_read_back_and_bound_by_the_root() {
        let content = b"the redacted capture".as_slice();
        let line = |agent: &str| {
            format!("{{\"agent\":\"{agent}\",\"schema_version\":1,\"session\":\"s\"}}\n")
        };
        let honest = line("codex");
        let (with, id) = artifact_bound_shard(content, "sessions.jsonl", honest.as_bytes());
        let out = shard_passes(&with).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "honest: {:?}", out.worst);
        assert_eq!(out.checked, 1);

        let edited = with.with_artifact(&id, "sessions.jsonl", line("claude-code").as_bytes());
        assert!(
            matches!(shard_passes(&edited).unwrap().worst, Verdict::Tamper(_)),
            "an edited agent must not verify"
        );
    }

    /// **The frozen window journal is read back and bound like every leaf.**
    /// A shard carrying `window_journal.jsonl` verifies, and turning a
    /// declined read into a handed one is tamper.
    #[test]
    fn a_frozen_window_journal_is_read_back_and_bound_by_the_root() {
        let content = b"the redacted capture".as_slice();
        let line = |kind: &str| {
            let window = "a".repeat(64);
            format!("{{\"at\":1,\"kind\":\"{kind}\",\"schema_version\":1,\"turn\":1,\"window\":\"{window}\"}}\n")
        };
        let honest = line("deny");
        let (with, id) = artifact_bound_shard(content, "window_journal.jsonl", honest.as_bytes());
        let out = shard_passes(&with).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "honest: {:?}", out.worst);
        assert_eq!(out.checked, 1);

        let edited = with.with_artifact(&id, "window_journal.jsonl", line("read").as_bytes());
        assert!(
            matches!(shard_passes(&edited).unwrap().worst, Verdict::Tamper(_)),
            "an edited journal row must not verify"
        );
    }

    /// A shard carrying `session_worktrees.jsonl` verifies, and moving an
    /// observation into another member is tamper.
    #[test]
    fn frozen_worktree_observations_are_read_back_and_bound_by_the_root() {
        let content = b"the redacted capture".as_slice();
        let line = |name: &str| {
            format!(
                "{{\"at\":1,\"basis\":\"payload_cwd\",\"linked\":true,\"name\":\"{name}\",\
                 \"schema_version\":1,\"session\":\"{FIXTURE_SESSION}\",\"token\":\"0123456789ab\"}}\n"
            )
        };
        let honest = line("lane-a");
        let (with, id) =
            artifact_bound_shard(content, "session_worktrees.jsonl", honest.as_bytes());
        let out = shard_passes(&with).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "honest: {:?}", out.worst);
        assert_eq!(out.checked, 1);

        let edited = with.with_artifact(&id, "session_worktrees.jsonl", line("lane-b").as_bytes());
        assert!(
            matches!(shard_passes(&edited).unwrap().worst, Verdict::Tamper(_)),
            "an edited observation must not verify"
        );
    }

    /// A shard carrying `tool_results.jsonl` verifies, and an edited token
    /// count is tamper.
    #[test]
    fn frozen_tool_results_are_read_back_and_bound_by_the_root() {
        let content = b"the redacted capture".as_slice();
        let line = |tokens: u32| {
            format!(
                "{{\"basis\":\"in_process\",\"channel\":\"search\",\"id\":\"{}\",\
                 \"output_tokens\":{tokens},\"recorded_at\":1,\"schema_version\":1,\
                 \"session\":\"{FIXTURE_SESSION}\",\"tokenizer\":\"cl100k_base\",\
                 \"tool\":\"Glob\"}}\n",
                "a".repeat(64)
            )
        };
        let honest = line(40);
        let (with, id) = artifact_bound_shard(content, "tool_results.jsonl", honest.as_bytes());
        let out = shard_passes(&with).unwrap();
        assert!(matches!(out.worst, Verdict::Ok), "honest: {:?}", out.worst);
        assert_eq!(out.checked, 1);

        let edited = with.with_artifact(&id, "tool_results.jsonl", line(4_000).as_bytes());
        assert!(
            matches!(shard_passes(&edited).unwrap().worst, Verdict::Tamper(_)),
            "an edited token count must not verify"
        );
    }

    /// A shard carrying one collected subagent transcript, as the archive
    /// writes it, with `tamper` applied to the stored `transcript.jsonl`.
    fn collected_task_shard(tamper: impl Fn(&mut Vec<u8>)) -> MemorySubstrate {
        use rift_shadow::backend::{
            shard_merkle_root, ShardLeafBytes, TaskLeafBytes, TaskProvenance, TaskTranscriptState,
            ROOT_BOUND_ID_MIN_CANON_VERSION,
        };
        let content = b"parent capture".as_slice();
        let report = fixture_report();
        let raw = rift_core::SecretBytes::new(b"{\"type\":\"user\",\"text\":\"sub\"}\n".to_vec());
        let packs = rift_shadow::redact::RulePackSet::builtin();
        let (pair, _) = rift_shadow::archived_transcript(&raw, 0, &packs).unwrap();
        let (full, compact) = (
            pair.full.as_slice().to_vec(),
            pair.compact.as_slice().to_vec(),
        );
        let task_meta = rift_shadow::backend::ArchivedTask {
            tool_use_id: "toolu_s".to_owned(),
            agent_id: "a1".to_owned(),
            started_at_ms: 4,
            ended_at_ms: 8,
            input_tokens: None,
            output_tokens: None,
            transcript: Some(pair),
            provenance: Some(TaskProvenance {
                subagent_type: Some("Explore".into()),
                parent_session: FIXTURE_SESSION.into(),
                worktree: None,
                spawn_matched: true,
                transcript: TaskTranscriptState::Sealed,
            }),
        }
        .metadata_json();
        let task_leaves = [TaskLeafBytes {
            tool_use_id: "toolu_s",
            metadata: &task_meta,
            transcript_compact: Some(&compact),
            transcript_full: Some(&full),
        }];
        let leaves = ShardLeafBytes {
            boundary_gaps: None,
            content_redacted: content,
            redaction_report: report.as_bytes(),
            tasks: &task_leaves,
            ..ShardLeafBytes::default()
        };
        let root = shard_merkle_root(&leaves).to_hex();
        let id = rift_shadow::condense::checkpoint_ids_for_shard(
            &[FIXTURE_SESSION.to_owned()],
            ROOT_BOUND_ID_MIN_CANON_VERSION,
            &leaves,
        )
        .remove(0);
        let mut stored = full.clone();
        tamper(&mut stored);
        MemorySubstrate::local()
            .with_shard(shard(&id, None, 1))
            .with_artifact(&id, "redaction_report.json", report.as_bytes())
            .with_artifact(&id, "content.redacted", content)
            .with_artifact(&id, "tasks/toolu_s/metadata.json", &task_meta)
            .with_artifact(&id, "tasks/toolu_s/transcript.compact.jsonl", &compact)
            .with_artifact(&id, "tasks/toolu_s/transcript.jsonl", &stored)
            .with_artifact(
                &id,
                "metadata.json",
                format!(
                    r#"{{"canon_version":{ROOT_BOUND_ID_MIN_CANON_VERSION},"strategy":"manual-commit","merkle_root":"{root}","sessions":["{FIXTURE_SESSION}"],"tasks":["toolu_s"]}}"#
                )
                .as_bytes(),
            )
    }

    /// One byte changed in a collected subagent transcript fails verify, and
    /// the finding names the subtree rather than only the checkpoint.
    #[test]
    fn a_one_byte_tamper_in_a_subagent_transcript_names_its_subtree() {
        let honest = shard_passes(&collected_task_shard(|_| ())).unwrap();
        assert!(
            matches!(honest.worst, Verdict::Ok),
            "honest: {:?}",
            honest.worst
        );
        let flipped = shard_passes(&collected_task_shard(|b| b[0] ^= 1)).unwrap();
        assert!(
            matches!(&flipped.worst, Verdict::Tamper(m) if m.contains("task subtree 'toolu_s' transcript.jsonl")),
            "{:?}",
            flipped.worst
        );
        let dropped = shard_passes(&collected_task_shard(Vec::clear)).unwrap();
        assert!(
            matches!(dropped.worst, Verdict::Tamper(_)),
            "{:?}",
            dropped.worst
        );
    }
}

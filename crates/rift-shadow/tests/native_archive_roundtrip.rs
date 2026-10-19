//! The native archive writes a shard the recall plane reads back:
//! a native-produced checkpoint round-trips through the production `gix` substrate
//! reader and the artifact bridge, byte-for-byte. This is the cross-implementation
//! `verify` in the direction we can exercise without the pinned binary (native
//! produces → gix reads); it also pins determinism and the redaction-gate survival.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::Path;

use rift_core::SecretBytes;
use rift_recall_substrate::{GixSubstrate, SubstrateReader};
use rift_shadow::backend::{CheckpointArchive, DigestLeafBytes, RedactionReportView};
use rift_shadow::{fixture_archive, NativeBackend, ShadowBackend};

struct MovingNativeTip {
    source: GixSubstrate,
    repo: std::path::PathBuf,
    second_tip: String,
    switched: std::cell::Cell<bool>,
}

impl SubstrateReader for MovingNativeTip {
    fn is_local_only(&self) -> bool {
        self.source.is_local_only()
    }
    fn checkpoints(&self) -> rift_core::Result<Vec<rift_recall_substrate::CheckpointShard>> {
        self.source.checkpoints()
    }
    fn blob(&self, id: &str) -> rift_core::Result<Option<Vec<u8>>> {
        self.source.blob(id)
    }
    fn read_artifact(&self, id: &str, rel: &str) -> rift_core::Result<Option<Vec<u8>>> {
        self.source.read_artifact(id, rel)
    }
    fn read_native_artifact(&self, id: &str, rel: &str) -> rift_core::Result<Option<Vec<u8>>> {
        if rel == "full.jsonl" {
            origin_git(
                &self.repo,
                &[
                    "update-ref",
                    rift_recall_substrate::Dialect::Rift.checkpoint_ref(),
                    &self.second_tip,
                ],
            );
            self.switched.set(true);
        }
        self.source.read_native_artifact(id, rel)
    }
}

/// Init a real git repo with a deterministic default branch.
fn init_repo(dir: &Path) {
    let ok = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(dir)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git init");
}

/// A native backend writing into a real repo at `dir`.
fn native_backend(dir: &Path) -> NativeBackend<rift_git::PlumbingGitWrite> {
    let repo = rift_git::discover(dir).unwrap();
    NativeBackend::new(rift_git::PlumbingGitWrite::new(repo))
}

#[test]
fn native_shard_roundtrips_through_gix_substrate() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let archive = fixture_archive("two_turn");
    let backend = native_backend(tmp.path());

    backend.archive_checkpoint(&archive).unwrap();

    // The production reader enumerates exactly the shard we wrote.
    let sub = GixSubstrate::open(tmp.path()).unwrap();
    let shards = sub.checkpoints().unwrap();
    assert_eq!(shards.len(), 1, "one checkpoint on the branch");
    assert_eq!(shards[0].checkpoint_id, archive.checkpoint_id);

    // Every artifact reads back BYTE-identical to what the native encoder produced.
    let id = &archive.checkpoint_id;
    assert_eq!(
        sub.read_artifact(id, "metadata.json").unwrap().unwrap(),
        archive.metadata_json()
    );
    assert_eq!(
        sub.read_artifact(id, "redaction_report.json")
            .unwrap()
            .unwrap(),
        archive.redaction_report_json()
    );
    assert_eq!(
        sub.read_artifact(id, "full.jsonl").unwrap().unwrap(),
        archive.full_jsonl().unwrap()
    );
    assert_eq!(
        sub.read_artifact(id, "content_hash.txt").unwrap().unwrap(),
        archive.content_hash_txt()
    );
}

/// **The task subtree, over real git objects.** A task-carrying shard writes
/// `tasks/<tool-use-id>/` as a genuine nested tree; every task artifact reads
/// back byte-identical through the production reader (nested `rel` paths —
/// `lookup_entry_by_path` — not a flat-name special case), and the recorded
/// root re-derives from those read-back bytes through the SAME leaf type the
/// writer used. This is the read-side half of the signal test: the write-side
/// half (`a_task_subtree_joins_the_leaf_spec_and_moves_the_id`) lives with
/// the backend.
#[test]
fn a_task_subtree_roundtrips_and_the_root_rederives_from_read_back_bytes() {
    use rift_shadow::backend::{shard_merkle_root, ArchivedTask, ShardLeafBytes, TaskLeafBytes};

    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());

    let packs = rift_shadow_redact::RulePackSet::builtin();
    let (pair, _lines) = rift_shadow::archived_transcript(
        &SecretBytes::new(br#"{"type":"user","message":"dispatch the subagent"}"#.to_vec()),
        0,
        &packs,
    )
    .unwrap();
    let task = ArchivedTask {
        tool_use_id: "toolu_roundtrip".to_owned(),
        agent_id: "explore".to_owned(),
        started_at_ms: 4,
        ended_at_ms: 8,
        input_tokens: Some(120),
        output_tokens: Some(34),
        transcript: Some(pair),
        provenance: None,
    };
    let mut archive = archive_with_transcript(tmp.path(), b"parent capture", None);
    archive.tasks = vec![task];
    archive.checkpoint_id = archive.derived_checkpoint_id().unwrap();
    native_backend(tmp.path())
        .archive_checkpoint(&archive)
        .unwrap();

    let sub = GixSubstrate::open(tmp.path()).unwrap();
    let id = &archive.checkpoint_id;
    let read = |name: &str| sub.read_artifact(id, name).unwrap();

    // Task artifacts read back byte-identical, through nested paths.
    let task = &archive.tasks[0];
    let task_meta = read("tasks/toolu_roundtrip/metadata.json").expect("task metadata");
    assert_eq!(task_meta, task.metadata_json());
    let task_compact =
        read("tasks/toolu_roundtrip/transcript.compact.jsonl").expect("task compact");
    let task_full = read("tasks/toolu_roundtrip/transcript.jsonl").expect("task transcript");
    let sealed = task.transcript.as_ref().unwrap();
    assert_eq!(task_compact, sealed.compact.as_slice());
    assert_eq!(task_full, sealed.full.as_slice());

    // The parent metadata enumerates the subtree…
    let meta: serde_json::Value = serde_json::from_slice(&read("metadata.json").unwrap()).unwrap();
    assert_eq!(
        meta.get("tasks")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(1),
        "metadata lists the task"
    );

    // …and the recorded root re-derives from the READ-BACK bytes.
    let content = read("content.redacted").unwrap();
    let report = read("redaction_report.json").unwrap();
    let task_leaves = [TaskLeafBytes {
        tool_use_id: "toolu_roundtrip",
        metadata: &task_meta,
        transcript_compact: Some(&task_compact),
        transcript_full: Some(&task_full),
    }];
    let derived = shard_merkle_root(&ShardLeafBytes {
        boundary_gaps: None,
        content_redacted: &content,
        redaction_report: &report,
        tasks: &task_leaves,
        ..ShardLeafBytes::default()
    });
    assert_eq!(
        meta.get("merkle_root").and_then(serde_json::Value::as_str),
        Some(derived.to_hex().as_str()),
        "the recorded root re-derives over the task leaves as read back"
    );
}

/// **Writer ≡ verifier, over the bytes that actually landed in git.**
///
/// The root in `metadata.json` is written by `CheckpointArchive::merkle_root`
/// from the bytes the archive is about to commit; `rift verify` re-derives it
/// from the bytes it reads back out of the shard. This test is the pin that
/// reds if those two ever stop being the same function over the same leaves —
/// the failure mode that produced the vacuous constant in the first place, and
/// the one this program has now paid for three times under other names.
///
/// It deliberately re-derives from `SubstrateReader::read_artifact`, i.e. the
/// exact call `verify` makes, rather than from the in-memory archive.
#[test]
fn the_recorded_root_is_re_derivable_from_the_shard_as_written() {
    use rift_shadow::backend::{shard_merkle_root, ShardLeafBytes};

    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let archive = fixture_archive("two_turn");
    native_backend(tmp.path())
        .archive_checkpoint(&archive)
        .unwrap();

    let sub = GixSubstrate::open(tmp.path()).unwrap();
    let id = &archive.checkpoint_id;
    let read = |name: &str| sub.read_artifact(id, name).unwrap();

    let content = read("content.redacted").expect("content.redacted is always written");
    let report = read("redaction_report.json").expect("redaction_report.json is always written");
    let full = read("full.jsonl");
    let tool_calls = read("tool_calls.jsonl");
    let serve_audit = read("serve_audit.jsonl");
    let transcript_compact = read("transcript.compact.jsonl");
    let transcript_full = read("transcript.jsonl");
    let usage = read("usage.jsonl");
    let sessions = read("sessions.jsonl");
    let window_journal = read("window_journal.jsonl");
    let session_worktrees = read("session_worktrees.jsonl");
    let tool_results = read("tool_results.jsonl");
    let leaves = ShardLeafBytes {
        boundary_gaps: None,
        content_redacted: &content,
        full_jsonl: full.as_deref(),
        redaction_report: &report,
        serve_audit: serve_audit.as_deref(),
        sessions: sessions.as_deref(),
        usage: usage.as_deref(),
        window_journal: window_journal.as_deref(),
        session_worktrees: session_worktrees.as_deref(),
        tool_calls: tool_calls.as_deref(),
        tool_results: tool_results.as_deref(),
        transcript_compact: transcript_compact.as_deref(),
        transcript_full: transcript_full.as_deref(),
        tasks: &[],
        spills: &[],
        originals: &[],
        session_metas: &[],
        digests: DigestLeafBytes::default(),
    };
    let derived = shard_merkle_root(&leaves);

    let meta: serde_json::Value =
        serde_json::from_slice(&read("metadata.json").unwrap()).expect("metadata parses");
    let recorded = meta
        .get("merkle_root")
        .and_then(serde_json::Value::as_str)
        .expect("the shard records a root");
    assert_eq!(
        recorded,
        derived.to_hex(),
        "the recorded root must be re-derivable from the shard's own artifacts"
    );

    // Calibration: the assertion above is not vacuously true. One flipped byte
    // in ONE leaf artifact must move the re-derivation away from the record —
    // if it did not, the check would pass for any shard at all.
    let mut tampered = content.clone();
    tampered.push(b'x');
    let after = shard_merkle_root(&ShardLeafBytes {
        boundary_gaps: None,
        content_redacted: &tampered,
        ..leaves
    });
    assert_ne!(
        after.to_hex(),
        recorded,
        "a changed leaf artifact must change the derived root"
    );
}

#[tokio::test]
async fn native_shard_is_ingestible_and_reingest_is_a_noop() {
    use rift_store::{CanonicalStore, SqliteStore};
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let archive = fixture_archive("two_turn");
    native_backend(tmp.path())
        .archive_checkpoint(&archive)
        .unwrap();

    let store = SqliteStore::open_in_memory().unwrap();
    store.create_canonical_schema().unwrap();
    let sub = GixSubstrate::open(tmp.path()).unwrap();

    // The bridge projects the native-written shard (a native producer is trusted
    // by the recall plane exactly like the wrapper's shards).
    let r1 = rift_shadow::ingest_from_substrate(&store, &sub, "repo-p7", "claude-code", 1)
        .await
        .unwrap();
    assert_eq!(r1.shards_projected, 1);
    assert!(
        r1.refused.is_empty(),
        "the native redaction report is valid"
    );
    assert_eq!(r1.events_derived, 3, "the two-turn fixture's three events");
    assert!(store.count("checkpoint").await.unwrap() >= 1);

    // Re-ingest is a set-union no-op (content addressing).
    let r2 = rift_shadow::ingest_from_substrate(&store, &sub, "repo-p7", "claude-code", 2)
        .await
        .unwrap();
    assert_eq!(r2.events_inserted, 0, "re-ingest inserts nothing");
}

#[test]
fn native_archive_is_byte_deterministic_across_runs() {
    // Two independent repos, the identical archive → identical branch tip commit
    // (the plumbing commit is byte-reproducible: fixed identity + content-derived
    // date). BYTE parity's prerequisite.
    let tips: Vec<String> = (0..2)
        .map(|_| {
            let tmp = tempfile::tempdir().unwrap();
            init_repo(tmp.path());
            let archive = fixture_archive("two_turn");
            native_backend(tmp.path())
                .archive_checkpoint(&archive)
                .unwrap();
            let repo = rift_git::discover(tmp.path()).unwrap();
            rift_git::resolve_commit(&repo, "refs/heads/rift/checkpoints/v1")
                .unwrap()
                .unwrap()
        })
        .collect();
    assert_eq!(tips[0], tips[1], "identical archive → identical commit oid");
}

#[test]
fn a_seeded_secret_never_reaches_any_native_artifact() {
    // The redaction gate survives the native write path: a seeded secret in the raw
    // capture is masked before it can address anything durable, and never appears in
    // any artifact byte on the branch.
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let secret = "AKIAIOSFODNN7EXAMPLE";
    let out = rift_shadow_redact::redact(&SecretBytes::new(
        format!("aws key {secret} in capture").into_bytes(),
    ))
    .unwrap();
    let archive = CheckpointArchive {
        observed_head: None,
        boundary_gaps: None,
        repo: tmp.path().to_path_buf(),
        commit: Some("2".repeat(40)),
        checkpoint_id: "cc".to_string() + &"dd".repeat(31),
        sessions: vec!["01JSECRET".into()],
        strategy: "manual-commit".into(),
        prev_hash: None,
        events: Vec::new(),
        read_facts: Vec::new(),
        serve_audit: None,
        usage: Vec::new(),
        session_agents: std::collections::BTreeMap::new(),
        window_journal: Vec::new(),
        worktree_observations: Vec::new(),
        tool_results: Vec::new(),
        content: out.redacted,
        report: RedactionReportView::from_redact(&out.report),
        transcript: None,
        tasks: Vec::new(),
        created_at_ms: Some(1_700_000_000_000),
        // A hand-built archive is in no repository: the member is ABSENT, which
        // is what keeps every frozen anchor and golden byte-identical.
        worktree: None,
        session_worktrees: std::collections::BTreeMap::new(),
    };
    native_backend(tmp.path())
        .archive_checkpoint(&archive)
        .unwrap();

    let sub = GixSubstrate::open(tmp.path()).unwrap();
    let id = &archive.checkpoint_id;
    for artifact in [
        "metadata.json",
        "redaction_report.json",
        "content_hash.txt",
        "content.redacted",
    ] {
        if let Some(bytes) = sub.read_artifact(id, artifact).unwrap() {
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains(secret),
                "seeded secret leaked into native artifact {artifact}"
            );
        }
    }
}

/// A sealed transcript pair for the dedup tests below, over fixed raw bytes —
/// built through the product's own one-writer flow so the artifacts are real.
fn sealed_transcript(raw: &[u8]) -> rift_shadow::backend::ArchivedTranscript {
    rift_shadow::archived_transcript(
        &SecretBytes::new(raw.to_vec()),
        0,
        &rift_shadow::redact::RulePackSet::builtin(),
    )
    .unwrap()
    .0
}

/// A minimal valid archive over `capture` carrying `transcript`, with its id
/// derived by the writer's own rule so it addresses its bytes.
fn archive_with_transcript(
    repo: &Path,
    capture: &[u8],
    transcript: Option<rift_shadow::backend::ArchivedTranscript>,
) -> CheckpointArchive {
    let out = rift_shadow_redact::redact(&SecretBytes::new(capture.to_vec())).unwrap();
    let mut archive = CheckpointArchive {
        observed_head: None,
        boundary_gaps: None,
        repo: repo.to_path_buf(),
        commit: Some("3".repeat(40)),
        checkpoint_id: String::new(),
        sessions: vec!["01JDEDUP".into()],
        strategy: "manual-commit".into(),
        prev_hash: None,
        events: Vec::new(),
        read_facts: Vec::new(),
        serve_audit: None,
        usage: Vec::new(),
        session_agents: std::collections::BTreeMap::new(),
        window_journal: Vec::new(),
        worktree_observations: Vec::new(),
        tool_results: Vec::new(),
        content: out.redacted,
        report: RedactionReportView::from_redact(&out.report),
        transcript,
        tasks: Vec::new(),
        created_at_ms: Some(1_700_000_000_000),
        // A hand-built archive is in no repository: the member is ABSENT, which
        // is what keeps every frozen anchor and golden byte-identical.
        worktree: None,
        session_worktrees: std::collections::BTreeMap::new(),
    };
    archive.checkpoint_id = archive.derived_checkpoint_id().unwrap();
    archive
}

/// **The frozen serve envelopes survive the trip through git byte for byte**,
/// and they are a leaf: the root the shard records re-derives from the bytes
/// read back, and the same archive without them addresses a different shard.
#[test]
fn a_frozen_serve_audit_reads_back_byte_for_byte_and_is_bound_by_the_id() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let lines = b"{\"idempotency_key\":\"k1\",\"schema\":1,\"verb\":\"memd.grep_capped\"}\n";
    let mut archive = archive_with_transcript(tmp.path(), b"the capture", None);
    let bare_id = archive.checkpoint_id.clone();
    archive.serve_audit = Some(lines.to_vec());
    archive.checkpoint_id = archive.derived_checkpoint_id().unwrap();
    assert_ne!(
        archive.checkpoint_id, bare_id,
        "the artifact is bound by the id"
    );
    native_backend(tmp.path())
        .archive_checkpoint(&archive)
        .unwrap();

    let sub = GixSubstrate::open(tmp.path()).unwrap();
    let read = |name: &str| sub.read_artifact(&archive.checkpoint_id, name).unwrap();
    assert_eq!(read("serve_audit.jsonl").as_deref(), Some(lines.as_slice()));
    let meta: serde_json::Value = serde_json::from_slice(&read("metadata.json").unwrap()).unwrap();
    assert_eq!(
        meta.get("merkle_root").and_then(serde_json::Value::as_str),
        Some(archive.merkle_root().to_hex().as_str()),
        "the recorded root is the one over the artifact as written"
    );
}

/// **The settled usage survives the trip through git byte for byte**, and it
/// is a leaf: the root the shard records re-derives from the bytes read
/// back, and the same archive without it addresses a different shard.
#[test]
fn settled_usage_reads_back_byte_for_byte_and_is_bound_by_the_id() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut archive = archive_with_transcript(tmp.path(), b"the capture", None);
    let bare_id = archive.checkpoint_id.clone();
    archive.usage = vec![rift_shadow::backend::ArchivedUsageFact {
        session: archive.sessions[0].clone(),
        turn: 1,
        call_id: "c".repeat(64),
        model: "claude-fable-5".to_owned(),
        at: 1_000,
        uncached_input: 20,
        cached_input: 30,
        cache_creation: 0,
        output: 2,
        method: "provider_reported".to_owned(),
    }];
    archive.checkpoint_id = archive.derived_checkpoint_id().unwrap();
    assert_ne!(
        archive.checkpoint_id, bare_id,
        "the artifact is bound by the id"
    );
    native_backend(tmp.path())
        .archive_checkpoint(&archive)
        .unwrap();

    let sub = GixSubstrate::open(tmp.path()).unwrap();
    let read = |name: &str| sub.read_artifact(&archive.checkpoint_id, name).unwrap();
    assert_eq!(read("usage.jsonl"), archive.usage_jsonl());
    let meta: serde_json::Value = serde_json::from_slice(&read("metadata.json").unwrap()).unwrap();
    assert_eq!(
        meta.get("merkle_root").and_then(serde_json::Value::as_str),
        Some(archive.merkle_root().to_hex().as_str()),
        "the recorded root is the one over the artifact as written"
    );
}

/// Half one: **re-condense of an unchanged session is a no-op
/// on the SHARD** — same capture, same transcript, second archive; the branch
/// tip does not move and no second shard appears. Asserted on the shard (the
/// commit oid covers every byte of the tree), deliberately NOT on
/// `content_hash.txt`: nothing verifies that artifact, and a no-op
/// test that read it would be certifying a file nothing verifies.
#[test]
fn re_condense_of_an_unchanged_session_is_a_noop_on_the_shard() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let t = sealed_transcript(br#"{"type":"user","message":"hello"}"#);
    let archive = archive_with_transcript(tmp.path(), b"the capture", Some(t));
    let backend = native_backend(tmp.path());
    backend.archive_checkpoint(&archive).unwrap();
    let repo = rift_git::discover(tmp.path()).unwrap();
    let tip1 = rift_git::resolve_commit(&repo, "refs/heads/rift/checkpoints/v1")
        .unwrap()
        .unwrap();

    backend.archive_checkpoint(&archive).unwrap();
    let tip2 = rift_git::resolve_commit(&repo, "refs/heads/rift/checkpoints/v1")
        .unwrap()
        .unwrap();
    assert_eq!(
        tip1, tip2,
        "an unchanged session re-archives to the same tip"
    );
    let sub = GixSubstrate::open(tmp.path()).unwrap();
    assert_eq!(
        sub.checkpoints().unwrap().len(),
        1,
        "…and no second shard appears"
    );
}

/// Half two: **identical transcript bytes across DIFFERENT
/// checkpoints share one blob** — the dedup is git's own content addressing,
/// measured on real git objects: `git ls-tree` reports the same blob oid under
/// both shards for both transcript artifacts.
#[test]
fn identical_transcript_bytes_share_one_blob_across_checkpoints() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let raw = br#"{"type":"user","message":"the same conversation"}"#;
    let a = archive_with_transcript(tmp.path(), b"capture one", Some(sealed_transcript(raw)));
    let b = archive_with_transcript(tmp.path(), b"capture two", Some(sealed_transcript(raw)));
    assert_ne!(
        a.checkpoint_id, b.checkpoint_id,
        "two different captures are two different checkpoints"
    );
    let backend = native_backend(tmp.path());
    backend.archive_checkpoint(&a).unwrap();
    backend.archive_checkpoint(&b).unwrap();

    let tree = std::process::Command::new("git")
        .args(["ls-tree", "-r", "refs/heads/rift/checkpoints/v1"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&tree.stdout).into_owned();
    for artifact in ["transcript.compact.jsonl", "transcript.jsonl"] {
        let oids: Vec<&str> = listing
            .lines()
            .filter(|l| l.ends_with(artifact))
            .filter_map(|l| l.split_whitespace().nth(2))
            .collect();
        assert_eq!(
            oids.len(),
            2,
            "{artifact} present in both shards: {listing}"
        );
        assert_eq!(
            oids[0], oids[1],
            "identical {artifact} bytes must share one git blob (CAS)"
        );
    }
}

/// **What the writer ACTUALLY commits, measured on real git objects.**
///
/// `CheckpointArchive::artifact_map` is what the goldens compare and what the
/// registry guard walks; `NativeBackend::write_shard` is what builds the tree.
/// Two enumerations of one thing drift, and the drift is invisible from either
/// side alone — which is the exact shape of the unlisted-artifact hole this
/// item exists to close. So the binding is a `git ls-tree -r` over the shard
/// subtree in a real repository: the paths GIT holds, compared with the paths
/// the map claims, and every one of them declared.
#[test]
fn the_committed_shard_tree_is_exactly_the_declared_artifact_set() {
    use std::collections::BTreeSet;

    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let archive =
        rift_shadow::shard_fixture::every_artifact_archive().expect("the maximal fixture builds");
    native_backend(tmp.path())
        .archive_checkpoint(&archive)
        .unwrap();

    let committed = shard_tree_paths(tmp.path(), &archive.shard_path());
    let mapped: BTreeSet<String> = archive.artifact_map().keys().cloned().collect();
    assert_eq!(
        committed, mapped,
        "the shard tree git holds and artifact_map() disagree"
    );
    for path in &committed {
        assert!(
            rift_shadow::artifact_registry::is_declared(path).is_some(),
            "{path} is committed into a shard but declared nowhere"
        );
    }
}

/// **A segmented transcript on real git objects.** The parts land as the
/// `transcript.segments` tree the map names, every path is declared, no
/// `transcript.jsonl` blob is written, and the one resolver reads the parts
/// back through gix — on both origins — as the whole document the root binds.
#[test]
fn a_segmented_transcript_commits_its_parts_and_reads_back_whole() {
    use rift_shadow::backend::{Segments, TranscriptBody};
    use std::collections::BTreeSet;

    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let doc = b"{\"type\":\"user\",\"text\":\"one\"}\n{\"type\":\"user\",\"text\":\"two\"}\n";
    let mut pair = sealed_transcript(doc);
    let whole = pair.full.as_slice().to_vec();
    let seal_line = |line: &[u8]| {
        rift_shadow_redact::redact(&SecretBytes::new(line.to_vec()))
            .unwrap()
            .redacted
    };
    let parts = doc
        .split_inclusive(|b| *b == b'\n')
        .map(seal_line)
        .collect();
    pair.full = TranscriptBody::Segments(Segments::new(parts).unwrap());
    let archive = archive_with_transcript(tmp.path(), b"capture", Some(pair));
    native_backend(tmp.path())
        .archive_checkpoint(&archive)
        .unwrap();

    let committed = shard_tree_paths(tmp.path(), &archive.shard_path());
    let mapped: BTreeSet<String> = archive.artifact_map().keys().cloned().collect();
    assert_eq!(committed, mapped, "the tree git holds and the map agree");
    assert!(
        committed.contains("transcript.segments/000001"),
        "{committed:?}"
    );
    assert!(!committed.contains("transcript.jsonl"), "{committed:?}");
    for path in &committed {
        let declared = rift_shadow::artifact_registry::is_declared(path);
        assert!(declared.is_some(), "{path} declared nowhere");
    }
    let sub = GixSubstrate::open(tmp.path()).unwrap();
    let id = archive.checkpoint_id.as_str();
    let any = sub.read_artifact(id, "transcript.jsonl").unwrap();
    assert_eq!(any.as_deref(), Some(whole.as_slice()));
    let native = sub.read_native_artifact(id, "transcript.jsonl").unwrap();
    assert_eq!(native.as_deref(), Some(whole.as_slice()));
}

/// **The planted-garbage case, on real git objects.** A blob written into the
/// shard subtree under a name the enumeration does not carry is exactly what an
/// attacker (or a careless writer) adds: it changes no Merkle leaf, so the
/// root, the recorded root and the checkpoint id all still agree, and every
/// existing pass stays green. The declared-set check is what sees it.
#[test]
fn a_blob_planted_into_a_real_shard_tree_is_undeclared() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let archive =
        rift_shadow::shard_fixture::every_artifact_archive().expect("the maximal fixture builds");
    native_backend(tmp.path())
        .archive_checkpoint(&archive)
        .unwrap();
    let shard = archive.shard_path();

    // The plant, through git's own plumbing: a real blob, a real tree, a real
    // commit on the real checkpoint ref.
    let oid = git_out(tmp.path(), &["hash-object", "-w", "--stdin"], b"planted\n");
    let index = tmp.path().join("plant.index");
    git_env(
        tmp.path(),
        &index,
        &["read-tree", "refs/heads/rift/checkpoints/v1^{tree}"],
    );
    git_env(
        tmp.path(),
        &index,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{oid},{shard}/notes.txt"),
        ],
    );
    let tree = git_env(tmp.path(), &index, &["write-tree"]);
    let commit = git_env(
        tmp.path(),
        &index,
        &[
            "commit-tree",
            &tree,
            "-p",
            "refs/heads/rift/checkpoints/v1",
            "-m",
            "planted",
        ],
    );
    git_env(
        tmp.path(),
        &index,
        &["update-ref", "refs/heads/rift/checkpoints/v1", &commit],
    );

    // Git now holds an artifact the archive never wrote…
    let committed = shard_tree_paths(tmp.path(), &shard);
    assert!(
        committed.contains("notes.txt"),
        "the plant must actually be in the tree: {committed:?}"
    );
    // …the Merkle root is UNMOVED, which is why nothing else catches it…
    let reread =
        rift_shadow::shard_fixture::every_artifact_archive().expect("the maximal fixture builds");
    assert_eq!(
        reread.merkle_root().to_hex(),
        archive.merkle_root().to_hex(),
        "an unlisted artifact contributes no leaf — that is the hole"
    );
    // …and the declared-set check is what reds.
    let undeclared: Vec<&String> = committed
        .iter()
        .filter(|p| rift_shadow::artifact_registry::is_declared(p).is_none())
        .collect();
    assert_eq!(
        undeclared.len(),
        1,
        "exactly the plant is undeclared, got {undeclared:?}"
    );
    assert_eq!(undeclared[0], "notes.txt");
}

/// The shard-relative paths git holds under `shard`, from `ls-tree -r`.
fn shard_tree_paths(dir: &Path, shard: &str) -> std::collections::BTreeSet<String> {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args([
            "ls-tree",
            "-r",
            "--name-only",
            &format!("refs/heads/rift/checkpoints/v1:{shard}"),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git ls-tree: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Run git with `stdin`, returning its trimmed stdout.
fn git_out(dir: &Path, args: &[&str], stdin: &[u8]) -> String {
    use std::io::Write as _;
    let mut child = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Run git against a scratch index file (so the plant never touches the
/// worktree's own index), returning its trimmed stdout.
fn git_env(dir: &Path, index: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .env("GIT_INDEX_FILE", index)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn origin_gap_archive() -> CheckpointArchive {
    use rift_core::boundary_gap::*;
    let mut archive = fixture_archive("two_turn");
    let native = archive.sessions.first().unwrap().clone();
    archive
        .session_agents
        .insert(native.clone(), "claude-code".into());
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
    let carrier =
        SessionKey::parse_hex(&rift_core::capture_session_row_id("claude-code", &native)).unwrap();
    archive.boundary_gaps = Some(
        rift_shadow::backend::FrozenBoundaryGaps::new(vec![GapEnvelope::new(carrier, batch)])
            .unwrap(),
    );
    archive.checkpoint_id = archive.derived_checkpoint_id().unwrap();
    archive
}

fn origin_git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_INDEX_FILE", dir.join("origin-private-index"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[tokio::test]
async fn a_native_gap_cannot_authenticate_a_required_leaf_from_only_the_legacy_origin() {
    use rift_recall_substrate::Dialect;
    use rift_store::{CanonicalStore, SqliteStore};
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let archive = origin_gap_archive();
    native_backend(tmp.path())
        .archive_checkpoint(&archive)
        .unwrap();
    let sub = GixSubstrate::open(tmp.path()).unwrap();
    let control = SqliteStore::open_in_memory().unwrap();
    control.create_canonical_schema().unwrap();
    let positive = rift_shadow::ingest_from_substrate(&control, &sub, "repo", "claude-code", 100)
        .await
        .unwrap();
    assert_eq!((positive.shards_projected, positive.refused.len()), (1, 0));
    let id = &archive.checkpoint_id;
    let metadata = sub.read_artifact(id, "metadata.json").unwrap().unwrap();
    let full = sub.read_artifact(id, "full.jsonl").unwrap().unwrap();
    let native_ref = Dialect::Rift.checkpoint_ref();
    let original = origin_git(tmp.path(), &["rev-parse", native_ref]);
    origin_git(
        tmp.path(),
        &["update-ref", Dialect::Legacy.checkpoint_ref(), &original],
    );
    origin_git(tmp.path(), &["read-tree", native_ref]);
    let leaf = format!("{}/{}/full.jsonl", &id[..2], &id[2..]);
    assert!(
        origin_git(tmp.path(), &["ls-tree", "-r", "--name-only", native_ref])
            .lines()
            .any(|path| path == leaf),
        "the required native leaf exists before removal"
    );
    origin_git(tmp.path(), &["update-index", "--force-remove", &leaf]);
    let tree = origin_git(tmp.path(), &["write-tree"]);
    let changed = origin_git(
        tmp.path(),
        &["commit-tree", &tree, "-m", "fixture missing native leaf"],
    );
    origin_git(tmp.path(), &["update-ref", native_ref, &changed]);
    let hybrid = GixSubstrate::open(tmp.path()).unwrap();
    assert_eq!(hybrid.checkpoints().unwrap()[0].dialect, Dialect::Rift);
    assert_eq!(hybrid.checkpoints().unwrap()[0].checkpoint_id, *id);
    assert_eq!(
        hybrid.read_artifact(id, "metadata.json").unwrap().unwrap(),
        metadata
    );
    assert_eq!(
        hybrid.read_artifact(id, "full.jsonl").unwrap().unwrap(),
        full
    );
    assert!(
        !origin_git(tmp.path(), &["ls-tree", "-r", "--name-only", native_ref])
            .lines()
            .any(|p| p == leaf)
    );
    let store = SqliteStore::open_in_memory().unwrap();
    store.create_canonical_schema().unwrap();
    let actual = rift_shadow::ingest_from_substrate(&store, &hybrid, "repo", "claude-code", 100)
        .await
        .unwrap();
    assert_eq!(
        (
            actual.shards_projected,
            actual.refused.len(),
            store.count("checkpoint").await.unwrap(),
            store.count("turn_boundary_gap_batch").await.unwrap(),
            store.count("turn_boundary_gap_ticket").await.unwrap()
        ),
        (0, 1, 0, 0, 0),
        "a complete root must come from the native origin alone"
    );
}

fn incomplete_native_tip(repo: &Path, original: &str, id: &str, missing: &str) -> String {
    origin_git(repo, &["read-tree", original]);
    let path = format!("{}/{}/{missing}", &id[..2], &id[2..]);
    assert!(
        origin_git(repo, &["ls-tree", "-r", "--name-only", original])
            .lines()
            .any(|p| p == path)
    );
    origin_git(repo, &["update-index", "--force-remove", &path]);
    let tree = origin_git(repo, &["write-tree"]);
    origin_git(
        repo,
        &[
            "commit-tree",
            &tree,
            "-m",
            "fixture incomplete native snapshot",
        ],
    )
}

fn assert_native_tip_membership(
    repo: &Path,
    original: &str,
    tip: &str,
    id: &str,
    missing: &str,
    present: &str,
) {
    let prefix = format!("{}/{}", &id[..2], &id[2..]);
    let names = origin_git(repo, &["ls-tree", "-r", "--name-only", tip]);
    assert!(!names.lines().any(|p| p == format!("{prefix}/{missing}")));
    assert!(names.lines().any(|p| p == format!("{prefix}/{present}")));
    for name in [
        "metadata.json",
        "turn_boundary_gaps.jsonl",
        "redaction_report.json",
        present,
    ] {
        let actual = origin_git(repo, &["rev-parse", &format!("{tip}:{prefix}/{name}")]);
        let expected = origin_git(repo, &["rev-parse", &format!("{original}:{prefix}/{name}")]);
        assert_eq!(actual, expected, "unchanged native {name}");
    }
}

async fn incomplete_tip_refuses_with_a_fresh_reader(repo: &Path, tip: &str) {
    use rift_recall_substrate::Dialect;
    use rift_store::{CanonicalStore, SqliteStore};
    origin_git(repo, &["update-ref", Dialect::Rift.checkpoint_ref(), tip]);
    let source = GixSubstrate::open(repo).unwrap();
    let store = SqliteStore::open_in_memory().unwrap();
    store.create_canonical_schema().unwrap();
    let report = rift_shadow::ingest_from_substrate(&store, &source, "repo", "claude-code", 100)
        .await
        .unwrap();
    assert_eq!((report.shards_projected, report.refused.len()), (0, 1));
    for table in [
        "checkpoint",
        "turn_boundary_gap_batch",
        "turn_boundary_gap_ticket",
        "session",
        "capture_session_identity",
        "captured_event",
    ] {
        assert_eq!(store.count(table).await.unwrap(), 0);
    }
}

#[tokio::test]
async fn a_gap_cannot_be_assembled_across_two_incomplete_native_tips() {
    use rift_recall_substrate::Dialect;
    use rift_store::{CanonicalStore, SqliteStore};
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let archive = origin_gap_archive();
    native_backend(tmp.path())
        .archive_checkpoint(&archive)
        .unwrap();
    let complete = GixSubstrate::open(tmp.path()).unwrap();
    let control = SqliteStore::open_in_memory().unwrap();
    control.create_canonical_schema().unwrap();
    let report =
        rift_shadow::ingest_from_substrate(&control, &complete, "repo", "claude-code", 100)
            .await
            .unwrap();
    assert_eq!((report.shards_projected, report.refused.len()), (1, 0));
    let native_ref = Dialect::Rift.checkpoint_ref();
    let original = origin_git(tmp.path(), &["rev-parse", native_ref]);
    let id = &archive.checkpoint_id;
    let a = incomplete_native_tip(tmp.path(), &original, id, "full.jsonl");
    let b = incomplete_native_tip(tmp.path(), &original, id, "content.redacted");
    assert_ne!(a, b);
    assert_native_tip_membership(
        tmp.path(),
        &original,
        &a,
        id,
        "full.jsonl",
        "content.redacted",
    );
    assert_native_tip_membership(
        tmp.path(),
        &original,
        &b,
        id,
        "content.redacted",
        "full.jsonl",
    );
    incomplete_tip_refuses_with_a_fresh_reader(tmp.path(), &a).await;
    incomplete_tip_refuses_with_a_fresh_reader(tmp.path(), &b).await;
    origin_git(tmp.path(), &["update-ref", native_ref, &a]);
    let moving = MovingNativeTip {
        source: GixSubstrate::open(tmp.path()).unwrap(),
        repo: tmp.path().to_owned(),
        second_tip: b.clone(),
        switched: false.into(),
    };
    let store = SqliteStore::open_in_memory().unwrap();
    store.create_canonical_schema().unwrap();
    let report = rift_shadow::ingest_from_substrate(&store, &moving, "repo", "claude-code", 100)
        .await
        .unwrap();
    assert!(moving.switched.get());
    assert_eq!(origin_git(tmp.path(), &["rev-parse", native_ref]), b);
    eprintln!(
        "GAP_NATIVE_TIP_FIXTURE {}",
        serde_json::json!({"checkpoint_id":id,"complete_tip":original,"first_tip_missing_full":a,"second_tip_missing_content":b})
    );
    assert_eq!(
        (report.shards_projected, report.refused.len()),
        (0, 1),
        "neither immutable native tip contains the authenticated complete shard"
    );
    for table in [
        "checkpoint",
        "turn_boundary_gap_batch",
        "turn_boundary_gap_ticket",
        "session",
        "capture_session_identity",
        "captured_event",
    ] {
        assert_eq!(store.count(table).await.unwrap(), 0);
    }
}

fn legacy_native_root_and_ids(source: &impl SubstrateReader, id: &str) -> (String, Vec<String>) {
    use rift_shadow::backend::native_read::{shard_header, NativeArtifacts};
    let header = shard_header(source, id).unwrap();
    let native = NativeArtifacts::read(source, id, &header).unwrap();
    let report = source
        .read_artifact(id, "redaction_report.json")
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
    (
        rift_shadow::backend::shard_merkle_root(&leaves).to_hex(),
        rift_shadow::condense::checkpoint_ids_for_shard(&header.sessions, 3, &leaves),
    )
}

#[test]
fn deleting_a_writer_emitted_empty_original_refuses_gap_binding_despite_unchanged_hashes() {
    use rift_recall_substrate::Dialect;
    use rift_shadow::backend::native_read::load_native_boundary_gaps;
    use rift_shadow::backend::{ArchivedOriginal, OriginalState};
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut archive = origin_gap_archive();
    let packs = rift_shadow_redact::RulePackSet::builtin();
    // An empty body can be gated and written by the typed backend, but is NOT
    // a valid owned original envelope. This is only a content/presence proof,
    // never a positive claim about CLI original-result rendering.
    let empty =
        rift_shadow::transcript::sealed_original(&SecretBytes::new(vec![]), &packs).unwrap();
    assert!(rift_shadow::backend::rewrite::parse_original_record(empty.as_slice()).is_none());
    let (mut pair, _) = rift_shadow::archived_transcript(
        &SecretBytes::new(br#"{"type":"user","message":"safe"}"#.to_vec()),
        0,
        &packs,
    )
    .unwrap();
    pair.originals.push(ArchivedOriginal {
        tool_use_id: "empty".into(),
        agent_id: None,
        form: None,
        replaced: vec![],
        line: None,
        rewritten: None,
        state: OriginalState::Sealed,
        body: Some(empty.into()),
    });
    archive.transcript = Some(pair);
    archive.checkpoint_id = archive.derived_checkpoint_id().unwrap();
    native_backend(tmp.path())
        .archive_checkpoint(&archive)
        .unwrap();
    let present = GixSubstrate::open(tmp.path()).unwrap();
    let id = &archive.checkpoint_id;
    assert_eq!(
        present
            .read_native_artifact(id, "tool-originals/empty.json")
            .unwrap(),
        Some(vec![])
    );
    let shard = present.checkpoints().unwrap().remove(0);
    let gap = archive.boundary_gaps.as_ref().unwrap().as_bytes();
    assert!(load_native_boundary_gaps(&present, &shard, gap).is_ok());
    let binding = legacy_native_root_and_ids(&present, id);
    assert_eq!(binding, (archive.merkle_root().to_hex(), vec![id.clone()]));
    let original = origin_git(tmp.path(), &["rev-parse", Dialect::Rift.checkpoint_ref()]);
    let missing_tip = incomplete_native_tip(tmp.path(), &original, id, "tool-originals/empty.json");
    assert_native_tip_membership(
        tmp.path(),
        &original,
        &missing_tip,
        id,
        "tool-originals/empty.json",
        "full.jsonl",
    );
    origin_git(
        tmp.path(),
        &["update-ref", Dialect::Rift.checkpoint_ref(), &missing_tip],
    );
    let missing = GixSubstrate::open(tmp.path()).unwrap();
    assert_eq!(
        missing
            .read_native_artifact(id, "tool-originals/empty.json")
            .unwrap(),
        None
    );
    assert_eq!(legacy_native_root_and_ids(&missing, id), binding);
    assert!(load_native_boundary_gaps(&missing, &shard, gap).is_err());
}

#[test]
fn native_snapshot_pins_initial_absence_or_a_complete_tree_until_reader_reopen() {
    use rift_recall_substrate::Dialect;
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let archive = origin_gap_archive();
    let id = &archive.checkpoint_id;
    let absent = GixSubstrate::open(tmp.path()).unwrap();
    assert_eq!(absent.read_native_artifact(id, "full.jsonl").unwrap(), None);
    native_backend(tmp.path())
        .archive_checkpoint(&archive)
        .unwrap();
    assert_eq!(absent.read_native_artifact(id, "full.jsonl").unwrap(), None);
    assert_eq!(
        absent.read_artifact(id, "full.jsonl").unwrap(),
        archive.full_jsonl()
    );
    assert_eq!(absent.checkpoints().unwrap().len(), 1);
    let pinned = GixSubstrate::open(tmp.path()).unwrap();
    assert_eq!(
        pinned.read_native_artifact(id, "full.jsonl").unwrap(),
        archive.full_jsonl()
    );
    let original = origin_git(tmp.path(), &["rev-parse", Dialect::Rift.checkpoint_ref()]);
    let missing = incomplete_native_tip(tmp.path(), &original, id, "full.jsonl");
    origin_git(
        tmp.path(),
        &["update-ref", Dialect::Rift.checkpoint_ref(), &missing],
    );
    assert_eq!(
        pinned.read_native_artifact(id, "full.jsonl").unwrap(),
        archive.full_jsonl()
    );
    assert_eq!(pinned.read_artifact(id, "full.jsonl").unwrap(), None);
    let reopened = GixSubstrate::open(tmp.path()).unwrap();
    assert_eq!(
        reopened.read_native_artifact(id, "full.jsonl").unwrap(),
        None
    );
}

#[tokio::test]
async fn a_gap_carried_only_by_the_legacy_ref_cannot_claim_native_authentication() {
    use rift_recall_substrate::Dialect;
    use rift_store::{CanonicalStore, SqliteStore};
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let archive = origin_gap_archive();
    native_backend(tmp.path())
        .archive_checkpoint(&archive)
        .unwrap();
    let native = GixSubstrate::open(tmp.path()).unwrap();
    let control = SqliteStore::open_in_memory().unwrap();
    control.create_canonical_schema().unwrap();
    let positive =
        rift_shadow::ingest_from_substrate(&control, &native, "repo", "claude-code", 100)
            .await
            .unwrap();
    assert_eq!((positive.shards_projected, positive.refused.len()), (1, 0));
    let id = &archive.checkpoint_id;
    let metadata = native.read_native_artifact(id, "metadata.json").unwrap();
    let gap = native
        .read_native_artifact(id, "turn_boundary_gaps.jsonl")
        .unwrap();
    let original = origin_git(tmp.path(), &["rev-parse", Dialect::Rift.checkpoint_ref()]);
    origin_git(
        tmp.path(),
        &["update-ref", Dialect::Legacy.checkpoint_ref(), &original],
    );
    origin_git(
        tmp.path(),
        &["update-ref", "-d", Dialect::Rift.checkpoint_ref()],
    );
    let legacy = GixSubstrate::open(tmp.path()).unwrap();
    assert_eq!(
        legacy.checkpoints().unwrap().first().unwrap().dialect,
        Dialect::Legacy
    );
    assert_eq!(legacy.read_artifact(id, "metadata.json").unwrap(), metadata);
    assert_eq!(
        legacy
            .read_artifact(id, "turn_boundary_gaps.jsonl")
            .unwrap(),
        gap
    );
    assert_eq!(
        legacy.read_native_artifact(id, "metadata.json").unwrap(),
        None
    );
    let store = SqliteStore::open_in_memory().unwrap();
    store.create_canonical_schema().unwrap();
    let report = rift_shadow::ingest_from_substrate(&store, &legacy, "repo", "claude-code", 100)
        .await
        .unwrap();
    assert_eq!((report.shards_projected, report.refused.len()), (0, 1));
    for table in [
        "checkpoint",
        "turn_boundary_gap_batch",
        "turn_boundary_gap_ticket",
        "session",
        "capture_session_identity",
        "captured_event",
    ] {
        assert_eq!(store.count(table).await.unwrap(), 0);
    }
}

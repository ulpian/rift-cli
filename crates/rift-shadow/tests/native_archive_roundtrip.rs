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
        repo: tmp.path().to_path_buf(),
        commit: "2".repeat(40),
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
        repo: repo.to_path_buf(),
        commit: "3".repeat(40),
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

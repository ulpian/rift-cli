//! The native archive writes a shard the recall plane reads back (P7 story 17):
//! a native-produced checkpoint round-trips through the production `gix` substrate
//! reader and the artifact bridge, byte-for-byte. This is the cross-implementation
//! `verify` in the direction we can exercise without the pinned binary (native
//! produces → gix reads); it also pins determinism and the redaction-gate survival.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::Path;

use rift_core::SecretBytes;
use rift_recall_substrate::{GixSubstrate, SubstrateReader};
use rift_shadow::backend::{CheckpointArchive, RedactionReportView};
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
    // date). BYTE parity's prerequisite (P7 story 20).
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
    // any artifact byte on the branch (P7 story 5/15).
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
        merkle_root: rift_shadow::checkpoint_merkle_root(&[]),
        events: Vec::new(),
        content: out.redacted,
        report: RedactionReportView::from_redact(&out.report),
        created_at_ms: 1_700_000_000_000,
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

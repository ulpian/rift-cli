//! End-to-end: a checkpoint minted on an **unborn HEAD** is a first-class
//! record. A condense never needs a commit (L-0A §1.1, Q78): the shard names
//! no anchor — the `commit` member is absent, never `""` — and every reader
//! takes the absence as the answer. The anchorless shard verifies, rebuilds a
//! projection from an empty cache with no `checkpoint_commit` row and no
//! issue, and round-trips through `bundle export`/`import`/`rebuild`/`verify`.
//!
//! The condense road here is the sweep (`doctor --fix` over a crashed
//! session), which is one of the three condense roads and the one that needs
//! nothing of the worker. The user's HEAD stays unborn throughout.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    checkpoint_ref_exists, doctor_fix_json, family_projection_db, git_in, only_checkpoint_id,
    rift_json, seed_session, session_phase, sessions_leg, shard_blob, shard_path_of,
    sync_env_with_data,
};

const CHECKPOINTS_REF: &str = "refs/heads/rift/checkpoints/v1";

/// A repository with no commit and one crashed session, condensed by the
/// sweep into an anchorless checkpoint: `(repo, cache, data)`.
fn anchorless_repo(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-q", "-b", "main", "."]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git_in(&repo, &["add", "a.txt"]);
    seed_session(&repo, "crashed-1", 0);
    let cache = tmp.join("cache");
    let data = tmp.join("data");
    let env = sync_env_with_data(&cache, &data);
    let doc = doctor_fix_json(&repo, &env);
    let sessions = sessions_leg(&doc);
    assert_eq!(
        sessions["finalized"],
        serde_json::json!(["crashed-1"]),
        "the sweep condensed the crashed session with no commit to anchor to: {sessions}"
    );
    assert_eq!(session_phase(&repo, "crashed-1"), "ended");
    assert!(checkpoint_ref_exists(&repo));
    assert!(
        !Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success(),
        "the user's HEAD is still unborn"
    );
    (repo, cache, data)
}

/// Rows of `sql`'s first column over the family's capture projection.
fn projection_count(repo: &Path, cache: &Path, table: &str) -> i64 {
    use rift_store::CanonicalStore as _;
    let store = rift_store::open_capture_projection(family_projection_db(repo, cache)).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let rows = rt
        .block_on(store.query(&format!("SELECT COUNT(*) FROM {table}"), &[]))
        .unwrap();
    rows[0].get_i64(0).unwrap()
}

/// The shard's `metadata.json` names no `commit` member at all — the
/// absent-when-empty rule every optional member of the document follows —
/// and `verify` is green over it.
#[test]
fn an_anchorless_shard_omits_the_commit_member_and_verifies() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cache, data) = anchorless_repo(tmp.path());
    let env = sync_env_with_data(&cache, &data);

    let metadata: serde_json::Value =
        serde_json::from_str(&shard_blob(&repo, &shard_path_of(&repo, "metadata.json"))).unwrap();
    assert!(
        metadata.get("commit").is_none(),
        "an anchorless shard writes no commit member: {metadata}"
    );
    assert_eq!(metadata["sessions"], serde_json::json!(["crashed-1"]));
    assert_eq!(metadata["strategy"], "manual-commit");

    let verify = rift_json(&repo, &["--json", "verify"], &env);
    assert!(
        verify.get("error").is_none(),
        "an anchorless shard verifies like any other: {verify}"
    );
    assert_eq!(only_checkpoint_id(&repo).len(), 64);
}

/// From an **empty cache**, `rift rebuild` projects the anchorless shard with
/// no parse issue, no `checkpoint_commit` row and no invented anchor.
#[test]
fn an_anchorless_shard_rebuilds_from_an_empty_cache_with_no_commit_row() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, _cache, data) = anchorless_repo(tmp.path());
    let fresh = tmp.path().join("fresh-cache");
    let env = sync_env_with_data(&fresh, &data);

    let rebuilt = rift_json(&repo, &["--json", "rebuild"], &env);
    assert_eq!(rebuilt["parse_issues"], 0, "{rebuilt}");
    assert_eq!(rebuilt["shards_projected"], 1, "{rebuilt}");
    assert_eq!(projection_count(&repo, &fresh, "checkpoint"), 1);
    assert_eq!(
        projection_count(&repo, &fresh, "checkpoint_commit"),
        0,
        "absence is the answer: no anchor row is invented"
    );
    let verify = rift_json(&repo, &["--json", "verify"], &env);
    assert!(verify.get("error").is_none(), "{verify}");
}

/// The offline bundle carries the anchorless shard into a fresh repository
/// whose refs come out identical, and the destination rebuilds and verifies
/// with zero commit rows — the same round trip an anchored record makes.
#[test]
fn an_anchorless_record_round_trips_through_a_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cache, data) = anchorless_repo(tmp.path());
    let env = sync_env_with_data(&cache, &data);
    let src_ref = git_in(&repo, &["rev-parse", CHECKPOINTS_REF])
        .trim()
        .to_owned();

    let out = tmp.path().join("x.riftbundle");
    let exported = rift_json(
        &repo,
        &["--json", "bundle", "export", "--out", out.to_str().unwrap()],
        &env,
    );
    assert!(exported["links"].as_u64().unwrap() >= 1, "{exported}");

    let target = tmp.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    git_in(&target, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(target.join(".rift")).unwrap();
    std::fs::write(
        target.join(".rift/config.toml"),
        "schema_version = 1\n[project]\nname = \"target\"\n",
    )
    .unwrap();
    let tcache = tmp.path().join("tcache");
    let tenv = sync_env_with_data(&tcache, &data);
    let imported = rift_json(
        &target,
        &["--json", "bundle", "import", out.to_str().unwrap()],
        &tenv,
    );
    assert_eq!(
        imported["applied"].as_array().unwrap().len(),
        2,
        "{imported}"
    );
    assert_eq!(
        git_in(&target, &["rev-parse", CHECKPOINTS_REF]).trim(),
        src_ref,
        "the checkpoint ref is identical after the round trip"
    );
    let rebuilt = rift_json(&target, &["--json", "rebuild"], &tenv);
    assert_eq!(rebuilt["parse_issues"], 0, "{rebuilt}");
    assert_eq!(projection_count(&target, &tcache, "checkpoint"), 1);
    assert_eq!(projection_count(&target, &tcache, "checkpoint_commit"), 0);
    let verify = rift_json(&target, &["--json", "verify"], &tenv);
    assert!(verify.get("error").is_none(), "{verify}");
}

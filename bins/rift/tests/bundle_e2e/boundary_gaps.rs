//! A native recovery audit travels as authenticated source provenance, not
//! destination work or evidence that any source ticket was retired.

use super::*;
use rift_core::boundary_gap::*;
use rift_recall_substrate::{GixSubstrate, SubstrateReader};
use rift_shadow::{fixture_archive, NativeBackend, ShadowBackend};
use rift_store::{CanonicalStore, SqliteStore};

fn anchor(repo: &Path) -> String {
    std::fs::write(repo.join("a.txt"), "safe anchor\n").unwrap();
    git_in(repo, &["add", "a.txt"]);
    let tree = git_in(repo, &["write-tree"]);
    let out = Command::new("git")
        .args([
            "commit-tree",
            tree.trim(),
            "-m",
            "fixed native audit anchor",
        ])
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", repo.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    git_in(repo, &["update-ref", "refs/heads/main", &id]);
    id
}

fn source_audit(repo: &Path) -> rift_shadow::backend::CheckpointArchive {
    let mut archive = fixture_archive("two_turn");
    repo.clone_into(&mut archive.repo);
    archive.commit = anchor(repo);
    let native = archive.sessions.first().unwrap().clone();
    archive
        .session_agents
        .insert(native.clone(), "claude-code".into());
    let tickets = [1u8, 2]
        .into_iter()
        .map(|n| {
            GapTicket::new(GapTicketFields {
                source_key: SourceKey::parse_hex(&format!("{n:02x}").repeat(32)).unwrap(),
                context_digest: ContextDigest::parse_hex(&format!("{:02x}", n + 2).repeat(32))
                    .unwrap(),
                ticket_digest: TicketDigest::parse_hex(&format!("{:02x}", n + 4).repeat(32))
                    .unwrap(),
                session_key: SessionKey::parse_hex(&format!("{:02x}", n + 6).repeat(32)).unwrap(),
                turn_ordinal: u32::from(n),
                boundary: BoundaryEdge::End,
                head_oid: Some(archive.commit.clone()),
                captured_at: 10,
                queued_at: 20,
                attempts: u32::from(n),
            })
            .unwrap()
        })
        .collect();
    let batch = GapBatch::new(
        rift_git::repo_id_for(repo).unwrap(),
        MemberKey::parse_hex(&"55".repeat(32)).unwrap(),
        EvidenceDigest::parse_hex(&"66".repeat(32)).unwrap(),
        30,
        tickets,
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

fn initialize_source(repo: &Path, cache: &Path) -> (String, Vec<u8>) {
    let archive = source_audit(repo);
    let writer = rift_git::PlumbingGitWrite::new(rift_git::discover(repo).unwrap());
    NativeBackend::new(writer)
        .archive_checkpoint(&archive)
        .unwrap();
    let (code, out, err) = rift(repo, cache, &["--json", "chain", "init"]);
    assert_eq!(code, 0, "chain init: {out}{err}");
    imported_repository_verifies(repo, cache);
    (
        archive.checkpoint_id,
        archive.boundary_gaps.unwrap().as_bytes().to_vec(),
    )
}

async fn assert_only_carrier_facts(target: &Path, expected: &[GapEnvelope]) {
    let source = GixSubstrate::open(target).unwrap();
    let store = SqliteStore::open_in_memory().unwrap();
    store.create_canonical_schema().unwrap();
    let report =
        rift_shadow::ingest_from_substrate(&store, &source, "destination", "claude-code", 100)
            .await
            .unwrap();
    assert_eq!(
        (
            report.shards_projected,
            report.refused.len(),
            report.parse_issues
        ),
        (1, 0, 0)
    );
    assert_eq!(
        rift_store::boundary_gap_batch(&store, expected.first().unwrap().batch().gap_id())
            .await
            .unwrap()
            .as_ref(),
        Some(expected.first().unwrap().batch())
    );
    for table in [
        "session",
        "capture_session_identity",
        "session_in_checkpoint",
        "checkpoint",
    ] {
        assert_eq!(store.count(table).await.unwrap(), 1);
    }
    assert_eq!(store.count("turn_boundary_gap_ticket").await.unwrap(), 2);
    for table in [
        "turn_diff_hunk",
        "attribution",
        "attribution_hunk",
        "token_usage",
    ] {
        assert_eq!(store.count(table).await.unwrap(), 0);
    }
}

fn cli_projection(target: &Path, cache: &Path) -> Option<SqliteStore> {
    let paths = rift_git::discover(target).unwrap();
    let lookup = |key: &str| (key == "RIFT_CACHE_DIR").then(|| cache.to_str().unwrap().to_owned());
    let rift_git::FamilyState::Minted(family) =
        rift_git::family::resolve_family_under(lookup, &paths, rift_git::MintPolicy::Never)
            .unwrap()
    else {
        return None;
    };
    let db = rift_store::projection_db_for_family_under(cache, &family);
    db.exists()
        .then(|| SqliteStore::open_read_only(&db).unwrap())
}

async fn actual_cli_facts(
    target: &Path,
    cache: &Path,
) -> Vec<(&'static str, Vec<rift_store::Row>)> {
    let Some(store) = cli_projection(target, cache) else {
        return Vec::new();
    };
    let mut facts = vec![];
    for table in [
        "turn_boundary_gap_batch",
        "turn_boundary_gap_ticket",
        "checkpoint",
        "session",
        "capture_session_identity",
        "session_in_checkpoint",
        "captured_event",
        "turn",
        "turn_diff_hunk",
        "attribution",
        "attribution_hunk",
        "token_usage",
    ] {
        facts.push((
            table,
            store
                .query(&format!("SELECT * FROM {table} ORDER BY 1"), &[])
                .await
                .unwrap(),
        ));
    }
    facts
}

async fn assert_actual_cli_projection(target: &Path, cache: &Path, expected: &[GapEnvelope]) {
    let store =
        cli_projection(target, cache).expect("the actual CLI rebuild creates its projection");
    assert_eq!(
        rift_store::boundary_gap_batch(&store, expected.first().unwrap().batch().gap_id())
            .await
            .unwrap()
            .as_ref(),
        Some(expected.first().unwrap().batch())
    );
    for table in [
        "checkpoint",
        "session",
        "capture_session_identity",
        "session_in_checkpoint",
        "turn_boundary_gap_batch",
    ] {
        assert_eq!(store.count(table).await.unwrap(), 1);
    }
    assert_eq!(store.count("turn_boundary_gap_ticket").await.unwrap(), 2);
    assert_eq!(store.count("captured_event").await.unwrap(), 3);
    for table in [
        "turn_diff_hunk",
        "attribution",
        "attribution_hunk",
        "token_usage",
    ] {
        assert_eq!(store.count(table).await.unwrap(), 0);
    }
}

#[tokio::test]
async fn a_native_gap_bundle_preserves_source_claims_across_different_local_identities() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = empty_target(tmp.path(), "audit-source");
    let cache = tmp.path().join("audit-source-cache");
    let (id, bytes) = initialize_source(&repo, &cache);
    let expected = decode_checked(&bytes).unwrap();
    let source = GixSubstrate::open(&repo).unwrap();
    assert_eq!(
        source
            .read_native_artifact(&id, "turn_boundary_gaps.jsonl")
            .unwrap()
            .unwrap(),
        bytes
    );
    assert_eq!(
        expected.first().unwrap().batch().repo_id(),
        &rift_git::repo_id_for(&repo).unwrap()
    );
    let bundle = tmp.path().join("audit.riftbundle");
    export_whole_chain(&repo, &cache, &bundle);
    let target = empty_target(tmp.path(), "audit-destination");
    let target_cache = tmp.path().join("audit-target-cache");
    assert_ne!(
        rift_git::repo_id_for(&repo).unwrap(),
        rift_git::repo_id_for(&target).unwrap()
    );
    assert_ne!(
        rift_git::discover(&repo).unwrap().worktree_token(),
        rift_git::discover(&target).unwrap().worktree_token()
    );
    let checkpoint = ref_oid(&repo, CHECKPOINTS_REF).unwrap();
    let chain = ref_oid(&repo, CHAIN_REF).unwrap();
    assert!(actual_cli_facts(&target, &target_cache)
        .await
        .iter()
        .all(|(_, rows)| rows.is_empty()));
    first_import_carries_both_refs(&target, &target_cache, &bundle, &checkpoint, &chain);
    assert!(
        actual_cli_facts(&target, &target_cache)
            .await
            .iter()
            .all(|(_, rows)| rows.is_empty()),
        "refs-only import must not project any gap or native facts"
    );
    imported_repository_verifies(&target, &target_cache);
    assert_actual_cli_projection(&target, &target_cache, &expected).await;
    let projected = actual_cli_facts(&target, &target_cache).await;
    let imported = GixSubstrate::open(&target)
        .unwrap()
        .read_native_artifact(&id, "turn_boundary_gaps.jsonl")
        .unwrap()
        .unwrap();
    assert_eq!(imported, bytes);
    assert_eq!(decode_checked(&imported).unwrap(), expected);
    assert_only_carrier_facts(&target, &expected).await;
    re_import_applies_nothing(&target, &target_cache, &bundle, &checkpoint);
    assert_eq!(
        actual_cli_facts(&target, &target_cache).await,
        projected,
        "reimport is refs-only"
    );
    imported_repository_verifies(&target, &target_cache);
    assert_actual_cli_projection(&target, &target_cache, &expected).await;
    assert_eq!(
        actual_cli_facts(&target, &target_cache).await,
        projected,
        "real CLI rebuild replays facts without duplicates"
    );
    assert_eq!(
        GixSubstrate::open(&target)
            .unwrap()
            .read_native_artifact(&id, "turn_boundary_gaps.jsonl")
            .unwrap()
            .unwrap(),
        bytes
    );
}

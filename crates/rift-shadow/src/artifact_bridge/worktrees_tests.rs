//! The capture rebuild reads `session_worktrees.jsonl`: every frozen
//! observation of a session the projection does not already place is
//! restored, headless, into the relation its basis belongs to, with this
//! machine's checkout for path.

use std::path::Path;
use std::process::Command;

use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};
use rift_store::{record_session_worktree, CanonicalStore, SessionWorktreeRow, SqliteStore};

use crate::backend::{rfc3339_utc_ms, session_worktrees_jsonl_bytes, ArchivedWorktreeObservation};
use crate::ids;

use super::ingest_from_substrate;

const NATIVE: &str = "019a-worktree-native";
const REPO: &str = "repo-1";
const CREATED_AT: i64 = 1_790_000_000_000;
const REBUILD_NOW: i64 = 1_790_000_900_000;
const TOKEN: &str = "0123456789ab";

/// Every column of both relations but the path, in a fixed order.
const OBSERVATIONS: &str = "SELECT observation_id, session_id, worktree_token, worktree_name, \
     is_linked, branch, head_oid, subagent_id, basis, first_observed_at, schema_version \
     FROM {relation} ORDER BY observation_id";

fn store() -> SqliteStore {
    let s = SqliteStore::open_in_memory().unwrap();
    s.create_canonical_schema().unwrap();
    s
}

fn shard_id() -> String {
    "cf".repeat(32)
}

/// A header-only shard naming [`NATIVE`], carrying `artifact` as its
/// `session_worktrees.jsonl`.
fn shard_with(artifact: &[u8]) -> MemorySubstrate {
    let id = shard_id();
    let report = format!(
        r#"{{"total_spans":0,"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
        "c".repeat(64)
    );
    let metadata = format!(
        r#"{{"created_at":"{}","strategy":"manual-commit","merkle_root":"{}","sessions":["{NATIVE}"]}}"#,
        rfc3339_utc_ms(CREATED_AT),
        "d".repeat(64)
    );
    let shard = CheckpointShard {
        checkpoint_id: id.clone(),
        dialect: Dialect::Rift,
        prev: None,
        ordinal: 0,
    };
    MemorySubstrate::local()
        .with_shard(shard)
        .with_artifact(&id, "redaction_report.json", report.as_bytes())
        .with_artifact(&id, "metadata.json", metadata.as_bytes())
        .with_artifact(&id, "session_worktrees.jsonl", artifact)
}

/// An observation of [`NATIVE`] in the member `name` under `basis`.
fn seen(name: &str, basis: &str, at: i64) -> ArchivedWorktreeObservation {
    ArchivedWorktreeObservation {
        session: NATIVE.to_owned(),
        token: TOKEN.to_owned(),
        name: name.to_owned(),
        linked: name != "main",
        branch: Some("wt/a".to_owned()),
        subagent: None,
        basis: basis.to_owned(),
        at,
    }
}

/// The row a writer records for `seen` at `head`, at `work_tree`: the live
/// hook's with a head, the rebuild's without.
fn written_row(
    seen: &ArchivedWorktreeObservation,
    head: Option<&str>,
    work_tree: &str,
) -> SessionWorktreeRow {
    let session_id = ids::session_row_id("claude-code", NATIVE);
    let state_id = ids::session_worktree_observation_id(
        &session_id,
        &seen.token,
        seen.branch.as_deref(),
        head,
        seen.subagent.as_deref(),
    );
    SessionWorktreeRow {
        observation_id: ids::session_worktree_filed_id(&state_id, &seen.basis),
        session_id,
        worktree_token: seen.token.clone(),
        worktree_name: seen.name.clone(),
        is_linked: i64::from(seen.linked),
        work_tree: work_tree.to_owned(),
        branch: seen.branch.clone(),
        head_oid: head.map(str::to_owned),
        subagent_id: seen.subagent.clone(),
        basis: seen.basis.clone(),
        first_observed_at: seen.at,
        schema_version: 1,
    }
}

async fn rebuild(s: &SqliteStore, sub: &MemorySubstrate) -> super::BridgeReport {
    ingest_from_substrate(s, sub, REPO, "claude-code", REBUILD_NOW)
        .await
        .unwrap()
}

/// Every row of `relation` but its path, rendered.
async fn rows(s: &SqliteStore, relation: &str) -> Vec<String> {
    let sql = OBSERVATIONS.replace("{relation}", relation);
    let found = s.query(&sql, &[]).await.unwrap();
    found.iter().map(|r| format!("{r:?}")).collect()
}

async fn paths(s: &SqliteStore) -> Vec<String> {
    let found = s
        .query("SELECT work_tree FROM session_worktree", &[])
        .await
        .unwrap();
    found.iter().map(|r| r.get_text(0).unwrap()).collect()
}

/// ★ Into an empty projection a frozen observation restores the row a
/// writer with no head derives for the same state, id included, and a second
/// pass writes nothing; over a live projection that already places the
/// session, nothing is written, so no headless row sits beside a live one.
#[tokio::test]
async fn a_frozen_observation_fills_an_empty_projection_and_leaves_a_live_one_alone() {
    let observations = [
        seen("main", "payload_cwd", 10),
        seen("lane-a", "shell_cd", 20),
    ];
    let artifact = session_worktrees_jsonl_bytes(&observations).unwrap();
    let sub = shard_with(&artifact);

    let live = store();
    let head = "e".repeat(40);
    let row = written_row(&observations[0], Some(&head), "/live/checkout");
    assert!(record_session_worktree(&live, &row).await.unwrap());
    let before = rows(&live, "session_worktree").await;
    let report = rebuild(&live, &sub).await;
    assert_eq!(report.parse_issues, 0, "{:?}", report.parse_notes);
    assert_eq!(rows(&live, "session_worktree").await, before);
    assert_eq!(live.count("session_worktree_cd").await.unwrap(), 0);

    let rebuilt = store();
    rebuild(&rebuilt, &sub).await;
    let want = store();
    for observation in &observations {
        let row = written_row(observation, None, "");
        assert!(record_session_worktree(&want, &row).await.unwrap());
    }
    for relation in ["session_worktree", "session_worktree_cd"] {
        assert_eq!(rows(&rebuilt, relation).await, rows(&want, relation).await);
    }
    let first = rows(&rebuilt, "session_worktree").await;
    rebuild(&rebuilt, &sub).await;
    assert_eq!(rows(&rebuilt, "session_worktree").await, first, "no-op");
}

/// A `shell_cd` observation lands in `session_worktree_cd` and only there,
/// under the id that covers its basis.
#[tokio::test]
async fn a_shell_cd_observation_is_restored_into_the_cd_relation() {
    let cd = seen("lane-a", "shell_cd", 20);
    let sub = shard_with(&session_worktrees_jsonl_bytes(std::slice::from_ref(&cd)).unwrap());
    let rebuilt = store();
    rebuild(&rebuilt, &sub).await;
    assert_eq!(rebuilt.count("session_worktree").await.unwrap(), 0);
    let found = rebuilt
        .query("SELECT observation_id FROM session_worktree_cd", &[])
        .await
        .unwrap();
    let ids: Vec<String> = found.iter().map(|r| r.get_text(0).unwrap()).collect();
    assert_eq!(ids, [written_row(&cd, None, "").observation_id]);
}

/// A line carrying an undeclared member, or one the writer could not have
/// frozen, is a named issue and is skipped; its neighbours still land.
#[tokio::test]
async fn a_line_outside_the_grammar_is_a_named_issue_and_is_skipped() {
    let good = session_worktrees_jsonl_bytes(&[seen("main", "payload_cwd", 10)]).unwrap();
    let good = String::from_utf8(good).unwrap();
    let path = good.replace("\"at\":10", "\"at\":11,\"work_tree\":\"/Users/x\"");
    let basis = good.replace("payload_cwd", "guessed");
    let sub = shard_with(format!("{path}{basis}{good}").as_bytes());
    let rebuilt = store();
    let report = rebuild(&rebuilt, &sub).await;
    assert_eq!(report.parse_issues, 2);
    let notes = report.parse_notes.join("\n");
    assert!(notes.contains("session_worktrees.jsonl line 1 skipped: carries an undeclared member"));
    assert!(notes.contains("line 2 skipped: a member falls outside its declared shape"));
    assert!(!notes.contains("/Users/x"), "{notes}");
    assert_eq!(rebuilt.count("session_worktree").await.unwrap(), 1);
}

/// The path is this machine's checkout of the member the token names, and
/// the empty string when no member here carries the token.
#[tokio::test]
async fn the_path_resolves_to_this_machines_checkout_or_stays_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    git_init(&repo);
    let found = rift_git::discover(&repo).unwrap();
    let here = ArchivedWorktreeObservation {
        token: found.worktree_token(),
        ..seen("main", "payload_cwd", 10)
    };
    let elsewhere = ArchivedWorktreeObservation {
        token: "ffffffffffff".to_owned(),
        ..seen("lane-z", "payload_cwd", 20)
    };
    let artifact = session_worktrees_jsonl_bytes(&[here, elsewhere]).unwrap();
    let sub = shard_with(&artifact).with_worktree_root(&repo);
    let rebuilt = store();
    rebuild(&rebuilt, &sub).await;
    let mut found = paths(&rebuilt).await;
    found.sort();
    let canonical = std::fs::canonicalize(&repo).unwrap();
    assert_eq!(found, ["", canonical.to_string_lossy().as_ref()]);
}

/// An artifact with no line restores no observation and names no issue.
#[tokio::test]
async fn an_empty_artifact_restores_nothing() {
    let sub = shard_with(b"");
    let rebuilt = store();
    let report = rebuild(&rebuilt, &sub).await;
    assert_eq!(report.parse_issues, 0);
    assert_eq!(rebuilt.count("session_worktree").await.unwrap(), 0);
}

fn git_init(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let out = Command::new("git")
        .current_dir(dir)
        .args(["init", "-q", "-b", "main"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(out.status.success());
}

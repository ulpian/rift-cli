//! A pull's claim survives the shard: through the shipped binary, a pull that
//! named a held revision books its claim row, the session's end freezes the
//! claim into the checkpoint's `serve_audit.jsonl`, and a cold recall rebuild
//! from the shards alone writes the same claim row back.
//!
//! A file of its own because the claim is the one serve fact the serve-audit
//! suite's round trip does not cover, and that suite is near its ceiling.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};
use std::process::Stdio;

mod common;
use common::rift_cmd;

#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;

use hooks_fixture::{
    checkpoint_shards, rift_json, seed_live_session_repo, shard_blob_bytes, sync_env, LIVE_SESSION,
};
use rift_store::{CanonicalStore, SqliteStore};

/// A revision the caller says it holds, which no read in this family records.
const HELD: &str = "abababababababababababababababababababababababababababababababab";

/// A repository with a live session, Recall on and set up on this machine.
fn served_session_repo(tmp: &Path) -> (PathBuf, PathBuf) {
    let repo = seed_live_session_repo(tmp);
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift/config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    let cache = tmp.join("cache");
    common::provision_here(&repo, &cache);
    (repo, cache)
}

/// Pull `known-state` through the shipped CLI naming one held revision, and
/// hand back the answer and the decision's key it printed.
fn pull_naming_a_held_revision(repo: &Path, env: &[(&str, &str)]) -> (String, String) {
    let args = [
        "recall",
        "known-state",
        "--task",
        "t",
        "--file",
        "a.txt",
        "--known-hash",
        HELD,
    ];
    let mut cmd = rift_cmd(repo);
    cmd.args(args).stdin(Stdio::null()).stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    let audit_id = stdout
        .lines()
        .find_map(|line| line.strip_prefix("serve: "))
        .and_then(|keys| keys.split_once(' '))
        .map_or_else(
            || panic!("the answer names its keys: {stdout}"),
            |(_, audit_id)| audit_id.to_owned(),
        );
    (stdout, audit_id)
}

/// Claude Code's `PostToolUse` for the Bash call that ran the pull, which is
/// what attributes the serve to the session.
fn attribute_to_the_session(repo: &Path, stdout: &str, env: &[(&str, &str)]) {
    let command = format!("rift recall known-state --task t --file a.txt --known-hash {HELD}");
    let payload = serde_json::json!({
        "session_id": LIVE_SESSION,
        "hook_event_name": "PostToolUse",
        "cwd": repo,
        "tool_name": "Bash",
        "tool_input": { "command": command, "description": "Ask Rift what changed" },
        "tool_response": { "stdout": stdout, "stderr": "", "interrupted": false, "isImage": false },
        "tool_use_id": "toolu_e2e_pull_claim",
    });
    hooks_fixture::agent_hook(
        repo,
        "PostToolUse",
        LIVE_SESSION,
        1,
        &payload.to_string(),
        env,
    );
}

/// Every claim row a projection holds, every column.
fn claim_rows(store: &SqliteStore) -> Vec<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let sql = "SELECT audit_id || '|' || stated_count || '|' || recorded_count || '|' || \
               unrecorded_count || '|' || basis || '|' || schema_version \
               FROM memd_audit_pull_claim ORDER BY audit_id";
    rt.block_on(store.query(sql, &[]))
        .unwrap()
        .iter()
        .map(|row| row.get_text(0).unwrap())
        .collect()
}

/// A cold recall rebuild: a fresh projection that has seen nothing but the
/// checkpoint shards the repository holds, ingested by the ledger.
fn cold_rebuild(repo: &Path) -> SqliteStore {
    let fresh = SqliteStore::open_in_memory().unwrap();
    rift_store::apply_migrations(&fresh, &rift_store::recall_migrations()).unwrap();
    let substrate = rift_recall_substrate::GixSubstrate::open(repo).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let report = rt
        .block_on(rift_recall_ledger::ingest::ingest(&fresh, &substrate))
        .unwrap();
    assert!(report.issues.is_empty(), "{:?}", report.issues);
    fresh
}

/// ★ **A pull's claim, condensed and rebuilt.** The live projection books the
/// claim of a pull that named one held revision; the session's end freezes it
/// into the shard beside the serve; and a cold rebuild from the shards alone
/// holds the same claim row — the same key, the same three counts, the same
/// rule.
#[test]
fn a_pulls_claim_condensed_into_the_shard_comes_back_on_a_cold_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, cache) = served_session_repo(tmp.path());
    let env = sync_env(&cache);

    let (stdout, audit_id) = pull_naming_a_held_revision(&repo, &env);
    attribute_to_the_session(&repo, &stdout, &env);
    let repo_id = rift_store::recall_repo_id(&repo).unwrap();
    let db = rift_store::recall_db_under(&cache, &repo_id);
    let live =
        claim_rows(&rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap());
    let [row] = live.as_slice() else {
        panic!("one claim row: {live:?}");
    };
    assert!(row.starts_with(&format!("{audit_id}|1|0|1|")), "{row}");

    rift_json(&repo, &["session", "stop", LIVE_SESSION, "--json"], &env);
    let [checkpoint] = checkpoint_shards(&repo).try_into().unwrap();
    let frozen = String::from_utf8(shard_blob_bytes(
        &repo,
        &format!("{checkpoint}/serve_audit.jsonl"),
    ))
    .unwrap();
    let claimed: Vec<serde_json::Value> = frozen
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|line| line["idempotency_key"] == audit_id.as_str())
        .collect();
    let [decision] = claimed.as_slice() else {
        panic!("the pull's decision is frozen once: {frozen}");
    };
    assert_eq!(decision["payload"]["pull_claim"]["stated"], 1, "{decision}");

    assert_eq!(claim_rows(&cold_rebuild(&repo)), live);
}

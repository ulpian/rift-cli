//! The capture rebuild reads `sessions.jsonl`: every session a shard names is
//! rebuilt under its own agent, with the ids the live path writes for it.

use std::collections::BTreeMap;

use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};
use rift_shadow_hooks::{AdapterRegistry, AgentId, RawHook};
use rift_store::{CanonicalStore, SqliteStore};

use crate::backend::{rfc3339_utc_ms, sessions_jsonl_bytes, usage_jsonl_bytes, ArchivedUsageFact};
use crate::bridge::ingest_hook;

use super::ingest_from_substrate;

const NATIVE: &str = "019a-codex-native";
const REPO: &str = "repo-1";
/// The shard's own capture time.
const CREATED_AT: i64 = 1_790_000_000_000;
/// When the rebuild runs: later, and never a row's date.
const REBUILD_NOW: i64 = 1_790_000_900_000;

fn store() -> SqliteStore {
    let s = SqliteStore::open_in_memory().unwrap();
    s.create_canonical_schema().unwrap();
    s
}

fn shard_id() -> String {
    "ce".repeat(32)
}

/// A header-only shard naming [`NATIVE`], with no agent artifact.
fn shard_without_agents() -> MemorySubstrate {
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
}

/// The same shard, with the archive's own `sessions.jsonl` naming `agent`.
fn shard_naming(agent: &str) -> MemorySubstrate {
    let agents = BTreeMap::from([(NATIVE.to_owned(), agent.to_owned())]);
    let bytes = sessions_jsonl_bytes(&agents).unwrap();
    shard_without_agents().with_artifact(&shard_id(), "sessions.jsonl", &bytes)
}

async fn rows(s: &SqliteStore, sql: &str) -> Vec<String> {
    let found = s.query(sql, &[]).await.unwrap();
    found.iter().map(|r| format!("{r:?}")).collect()
}

async fn session_rows(s: &SqliteStore) -> Vec<(String, String, String, i64)> {
    let found = s
        .query(
            "SELECT session_id, agent, model_primary, started_at FROM session ORDER BY session_id",
            &[],
        )
        .await
        .unwrap();
    found
        .iter()
        .map(|r| {
            (
                r.get_text(0).unwrap(),
                r.get_text(1).unwrap(),
                r.get_text(2).unwrap(),
                r.get_i64(3).unwrap(),
            )
        })
        .collect()
}

/// The session id and identity row the live hook path writes for a Codex
/// session, read back from a projection that path wrote.
async fn live_codex_ids() -> (String, Vec<String>) {
    let live = store();
    let tmp = tempfile::tempdir().unwrap();
    let raw = RawHook {
        agent: AgentId::CODEX,
        verb: "SessionStart".into(),
        session: NATIVE.into(),
        turn: None,
        occurred_at_ms: 100,
        payload: serde_json::json!({}),
    };
    let registry = AdapterRegistry::with_builtin();
    ingest_hook(&raw, &registry, &live, tmp.path(), "codex", REPO, 100, None)
        .await
        .unwrap();
    let session = live
        .query("SELECT session_id FROM session", &[])
        .await
        .unwrap();
    let id = session.first().unwrap().get_text(0).unwrap();
    let identity = rows(
        &live,
        "SELECT session_id, agent, native_session_id FROM capture_session_identity",
    )
    .await;
    (id, identity)
}

#[tokio::test]
async fn a_codex_session_rebuilds_under_the_ids_the_live_path_writes() {
    let (live_id, live_identity) = live_codex_ids().await;
    assert_eq!(live_id, crate::ids::session_row_id("codex", NATIVE));

    let rebuilt = store();
    let report = ingest_from_substrate(
        &rebuilt,
        &shard_naming("codex"),
        REPO,
        "claude-code",
        REBUILD_NOW,
    )
    .await
    .unwrap();

    assert_eq!(report.parse_issues, 0);
    assert_eq!(
        session_rows(&rebuilt).await,
        vec![(
            live_id.clone(),
            "codex".to_owned(),
            String::new(),
            CREATED_AT
        )],
        "a session with no usage is dated by its shard and names no model"
    );
    assert_eq!(
        rows(
            &rebuilt,
            "SELECT session_id, agent, native_session_id FROM capture_session_identity"
        )
        .await,
        live_identity
    );
    let linked = rows(&rebuilt, "SELECT session_id FROM session_in_checkpoint").await;
    assert_eq!(
        linked,
        rows(&rebuilt, "SELECT session_id FROM session").await
    );
}

#[tokio::test]
async fn a_shard_without_the_agent_artifact_rebuilds_exactly_as_before() {
    let rebuilt = store();
    ingest_from_substrate(
        &rebuilt,
        &shard_without_agents(),
        REPO,
        "claude-code",
        REBUILD_NOW,
    )
    .await
    .unwrap();

    assert!(
        session_rows(&rebuilt).await.is_empty(),
        "no usage, no session row"
    );
    let linked = rebuilt
        .query("SELECT session_id FROM session_in_checkpoint", &[])
        .await
        .unwrap();
    assert_eq!(
        linked.first().unwrap().get_text(0).unwrap(),
        crate::ids::session_row_id("claude-code", NATIVE),
        "the walk's default agent keys a session the shard names no agent for"
    );
}

/// A usage line names the model and the session's earliest instant, and the
/// usage line's model wins over the empty one the session line would give.
#[tokio::test]
async fn a_usage_line_still_dates_and_names_a_session_its_agent_line_keys() {
    let usage = ArchivedUsageFact {
        session: NATIVE.to_owned(),
        turn: 1,
        call_id: "call-1".to_owned(),
        model: "gpt-5-codex".to_owned(),
        at: CREATED_AT - 5_000,
        uncached_input: 20,
        cached_input: 0,
        cache_creation: 0,
        output: 2,
        method: "provider_reported".to_owned(),
    };
    let sub = shard_naming("codex").with_artifact(
        &shard_id(),
        "usage.jsonl",
        &usage_jsonl_bytes(&[usage]).unwrap(),
    );
    let rebuilt = store();
    ingest_from_substrate(&rebuilt, &sub, REPO, "claude-code", REBUILD_NOW)
        .await
        .unwrap();

    let id = crate::ids::session_row_id("codex", NATIVE);
    assert_eq!(
        session_rows(&rebuilt).await,
        vec![(
            id.clone(),
            "codex".to_owned(),
            "gpt-5-codex".to_owned(),
            CREATED_AT - 5_000
        )]
    );
    let turn = rows(&rebuilt, "SELECT session_id FROM turn").await;
    assert_eq!(turn, rows(&rebuilt, "SELECT session_id FROM session").await);
}

/// An agent outside the first-party vocabulary is frozen as `withheld`, and
/// the reader skips that line: the session keeps the walk's default agent and
/// the skip is named.
#[tokio::test]
async fn a_withheld_agent_is_skipped_and_named() {
    let rebuilt = store();
    let report = ingest_from_substrate(
        &rebuilt,
        &shard_naming("my-agent"),
        REPO,
        "claude-code",
        REBUILD_NOW,
    )
    .await
    .unwrap();

    assert_eq!(report.parse_issues, 1);
    assert!(
        report
            .parse_notes
            .iter()
            .any(|n| n.contains("sessions.jsonl line 1")),
        "{:?}",
        report.parse_notes
    );
    assert!(session_rows(&rebuilt).await.is_empty());
    let linked = rebuilt
        .query("SELECT session_id FROM session_in_checkpoint", &[])
        .await
        .unwrap();
    assert_eq!(
        linked.first().unwrap().get_text(0).unwrap(),
        crate::ids::session_row_id("claude-code", NATIVE)
    );
}

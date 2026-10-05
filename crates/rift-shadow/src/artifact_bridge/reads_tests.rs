//! The capture rebuild reads the denominator's members off `tool_calls.jsonl`:
//! a read captured through the live hook path, frozen the way the archive
//! freezes it, rebuilds into the same four rows.

use std::collections::BTreeMap;
use std::path::Path;

use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};
use rift_shadow_hooks::{AdapterRegistry, AgentId, RawHook};
use rift_store::{session_read_facts, CanonicalStore, SqliteStore};

use crate::backend::{sessions_jsonl_bytes, tool_calls_jsonl_bytes, ArchivedReadFact};
use crate::bridge::ingest_hook;
use crate::project::{ReadContent, TokenEstimate};

use super::ingest_from_substrate;

const SESSION: &str = "01JREBUILDREADS";
const AGENT: &str = "claude-code";
const REPO: &str = "repo-1";
const OID: &str = "0123456789abcdef0123456789abcdef01234567";
const REBUILD_NOW: i64 = 1_790_000_900_000;

/// The four relations the savings denominator joins a read through, each
/// dumped whole and in a fixed order so two projections compare field by field.
const DENOMINATOR_ROWS: [&str; 4] = [
    "SELECT tool_call_id, turn_id, tool_name, started_at, schema_version FROM tool_call \
     ORDER BY tool_call_id",
    "SELECT tool_call_id, file_revision_id FROM tool_reads_file ORDER BY tool_call_id",
    "SELECT file_revision_id, path, blob_oid, content_sha256, schema_version FROM file_revision \
     ORDER BY file_revision_id",
    "SELECT tool_call_id, input_tokens, tokenizer, schema_version FROM file_read_tokens \
     ORDER BY tool_call_id",
];

fn store() -> SqliteStore {
    let s = SqliteStore::open_in_memory().unwrap();
    s.create_canonical_schema().unwrap();
    s
}

async fn hook(s: &SqliteStore, git_dir: &Path, verb: &str, at: i64, payload: serde_json::Value) {
    let raw = RawHook {
        agent: AgentId::CLAUDE_CODE,
        verb: verb.into(),
        session: SESSION.into(),
        turn: None,
        occurred_at_ms: at,
        payload,
    };
    let hasher = |path: &str| {
        Some(ReadContent {
            rel_path: path.to_owned(),
            blob_oid: OID.into(),
            content_sha256: "ab".repeat(32),
            tokens: Some(TokenEstimate {
                input_tokens: 420,
                tokenizer: "cl100k_base".into(),
            }),
        })
    };
    let registry = AdapterRegistry::with_builtin();
    ingest_hook(&raw, &registry, s, git_dir, AGENT, REPO, at, Some(&hasher))
        .await
        .unwrap();
}

/// One turn that reads one file, through the live hook path.
async fn live_read(s: &SqliteStore) {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path().join("git");
    hook(s, &git_dir, "SessionStart", 100, serde_json::json!({})).await;
    let prompt = serde_json::json!({"prompt": "go"});
    hook(s, &git_dir, "UserPromptSubmit", 200, prompt).await;
    let read = serde_json::json!({"tool_name": "Read", "tool_input": {"file_path": "src/lib.rs"}});
    hook(s, &git_dir, "PostToolUse", 300, read).await;
}

/// The read facts as the archiving hook maps them.
async fn frozen_facts(live: &SqliteStore) -> Vec<ArchivedReadFact> {
    let row_id = crate::ids::session_row_id(AGENT, SESSION);
    let facts = session_read_facts(live, &row_id).await.unwrap();
    assert!(!facts.is_empty(), "the live path recorded a read");
    facts
        .into_iter()
        .map(|f| ArchivedReadFact {
            tool_use_id: f.tool_call_id,
            tool_name: f.tool_name,
            path: f.path,
            content_sha256: f.content_sha256,
            input_tokens: f.input_tokens,
            at: Some(f.started_at),
            turn: Some(f.turn_ordinal),
            blob_oid: Some(f.blob_oid),
            tokenizer: f.tokenizer,
        })
        .collect()
}

fn shard_id() -> String {
    "ad".repeat(32)
}

/// A shard naming [`SESSION`] under its agent, carrying `tool_calls`.
fn shard_with(tool_calls: &[u8]) -> MemorySubstrate {
    let id = shard_id();
    let report = format!(
        r#"{{"total_spans":0,"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
        "c".repeat(64)
    );
    let metadata = format!(
        r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["{SESSION}"]}}"#,
        "d".repeat(64)
    );
    let agents = BTreeMap::from([(SESSION.to_owned(), AGENT.to_owned())]);
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
        .with_artifact(
            &id,
            "sessions.jsonl",
            &sessions_jsonl_bytes(&agents).unwrap(),
        )
        .with_artifact(&id, "tool_calls.jsonl", tool_calls)
}

async fn dump(s: &SqliteStore) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for sql in DENOMINATOR_ROWS {
        let rows = s.query(sql, &[]).await.unwrap();
        out.push(rows.iter().map(|r| format!("{r:?}")).collect());
    }
    out
}

#[tokio::test]
async fn a_read_rebuilt_from_its_shard_line_matches_the_live_rows_field_for_field() {
    let live = store();
    live_read(&live).await;
    let bytes = tool_calls_jsonl_bytes(&frozen_facts(&live).await).unwrap();

    let rebuilt = store();
    let report = ingest_from_substrate(&rebuilt, &shard_with(&bytes), REPO, AGENT, REBUILD_NOW)
        .await
        .unwrap();

    assert_eq!(report.parse_issues, 0);
    let rows = dump(&live).await;
    assert!(rows.iter().all(|relation| relation.len() == 1), "{rows:?}");
    assert_eq!(dump(&rebuilt).await, rows);
    let turn = rebuilt
        .query("SELECT session_id FROM turn", &[])
        .await
        .unwrap();
    assert_eq!(
        turn.first().unwrap().get_text(0).unwrap(),
        crate::ids::session_row_id(AGENT, SESSION),
        "the read's turn hangs off the session the shard names"
    );
}

/// A line written before the members existed is still the recall plane's read
/// fact, and it names no row this reader may write.
#[tokio::test]
async fn an_old_read_line_projects_nothing_and_is_not_an_issue() {
    let old = ArchivedReadFact {
        tool_use_id: "call-1".into(),
        tool_name: "Read".into(),
        path: "src/lib.rs".into(),
        content_sha256: "ab".repeat(32),
        input_tokens: Some(420),
        at: None,
        turn: None,
        blob_oid: None,
        tokenizer: None,
    };
    let bytes = tool_calls_jsonl_bytes(&[old]).unwrap();
    let rebuilt = store();
    let report = ingest_from_substrate(&rebuilt, &shard_with(&bytes), REPO, AGENT, REBUILD_NOW)
        .await
        .unwrap();

    assert_eq!(report.parse_issues, 0);
    assert!(dump(&rebuilt).await.iter().all(Vec::is_empty));
}

/// The recall plane's reader accepts the new members: they would otherwise
/// cost every new shard its read facts.
#[test]
fn the_recall_reader_still_reads_a_line_with_the_new_members() {
    let fact = ArchivedReadFact {
        tool_use_id: "call-1".into(),
        tool_name: "Read".into(),
        path: "src/lib.rs".into(),
        content_sha256: "ab".repeat(32),
        input_tokens: Some(420),
        at: Some(300),
        turn: Some(1),
        blob_oid: Some(OID.into()),
        tokenizer: Some("cl100k_base".into()),
    };
    let bytes = tool_calls_jsonl_bytes(&[fact]).unwrap();
    let out = rift_recall_substrate::parse::parse_tool_calls(&bytes, "shard");
    assert!(out.issues.is_empty(), "{:?}", out.issues);
    assert_eq!(out.value.len(), 1);
    assert_eq!(out.value[0].path.as_deref(), Some("src/lib.rs"));
    assert_eq!(out.value[0].input_tokens, Some(420));
}

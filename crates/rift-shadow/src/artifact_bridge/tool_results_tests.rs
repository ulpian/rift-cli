//! The capture rebuild reads `tool_results.jsonl`: every frozen measured
//! tool result is restored under its own id, its session keyed under the
//! agent the shard names, at the instant it froze; a live row wins.

use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};
use rift_store::{record_tool_result_tokens, CanonicalStore, SqliteStore};

use crate::backend::{rfc3339_utc_ms, tool_results_jsonl_bytes, ArchivedToolResult};
use crate::ids;

use super::ingest_from_substrate;

const NATIVE: &str = "019a-search-native";
const REPO: &str = "repo-1";
const CREATED_AT: i64 = 1_790_000_000_000;
const REBUILD_NOW: i64 = 1_790_000_900_000;

/// Every column of the relation, in a fixed order.
const ROWS: &str = "SELECT tool_result_id, session_id, tool_name, channel, output_tokens, \
     tokenizer, basis, recorded_at, schema_version FROM tool_result_tokens \
     ORDER BY tool_result_id";

fn store() -> SqliteStore {
    let s = SqliteStore::open_in_memory().unwrap();
    s.create_canonical_schema().unwrap();
    s
}

fn shard_id() -> String {
    "ce".repeat(32)
}

/// A header-only shard naming [`NATIVE`] as a Codex session, carrying
/// `artifact` as its `tool_results.jsonl` when one is given.
fn shard_with(artifact: Option<&[u8]>) -> MemorySubstrate {
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
    let agents = format!(r#"{{"agent":"codex","schema_version":1,"session":"{NATIVE}"}}"#);
    let shard = CheckpointShard {
        checkpoint_id: id.clone(),
        dialect: Dialect::Rift,
        prev: None,
        ordinal: 0,
    };
    let sub = MemorySubstrate::local()
        .with_shard(shard)
        .with_artifact(&id, "redaction_report.json", report.as_bytes())
        .with_artifact(&id, "metadata.json", metadata.as_bytes())
        .with_artifact(&id, "sessions.jsonl", format!("{agents}\n").as_bytes());
    match artifact {
        Some(bytes) => sub.with_artifact(&id, "tool_results.jsonl", bytes),
        None => sub,
    }
}

/// A measured search of [`NATIVE`] under the id `n` repeated.
fn searched(n: char, tool: &str, basis: &str, tokens: u32) -> ArchivedToolResult {
    ArchivedToolResult {
        id: n.to_string().repeat(64),
        session: NATIVE.to_owned(),
        tool: tool.to_owned(),
        channel: "search".to_owned(),
        output_tokens: tokens,
        tokenizer: "cl100k_base".to_owned(),
        basis: basis.to_owned(),
        recorded_at: CREATED_AT - 5_000,
    }
}

/// Record `result` as the live worker records it, under Codex's session row.
async fn record_live(s: &SqliteStore, result: &ArchivedToolResult) {
    let session_row = ids::session_row_id("codex", NATIVE);
    let recorded = record_tool_result_tokens(
        s,
        &result.id,
        &session_row,
        &result.tool,
        &result.channel,
        result.output_tokens,
        &result.tokenizer,
        &result.basis,
        result.recorded_at,
        1,
    );
    assert!(recorded.await.unwrap());
}

async fn rebuild(s: &SqliteStore, sub: &MemorySubstrate) -> super::BridgeReport {
    ingest_from_substrate(s, sub, REPO, "claude-code", REBUILD_NOW)
        .await
        .unwrap()
}

/// Every row of the relation, rendered.
async fn rows(s: &SqliteStore) -> Vec<String> {
    let found = s.query(ROWS, &[]).await.unwrap();
    found.iter().map(|r| format!("{r:?}")).collect()
}

/// ★ A live row and the same row rebuilt from its shard are equal field for
/// field: the frozen id, the session under the shard's own agent rather than
/// the walk's default, and the frozen instant rather than the rebuild clock.
/// A second pass writes nothing.
#[tokio::test]
async fn a_live_row_and_the_row_rebuilt_from_its_shard_are_field_for_field_equal() {
    let results = [
        searched('a', "Glob", "in_process", 64),
        searched('b', "Bash", "unmeasured", 0),
    ];
    let live = store();
    for result in &results {
        record_live(&live, result).await;
    }
    let artifact = tool_results_jsonl_bytes(&results).unwrap();
    let sub = shard_with(Some(&artifact));

    let rebuilt = store();
    let report = rebuild(&rebuilt, &sub).await;
    assert_eq!(report.parse_issues, 0, "{:?}", report.parse_notes);
    assert_eq!(rows(&rebuilt).await, rows(&live).await);
    rebuild(&rebuilt, &sub).await;
    assert_eq!(rows(&rebuilt).await, rows(&live).await, "no-op");
}

/// Levelling into the live projection writes nothing: every frozen id is
/// already a live row, and the live row wins.
#[tokio::test]
async fn levelling_into_the_live_projection_leaves_its_rows_alone() {
    let result = searched('a', "Grep", "reexecuted", 12);
    let live = store();
    record_live(&live, &result).await;
    let before = rows(&live).await;
    let edited = ArchivedToolResult {
        output_tokens: 99,
        ..result
    };
    let artifact = tool_results_jsonl_bytes(&[edited]).unwrap();
    rebuild(&live, &shard_with(Some(&artifact))).await;
    assert_eq!(rows(&live).await, before);
}

/// A shard written before the artifact existed restores no tool result.
#[tokio::test]
async fn a_shard_without_the_artifact_restores_no_tool_result() {
    let rebuilt = store();
    let report = rebuild(&rebuilt, &shard_with(None)).await;
    assert_eq!(report.parse_issues, 0, "{:?}", report.parse_notes);
    assert_eq!(rebuilt.count("tool_result_tokens").await.unwrap(), 0);
}

/// A line outside the grammar is a named parse issue and skipped; a line
/// whose tokenizer was withheld restores nothing; the good line lands.
#[tokio::test]
async fn a_refused_or_withheld_line_restores_nothing_and_the_rest_lands() {
    let good = tool_results_jsonl_bytes(&[searched('a', "Glob", "in_process", 5)]).unwrap();
    let withheld = ArchivedToolResult {
        tokenizer: "o200k_base".to_owned(),
        ..searched('b', "Glob", "in_process", 7)
    };
    let withheld = tool_results_jsonl_bytes(&[withheld]).unwrap();
    let foreign = String::from_utf8(good.clone())
        .unwrap()
        .replace("\"search\"", "\"web\"");
    let undeclared = String::from_utf8(good.clone())
        .unwrap()
        .replace("\"tool\":", "\"path\":\"src/lib.rs\",\"tool\":");
    let artifact = [
        good.as_slice(),
        &withheld,
        foreign.as_bytes(),
        undeclared.as_bytes(),
    ]
    .concat();
    let rebuilt = store();
    let report = rebuild(&rebuilt, &shard_with(Some(&artifact))).await;
    assert_eq!(report.parse_issues, 2, "{:?}", report.parse_notes);
    assert!(report
        .parse_notes
        .iter()
        .all(|n| n.contains("tool_results.jsonl") && !n.contains("src/lib.rs")));
    let ids: Vec<String> = rebuilt
        .query("SELECT tool_result_id FROM tool_result_tokens", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get_text(0).unwrap())
        .collect();
    assert_eq!(ids, ["a".repeat(64)]);
}

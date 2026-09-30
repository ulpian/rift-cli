//! The capture rebuild reads `usage.jsonl`: a turn settled through the live
//! hook path, frozen the way the archive freezes it, and rebuilt from the
//! shard alone into a second projection.

use std::path::Path;

use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate, SubstrateReader as _};
use rift_shadow_hooks::{AdapterRegistry, AgentId, RawHook};
use rift_store::{record_price, CanonicalStore, PriceTableRow, SqliteStore};

use crate::backend::{usage_jsonl_bytes, ArchivedUsageFact};
use crate::bridge::{ingest_hook, settle_closed_turn_usage, SettleMode};

use super::ingest_from_substrate;

const SESSION: &str = "01JREBUILDUSAGE";
const AGENT: &str = "claude-code";
const REPO: &str = "repo-1";
const PRICED: &str = "claude-fable-5";

/// The settle's clock: after the catalog row was recorded, so the call prices.
const SETTLE_NOW: i64 = 1_790_000_000_000;
/// The rebuild's clock, later again: a rebuild never runs at settle time.
const REBUILD_NOW: i64 = 1_790_000_500_000;

/// The relations one settled session writes through the live path.
const SESSION_RELATIONS: &[&str] = &[
    "session",
    "capture_session_identity",
    "turn",
    "model_call",
    "token_usage",
    "cost_fact",
    "captured_event",
    "session_event",
    "tool_call",
    "model_observation",
    "main_context_observation",
    "subagent_session",
];

fn store() -> SqliteStore {
    let s = SqliteStore::open_in_memory().unwrap();
    s.create_canonical_schema().unwrap();
    s
}

/// A projection whose catalog prices [`PRICED`] in all four classes, the way
/// the migration seed does in a real one.
async fn priced_store() -> SqliteStore {
    let s = store();
    for (class, micros) in [
        ("uncached_input", 10_000_000),
        ("cached_input", 1_000_000),
        ("cache_creation", 12_500_000),
        ("output", 50_000_000),
    ] {
        let row = PriceTableRow {
            price_id: format!("test-{PRICED}-{class}"),
            model: PRICED.to_owned(),
            token_class: class.to_owned(),
            unit_price_micros: micros,
            currency: "USD".to_owned(),
            valid_from: 0,
            valid_to: None,
            recorded_at: 1_784_505_600_000,
            supersedes_id: None,
            schema_version: 1,
        };
        record_price(&s, &row).await.unwrap();
    }
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
    let registry = AdapterRegistry::with_builtin();
    ingest_hook(&raw, &registry, s, git_dir, AGENT, REPO, at, None)
        .await
        .unwrap();
}

async fn settle(s: &SqliteStore, git_dir: &Path, mode: SettleMode) {
    settle_closed_turn_usage(s, git_dir, SESSION, AGENT, REPO, mode, SETTLE_NOW)
        .await
        .unwrap();
}

/// One turn through the live path, each hook at its own instant, with the
/// transcript written after the Stop and the turn settled on `model`'s call.
async fn live_session(s: &SqliteStore, model: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path().join("git");
    let transcript = tmp.path().join("t.jsonl");
    let path = transcript.display().to_string();
    hook(
        s,
        &git_dir,
        "SessionStart",
        100,
        serde_json::json!({"transcript_path": path}),
    )
    .await;
    let after = SettleMode::AfterHook {
        turn_closed_here: false,
    };
    settle(s, &git_dir, after).await;
    hook(
        s,
        &git_dir,
        "UserPromptSubmit",
        200,
        serde_json::json!({"prompt": "go"}),
    )
    .await;
    let tool = serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "ls"}});
    hook(s, &git_dir, "PreToolUse", 300, tool).await;
    hook(
        s,
        &git_dir,
        "Stop",
        400,
        serde_json::json!({"stop_hook_active": false}),
    )
    .await;
    let usage = serde_json::json!({"input_tokens": 20, "output_tokens": 7,
        "cache_read_input_tokens": 300, "cache_creation_input_tokens": 40});
    let lines = [
        serde_json::json!({"type": "user", "message": {"content": "go"}}),
        serde_json::json!({"type": "assistant", "message": {"model": model, "usage": usage}}),
    ];
    let body: Vec<String> = lines.iter().map(ToString::to_string).collect();
    std::fs::write(&transcript, body.join("\n") + "\n").unwrap();
    settle(s, &git_dir, SettleMode::Final).await;
}

/// The shard the archive writes for [`SESSION`]: the header, the redaction
/// report, and `usage.jsonl` frozen from the live projection's rows by the
/// mapping the archiving hook applies. No `full.jsonl`: the native archive
/// threads no event stream, so a real shard carries none.
async fn shard_of(live: &SqliteStore) -> MemorySubstrate {
    let row_id = crate::ids::session_row_id(AGENT, SESSION);
    let facts = rift_store::capture::session_usage_facts(live, &row_id)
        .await
        .unwrap();
    let usage: Vec<ArchivedUsageFact> = facts
        .into_iter()
        .map(|f| ArchivedUsageFact {
            session: SESSION.to_owned(),
            turn: f.turn_ordinal,
            call_id: f.call_id,
            model: f.model,
            at: f.started_at,
            uncached_input: f.uncached_input,
            cached_input: f.cached_input,
            cache_creation: f.cache_creation,
            output: f.output,
            method: f.counter_method,
        })
        .collect();
    assert!(!usage.is_empty(), "the live settle recorded a call");
    header_only_shard().with_artifact(
        &shard_id(),
        "usage.jsonl",
        &usage_jsonl_bytes(&usage).unwrap(),
    )
}

fn shard_id() -> String {
    "ab".repeat(32)
}

fn header_only_shard() -> MemorySubstrate {
    let id = shard_id();
    let report = format!(
        r#"{{"total_spans":0,"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
        "c".repeat(64)
    );
    let metadata = format!(
        r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["{SESSION}"]}}"#,
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

/// Every row of `table`, each column rendered by SQLite's `quote` and paired
/// with its name, ordered by the first column.
async fn rows(s: &SqliteStore, table: &str, columns: &[&str]) -> Vec<Vec<(String, String)>> {
    let select: Vec<String> = columns.iter().map(|c| format!("quote({c})")).collect();
    let sql = format!("SELECT {} FROM {table} ORDER BY 1", select.join(", "));
    let found = s.query(&sql, &[]).await.unwrap();
    found
        .iter()
        .map(|row| {
            let values = (0..columns.len()).map(|i| row.get_text(i).unwrap());
            columns
                .iter()
                .map(|c| (*c).to_owned())
                .zip(values)
                .collect()
        })
        .collect()
}

/// The rows of `table` for the settled turn only: the live path also records
/// the session-start turn, which carries no call and so no line.
async fn settled_turn_rows(s: &SqliteStore) -> Vec<Vec<(String, String)>> {
    let columns = [
        "turn_id",
        "session_id",
        "ordinal",
        "started_at",
        "schema_version",
    ];
    let all = rows(s, "turn", &columns).await;
    all.into_iter()
        .filter(|row| row.iter().any(|(c, v)| c == "ordinal" && v == "1"))
        .collect()
}

fn without(rows: &[Vec<(String, String)>], excepted: &[&str]) -> Vec<Vec<(String, String)>> {
    rows.iter()
        .map(|row| {
            row.iter()
                .filter(|(c, _)| !excepted.contains(&c.as_str()))
                .cloned()
                .collect()
        })
        .collect()
}

fn column(rows: &[Vec<(String, String)>], name: &str) -> Vec<String> {
    rows.iter()
        .flat_map(|row| {
            row.iter()
                .filter(|(c, _)| c == name)
                .map(|(_, v)| v.clone())
        })
        .collect()
}

const SESSION_COLUMNS: [&str; 6] = [
    "session_id",
    "agent",
    "model_primary",
    "repo_id",
    "started_at",
    "schema_version",
];
const CALL_COLUMNS: [&str; 5] = [
    "call_id",
    "turn_id",
    "model",
    "started_at",
    "schema_version",
];
const USAGE_COLUMNS: [&str; 7] = [
    "call_id",
    "uncached_input",
    "cached_input",
    "cache_creation",
    "output",
    "counter_method",
    "schema_version",
];
const COST_COLUMNS: [&str; 8] = [
    "cost_fact_id",
    "call_id",
    "amount_micros",
    "valid_from",
    "valid_to",
    "recorded_at",
    "supersedes_id",
    "schema_version",
];

async fn rebuild(from: &MemorySubstrate, now_ms: i64) -> SqliteStore {
    let rebuilt = priced_store().await;
    let report = ingest_from_substrate(&rebuilt, from, REPO, AGENT, now_ms)
        .await
        .unwrap();
    assert_eq!(report.parse_issues, 0, "{}", report.render());
    rebuilt
}

async fn counts(s: &SqliteStore, tables: &[String]) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    for table in tables {
        out.push((table.clone(), s.count(table).await.unwrap()));
    }
    out
}

async fn every_table(s: &SqliteStore) -> Vec<String> {
    let sql = "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name";
    let found = s.query(sql, &[]).await.unwrap();
    found.iter().map(|r| r.get_text(0).unwrap()).collect()
}

fn session_relations() -> Vec<String> {
    SESSION_RELATIONS.iter().map(|t| (*t).to_owned()).collect()
}

/// The call, its usage and its cost come back column for column; the cost
/// fact's id and transaction time are the rebuild's clock where the live
/// ones are the settle's. The session and turn rows come back under the live
/// ids, but three of their columns are not in the artifact: the live session
/// is dated by its first hook and named by that hook's model, the live turn by
/// its prompt, and the shard records only the calls.
#[tokio::test]
async fn a_rebuild_from_the_shard_alone_reproduces_the_settled_calls_rows() {
    let live = priced_store().await;
    live_session(&live, PRICED).await;
    let rebuilt = rebuild(&shard_of(&live).await, REBUILD_NOW).await;

    for (table, columns) in [
        ("model_call", &CALL_COLUMNS[..]),
        ("token_usage", &USAGE_COLUMNS[..]),
    ] {
        let expected = rows(&live, table, columns).await;
        assert_eq!(expected.len(), 1, "{table}");
        assert_eq!(rows(&rebuilt, table, columns).await, expected, "{table}");
    }
    let usage = rows(&rebuilt, "token_usage", &USAGE_COLUMNS).await;
    assert_eq!(
        column(&usage, "cached_input"),
        ["300"],
        "every class travels"
    );

    let transaction_time = ["cost_fact_id", "recorded_at"];
    let live_cost = rows(&live, "cost_fact", &COST_COLUMNS).await;
    let rebuilt_cost = rows(&rebuilt, "cost_fact", &COST_COLUMNS).await;
    assert_eq!(live_cost.len(), 1, "the live settle priced the call");
    assert_eq!(
        without(&rebuilt_cost, &transaction_time),
        without(&live_cost, &transaction_time)
    );
    assert_eq!(column(&live_cost, "recorded_at"), [SETTLE_NOW.to_string()]);
    assert_eq!(
        column(&rebuilt_cost, "recorded_at"),
        [REBUILD_NOW.to_string()]
    );

    let not_in_the_artifact = ["model_primary", "started_at"];
    let live_session_row = rows(&live, "session", &SESSION_COLUMNS).await;
    let rebuilt_session_row = rows(&rebuilt, "session", &SESSION_COLUMNS).await;
    assert_eq!(
        without(&rebuilt_session_row, &not_in_the_artifact),
        without(&live_session_row, &not_in_the_artifact)
    );
    assert_eq!(
        column(&live_session_row, "started_at"),
        ["100"],
        "the live session opened at its first hook"
    );
    assert_eq!(
        column(&rebuilt_session_row, "started_at"),
        ["400"],
        "the rebuilt one at its first call"
    );
    assert_eq!(
        column(&rebuilt_session_row, "model_primary"),
        [format!("'{PRICED}'")]
    );

    let live_turn = settled_turn_rows(&live).await;
    let rebuilt_turn = settled_turn_rows(&rebuilt).await;
    assert_eq!(
        without(&rebuilt_turn, &["started_at"]),
        without(&live_turn, &["started_at"])
    );
    assert_eq!(
        column(&live_turn, "started_at"),
        ["200"],
        "the live turn opened at its prompt"
    );
    assert_eq!(
        column(&rebuilt_turn, "started_at"),
        ["400"],
        "the rebuilt one at its call"
    );
}

/// With the settle's clock the rebuilt cost fact is the live one whole, id and
/// all: the id is derived by the live rule, not a second one.
#[tokio::test]
async fn a_rebuild_on_the_settles_clock_reproduces_the_cost_fact_whole() {
    let live = priced_store().await;
    live_session(&live, PRICED).await;
    let rebuilt = rebuild(&shard_of(&live).await, SETTLE_NOW).await;
    let live_cost = rows(&live, "cost_fact", &COST_COLUMNS).await;
    assert_eq!(live_cost.len(), 1);
    assert_eq!(rows(&rebuilt, "cost_fact", &COST_COLUMNS).await, live_cost);
}

#[tokio::test]
async fn rebuilding_over_the_live_projection_inserts_nothing_and_neither_does_a_second_rebuild() {
    let live = priced_store().await;
    live_session(&live, PRICED).await;
    let shard = shard_of(&live).await;
    let before = counts(&live, &session_relations()).await;
    ingest_from_substrate(&live, &shard, REPO, AGENT, REBUILD_NOW)
        .await
        .unwrap();
    assert_eq!(
        counts(&live, &session_relations()).await,
        before,
        "a live row wins every key"
    );

    let tables = every_table(&live).await;
    let after_first = counts(&live, &tables).await;
    ingest_from_substrate(&live, &shard, REPO, AGENT, REBUILD_NOW + 1)
        .await
        .unwrap();
    assert_eq!(
        counts(&live, &tables).await,
        after_first,
        "a second rebuild inserts nothing"
    );
}

#[tokio::test]
async fn an_unpriced_model_gets_its_usage_rows_and_no_cost_fact_as_live() {
    let live = priced_store().await;
    live_session(&live, "claude-unpriced-9").await;
    let rebuilt = rebuild(&shard_of(&live).await, REBUILD_NOW).await;
    assert_eq!(
        live.count("cost_fact").await.unwrap(),
        0,
        "live priced nothing"
    );
    assert_eq!(rebuilt.count("cost_fact").await.unwrap(), 0);
    assert_eq!(
        rows(&rebuilt, "token_usage", &USAGE_COLUMNS).await,
        rows(&live, "token_usage", &USAGE_COLUMNS).await
    );
    assert_eq!(rebuilt.count("model_call").await.unwrap(), 1);
}

/// A shard written before the artifact projects its header and nothing of
/// usage, and the watermark still lands.
#[tokio::test]
async fn a_shard_without_usage_ingests_as_before_and_still_writes_the_watermark() {
    let rebuilt = priced_store().await;
    let report = ingest_from_substrate(&rebuilt, &header_only_shard(), REPO, AGENT, REBUILD_NOW)
        .await
        .unwrap();
    assert_eq!((report.shards_projected, report.parse_issues), (1, 0));
    assert_eq!(report.tip, Some(shard_id()));
    for table in ["session", "turn", "model_call", "token_usage", "cost_fact"] {
        assert_eq!(rebuilt.count(table).await.unwrap(), 0, "{table}");
    }
    assert_eq!(rebuilt.count("session_in_checkpoint").await.unwrap(), 1);
    assert_eq!(rebuilt.count("ingest_watermark").await.unwrap(), 1);
}

/// A refused line is named against its shard and never stops the walk: the
/// good line's rows land and the watermark is written.
#[tokio::test]
async fn a_refused_usage_line_is_named_and_the_walk_goes_on() {
    let live = priced_store().await;
    live_session(&live, PRICED).await;
    let id = shard_id();
    let frozen = shard_of(&live)
        .await
        .read_artifact(&id, "usage.jsonl")
        .unwrap()
        .unwrap();
    let mut bytes = b"{\"session\": 1}\n".to_vec();
    bytes.extend_from_slice(&frozen);
    let shard = header_only_shard().with_artifact(&id, "usage.jsonl", &bytes);
    let rebuilt = priced_store().await;
    let report = ingest_from_substrate(&rebuilt, &shard, REPO, AGENT, REBUILD_NOW)
        .await
        .unwrap();
    assert_eq!(report.parse_issues, 1);
    assert_eq!(
        report.parse_notes,
        [format!(
            "{id}: usage.jsonl line 1 skipped: misses member `schema_version`"
        )]
    );
    assert_eq!(rebuilt.count("token_usage").await.unwrap(), 1);
    assert_eq!(report.tip, Some(id));
}

/// The model of the second turn's call, which the catalog does not price.
const UNPRICED: &str = "claude-unpriced-9";

/// The columns the shard cannot carry, by relation: the session's opening
/// instant and primary model, the turn's opening instant, and the cost fact's
/// id and transaction time. Every other column of the five relations must
/// come back as the live settle wrote it.
const DECLARED_EXCEPTIONS: [(&str, &[&str]); 5] = [
    ("session", &["started_at", "model_primary"]),
    ("turn", &["started_at"]),
    ("model_call", &[]),
    ("token_usage", &[]),
    ("cost_fact", &["cost_fact_id", "recorded_at"]),
];

/// One assistant line of a transcript: `model`'s call, spending `output`.
fn assistant_line(model: &str, output: u32) -> String {
    let usage = serde_json::json!({"input_tokens": 20, "output_tokens": output,
        "cache_read_input_tokens": 300, "cache_creation_input_tokens": 40});
    serde_json::json!({"type": "assistant", "message": {"model": model, "usage": usage}})
        .to_string()
}

fn append_to(transcript: &Path, lines: &[String]) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(transcript)
        .unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}

/// Two turns through the live path, each settled the way the hooks settle
/// it: the first by the prompt that opens the second, the second by the
/// session's end. The first call is [`PRICED`], the second [`UNPRICED`].
async fn two_turn_session(s: &SqliteStore) {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = tmp.path().join("git");
    let transcript = tmp.path().join("t.jsonl");
    let path = transcript.display().to_string();
    let open = serde_json::json!({"transcript_path": path});
    let after = SettleMode::AfterHook {
        turn_closed_here: false,
    };
    let prompt = || serde_json::json!({"prompt": "go"});
    let stop = || serde_json::json!({"stop_hook_active": false});
    let user = serde_json::json!({"type": "user", "message": {"content": "go"}}).to_string();
    hook(s, &git_dir, "SessionStart", 100, open).await;
    settle(s, &git_dir, after).await;
    hook(s, &git_dir, "UserPromptSubmit", 200, prompt()).await;
    hook(s, &git_dir, "Stop", 400, stop()).await;
    append_to(&transcript, &[user.clone(), assistant_line(PRICED, 7)]);
    hook(s, &git_dir, "UserPromptSubmit", 500, prompt()).await;
    settle(s, &git_dir, after).await;
    hook(s, &git_dir, "Stop", 700, stop()).await;
    append_to(&transcript, &[user, assistant_line(UNPRICED, 11)]);
    settle(s, &git_dir, SettleMode::Final).await;
}

/// Every column of `table`, in declaration order, read off the schema so a
/// column added later is compared without this file changing.
async fn columns_of(s: &SqliteStore, table: &str) -> Vec<String> {
    let sql = format!("SELECT name FROM pragma_table_info('{table}') ORDER BY cid");
    let found = s.query(&sql, &[]).await.unwrap();
    found.iter().map(|r| r.get_text(0).unwrap()).collect()
}

/// Every row of `table` over every column; the turns only where a call
/// hangs off them, since the session-start turn carries none and no line.
async fn every_column(s: &SqliteStore, table: &str) -> Vec<Vec<(String, String)>> {
    let columns = columns_of(s, table).await;
    let names: Vec<&str> = columns.iter().map(String::as_str).collect();
    let all = rows(s, table, &names).await;
    if table != "turn" {
        return all;
    }
    let called = column(&rows(s, "model_call", &["turn_id"]).await, "turn_id");
    all.into_iter()
        .filter(|row| {
            row.iter()
                .any(|(c, v)| c == "turn_id" && called.contains(v))
        })
        .collect()
}

/// ★ Two calls in two turns, on two models, one of them unpriced: every
/// column of the session, its turns, its calls, their usage and the one cost
/// fact comes back from the shard as the live settle wrote it, except the
/// columns [`DECLARED_EXCEPTIONS`] names — so a column that starts to differ
/// fails here rather than passing unseen.
#[tokio::test]
async fn every_column_the_live_settle_wrote_comes_back_but_the_declared_exceptions() {
    let live = priced_store().await;
    two_turn_session(&live).await;
    let rebuilt = rebuild(&shard_of(&live).await, REBUILD_NOW).await;

    let expected_rows = [
        ("session", 1),
        ("turn", 2),
        ("model_call", 2),
        ("token_usage", 2),
        ("cost_fact", 1),
    ];
    for ((table, excepted), (counted, n)) in DECLARED_EXCEPTIONS.iter().zip(expected_rows) {
        assert_eq!(*table, counted);
        let live_rows = every_column(&live, table).await;
        let rebuilt_rows = every_column(&rebuilt, table).await;
        assert_eq!(live_rows.len(), n, "{table}: {live_rows:?}");
        assert_eq!(
            without(&rebuilt_rows, excepted),
            without(&live_rows, excepted),
            "{table}"
        );
    }
    let calls = every_column(&rebuilt, "model_call").await;
    let mut models = column(&calls, "model");
    models.sort();
    assert_eq!(models, [format!("'{PRICED}'"), format!("'{UNPRICED}'")]);
    let cost = every_column(&live, "cost_fact").await;
    let priced_call: Vec<String> = calls
        .iter()
        .filter(|row| {
            row.iter()
                .any(|(c, v)| c == "model" && *v == format!("'{PRICED}'"))
        })
        .flat_map(|row| column(std::slice::from_ref(row), "call_id"))
        .collect();
    assert_eq!(
        column(&cost, "call_id"),
        priced_call,
        "only the priced call"
    );
}

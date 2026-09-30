//! How much of a list-shaped pull answer is JSON structure rather than
//! content, measured on the answers the real dispatcher ships.
//!
//! A pull answer is charged at the accounting seam as the tokens of its
//! serialized payload, so every key, quote, brace and comma is paid for. Whether
//! that structure is a material share of the bill decides whether a compact
//! rendering is ever worth building, and the only honest way to answer it is to
//! split what ships: the payload's leaf values alone (every string and number,
//! in document order, one per line) are the content, and the rest of the count
//! is scaffolding.
//!
//! The corpus is the eval harness's deterministic one, seeded into an
//! in-memory recall projection through the stores' public writers, with a
//! fixed clock and fixed ids. Each scenario's changed files are the delta and
//! the unchanged ones are what the caller says it holds.
//!
//! It is test-only and lives in xtask because it is a measurement over several
//! crates' public surfaces rather than a property of any one of them; the
//! committed snapshot is the published figure, and it goes red when a payload's
//! shape moves.

use std::fmt::Write as _;

use rift_core::Timestamp;
use rift_recall_eval::{deterministic_corpus, Scenario};
use rift_recall_policy::{PolicyEngine, TurnBudget};
use rift_recall_serve::{RecallContext, ServeDispatcher, ToolName, ToolRequest, ToolResponse};
use rift_recall_substrate::MemorySubstrate;
use rift_recall_types::{NoAncestry, NoProvenance, Scope};
use rift_store::{apply_migrations, recall_migrations, CanonicalStore, SqliteStore, Value};
use serde_json::Value as Json;

/// The verbs whose answer is a list of per-file or per-hunk entries.
///
/// `file_if_changed` and `read_summary` answer one file with one small object;
/// `retrieve_exact` and `doc_read` ship one body of bytes; `cost_hotspots` is
/// an awareness list rather than a pull; `search` answers from an index this
/// fixture has no way to hold.
const LIST_SHAPED: [ToolName; 4] = [
    ToolName::KnownState,
    ToolName::DiffSince,
    ToolName::ChangedHunks,
    ToolName::ContextPack,
];

/// The injected clock, the same instant the eval harness serves at.
const NOW_MS: i64 = 1_000_000_000;

/// The one checkpoint every read and hunk hangs off.
const CHECKPOINT: &str = "c1";

/// One answer's count, split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Split {
    /// The tokens of the serialized payload, exactly as the seam counts them.
    shipped: u32,
    /// The tokens of the payload's leaf values alone.
    content: u32,
}

/// One measured answer: which verb, over which scenario, split how.
struct Row {
    tool: ToolName,
    scenario: String,
    split: Split,
    omitted: Option<u64>,
}

/// The projection and substrate one scenario seeds, and what a caller of it
/// holds and looks at.
struct Fixture {
    store: SqliteStore,
    reader: MemorySubstrate,
    held: Vec<String>,
    focus: String,
}

impl Split {
    fn of(payload: &Json) -> Self {
        Self {
            shipped: rift_tokens::count(&payload.to_string()),
            content: rift_tokens::count(&content_text(payload)),
        }
    }

    fn scaffolding(self) -> u32 {
        self.shipped.saturating_sub(self.content)
    }

    fn saturates(self) -> bool {
        self.content > self.shipped
    }
}

/// Every list-shaped verb's answer over every corpus scenario, in corpus order
/// and then verb order.
fn measure_corpus() -> Vec<Row> {
    deterministic_corpus()
        .iter()
        .flat_map(|scenario| {
            let fixture = seed(scenario);
            LIST_SHAPED.map(|tool| {
                let resp = serve(&fixture, tool, &request(tool, scenario, &fixture));
                let split = Split::of(&resp.payload);
                assert_eq!(split.shipped, resp.estimated_tokens_returned);
                Row {
                    tool,
                    scenario: scenario.name.clone(),
                    split,
                    omitted: resp.payload.get("omitted").and_then(Json::as_u64),
                }
            })
        })
        .collect()
}

/// One real dispatch, in a turn of its own, so no answer's cap depends on the
/// one served before it.
fn serve(fixture: &Fixture, tool: ToolName, req: &ToolRequest) -> ToolResponse {
    let cfg = rift_config::EffectiveConfig::secure_default().unwrap();
    let server = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let ctx = RecallContext {
        store: &fixture.store,
        reader: &fixture.reader,
        snapshots: None,
        // A synthetic fixture has no repository to walk and nothing to contain.
        scope: Scope::unbounded("eval"),
        ancestry: &NoAncestry,
        provenance: &NoProvenance,
        now: Timestamp::from_millis(NOW_MS),
    };
    let mut budget = TurnBudget::new(4, u32::MAX);
    block(server.dispatch(tool, req, &ctx, &mut budget)).unwrap()
}

/// The request an agent sends for each verb: the manifest verbs at their
/// default cap, the ranked hunk list without a basis, and the pack at the
/// scenario's budget.
fn request(tool: ToolName, scenario: &Scenario, fixture: &Fixture) -> ToolRequest {
    let ranked = ToolRequest {
        task: Some(format!("locate: {}", scenario.decisive_fact)),
        files_in_focus: vec![fixture.focus.clone()],
        ..Default::default()
    };
    match tool {
        ToolName::DiffSince => ToolRequest {
            known_hashes: fixture.held.clone(),
            ..Default::default()
        },
        ToolName::ChangedHunks => ranked,
        ToolName::ContextPack => ToolRequest {
            known_hashes: fixture.held.clone(),
            token_budget: scenario.token_budget,
            ..ranked
        },
        _ => ToolRequest {
            known_hashes: fixture.held.clone(),
            ..ranked
        },
    }
}

/// The scenario seeded the way the eval harness seeds it: one checkpoint, and
/// per file a revision, a read at its honest token count, its blob, and, for a
/// changed file, the hunks against its content without the decisive fact.
fn seed(scenario: &Scenario) -> Fixture {
    let store = SqliteStore::open_in_memory().unwrap();
    apply_migrations(&store, &recall_migrations()).unwrap();
    insert(&store, "n_checkpoint", &checkpoint_row());
    let mut reader = MemorySubstrate::local();
    let mut held = Vec::new();
    for (idx, file) in scenario.files.iter().enumerate() {
        let sha = rift_recall_substrate::worktree::sha256_hex(file.content.as_bytes());
        let rev = format!("rev_{idx:03}");
        let size = i64::try_from(file.content.len()).unwrap();
        insert(
            &store,
            "n_file_revision",
            &revision_row(&rev, &file.path, &sha, size),
        );
        let tokens = i64::from(rift_tokens::count(&file.content));
        insert(&store, "n_file_read", &read_row(&rev, tokens));
        if file.changed {
            let old = without_fact(&file.content, &scenario.decisive_fact);
            let old_rev = format!("rev_old_{idx:03}");
            let persist = rift_recall_ledger::hunks::persist_hunks(
                &store,
                CHECKPOINT,
                &old_rev,
                &rev,
                old.as_bytes(),
                file.content.as_bytes(),
            );
            block(persist).unwrap();
        } else {
            held.push(sha.clone());
        }
        reader = reader.with_blob(&sha, file.content.as_bytes());
    }
    let focus = scenario.files.iter().find(|f| f.changed);
    let focus = focus.map(|f| f.path.clone()).unwrap();
    Fixture {
        store,
        reader,
        held,
        focus,
    }
}

fn insert(store: &SqliteStore, table: &str, row: &[(&str, Value)]) {
    let columns: Vec<&str> = row.iter().map(|(column, _)| *column).collect();
    let values: Vec<Value> = row.iter().map(|(_, value)| value.clone()).collect();
    block(store.insert_or_ignore(table, &columns, &values)).unwrap();
}

fn checkpoint_row() -> Vec<(&'static str, Value)> {
    vec![
        ("node_id", Value::text(CHECKPOINT)),
        ("dialect", Value::text("rift")),
        ("ordinal", Value::Int(1)),
        ("prev_checkpoint", Value::Null),
        ("strategy", Value::Null),
        ("merkle_root", Value::Null),
        ("developer", Value::Null),
    ]
}

fn revision_row(rev: &str, path: &str, sha: &str, size: i64) -> Vec<(&'static str, Value)> {
    vec![
        ("node_id", Value::text(rev)),
        ("path", Value::text(path)),
        ("blob_oid", Value::text(sha)),
        ("content_sha256", Value::text(sha)),
        ("size", Value::Int(size)),
    ]
}

fn read_row(rev: &str, tokens: i64) -> Vec<(&'static str, Value)> {
    vec![
        ("node_id", Value::text(format!("read_{rev}"))),
        ("checkpoint_id", Value::text(CHECKPOINT)),
        ("tool_call_id", Value::text(format!("tc_{rev}"))),
        ("file_revision", Value::text(rev)),
        ("input_tokens", Value::Int(tokens)),
        ("read_ordinal", Value::Int(1)),
        ("byte_range_lo", Value::Null),
        ("byte_range_hi", Value::Null),
    ]
}

fn without_fact(content: &str, fact: &str) -> String {
    content
        .lines()
        .filter(|line| !line.contains(fact))
        .fold(String::new(), |mut acc, line| {
            acc.push_str(line);
            acc.push('\n');
            acc
        })
}

/// The payload's leaf values alone: every string as its text and every number
/// as its digits, in the order the payload serializes (keys sorted), one per
/// line. Keys, quotes, punctuation, booleans and nulls are the structure and
/// are dropped.
fn content_text(payload: &Json) -> String {
    let mut leaves = Vec::new();
    push_leaves(payload, &mut leaves);
    leaves.join("\n")
}

fn push_leaves(value: &Json, acc: &mut Vec<String>) {
    match value {
        Json::String(text) => acc.push(text.clone()),
        Json::Number(number) => acc.push(number.to_string()),
        Json::Array(items) => items.iter().for_each(|item| push_leaves(item, acc)),
        Json::Object(fields) => fields.values().for_each(|field| push_leaves(field, acc)),
        Json::Bool(_) | Json::Null => (),
    }
}

/// The published table: every answer, then every verb over the corpus with
/// the range its scenarios span.
fn render(rows: &[Row]) -> String {
    let mut out =
        String::from("verb | scenario | shipped | content | scaffolding | share | omitted\n");
    for row in rows {
        let omitted = row
            .omitted
            .map_or_else(|| "-".to_owned(), |n| n.to_string());
        let Split { shipped, content } = row.split;
        let scaffolding = row.split.scaffolding();
        let share = percent(scaffolding, shipped);
        let verb = row.tool.verb_name();
        let scenario = &row.scenario;
        writeln!(
            out,
            "{verb} | {scenario} | {shipped} | {content} | {scaffolding} | {share} | {omitted}"
        )
        .unwrap();
    }
    out.push_str("\nverb | shipped | content | scaffolding | share | range\n");
    for tool in LIST_SHAPED {
        out.push_str(&render_verb(tool, rows));
    }
    let saturated = rows.iter().filter(|row| row.split.saturates()).count();
    writeln!(out, "\nsaturated: {saturated} of {}", rows.len()).unwrap();
    out
}

fn render_verb(tool: ToolName, rows: &[Row]) -> String {
    let splits: Vec<Split> = rows
        .iter()
        .filter(|row| row.tool == tool)
        .map(|row| row.split)
        .collect();
    let shipped: u32 = splits.iter().map(|split| split.shipped).sum();
    let content: u32 = splits.iter().map(|split| split.content).sum();
    let scaffolding: u32 = splits.iter().map(|split| split.scaffolding()).sum();
    let share = percent(scaffolding, shipped);
    let shares = splits
        .iter()
        .map(|split| permille(split.scaffolding(), split.shipped));
    let low = shares.clone().min().unwrap_or(0);
    let high = shares.max().unwrap_or(0);
    let verb = tool.verb_name();
    let (low, high) = (tenths(low), tenths(high));
    format!("{verb} | {shipped} | {content} | {scaffolding} | {share} | {low}%..{high}%\n")
}

/// A share in tenths of a percent, in integers so the table is the same on
/// every platform.
fn permille(part: u32, whole: u32) -> u64 {
    match whole {
        0 => 0,
        _ => u64::from(part) * 1000 / u64::from(whole),
    }
}

fn percent(part: u32, whole: u32) -> String {
    format!("{}%", tenths(permille(part, whole)))
}

fn tenths(permille: u64) -> String {
    format!("{}.{}", permille / 10, permille % 10)
}

fn block<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

#[test]
fn list_shaped_pull_answers_split_into_content_and_scaffolding_as_published() {
    let rows = measure_corpus();
    assert_eq!(rows.len(), 16);
    insta::assert_snapshot!("pull_overhead", render(&rows));
}

#[test]
fn a_hand_built_payload_splits_into_its_leaves_and_the_structure_around_them() {
    let payload = serde_json::json!({
        "path": "src/a.rs",
        "hunks": [{ "lo": 3, "hi": 9 }],
        "truncated": false,
        "note": null,
    });
    // Document order is the order the payload serializes in, keys sorted.
    assert_eq!(content_text(&payload), "9\n3\nsrc/a.rs");

    let split = Split::of(&payload);
    assert_eq!(split.shipped, rift_tokens::count(&payload.to_string()));
    assert_eq!(split.content, rift_tokens::count("9\n3\nsrc/a.rs"));
    assert_eq!(split.scaffolding(), split.shipped - split.content);
    assert!(split.scaffolding() > 0);

    let bare = serde_json::json!({ "hunks": [], "state": {}, "truncated": true });
    let split = Split::of(&bare);
    assert_eq!(split.content, 0);
    assert_eq!(split.scaffolding(), split.shipped);
}

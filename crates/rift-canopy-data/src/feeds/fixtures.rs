//! Store fixtures the card modules' tests share: a project with a projection,
//! and one seeder per relation those tests read back.
//!
//! A separate module because the seeders cross cards — a savings test reads
//! rows a tokens seeder wrote — and a fixture copied per module drifts.

use std::path::Path;

// The **legacy** per-path derivations, used here as the seeding oracle: a
// main work tree's family adopts exactly these ids (family identity), so a
// fixture seeded under them is the family's store, and the readers above
// must find it having resolved the family instead.
use rift_git::repo_id_for;
use rift_store::{
    projection_db_under, recall_db_under, recall_repo_id, CanonicalStore, SqliteStore, Value,
};

use crate::store::runtime;

/// The injected clock for recall-feed tests: 2026-07-20T00:00:00Z. Any
/// value ≥ the test price rows' `recorded_at` works; pinned for
/// determinism (no test reads the wall clock).
pub(super) const USD_NOW: i64 = 1_784_505_600_000;
pub(super) fn make_project(root: &Path, name: &str) -> String {
    let proj = root.join(name);
    std::fs::create_dir_all(&proj).unwrap();
    // A repository, because every store a feed opens is its family's.
    rift_git::test_support::fake_repo(&proj).unwrap();
    let repo = repo_id_for(&proj).unwrap();
    let db = projection_db_under(root, &repo);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let store = SqliteStore::open(&db).unwrap();
    store.create_canonical_schema().unwrap();
    proj.to_string_lossy().into_owned()
}

pub(super) fn open_proj(root: &Path, path: &str) -> SqliteStore {
    let repo = repo_id_for(Path::new(path)).unwrap();
    SqliteStore::open(projection_db_under(root, &repo)).unwrap()
}
/// Explicit attribution for fixtures whose facts belong to the main checkout.
pub(super) fn seed_main_worktree(store: &SqliteStore, rt: &tokio::runtime::Runtime, session: &str) {
    rt.block_on(rift_store::record_session_worktree(
        store,
        &rift_store::SessionWorktreeRow {
            observation_id: format!("main-{session}"),
            session_id: session.into(),
            worktree_token: "0123456789ab".into(),
            worktree_name: "main".into(),
            is_linked: 0,
            work_tree: "/fixture".into(),
            branch: None,
            head_oid: None,
            subagent_id: None,
            basis: "payload_cwd".into(),
            first_observed_at: 1,
            schema_version: 1,
        },
    ))
    .unwrap();
}

/// Seed one `(model_call, token_usage)` pair — the fixture helper the feed
/// tests (and the recall/nic feeds' tests) share.
pub(super) fn seed_token_fact(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    call_id: &str,
    started_at: i64,
    classes: (i64, i64, i64, i64),
) {
    seed_model_fact(store, rt, call_id, "claude-fable-5", started_at, classes);
}

/// [`seed_token_fact`] with the model made explicit — the USD-savings tests
/// need a real multi-model mix.
pub(super) fn seed_model_fact(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    call_id: &str,
    model: &str,
    started_at: i64,
    classes: (i64, i64, i64, i64),
) {
    seed_model_fact_in_turn(store, rt, call_id, "t1", model, started_at, classes);
}

/// [`seed_model_fact`] with the turn made explicit — the by-worktree axis
/// needs calls that reach **different sessions**, and a call reaches its
/// session only through its turn.
pub(super) fn seed_model_fact_in_turn(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    call_id: &str,
    turn_id: &str,
    model: &str,
    started_at: i64,
    classes: (i64, i64, i64, i64),
) {
    rt.block_on(store.insert(
        "model_call",
        &[
            "call_id",
            "turn_id",
            "model",
            "started_at",
            "schema_version",
        ],
        &[
            Value::text(call_id),
            Value::text(turn_id),
            Value::text(model),
            Value::Int(started_at),
            Value::Int(1),
        ],
    ))
    .unwrap();
    rt.block_on(store.insert(
        "token_usage",
        &[
            "call_id",
            "uncached_input",
            "cached_input",
            "cache_creation",
            "output",
            "counter_method",
            "schema_version",
        ],
        &[
            Value::text(call_id),
            Value::Int(classes.0),
            Value::Int(classes.1),
            Value::Int(classes.2),
            Value::Int(classes.3),
            Value::text("provider_reported"),
            Value::Int(1),
        ],
    ))
    .unwrap();
}
pub(super) fn seed_recall_db(root: &Path, path: &str, events: &[(i64, i64)]) {
    // Seed under `recall_repo_id` — the writer's authority the reader now
    // matches (recall.db is NOT keyed by the capture `repo_id_for`).
    let repo = recall_repo_id(Path::new(path)).unwrap();
    let db = recall_db_under(root, &repo);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let store = rift_store::open_recall_projection(&db).unwrap();
    let rt = runtime().unwrap();
    for (i, (t, avoided)) in events.iter().enumerate() {
        rt.block_on(store.insert(
            "memd_audit_event",
            &[
                "audit_id",
                "verb",
                "handle",
                "reason",
                "tokens_returned",
                "tokens_avoided",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text(format!("a{i}")),
                Value::text("memd.summary_served"),
                Value::text("h"),
                Value::Null,
                Value::Int(10),
                Value::Int(*avoided),
                Value::Int(*t),
                Value::Int(1),
            ],
        ))
        .unwrap();
        // The pull-side call sibling of that audit row, alternating over
        // the closed serve-channel pair so the card gets both series.
        rt.block_on(store.insert(
            "serve_call_event",
            &[
                "call_id",
                "channel",
                "tool_name",
                "terminal_verb",
                "tokens_returned",
                "tokens_avoided",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text(format!("c{i}")),
                Value::text(if i % 2 == 0 {
                    "hook_inject"
                } else {
                    "skill_cli"
                }),
                Value::text("rift_read_summary"),
                Value::text("memd.summary_served"),
                Value::Int(10),
                Value::Int(*avoided),
                Value::Int(*t),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }
}
/// Seeds `serve_call_event` — the channel-attributed
/// relation `recent_activity` counts for `PanelMetrics::serve_calls_5m`.
/// The channel value is irrelevant to a plain `COUNT(*)` window query, so
/// every row is tagged `skill_cli`.
pub(super) fn seed_serve_call_events(root: &Path, path: &str, events: &[(&str, i64)]) {
    let repo = recall_repo_id(Path::new(path)).unwrap();
    let db = recall_db_under(root, &repo);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let store = rift_store::open_recall_projection(&db).unwrap();
    let rt = runtime().unwrap();
    for (call_id, recorded_at) in events {
        rt.block_on(store.insert(
            "serve_call_event",
            &[
                "call_id",
                "channel",
                "tool_name",
                "terminal_verb",
                "tokens_returned",
                "tokens_avoided",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text(*call_id),
                Value::text("skill_cli"),
                Value::text("known-state"),
                Value::text("memd.internal_error"),
                Value::Int(0),
                Value::Int(0),
                Value::Int(*recorded_at),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }
}
/// Seed one captured read of `tokens` tokens by `agent` into the project's
/// capture projection: the six rows the measurement query joins.
pub(super) fn seed_one_read(root: &Path, path: &str, agent: &str, tokens: i64) {
    let store = open_proj(root, path);
    let rt = runtime().unwrap();
    seed_reading_session(&store, &rt, agent);
    seed_read_call(&store, &rt);
    seed_read_file(&store, &rt, tokens);
}

/// The session and turn a read happens in.
fn seed_reading_session(store: &SqliteStore, rt: &tokio::runtime::Runtime, agent: &str) {
    rt.block_on(store.insert(
        "session",
        &[
            "session_id",
            "agent",
            "model_primary",
            "repo_id",
            "started_at",
            "schema_version",
        ],
        &[
            Value::text("s1"),
            Value::text(agent.to_owned()),
            Value::text("m"),
            Value::text("r"),
            Value::Int(1),
            Value::Int(1),
        ],
    ))
    .unwrap();
    rt.block_on(store.insert(
        "turn",
        &[
            "turn_id",
            "session_id",
            "ordinal",
            "started_at",
            "schema_version",
        ],
        &[
            Value::text("t1"),
            Value::text("s1"),
            Value::Int(0),
            Value::Int(1),
            Value::Int(1),
        ],
    ))
    .unwrap();
}

/// The `Read` tool call itself.
fn seed_read_call(store: &SqliteStore, rt: &tokio::runtime::Runtime) {
    rt.block_on(store.insert(
        "tool_call",
        &[
            "tool_call_id",
            "turn_id",
            "tool_name",
            "started_at",
            "schema_version",
        ],
        &[
            Value::text("tc1"),
            Value::text("t1"),
            Value::text("Read"),
            Value::Int(2),
            Value::Int(1),
        ],
    ))
    .unwrap();
}

/// The file revision the call read, the edge to it, and what it cost.
fn seed_read_file(store: &SqliteStore, rt: &tokio::runtime::Runtime, tokens: i64) {
    rt.block_on(store.insert(
        "file_revision",
        &[
            "file_revision_id",
            "path",
            "blob_oid",
            "content_sha256",
            "schema_version",
        ],
        &[
            Value::text("fr1"),
            Value::text("src/lib.rs"),
            Value::text("b1"),
            Value::text("c".repeat(64)),
            Value::Int(1),
        ],
    ))
    .unwrap();
    rt.block_on(store.insert(
        "tool_reads_file",
        &["tool_call_id", "file_revision_id"],
        &[Value::text("tc1"), Value::text("fr1")],
    ))
    .unwrap();
    rt.block_on(store.insert(
        "file_read_tokens",
        &[
            "tool_call_id",
            "input_tokens",
            "tokenizer",
            "schema_version",
        ],
        &[
            Value::text("tc1"),
            Value::Int(tokens),
            Value::text("cl100k_base"),
            Value::Int(1),
        ],
    ))
    .unwrap();
}
/// Seed one captured event for an explicit native session identity.
pub(super) fn seed_event_for(
    store: &SqliteStore,
    rt: &tokio::runtime::Runtime,
    key: &str,
    session: &str,
    t: i64,
) {
    rt.block_on(store.insert(
        "captured_event",
        &[
            "idempotency_key",
            "session_id",
            "turn_ordinal",
            "event_ordinal",
            "kind",
            "trust",
            "payload_sha256",
            "occurred_at_ms",
            "schema_version",
        ],
        &[
            Value::text(key),
            Value::text(session),
            Value::Int(0),
            Value::Int(0),
            Value::text("start"),
            Value::text("trusted"),
            Value::text("sha"),
            Value::Int(t),
            Value::Int(1),
        ],
    ))
    .unwrap();
}

//! Offline dispatcher tests: the handlers run against an in-memory recall.db +
//! a `MemorySubstrate`, so the whole bounded surface is exercised without a live
//! `rmcp` transport.
use super::*;
use rift_config::EffectiveConfig;
use rift_recall_substrate::MemorySubstrate;
use rift_store::{apply_migrations, recall_migrations, CanonicalStore, SqliteStore, Value};

fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(f)
}

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn seeded_store() -> SqliteStore {
    let s = SqliteStore::open_in_memory().unwrap();
    apply_migrations(&s, &recall_migrations()).unwrap();
    block(s.insert_or_ignore(
        "n_checkpoint",
        &[
            "node_id",
            "dialect",
            "ordinal",
            "prev_checkpoint",
            "strategy",
            "merkle_root",
            "developer",
        ],
        &[
            Value::text("c1"),
            Value::text("rift"),
            Value::Int(1),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
        ],
    ))
    .unwrap();
    for (rev, path, sha, tokens) in [
        ("rev_a", "src/a.rs", SHA_A, 400),
        ("rev_b", "src/b.rs", SHA_B, 50),
    ] {
        block(s.insert_or_ignore(
            "n_file_revision",
            &["node_id", "path", "blob_oid", "content_sha256", "size"],
            &[
                Value::text(rev),
                Value::text(path),
                Value::text(sha),
                Value::text(sha),
                Value::Int(10),
            ],
        ))
        .unwrap();
        block(s.insert_or_ignore(
            "n_file_read",
            &[
                "node_id",
                "checkpoint_id",
                "tool_call_id",
                "file_revision",
                "input_tokens",
                "read_ordinal",
                "byte_range_lo",
                "byte_range_hi",
            ],
            &[
                Value::text(format!("read_{rev}")),
                Value::text("c1"),
                Value::text(format!("tc_{rev}")),
                Value::text(rev),
                Value::Int(tokens),
                Value::Int(1),
                Value::Null,
                Value::Null,
            ],
        ))
        .unwrap();
    }
    s
}

fn cfg() -> EffectiveConfig {
    EffectiveConfig::secure_default().unwrap()
}

fn ctx<'a>(
    store: &'a SqliteStore,
    reader: &'a MemorySubstrate,
) -> RecallContext<'a, SqliteStore, MemorySubstrate> {
    RecallContext {
        store,
        reader,
        scope: Scope::default_for("repo", u32::MAX),
        now: Timestamp::from_millis(1_000_000_000),
    }
}

fn ranking_req() -> ToolRequest {
    ToolRequest {
        task: Some("fix a.rs".into()),
        files_in_focus: vec!["src/a.rs".into()],
        token_budget: 2000,
        ..Default::default()
    }
}

const FOUR_STATES: &[&str] = &[
    "known_unchanged",
    "summary_included",
    "exact_included",
    "available_by_handle",
];

#[test]
fn tool_surface_is_bounded_to_eight_and_has_no_search() {
    assert_eq!(TOOL_SURFACE.len(), 8);
    assert!(!TOOL_SURFACE
        .iter()
        .any(|t| t.wire_name().contains("transcript") || t.wire_name().contains("search")));
    assert_eq!(
        ToolName::from_wire("rift_known_state"),
        Some(ToolName::KnownState)
    );
    assert_eq!(ToolName::from_wire("rift_search"), None);
}

#[test]
fn ranking_tool_requires_query_params() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let mut s = McpServer::new(TurnBudget::default(), PolicyEngine::new(&cfg));
    let bare = ToolRequest {
        token_budget: 1000,
        ..Default::default()
    };
    assert_eq!(
        block(s.dispatch(ToolName::KnownState, &bare, &ctx(&store, &reader))),
        Err(ToolError::QueryParamsRequired)
    );
}

#[test]
fn known_state_returns_four_state_envelopes_and_is_scope_bounded() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let mut s = McpServer::new(TurnBudget::default(), PolicyEngine::new(&cfg));
    let resp =
        block(s.dispatch(ToolName::KnownState, &ranking_req(), &ctx(&store, &reader))).unwrap();
    let envs = resp.payload["envelopes"].as_array().unwrap();
    assert_eq!(envs.len(), 2, "both in-scope revisions are surfaced");
    // Four-state totality: every envelope is in exactly one of the four states.
    for e in envs {
        let st = e["state"].as_str().unwrap();
        assert!(FOUR_STATES.contains(&st), "unexpected state {st}");
    }
    assert!(matches!(resp.event, MemdEvent::ManifestServed { .. }));
    assert_eq!(resp.tokenizer, tokens::TOKENIZER);
}

#[test]
fn known_hash_yields_known_unchanged_no_bytes() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let mut s = McpServer::new(TurnBudget::default(), PolicyEngine::new(&cfg));
    let req = ToolRequest {
        known_hashes: vec![SHA_A.into()],
        ..ranking_req()
    };
    let resp = block(s.dispatch(ToolName::KnownState, &req, &ctx(&store, &reader))).unwrap();
    let envs = resp.payload["envelopes"].as_array().unwrap();
    let a = envs.iter().find(|e| e["path"] == "src/a.rs").unwrap();
    assert_eq!(a["state"], "known_unchanged");
    // W2: the known_unchanged entry carries only a hash, never bytes.
    let json = resp.payload.to_string();
    assert!(!json.contains("\"content\""));
}

#[test]
fn budget_never_exceeded_over_a_turn() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    // tiny token ceiling: the manifest must truncate rather than overflow.
    let mut s = McpServer::new(TurnBudget::new(4, 3), PolicyEngine::new(&cfg));
    let resp =
        block(s.dispatch(ToolName::KnownState, &ranking_req(), &ctx(&store, &reader))).unwrap();
    assert!(
        resp.estimated_tokens_returned <= 3,
        "returned must not exceed the ceiling"
    );
    assert!(s.tokens_remaining() <= 3);
}

#[test]
fn per_turn_call_ceiling_is_enforced() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let mut s = McpServer::new(TurnBudget::new(2, 10_000), PolicyEngine::new(&cfg));
    let c = ctx(&store, &reader);
    assert!(block(s.dispatch(ToolName::CostHotspots, &ToolRequest::default(), &c)).is_ok());
    assert!(block(s.dispatch(ToolName::CostHotspots, &ToolRequest::default(), &c)).is_ok());
    assert_eq!(
        block(s.dispatch(ToolName::CostHotspots, &ToolRequest::default(), &c)),
        Err(ToolError::BudgetCallsExhausted)
    );
}

#[test]
fn exact_requires_reason_and_audits_grant_and_refusal() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local().with_blob(SHA_A, b"fn main() {}\n");
    // (1) no reason → refused, but STILL audited (Refused event — bijection).
    let mut s = McpServer::new(TurnBudget::default(), PolicyEngine::new(&cfg));
    let no_reason = ToolRequest {
        target: Some(format!("file:src/a.rs@sha256:{SHA_A}")),
        trust: Some(Trust::CodeFact),
        token_budget: 1000,
        ..Default::default()
    };
    let refused =
        block(s.dispatch(ToolName::RetrieveExact, &no_reason, &ctx(&store, &reader))).unwrap();
    assert!(matches!(refused.event, MemdEvent::Refused { .. }));
    assert_eq!(refused.payload["result"]["state"], "refused");

    // (2) with reason → exact bytes served + ExactRetrieved event carrying reason.
    let mut s2 = McpServer::new(TurnBudget::default(), PolicyEngine::new(&cfg));
    let with_reason = ToolRequest {
        reason: Some("debugging the crash".into()),
        ..no_reason
    };
    let granted =
        block(s2.dispatch(ToolName::RetrieveExact, &with_reason, &ctx(&store, &reader))).unwrap();
    match granted.event {
        MemdEvent::ExactRetrieved {
            reason,
            estimated_tokens_returned,
            ..
        } => {
            assert_eq!(reason, "debugging the crash");
            assert!(estimated_tokens_returned > 0);
        }
        other => panic!("expected ExactRetrieved, got {other:?}"),
    }
    assert_eq!(granted.payload["result"]["body"], "fn main() {}\n");
}

#[test]
fn untrusted_exact_is_refused_with_neutral_digest_only() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let mut s = McpServer::new(TurnBudget::default(), PolicyEngine::new(&cfg));
    let req = ToolRequest {
        target: Some("log:tool-1".into()),
        trust: Some(Trust::ToolOutputUntrusted),
        reason: Some("inspecting output".into()),
        token_budget: 1000,
        ..Default::default()
    };
    let resp = block(s.dispatch(ToolName::RetrieveExact, &req, &ctx(&store, &reader))).unwrap();
    // W9: untrusted content is never served raw — refused, only a neutral digest.
    assert!(matches!(resp.event, MemdEvent::Refused { .. }));
    assert_eq!(resp.estimated_tokens_returned, 0);
    assert_eq!(resp.payload["result"]["state"], "refused");
    assert!(resp.payload["result"]["neutral_digest"].is_object());
}

#[test]
fn file_if_changed_classifies_unchanged_changed_invalid() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let mut s = McpServer::new(TurnBudget::new(10, 10_000), PolicyEngine::new(&cfg));
    let c = ctx(&store, &reader);
    // unchanged: agent has the current hash
    let unchanged = block(s.dispatch(
        ToolName::FileIfChanged,
        &ToolRequest {
            target: Some("src/a.rs".into()),
            known_hashes: vec![SHA_A.into()],
            ..Default::default()
        },
        &c,
    ))
    .unwrap();
    assert_eq!(unchanged.payload["result"]["state"], "unchanged");
    // changed: agent has a different hash
    let changed = block(s.dispatch(
        ToolName::FileIfChanged,
        &ToolRequest {
            target: Some("src/a.rs".into()),
            known_hashes: vec!["deadbeef".into()],
            ..Default::default()
        },
        &c,
    ))
    .unwrap();
    assert_eq!(changed.payload["result"]["state"], "changed");
    // invalid: unknown path
    let invalid = block(s.dispatch(
        ToolName::FileIfChanged,
        &ToolRequest {
            target: Some("nope.rs".into()),
            ..Default::default()
        },
        &c,
    ))
    .unwrap();
    assert_eq!(invalid.payload["result"]["state"], "invalid");
}

#[test]
fn cost_hotspots_is_descriptive_only() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let mut s = McpServer::new(TurnBudget::default(), PolicyEngine::new(&cfg));
    let resp = block(s.dispatch(
        ToolName::CostHotspots,
        &ToolRequest::default(),
        &ctx(&store, &reader),
    ))
    .unwrap();
    assert_eq!(resp.payload["note"], COST_NOTE);
    // no directive-shaped text in the note (W7)
    let note = resp.payload["note"].as_str().unwrap().to_lowercase();
    assert!(!note.contains("avoid") && !note.contains("don't") && !note.contains("do not"));
    assert!(!resp.payload["hotspots"].as_array().unwrap().is_empty());
}

#[test]
fn context_pack_is_byte_identical_keyed_and_budget_bounded() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let mut s = McpServer::new(TurnBudget::default(), PolicyEngine::new(&cfg));
    let req = ToolRequest {
        token_budget: 500,
        ..ranking_req()
    };
    let resp = block(s.dispatch(ToolName::ContextPack, &req, &ctx(&store, &reader))).unwrap();
    assert!(matches!(resp.event, MemdEvent::PackBuilt { .. }));
    let key = resp.payload["byte_identical_key"].as_str().unwrap();
    assert_eq!(key.len(), 64, "the cache key is a sha256 hex");
    assert!(resp.payload["total_tokens"].as_u64().unwrap() <= 500);
}

#[test]
fn decode_request_is_total_and_rejects_unknown_tools() {
    // unknown tool → protocol error
    assert_eq!(
        decode_request("rift_search", &serde_json::json!({})),
        Err(DecodeError::UnknownTool)
    );
    // well-formed params decode
    let (tool, req) = decode_request(
        "rift_known_state",
        &serde_json::json!({ "task": "x", "files_in_focus": ["a.rs"], "token_budget": 500 }),
    )
    .unwrap();
    assert_eq!(tool, ToolName::KnownState);
    assert_eq!(req.task.as_deref(), Some("x"));
    assert_eq!(req.token_budget, 500);
}

proptest::proptest! {
    /// Fuzz: the request decoder never panics on arbitrary JSON shapes for a
    /// known tool, and ill-typed fields default rather than abort.
    #[test]
    fn decode_never_panics(s in ".{0,200}") {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
            let _ = decode_request("rift_known_state", &v);
            let _ = decode_request("rift_retrieve_exact", &v);
            let _ = decode_request("bogus_tool", &v);
        }
        // also exercise raw non-JSON via a wrapped value
        let _ = decode_request("rift_diff_since", &serde_json::Value::String(s));
    }
}

#[test]
fn token_accounting_is_non_negative_and_honest() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let mut s = McpServer::new(TurnBudget::default(), PolicyEngine::new(&cfg));
    let resp =
        block(s.dispatch(ToolName::KnownState, &ranking_req(), &ctx(&store, &reader))).unwrap();
    // avoided is the captured re-read cost (400 + 50) and dominates the manifest.
    assert!(resp.estimated_tokens_avoided >= resp.estimated_tokens_returned);
    assert!(resp.estimated_tokens_avoided > 0);
}

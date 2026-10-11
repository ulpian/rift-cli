//! Pulls serve the outline the store holds for the revision the checkout
//! holds, through the dispatcher every surface calls: `read_summary` answers
//! with it, `known_state` includes it, the context pack renders it unnamed,
//! and every answer about a file with no such binding is what it was.

use rift_recall_policy::{PolicyEngine, TurnBudget};
use rift_recall_substrate::worktree::sha256_hex;
use rift_recall_substrate::MemorySubstrate;
use rift_store::{CanonicalStore, SqliteStore, Value};
use serde_json::json;

use super::fixtures::*;
use super::outline::bind_outline;
use crate::*;

const PATH: &str = "src/a.rs";
const BYTES: &[u8] = b"pub fn a() -> u32 {\n    1\n}\n";
const OUTLINE: &str = "3 lines, 28 bytes, 1 top-level defs\nfn a L1-L3\n";

/// One checkpoint that read `src/a.rs` once, with no summary bound yet.
fn projection() -> SqliteStore {
    let store = open_store();
    seed_checkpoint(&store, "c1", 1);
    let sha = sha256_hex(BYTES);
    seed_revision(&store, "rev_a", PATH, &sha, &sha, 3);
    seed_read(&store, "c1", "rev_a", 900, 1);
    store
}

fn serve(
    store: &SqliteStore,
    reader: &MemorySubstrate,
    tool: ToolName,
    req: &ToolRequest,
) -> ToolResponse {
    let cfg = cfg();
    let server = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::new(8, 100_000);
    block(server.dispatch(tool, req, &ctx(store, reader), &mut budget)).unwrap()
}

fn asking(known: Vec<String>) -> ToolRequest {
    ToolRequest {
        target: Some(PATH.into()),
        known_hashes: known,
        token_budget: 4000,
        ..ranking_req()
    }
}

/// A binding row written straight to the store, for the shapes the writer
/// never produces: an older extractor version, or no digest behind it.
fn plant_binding(store: &SqliteStore, extractor_version: i64) {
    let sha = sha256_hex(BYTES);
    block(store.insert_or_ignore(
        "summary_binding",
        &[
            "summary_id",
            "path",
            "source_sha256",
            "bound_blob_oid",
            "extractor_ver",
            "policy_ver",
            "redaction_ver",
            "digest_ref",
            "created_at",
        ],
        &[
            Value::text(format!("planted-{extractor_version}")),
            Value::text(PATH),
            Value::text(&sha),
            Value::text(&sha),
            Value::Int(extractor_version),
            Value::Int(1),
            Value::Int(1),
            Value::text("no-such-digest"),
            Value::Int(0),
        ],
    ))
    .unwrap();
}

#[test]
fn read_summary_answers_with_the_stored_outline_its_key_and_versions() {
    let store = projection();
    let key = bind_outline(&store, PATH, BYTES).unwrap();
    let checkout = Checkout::holding(&[(PATH, BYTES)]);
    let sha = sha256_hex(BYTES);
    let resp = serve(
        &store,
        &checkout.reader(),
        ToolName::ReadSummary,
        &asking(vec![sha.clone()]),
    );
    let result = &resp.payload["result"];
    assert_eq!(result["state"], json!("summary"));
    assert_eq!(result["summary"], json!(OUTLINE));
    assert_eq!(result["summary_key"], json!(key));
    assert_eq!(result["source_hash"], json!(sha));
    assert_eq!(result["handle"], json!(format!("file:{PATH}@sha256:{sha}")));
    assert_eq!(
        result["versions"],
        json!({ "extractor": 3, "policy": 1, "redaction": 1 })
    );
    assert!(resp.estimated_tokens_avoided > 0, "{}", resp.payload);
}

#[test]
fn a_binding_of_another_revision_is_stale_and_none_is_not_bound() {
    let store = projection();
    let checkout = Checkout::holding(&[(PATH, BYTES)]);
    let req = asking(Vec::new());
    let unbound = serve(&store, &checkout.reader(), ToolName::ReadSummary, &req);
    assert_eq!(
        unbound.payload["result"],
        json!({ "state": "unavailable", "path": PATH, "reason": "not_bound" })
    );
    bind_outline(&store, PATH, BYTES).unwrap();
    let edited = b"pub fn a() -> u32 {\n    2\n}\n";
    checkout.write(PATH, edited);
    let stale = serve(&store, &checkout.reader(), ToolName::ReadSummary, &req);
    assert_eq!(
        stale.payload["result"],
        json!({ "state": "stale", "path": PATH, "current_hash": sha256_hex(edited) })
    );
}

#[test]
fn a_binding_under_an_older_extractor_is_not_bound_and_a_lost_digest_is_unavailable() {
    let store = projection();
    plant_binding(&store, 1);
    let checkout = Checkout::holding(&[(PATH, BYTES)]);
    let req = asking(Vec::new());
    let old = serve(&store, &checkout.reader(), ToolName::ReadSummary, &req);
    assert_eq!(old.payload["result"]["reason"], json!("not_bound"));
    plant_binding(&store, 3);
    let lost = serve(&store, &checkout.reader(), ToolName::ReadSummary, &req);
    assert_eq!(
        lost.payload["result"],
        json!({ "state": "unavailable", "path": PATH, "reason": "digest_unavailable" })
    );
    assert_eq!(lost.estimated_tokens_avoided, 0);
}

#[test]
fn known_state_includes_the_summary_of_a_file_the_caller_does_not_hold() {
    let store = projection();
    let checkout = Checkout::holding(&[(PATH, BYTES)]);
    let req = asking(Vec::new());
    let before = serve(&store, &checkout.reader(), ToolName::KnownState, &req);
    let key = bind_outline(&store, PATH, BYTES).unwrap();
    let after = serve(&store, &checkout.reader(), ToolName::KnownState, &req);
    let entry = &after.payload["envelopes"][0];
    assert_eq!(
        entry["state"],
        json!("summary_included"),
        "{}",
        after.payload
    );
    assert_eq!(entry["summary"], json!(OUTLINE));
    assert_eq!(entry["summary_key"], json!(key));
    assert_eq!(entry["versions"]["extractor"], json!(3));
    assert_eq!(
        before.payload["envelopes"][0]["state"],
        json!("available_by_handle")
    );
    assert!(after.estimated_tokens_returned > before.estimated_tokens_returned);
}

#[test]
fn known_state_includes_no_summary_once_the_checkout_moves() {
    let store = projection();
    bind_outline(&store, PATH, BYTES).unwrap();
    let checkout = Checkout::holding(&[(PATH, b"pub fn a() -> u32 {\n    2\n}\n")]);
    let resp = serve(
        &store,
        &checkout.reader(),
        ToolName::KnownState,
        &asking(Vec::new()),
    );
    assert!(
        !resp.payload.to_string().contains("summary_included"),
        "{}",
        resp.payload
    );
}

#[test]
fn the_context_pack_renders_the_outline_without_its_names() {
    let store = projection();
    bind_outline(&store, PATH, BYTES).unwrap();
    let checkout = Checkout::holding(&[(PATH, BYTES)]);
    let resp = serve(
        &store,
        &checkout.reader(),
        ToolName::ContextPack,
        &asking(Vec::new()),
    );
    let text = resp.payload.to_string();
    assert!(
        text.contains("summary: 3 lines, 28 bytes, 1 top-level defs; #1 fn L1-L3"),
        "{text}"
    );
}

#[test]
fn a_summary_that_does_not_fit_never_displaces_its_entry() {
    let store = projection();
    bind_outline(&store, PATH, BYTES).unwrap();
    let checkout = Checkout::holding(&[(PATH, BYTES)]);
    let tight = ToolRequest {
        token_budget: 90,
        ..asking(Vec::new())
    };
    let resp = serve(&store, &checkout.reader(), ToolName::KnownState, &tight);
    let entry = &resp.payload["envelopes"][0];
    assert_eq!(entry["path"], json!(PATH), "{}", resp.payload);
    assert_eq!(
        entry["state"],
        json!("available_by_handle"),
        "{}",
        resp.payload
    );
}

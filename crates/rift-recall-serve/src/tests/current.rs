//! Every answer that says a file is current or unchanged is checked against
//! the bytes on disk, through the dispatcher every surface calls.
//!
//! The counterexample these tests freeze: a family's projection records
//! revision A of a file, the file is then edited to B in the checkout, and no
//! checkpoint refreshes the projection. A caller holding A must be told the
//! file changed, not that it holds the current bytes, and no re-read may be
//! credited for it.

use rift_recall_policy::{PolicyEngine, TurnBudget};
use rift_recall_substrate::worktree::sha256_hex;
use rift_recall_substrate::MemorySubstrate;
use rift_store::SqliteStore;
use serde_json::json;

use super::fixtures::*;
use crate::*;

const PATH: &str = "src/a.rs";
const OTHER: &str = "src/b.rs";
const A_BYTES: &[u8] = b"fn a() -> u32 {\n    1\n}\n";
const B_BYTES: &[u8] = b"fn a() -> u32 {\n    2\n}\n";
const OTHER_BYTES: &[u8] = b"fn b() {}\n";
const A_TOKENS: i64 = 900;

/// The projection: revision A of `src/a.rs` and one other file, each read
/// once in one checkpoint, with a summary bound to A.
fn projection() -> SqliteStore {
    let store = open_store();
    seed_checkpoint(&store, "c1", 1);
    let (a, other) = (sha256_hex(A_BYTES), sha256_hex(OTHER_BYTES));
    seed_revision(&store, "rev_a", PATH, &a, &a, 10);
    seed_read(&store, "c1", "rev_a", A_TOKENS, 1);
    seed_revision(&store, "rev_b", OTHER, &other, &other, 10);
    seed_read(&store, "c1", "rev_b", 40, 1);
    super::outline::bind_outline(&store, PATH, A_BYTES).unwrap();
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

/// A request stating revision A, shaped for every verb at once.
fn holding_a() -> ToolRequest {
    ToolRequest {
        target: Some(PATH.into()),
        known_hashes: vec![sha256_hex(A_BYTES)],
        token_budget: 4000,
        ..ranking_req()
    }
}

/// What each current-claiming verb said about `src/a.rs`, and what it booked.
fn verdicts(store: &SqliteStore, reader: &MemorySubstrate) -> Vec<(ToolName, String, u32)> {
    let req = holding_a();
    [
        ToolName::FileIfChanged,
        ToolName::ReadSummary,
        ToolName::KnownState,
        ToolName::DiffSince,
        ToolName::ContextPack,
    ]
    .into_iter()
    .map(|tool| {
        let resp = serve(store, reader, tool, &req);
        (
            tool,
            resp.payload.to_string(),
            resp.estimated_tokens_avoided,
        )
    })
    .collect()
}

/// The defect itself: a file edited after the last checkpoint is changed for
/// a caller holding the old bytes, on every verb that answers about it, and
/// nothing is credited.
#[test]
fn a_file_edited_after_the_projection_is_changed_on_every_verb_and_earns_nothing() {
    let store = projection();
    let checkout = Checkout::holding(&[(PATH, B_BYTES), (OTHER, OTHER_BYTES)]);
    let reader = checkout.reader();
    let b = sha256_hex(B_BYTES);
    for (tool, payload, avoided) in verdicts(&store, &reader) {
        assert_eq!(avoided, 0, "{tool:?} credited a moved file: {payload}");
        assert!(
            !payload.contains("known_unchanged"),
            "{tool:?} called a moved file held: {payload}"
        );
    }
    let file = serve(&store, &reader, ToolName::FileIfChanged, &holding_a());
    assert_eq!(
        file.payload["result"],
        json!({ "state": "changed", "path": PATH, "new_hash": b })
    );
    let summary = serve(&store, &reader, ToolName::ReadSummary, &holding_a());
    assert_eq!(
        summary.payload["result"],
        json!({ "state": "stale", "path": PATH, "current_hash": b })
    );
    let delta = serve(&store, &reader, ToolName::DiffSince, &holding_a());
    assert_eq!(delta.payload["unchanged"], json!([]), "{}", delta.payload);
    assert_eq!(delta.payload["changed"][0]["worktree_sha256"], json!(b));
}

/// The bytes really unchanged: every verb says so, and each one that credits
/// a held file books its cost net of the answer.
#[test]
fn a_file_the_checkout_still_holds_is_unchanged_and_credited() {
    let store = projection();
    let checkout = Checkout::holding(&[(PATH, A_BYTES), (OTHER, OTHER_BYTES)]);
    let reader = checkout.reader();
    for tool in [
        ToolName::FileIfChanged,
        ToolName::ReadSummary,
        ToolName::KnownState,
        ToolName::DiffSince,
        ToolName::ContextPack,
    ] {
        let resp = serve(&store, &reader, tool, &holding_a());
        let full = u32::try_from(A_TOKENS).unwrap();
        assert_eq!(
            resp.estimated_tokens_avoided,
            full.saturating_sub(resp.estimated_tokens_returned),
            "{tool:?}: {}",
            resp.payload
        );
        assert!(resp.estimated_tokens_avoided > 0, "{tool:?}");
        assert_eq!(resp.avoided_basis, AvoidedBasis::StatedRecorded);
    }
    let file = serve(&store, &reader, ToolName::FileIfChanged, &holding_a());
    assert_eq!(file.payload["result"]["state"], json!("unchanged"));
    let delta = serve(&store, &reader, ToolName::DiffSince, &holding_a());
    assert_eq!(delta.payload["unchanged"], json!([PATH]));
}

/// The file the caller named, answered when its bytes cannot be read: never
/// unchanged, never credited, and the reason named so the caller reads it.
fn unavailable(store: &SqliteStore, reader: &MemorySubstrate, path: &str) -> String {
    let req = ToolRequest {
        target: Some(path.into()),
        known_hashes: vec![sha256_hex(A_BYTES)],
        ..Default::default()
    };
    let file = serve(store, reader, ToolName::FileIfChanged, &req);
    assert_eq!(file.estimated_tokens_avoided, 0, "{}", file.payload);
    assert_eq!(file.payload["result"]["state"], json!("unavailable"));
    let summary = serve(store, reader, ToolName::ReadSummary, &req);
    assert_eq!(summary.estimated_tokens_avoided, 0, "{}", summary.payload);
    assert_eq!(summary.payload["result"]["state"], json!("unavailable"));
    file.payload["result"]["reason"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// No checkout, no file, or an oversize file: an honest `unavailable` with its
/// reason, and nothing credited.
#[test]
fn a_file_the_checkout_cannot_show_is_unavailable_and_earns_nothing() {
    let store = projection();
    assert_eq!(
        unavailable(&store, &MemorySubstrate::local(), PATH),
        "no_worktree"
    );
    let empty = Checkout::holding(&[]);
    assert_eq!(unavailable(&store, &empty.reader(), PATH), "missing");
    let big = Checkout::holding(&[]);
    big.write(PATH, b"");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(big.path().join(PATH))
        .unwrap();
    // Sparse: the length is past the ceiling without writing the bytes.
    file.set_len(64 * 1024 * 1024).unwrap();
    assert_eq!(unavailable(&store, &big.reader(), PATH), "oversize");
}

/// A link, and a path that leaves the checkout through a linked directory,
/// are refused by name.
#[cfg(unix)]
#[test]
fn a_link_or_a_path_out_of_the_checkout_is_unavailable() {
    let store = projection();
    let linked = Checkout::holding(&[("src/real.rs", A_BYTES)]);
    std::os::unix::fs::symlink(linked.path().join("src/real.rs"), linked.path().join(PATH))
        .unwrap();
    assert_eq!(unavailable(&store, &linked.reader(), PATH), "symlink");

    let outside = Checkout::holding(&[("a.rs", A_BYTES)]);
    let escaping = Checkout::holding(&[]);
    std::os::unix::fs::symlink(outside.path(), escaping.path().join("out")).unwrap();
    let a = sha256_hex(A_BYTES);
    seed_revision(&store, "rev_out", "out/a.rs", &a, &a, 10);
    seed_read(&store, "c1", "rev_out", 5, 1);
    assert_eq!(
        unavailable(&store, &escaping.reader(), "out/a.rs"),
        "outside_root"
    );
}

/// A file on disk that no read recorded is refused as not found: the caller
/// reads it itself, and nothing is credited.
#[test]
fn an_untracked_file_is_refused_with_a_plain_read_left_to_the_caller() {
    let store = projection();
    let checkout = Checkout::holding(&[(PATH, A_BYTES), ("notes/todo.md", b"todo\n")]);
    let req = ToolRequest {
        target: Some("notes/todo.md".into()),
        known_hashes: vec![sha256_hex(b"todo\n")],
        ..Default::default()
    };
    let resp = serve(&store, &checkout.reader(), ToolName::FileIfChanged, &req);
    assert_eq!(
        resp.payload["result"],
        json!({ "state": "invalid", "path": "notes/todo.md", "reason": "path_not_found" })
    );
    assert_eq!(resp.estimated_tokens_avoided, 0);
}

/// A hash-bound retrieval of a revision the checkout no longer holds is still
/// served — it is history — and it says so; the current one carries no label.
#[test]
fn an_archived_revision_is_served_and_labelled_historical() {
    let store = projection();
    let checkout = Checkout::holding(&[(PATH, B_BYTES)]);
    let a = sha256_hex(A_BYTES);
    let reader = checkout.reader().with_blob(&a, A_BYTES);
    let req = ToolRequest {
        target: Some(format!("file:{PATH}@sha256:{a}")),
        reason: Some("what did it say before".into()),
        ..Default::default()
    };
    let old = serve(&store, &reader, ToolName::RetrieveExact, &req);
    assert_eq!(old.payload["result"]["state"], json!("exact"));
    assert_eq!(old.payload["result"]["historical"], json!(true));

    checkout.write(PATH, A_BYTES);
    let now = serve(&store, &reader, ToolName::RetrieveExact, &req);
    assert_eq!(now.payload["result"]["state"], json!("exact"));
    assert!(now.payload["result"].get("historical").is_none());
}

/// The push tier asks `diff_since` through a reader bound to no checkout. Its
/// answer is the projection's view, byte for byte what it was before any file
/// was checked, so the hook's cap and its completeness reading cannot move.
#[test]
fn a_delta_list_with_no_checkout_is_the_projections_view_unchanged() {
    let store = projection();
    let resp = serve(
        &store,
        &MemorySubstrate::local(),
        ToolName::DiffSince,
        &holding_a(),
    );
    let expected = json!({
        "tool": "diff_since",
        "changed": [{
            "path": OTHER,
            "new_sha256": sha256_hex(OTHER_BYTES),
            "provenance": { "checkpoints": { "to": "c1" }, "why": "rift why src/b.rs --json" },
        }],
        "unchanged": [PATH],
        "truncated": false,
        "omitted": 0,
    });
    assert_eq!(resp.payload.to_string(), expected.to_string());
}

/// …and it credits nothing, because nothing in it was checked against the
/// file, and the figure says which rule it was booked under.
#[test]
fn a_delta_list_with_no_checkout_books_zero_and_says_unverified() {
    let store = projection();
    let resp = serve(
        &store,
        &MemorySubstrate::local(),
        ToolName::DiffSince,
        &holding_a(),
    );
    assert_eq!(resp.estimated_tokens_avoided, 0);
    assert_eq!(resp.avoided_basis, AvoidedBasis::Unverified);
}

/// The projection holding two revisions of `src/a.rs`: A read in an older
/// checkpoint and B read in a newer one.
fn two_revision_projection() -> SqliteStore {
    let store = projection();
    seed_checkpoint(&store, "c2", 2);
    let b = sha256_hex(B_BYTES);
    seed_revision(&store, "rev_a2", PATH, &b, &b, 10);
    seed_read(&store, "c2", "rev_a2", A_TOKENS, 1);
    store
}

/// The rows of a delta list that name `path`, whether bare or tagged.
fn rows_for(list: &serde_json::Value, path: &str) -> Vec<serde_json::Value> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter(|row| row == &&json!(path) || row["path"] == json!(path))
        .cloned()
        .collect()
}

/// A file with two stored revisions is one row, decided against the bytes on
/// disk: stating the current hash is unchanged only, and stating the older
/// hash is one changed row that names the current hash.
#[test]
fn diff_since_reports_one_row_per_path_against_the_checkout() {
    let store = two_revision_projection();
    let checkout = Checkout::holding(&[(PATH, B_BYTES), (OTHER, OTHER_BYTES)]);
    let reader = checkout.reader();
    let b = sha256_hex(B_BYTES);
    let holding_b = ToolRequest {
        known_hashes: vec![b.clone()],
        ..holding_a()
    };
    let current = serve(&store, &reader, ToolName::DiffSince, &holding_b);
    assert_eq!(rows_for(&current.payload["unchanged"], PATH).len(), 1);
    assert_eq!(
        rows_for(&current.payload["changed"], PATH),
        Vec::<serde_json::Value>::new(),
        "{}",
        current.payload
    );
    let older = serve(&store, &reader, ToolName::DiffSince, &holding_a());
    assert_eq!(rows_for(&older.payload["unchanged"], PATH).len(), 0);
    let changed = rows_for(&older.payload["changed"], PATH);
    assert_eq!(changed.len(), 1, "{}", older.payload);
    assert_eq!(changed[0]["new_sha256"], json!(b));
}

/// A hand edit committed after the last ingest: the store holds only the
/// older revision, the checkout holds the edit, and the caller states the
/// edit's hash. The bytes on disk decide, so the file is unchanged and no
/// changed row names the stored, older hash.
#[test]
fn diff_since_stating_a_hash_the_store_never_saw_is_unchanged_when_the_checkout_holds_it() {
    let store = projection();
    let checkout = Checkout::holding(&[(PATH, B_BYTES), (OTHER, OTHER_BYTES)]);
    let reader = checkout.reader();
    let holding_b = ToolRequest {
        known_hashes: vec![sha256_hex(B_BYTES)],
        ..holding_a()
    };
    let resp = serve(&store, &reader, ToolName::DiffSince, &holding_b);
    assert_eq!(
        rows_for(&resp.payload["unchanged"], PATH).len(),
        1,
        "{}",
        resp.payload
    );
    assert_eq!(
        rows_for(&resp.payload["changed"], PATH),
        Vec::<serde_json::Value>::new(),
        "{}",
        resp.payload
    );
}

/// The same edit stated by nobody: the caller holds an unrelated hash, so the
/// file is one changed row naming the hash on disk, not the stored one.
#[test]
fn diff_since_names_the_checkout_hash_when_the_store_lags_the_edit() {
    let store = projection();
    let checkout = Checkout::holding(&[(PATH, B_BYTES), (OTHER, OTHER_BYTES)]);
    let reader = checkout.reader();
    let holding_other = ToolRequest {
        known_hashes: vec![sha256_hex(b"something else entirely")],
        ..holding_a()
    };
    let resp = serve(&store, &reader, ToolName::DiffSince, &holding_other);
    let changed = rows_for(&resp.payload["changed"], PATH);
    assert_eq!(changed.len(), 1, "{}", resp.payload);
    assert_eq!(changed[0]["new_sha256"], json!(sha256_hex(B_BYTES)));
}

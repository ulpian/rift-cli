//! The manifest verbs — `known_state`, `diff_since`, `file_if_changed` — the
//! cap that bounds what they ship, and the containment facts they report.

use rift_core::Timestamp;
use rift_recall_events::MemdEvent;
use rift_recall_policy::{PolicyEngine, TurnBudget};
use rift_recall_substrate::MemorySubstrate;
use rift_recall_types::{NoAncestry, NoProvenance, Scope};
use rift_store::{CanonicalStore, Value};

use super::fixtures::*;
use crate::verbs::{fit_to_cap, manifest_cap};
use crate::*;

const FOUR_STATES: &[&str] = &[
    "known_unchanged",
    "summary_included",
    "exact_included",
    "available_by_handle",
];

#[test]
fn ranking_tool_requires_query_params() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::default();
    let bare = ToolRequest {
        token_budget: 1000,
        ..Default::default()
    };
    assert_eq!(
        block(s.dispatch(
            ToolName::KnownState,
            &bare,
            &ctx(&store, &reader),
            &mut budget
        )),
        Err(ToolError::QueryParamsRequired)
    );
}

#[test]
fn known_state_returns_four_state_envelopes_and_is_scope_bounded() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::default();
    let resp = block(s.dispatch(
        ToolName::KnownState,
        &ranking_req(),
        &ctx(&store, &reader),
        &mut budget,
    ))
    .unwrap();
    let envs = resp.payload["envelopes"].as_array().unwrap();
    assert_eq!(envs.len(), 2, "both in-scope revisions are surfaced");
    // Four-state totality: every envelope is in exactly one of the four states.
    for e in envs {
        let st = e["state"].as_str().unwrap();
        assert!(FOUR_STATES.contains(&st), "unexpected state {st}");
    }
    assert!(matches!(resp.event, MemdEvent::ManifestServed { .. }));
    assert_eq!(resp.tokenizer, rift_tokens::TOKENIZER);
}

#[test]
fn known_hash_yields_known_unchanged_no_bytes() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::default();
    let req = ToolRequest {
        known_hashes: vec![SHA_A.into()],
        ..ranking_req()
    };
    let resp = block(s.dispatch(
        ToolName::KnownState,
        &req,
        &ctx(&store, &reader),
        &mut budget,
    ))
    .unwrap();
    let envs = resp.payload["envelopes"].as_array().unwrap();
    let a = envs.iter().find(|e| e["path"] == "src/a.rs").unwrap();
    assert_eq!(a["state"], "known_unchanged");
    // The known_unchanged entry carries only a hash, never bytes.
    let json = resp.payload.to_string();
    assert!(!json.contains("\"content\""));
}

/// The turn a known-unchanged entry cites is the one the binary read out of
/// the caller's journal for that exact path and revision — and only that one:
/// a held revision the journal names under no turn cites none, and the key is
/// absent rather than null so a view without evidence is unchanged.
#[test]
fn a_known_unchanged_entry_cites_the_turn_its_window_last_read_it() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let req = ToolRequest {
        known_hashes: vec![SHA_A.into(), SHA_B.into()],
        seen_turns: [(("src/a.rs".to_owned(), SHA_A.to_owned()), 7)].into(),
        ..ranking_req()
    };
    let resp = block(s.dispatch(
        ToolName::KnownState,
        &req,
        &ctx(&store, &reader),
        &mut TurnBudget::default(),
    ))
    .unwrap();
    let envs = resp.payload["envelopes"].as_array().unwrap();
    let entry = |path: &str| envs.iter().find(|e| e["path"] == path).unwrap();
    assert_eq!(entry("src/a.rs")["state"], "known_unchanged");
    assert_eq!(entry("src/a.rs")["last_seen_turn"], 7);
    assert_eq!(entry("src/b.rs")["state"], "known_unchanged");
    assert!(
        entry("src/b.rs").get("last_seen_turn").is_none(),
        "no evidence is no turn, not a null"
    );
}

#[test]
fn manifest_cap_composes_default_max_and_remaining() {
    // No request budget → the pack-parity default.
    let b = TurnBudget::new(4, 2000);
    assert_eq!(manifest_cap(&ToolRequest::default(), &b), 1200);
    // An explicit budget clamps to the hard maximum…
    let roomy = TurnBudget::new(4, 10_000);
    let big = ToolRequest {
        token_budget: 999_999,
        ..Default::default()
    };
    assert_eq!(manifest_cap(&big, &roomy), MAX_MANIFEST_TOKENS);
    // …and small explicit budgets pass through.
    let small = ToolRequest {
        token_budget: 50,
        ..Default::default()
    };
    assert_eq!(manifest_cap(&small, &roomy), 50);
    // The remaining turn budget always composes in.
    let drained = TurnBudget::new(4, 10);
    assert_eq!(manifest_cap(&ToolRequest::default(), &drained), 10);
}

#[test]
fn fit_to_cap_keeps_the_largest_fitting_prefix_deterministically() {
    let build = |kept: usize, truncated: bool| {
        serde_json::json!({
            "items": (0..kept).map(|i| format!("item-number-{i}")).collect::<Vec<_>>(),
            "truncated": truncated,
        })
    };
    // A cap sized to a mid prefix keeps a fitting prefix (postcondition).
    let cap = rift_tokens::count(&build(7, true).to_string());
    let (payload, kept) = fit_to_cap(cap, 20, &build);
    assert!(kept > 0 && kept < 20);
    assert!(rift_tokens::count(&payload.to_string()) <= cap);
    assert_eq!(payload["truncated"], true);
    // Deterministic: same inputs, same output.
    let (again, kept_again) = fit_to_cap(cap, 20, &build);
    assert_eq!(payload, again);
    assert_eq!(kept, kept_again);
    // A full fit is the fast path, untruncated.
    let (full, all) = fit_to_cap(u32::MAX, 20, &build);
    assert_eq!(all, 20);
    assert_eq!(full["truncated"], false);
    // Nothing droppable (n == 0): the skeleton ships UNtruncated even over a
    // tiny cap — nothing was omitted, so `truncated` must not lie.
    let (skeleton, none) = fit_to_cap(0, 0, &build);
    assert_eq!(none, 0);
    assert_eq!(skeleton["truncated"], false);
}

/// One manifest entry as a budget too small for its story leaves it: the tag,
/// and the command that still fetches the whole of what was dropped.
fn downgraded_entry(entry: &serde_json::Value) -> serde_json::Value {
    let mut out = entry.clone();
    let path = entry["path"].as_str().unwrap().to_owned();
    let obj = out.as_object_mut().unwrap();
    obj.insert(
        "provenance".to_owned(),
        serde_json::json!("available_by_handle"),
    );
    obj.insert(
        "why".to_owned(),
        serde_json::json!(format!("rift why {path} --json")),
    );
    out
}

#[test]
fn known_state_truncates_to_the_ranked_prefix() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let c = ctx(&store, &reader);
    // Full manifest first: both entries, rank order (src/a.rs is the focus).
    let mut roomy = TurnBudget::new(4, 10_000);
    let full = block(s.dispatch(ToolName::KnownState, &ranking_req(), &c, &mut roomy)).unwrap();
    let envs = full.payload["envelopes"].as_array().unwrap().clone();
    assert_eq!(envs.len(), 2);
    assert_eq!(
        envs[0]["path"], "src/a.rs",
        "rank order: the focus file leads"
    );
    // Every entry the caller does not hold carries its provenance, and the
    // fixture's oracle knows nothing, so what rides is the recall-side anchor
    // and the command for the rest.
    assert!(
        envs[0]["provenance"].is_object(),
        "a delta carries its provenance: {:#}",
        envs[0]
    );
    // The first sacrifice is the STORY, not a delta. Budget exactly the
    // two-entry payload with every provenance object downgraded, and both
    // entries survive with the tag and the command in place of the object.
    let bare: Vec<serde_json::Value> = envs.iter().map(downgraded_entry).collect();
    let told_plain = serde_json::json!({
        "tool": "known_state",
        "envelopes": bare,
        "truncated": false,
        "omitted": 0,
    });
    let req = ToolRequest {
        token_budget: rift_tokens::count(&told_plain.to_string()),
        ..ranking_req()
    };
    let mut tight = TurnBudget::new(4, 10_000);
    let downgraded = block(s.dispatch(ToolName::KnownState, &req, &c, &mut tight)).unwrap();
    assert_eq!(
        downgraded.payload, told_plain,
        "the story goes before any delta does"
    );
    // Budget exactly the one-entry truncated payload → the ranked prefix
    // survives, the tail drops, and the truncation is honest.
    let expected = serde_json::json!({
        "tool": "known_state",
        "envelopes": [bare[0].clone()],
        "truncated": true,
        "omitted": 1,
    });
    let req = ToolRequest {
        token_budget: rift_tokens::count(&expected.to_string()),
        ..ranking_req()
    };
    let mut budget = TurnBudget::new(4, 10_000);
    let resp = block(s.dispatch(ToolName::KnownState, &req, &c, &mut budget)).unwrap();
    assert_eq!(resp.payload, expected);
    // Anchor-then-charge: the anchor refilled the turn,
    // then the seam charged exactly the measured manifest.
    assert_eq!(
        budget.tokens_remaining(),
        10_000 - resp.estimated_tokens_returned,
        "the fresh turn allowance minus the seam's single charge"
    );
}

#[test]
fn diff_since_drops_unchanged_before_changed() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let c = ctx(&store, &reader);
    // The agent holds src/a.rs → a is unchanged, b is changed (n = 2). The
    // changed entry carries its provenance, and the unchanged path is a bare
    // string: nothing changed, so there is no change to have a story.
    let mut roomy = TurnBudget::new(4, 10_000);
    let req = ToolRequest {
        known_hashes: vec![SHA_A.into()],
        ..Default::default()
    };
    let full = block(s.dispatch(ToolName::DiffSince, &req, &c, &mut roomy)).unwrap();
    let changed = full.payload["changed"].as_array().unwrap().clone();
    assert_eq!(changed.len(), 1);
    assert!(changed[0]["provenance"].is_object(), "{:#}", changed[0]);
    assert_eq!(full.payload["unchanged"], serde_json::json!(["src/a.rs"]));
    // The tail that drops first is the unchanged path — but only after every
    // provenance object has already gone.
    let expected = serde_json::json!({
        "tool": "diff_since",
        "changed": [downgraded_entry(&changed[0])],
        "unchanged": [],
        "truncated": true,
        "omitted": 1,
    });
    let req = ToolRequest {
        known_hashes: vec![SHA_A.into()],
        token_budget: rift_tokens::count(&expected.to_string()),
        ..Default::default()
    };
    let mut budget = TurnBudget::new(4, 10_000);
    let resp = block(s.dispatch(ToolName::DiffSince, &req, &c, &mut budget)).unwrap();
    assert_eq!(
        resp.payload, expected,
        "the unchanged tail drops first; the changed entry (the tool's point) survives"
    );
}

#[test]
fn file_if_changed_classifies_unchanged_changed_invalid() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::new(10, 10_000);
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
        &mut budget,
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
        &mut budget,
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
        &mut budget,
    ))
    .unwrap();
    assert_eq!(invalid.payload["result"]["state"], "invalid");
}

/// A manifest says how much branch containment held back, and why — the
/// omission line's containment cause, which is what makes
/// `rift recall known-state` from a divergent member honest rather than merely
/// empty.
#[test]
fn a_manifest_reports_what_branch_containment_held_back() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let server = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::default();

    // A serving scope whose HEAD is unknown: the fixture's one checkpoint is
    // out of scope, and the difference between "nothing changed" and "I could
    // not tell" is the thing being asserted.
    let ctx = RecallContext {
        store: &store,
        reader: &reader,
        snapshots: None,
        scope: Scope::for_worktree(
            "repo",
            u32::MAX,
            None,
            rift_recall_types::WorktreeScope::Any,
        ),
        ancestry: &NoAncestry,
        provenance: &NoProvenance,
        now: Timestamp::from_millis(1_000_000_000),
    };
    let resp =
        block(server.dispatch(ToolName::KnownState, &ranking_req(), &ctx, &mut budget)).unwrap();
    let containment = &resp.payload["containment"];
    assert_eq!(containment["unknown_head"], serde_json::json!(true));
    assert!(
        containment["out_of_branch"].as_u64().unwrap() > 0,
        "the manifest must say how many, not merely serve nothing: {}",
        resp.payload
    );
    assert_eq!(containment["cross_branch_grant"], serde_json::json!(false));
    assert_eq!(
        resp.payload["envelopes"],
        serde_json::json!([]),
        "and nothing off the caller's line is served"
    );
}

/// Nothing held back, nothing said: the block costs the caller tokens out of a
/// capped manifest, so it appears only when it carries information.
#[test]
fn a_manifest_with_nothing_held_back_carries_no_containment_block() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let server = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::default();
    let resp = block(server.dispatch(
        ToolName::KnownState,
        &ranking_req(),
        &ctx(&store, &reader),
        &mut budget,
    ))
    .unwrap();
    assert!(
        resp.payload.get("containment").is_none(),
        "{}",
        resp.payload
    );
}

/// The hashes a caller quotes are resolved once, in the dispatcher, so every
/// verb whose answer turns on "do you already hold this" accepts the short
/// form the injected text can afford to carry.
///
/// The manifest renders twelve hex for a person and sixteen for a command,
/// because the whole address costs about forty tokens on a line that is
/// injected on every turn. A verb that only understood the whole address would
/// make that line unusable by the agent reading it.
#[test]
fn a_prefix_the_caller_holds_answers_the_verbs_that_ask_about_hashes() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let c = ctx(&store, &reader);
    let ask = |hash: &str| ToolRequest {
        target: Some("src/a.rs".into()),
        known_hashes: vec![hash.to_owned()],
        ..Default::default()
    };

    // Sixteen hex of the revision the store holds is that revision.
    let mut budget = TurnBudget::default();
    let resp = block(s.dispatch(
        ToolName::FileIfChanged,
        &ask(&SHA_A[..basis::MIN_BASIS_HEX]),
        &c,
        &mut budget,
    ))
    .unwrap();
    assert_eq!(
        resp.payload["result"]["state"],
        serde_json::json!("unchanged")
    );
    // And the whole address still is.
    let mut budget = TurnBudget::default();
    let whole = block(s.dispatch(ToolName::FileIfChanged, &ask(SHA_A), &c, &mut budget)).unwrap();
    assert_eq!(
        whole.payload["result"]["state"],
        serde_json::json!("unchanged")
    );

    // The delta verb reads the same resolved set: the prefix's file is the
    // unchanged one and the other file is the changed one.
    let mut budget = TurnBudget::default();
    let delta = block(s.dispatch(
        ToolName::DiffSince,
        &ask(&SHA_A[..basis::MIN_BASIS_HEX]),
        &c,
        &mut budget,
    ))
    .unwrap();
    assert_eq!(delta.payload["unchanged"], serde_json::json!(["src/a.rs"]));
    assert_eq!(
        delta.payload["changed"][0]["path"],
        serde_json::json!("src/b.rs")
    );

    // Twelve is the display form and is never an argument; a prefix two
    // revisions carry names neither. Both answer "changed", which is the
    // answer that cannot serve one revision's facts as another's.
    block(store.insert_or_ignore(
        "n_blob_oid_map",
        &["content_sha256", "blob_oid", "path", "first_seen"],
        &[
            // Sixteen characters of SHA_A, then a difference.
            Value::text(format!("{}{}", &SHA_A[..16], "c".repeat(48))),
            Value::text("oid_twin"),
            Value::text("src/twin.rs"),
            Value::Int(0),
        ],
    ))
    .unwrap();
    for given in [&SHA_A[..12], &SHA_A[..basis::MIN_BASIS_HEX]] {
        let mut budget = TurnBudget::default();
        let resp =
            block(s.dispatch(ToolName::FileIfChanged, &ask(given), &c, &mut budget)).unwrap();
        assert_eq!(
            resp.payload["result"]["state"],
            serde_json::json!("changed"),
            "{given:?} names no single revision, so nothing is known to be held"
        );
    }
}

#[test]
fn token_accounting_is_non_negative_and_honest() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::default();
    let resp = block(s.dispatch(
        ToolName::KnownState,
        &ranking_req(),
        &ctx(&store, &reader),
        &mut budget,
    ))
    .unwrap();
    // A manifest is never free: returned is the measured payload…
    assert!(resp.estimated_tokens_returned > 0);
    assert_eq!(
        resp.estimated_tokens_returned,
        rift_tokens::count(&resp.payload.to_string())
    );
    // …and avoided is NET: the captured re-read baseline (400 + 50) minus the
    // manifest's own cost, which still dominates for this corpus.
    assert!(resp.estimated_tokens_avoided >= resp.estimated_tokens_returned);
    assert!(resp.estimated_tokens_avoided > 0);
    assert_eq!(
        resp.estimated_tokens_avoided,
        450_u32.saturating_sub(resp.estimated_tokens_returned)
    );
}

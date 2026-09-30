//! What a pull is credited with: only the files the caller stated, that have
//! not moved, and that this family is recorded as having read.

use std::collections::BTreeSet;

use proptest::prelude::{prop_assert, prop_assert_eq, TestCaseError};
use rift_recall_policy::{PolicyEngine, TurnBudget};
use rift_recall_substrate::worktree::sha256_hex;
use rift_recall_substrate::MemorySubstrate;
use rift_store::SqliteStore;

use super::fixtures::*;
use super::pull::{pull, source_file, two_revisions};
use crate::verbs::credit::{Recorded, STATED_UNRECORDED};
use crate::*;

/// A corpus of `est.len()` files, each one revision read once in one
/// checkpoint, returned with its `(path, sha)` pairs in load order.
fn corpus(est: &[u32]) -> (SqliteStore, Vec<(String, String)>) {
    let s = open_store();
    seed_checkpoint(&s, "c1", 1);
    let mut files = Vec::new();
    for (i, tokens) in est.iter().enumerate() {
        let (path, sha, rev) = (
            format!("src/f{i:03}.rs"),
            format!("{:064x}", i + 1),
            format!("rev_{i:03}"),
        );
        seed_revision(&s, &rev, &path, &sha, &sha, 10);
        seed_read(&s, "c1", &rev, i64::from(*tokens), 1);
        files.push((path, sha));
    }
    (s, files)
}

fn serve(store: &SqliteStore, tool: ToolName, req: &ToolRequest) -> ToolResponse {
    try_serve(store, tool, req).unwrap()
}

fn try_serve(
    store: &SqliteStore,
    tool: ToolName,
    req: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let cfg = cfg();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::new(8, 100_000);
    block(s.dispatch(tool, req, &ctx(store, &reader), &mut budget))
}

fn stated(hashes: Vec<String>) -> ToolRequest {
    ToolRequest {
        task: Some("fix things".into()),
        files_in_focus: vec!["src/f000.rs".into()],
        token_budget: 4000,
        known_hashes: hashes,
        ..Default::default()
    }
}

/// The bound a list verb's figure must stay under, and whether it holds.
fn check_bound(resp: &ToolResponse, credited: u32) -> Result<(), TestCaseError> {
    let bound = credited.saturating_sub(resp.estimated_tokens_returned);
    prop_assert!(
        resp.estimated_tokens_avoided <= bound,
        "{:?}: avoided {} over the credited bound {bound}",
        resp.tool,
        resp.estimated_tokens_avoided
    );
    prop_assert_eq!(
        resp.estimated_tokens_returned,
        rift_tokens::count(&resp.payload.to_string())
    );
    Ok(())
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(48))]

    /// Over arbitrary candidates, stated hashes and recorded reads, a baseline
    /// is exactly the cost of the candidates that are stated, unchanged and
    /// recorded — a stated hash with no recorded read books nothing, and a
    /// call that stated nothing books nothing at all.
    #[test]
    fn a_baseline_counts_only_stated_unchanged_recorded_candidates(
        cands in proptest::collection::vec((0u8..16, 0u32..5000), 0..24),
        stated_ids in proptest::collection::btree_set(0u8..16, 0..16),
        recorded_ids in proptest::collection::btree_set(0u8..16, 0..16),
    ) {
        let hash = |id: u8| format!("{id:064x}");
        let hashes: Vec<(String, u32)> = cands.iter().map(|(id, est)| (hash(*id), *est)).collect();
        let stated_hashes: Vec<String> = stated_ids.iter().map(|id| hash(*id)).collect();
        let known: BTreeSet<&str> = stated_hashes.iter().map(String::as_str).collect();
        let recorded = Recorded::from_hashes(recorded_ids.iter().map(|id| hash(*id)).collect());
        let got = recorded.baseline(&known, hashes.iter().map(|(h, e)| (h.as_str(), *e)));
        let want = cands
            .iter()
            .filter(|(id, _)| stated_ids.contains(id) && recorded_ids.contains(id))
            .map(|(_, est)| *est)
            .fold(0u32, u32::saturating_add);
        prop_assert_eq!(got, want);
        let nothing_stated = recorded.baseline(
            &BTreeSet::new(),
            hashes.iter().map(|(h, e)| (h.as_str(), *e)),
        );
        prop_assert_eq!(nothing_stated, 0);
        for (h, est) in &hashes {
            if recorded.unrecorded(&known, h) {
                prop_assert_eq!(recorded.credit(&known, h, *est), 0);
            }
        }
    }

    /// Through the real dispatcher, every list verb's figure stays under the
    /// cost of what the caller stated minus what the answer cost, and a pull
    /// with no stated hash emits zero.
    #[test]
    fn every_list_verb_is_bounded_by_what_the_caller_stated(
        est in proptest::collection::vec(0u32..3000, 1..10),
        mask in proptest::collection::vec(proptest::bool::ANY, 10),
        foreign in proptest::bool::ANY,
    ) {
        let (store, files) = corpus(&est);
        let mut hashes: Vec<String> = files
            .iter()
            .zip(&mask)
            .filter(|(_, held)| **held)
            .map(|((_, sha), _)| sha.clone())
            .collect();
        if foreign {
            // A hash no revision carries is stated but matches nothing.
            hashes.push("f".repeat(64));
        }
        let credited = est
            .iter()
            .zip(&mask)
            .filter(|(_, held)| **held)
            .fold(0u32, |acc, (e, _)| acc.saturating_add(*e));
        let first_credit = if mask.first() == Some(&true) { est.first().copied().unwrap_or(0) } else { 0 };
        let mut req = stated(hashes.clone());
        req.target = files.first().map(|(p, _)| p.clone());
        for (tool, bound) in [
            (ToolName::KnownState, credited),
            (ToolName::DiffSince, credited),
            (ToolName::ContextPack, credited),
            (ToolName::FileIfChanged, first_credit),
        ] {
            let resp = match try_serve(&store, tool, &req) {
                Ok(resp) => resp,
                // Only the pack fails loud on a budget, and it claims nothing.
                Err(ToolError::PackBudgetTooSmall { .. }) => continue,
                Err(other) => return Err(TestCaseError::fail(format!("{other:?}"))),
            };
            check_bound(&resp, bound)?;
            prop_assert_eq!(resp.estimated_tokens_avoided, bound.saturating_sub(resp.estimated_tokens_returned));
            if hashes.is_empty() {
                prop_assert_eq!(resp.estimated_tokens_avoided, 0);
            }
        }
    }
}

/// The distortion this rule exists for: a delta list over hundreds of
/// candidates, asked with no stated hash, used to book every candidate's read
/// cost as avoided. It is a list, not a set of spared reads, and books zero.
#[test]
fn a_delta_list_over_many_candidates_with_nothing_stated_books_zero() {
    let est = vec![10_000u32; 240];
    let (store, files) = corpus(&est);
    let resp = serve(&store, ToolName::DiffSince, &ToolRequest::default());
    assert_eq!(
        resp.payload["changed"].as_array().map_or(0, Vec::len)
            + resp.payload["omitted"].as_u64().unwrap_or(0) as usize,
        files.len(),
        "the answer still lists every moved file: {}",
        resp.payload
    );
    assert_eq!(resp.estimated_tokens_avoided, 0);
    assert_eq!(
        AvoidedBasis::of(ToolName::DiffSince, &ToolRequest::default()),
        AvoidedBasis::NoneStated
    );

    // Stating two of them credits exactly those two.
    let two: Vec<String> = files.iter().take(2).map(|(_, s)| s.clone()).collect();
    let req = ToolRequest {
        known_hashes: two,
        ..Default::default()
    };
    let resp = serve(&store, ToolName::DiffSince, &req);
    assert_eq!(
        resp.estimated_tokens_avoided,
        20_000u32.saturating_sub(resp.estimated_tokens_returned)
    );
}

/// A file that moved spared no read of the bytes the caller holds.
#[test]
fn a_file_that_moved_books_nothing() {
    let (store, files) = corpus(&[900]);
    let req = ToolRequest {
        target: files.first().map(|(p, _)| p.clone()),
        known_hashes: vec!["e".repeat(64)],
        ..Default::default()
    };
    let resp = serve(&store, ToolName::FileIfChanged, &req);
    assert_eq!(
        resp.payload["result"]["state"],
        serde_json::json!("changed")
    );
    assert_eq!(resp.estimated_tokens_avoided, 0);
}

/// The pull against an older revision: `old` is what the caller holds, `new`
/// is the file now; with `recorded`, the family also has a read of `old` on
/// record, in a checkpoint older than the one that read `new`.
fn e1_pull(recorded: bool) -> (ToolResponse, Vec<u8>) {
    let old = source_file("");
    let new = source_file("9");
    let (store, reader) = two_revisions("src/a.rs", &old, &new, 400);
    if recorded {
        seed_checkpoint(&store, "c0", 0);
        let sha = sha256_hex(&old);
        seed_revision(&store, "rev_old", "src/a.rs", &sha, &sha, 10);
        seed_read(&store, "c0", "rev_old", 380, 1);
    }
    let cfg = cfg();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::new(8, 4000);
    let req = pull("src/a.rs", &sha256_hex(&old)[..basis::MIN_BASIS_HEX]);
    let resp = block(s.dispatch(
        ToolName::ChangedHunks,
        &req,
        &ctx(&store, &reader),
        &mut budget,
    ))
    .unwrap();
    (resp, new)
}

/// A basis this family recorded reading earns the current file's cost net of
/// the answer.
#[test]
fn a_hunk_pull_against_a_recorded_revision_books_the_current_file_net() {
    let (resp, new) = e1_pull(true);
    assert_eq!(resp.payload["state"], serde_json::json!("exact_included"));
    assert!(
        resp.payload.get("avoided_basis").is_none(),
        "{}",
        resp.payload
    );
    let full = rift_tokens::count(&String::from_utf8_lossy(&new));
    assert_eq!(
        resp.estimated_tokens_avoided,
        full.saturating_sub(resp.estimated_tokens_returned)
    );
    assert!(resp.estimated_tokens_avoided > 0);
}

/// A basis the caller named but this family never recorded reading earns
/// nothing, and the answer — the same changed lines — says why.
#[test]
fn a_hunk_pull_against_an_unrecorded_revision_books_zero() {
    let (resp, _) = e1_pull(false);
    assert_eq!(resp.payload["state"], serde_json::json!("exact_included"));
    assert!(resp.payload["content"].is_string(), "{}", resp.payload);
    assert_eq!(
        resp.payload["avoided_basis"],
        serde_json::json!(STATED_UNRECORDED)
    );
    assert_eq!(resp.estimated_tokens_avoided, 0);
    assert_eq!(resp.event.tokens_avoided(), 0);
}

/// A caller holding exactly the bytes on disk is still told so, but the
/// credit waits on a recorded read of them.
#[test]
fn a_held_unchanged_file_never_recorded_as_read_is_unchanged_and_uncredited() {
    let old = source_file("");
    let new = source_file("9");
    let (store, reader) = two_revisions("src/a.rs", &old, &new, 400);
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/a.rs"), &old).unwrap();
    let reader = reader.with_worktree_root(tmp.path());
    let cfg = cfg();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::new(8, 4000);
    let resp = block(s.dispatch(
        ToolName::ChangedHunks,
        &pull("src/a.rs", &sha256_hex(&old)),
        &ctx(&store, &reader),
        &mut budget,
    ))
    .unwrap();
    assert_eq!(resp.payload["state"], serde_json::json!("known_unchanged"));
    assert_eq!(
        resp.payload["avoided_basis"],
        serde_json::json!(STATED_UNRECORDED)
    );
    assert_eq!(resp.estimated_tokens_avoided, 0);
}

/// The ranked hunk shape answers about no file the caller said it held.
#[test]
fn the_ranked_hunk_shape_books_nothing() {
    let (store, _) = corpus(&[900, 800]);
    let resp = serve(&store, ToolName::ChangedHunks, &stated(Vec::new()));
    assert_eq!(resp.estimated_tokens_avoided, 0);
    assert_eq!(
        AvoidedBasis::of(ToolName::ChangedHunks, &stated(Vec::new())),
        AvoidedBasis::NotCredited
    );
}

/// The basis word is decided by the verb and the request, and each of the
/// four words is reached.
#[test]
fn the_basis_word_names_the_rule_the_figure_was_booked_under() {
    let held = stated(vec!["a".repeat(64)]);
    let targeted = ToolRequest {
        target: Some("src/a.rs".into()),
        ..held.clone()
    };
    let cases = [
        (ToolName::KnownState, &held, "stated_unchanged"),
        (ToolName::DiffSince, &stated(Vec::new()), "none_stated"),
        (ToolName::ChangedHunks, &targeted, "stated_unchanged"),
        (ToolName::ChangedHunks, &held, "not_credited"),
        (ToolName::ReadSummary, &targeted, "stated_unchanged"),
        (ToolName::ContextPack, &stated(Vec::new()), "none_stated"),
        (ToolName::FileIfChanged, &targeted, "stated_unchanged"),
        (ToolName::RetrieveExact, &held, "not_credited"),
        (ToolName::CostHotspots, &held, "not_credited"),
        (ToolName::DocRead, &held, "declared"),
        (ToolName::Search, &held, "declared"),
    ];
    for (tool, req, word) in cases {
        assert_eq!(AvoidedBasis::of(tool, req).as_str(), word, "{tool:?}");
    }
}

/// The tag lands only on an entry that was stated and never recorded as read.
#[test]
fn only_a_stated_unrecorded_entry_is_tagged() {
    let (held, read) = ("a".repeat(64), "b".repeat(64));
    let known: BTreeSet<&str> = [held.as_str(), read.as_str()].into();
    let recorded = Recorded::from_hashes(BTreeSet::from([read.clone()]));
    let mut tagged = serde_json::json!({ "path": "src/a.rs" });
    recorded.tag(&known, &held, &mut tagged);
    assert_eq!(
        tagged["avoided_basis"],
        serde_json::json!(STATED_UNRECORDED)
    );
    let mut plain = serde_json::json!({ "path": "src/b.rs" });
    recorded.tag(&known, &read, &mut plain);
    assert!(plain.get("avoided_basis").is_none());
    let mut unstated = serde_json::json!({ "path": "src/c.rs" });
    recorded.tag(&known, &"c".repeat(64), &mut unstated);
    assert!(unstated.get("avoided_basis").is_none());
}

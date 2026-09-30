//! What a pull is credited with: only the files the caller stated that the
//! checkout still holds and this family recorded reading — and, on the
//! answers that carry changed lines, a changed file whose older revision the
//! caller holds and this family recorded reading.

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::{prop_assert, prop_assert_eq, TestCaseError};
use rift_core::Timestamp;
use rift_recall_ledger::candidates::LoadedCandidate;
use rift_recall_policy::{PolicyEngine, TurnBudget};
use rift_recall_rank::Candidate;
use rift_recall_substrate::worktree::{sha256_hex, CurrentRefusal};
use rift_recall_substrate::MemorySubstrate;
use rift_store::SqliteStore;

use super::fixtures::*;
use super::pull::{pull, source_file, two_revisions};
use crate::verbs::credit::{Current, Evidence, STATED_UNRECORDED};
use crate::*;

/// A corpus of `est.len()` files, each one revision read once in one
/// checkpoint and held by the checkout, with its `(path, sha)` in load order.
fn corpus(est: &[u32]) -> (SqliteStore, Vec<(String, String)>, Checkout) {
    let s = open_store();
    seed_checkpoint(&s, "c1", 1);
    let checkout = Checkout::holding(&[]);
    let mut files = Vec::new();
    for (i, tokens) in est.iter().enumerate() {
        let path = format!("src/f{i:03}.rs");
        let bytes = format!("// file {i}\n");
        let sha = sha256_hex(bytes.as_bytes());
        let rev = format!("rev_{i:03}");
        seed_revision(&s, &rev, &path, &sha, &sha, 10);
        seed_read(&s, "c1", &rev, i64::from(*tokens), 1);
        checkout.write(&path, bytes.as_bytes());
        files.push((path, sha));
    }
    (s, files, checkout)
}

fn serve(
    store: &SqliteStore,
    reader: &MemorySubstrate,
    tool: ToolName,
    req: &ToolRequest,
) -> ToolResponse {
    try_serve(store, reader, tool, req).unwrap()
}

fn try_serve(
    store: &SqliteStore,
    reader: &MemorySubstrate,
    tool: ToolName,
    req: &ToolRequest,
) -> Result<ToolResponse, ToolError> {
    let cfg = cfg();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::new(8, 100_000);
    block(s.dispatch(tool, req, &ctx(store, reader), &mut budget))
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

/// A loaded candidate built by hand: revision `rev` of file `file`.
fn candidate(file: u8, rev: u8, est_tokens: u32) -> LoadedCandidate {
    LoadedCandidate {
        candidate: Candidate {
            id: format!("rev_{file}_{rev}"),
            path: format!("src/f{file}.rs"),
            error_signatures: BTreeSet::new(),
            last_touched: Timestamp::from_millis(0),
            outcomes: Vec::new(),
            est_tokens,
        },
        content_sha256: hash(file, rev),
        checkpoint: "c1".to_owned(),
    }
}

fn hash(file: u8, rev: u8) -> String {
    format!("{file:032x}{rev:032x}")
}

/// The bound a verb's figure must stay under, and the oracle that the figure
/// is the measured count of what it shipped.
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
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

    /// Over arbitrary candidates, stated revisions, recorded reads and what
    /// the checkout holds, a list's baseline is exactly the cost of the
    /// candidates that are stated, current and recorded, and a pack's adds
    /// the current changed files whose older revision is stated and recorded.
    /// Nothing stated books nothing; a stated revision never recorded books
    /// nothing.
    #[test]
    fn a_baseline_counts_only_stated_current_recorded_revisions(
        cands in proptest::collection::vec((0u8..6, 0u8..3, 0u32..5000), 0..18),
        stated_ids in proptest::collection::btree_set((0u8..6, 0u8..3), 0..12),
        recorded_ids in proptest::collection::btree_set((0u8..6, 0u8..3), 0..12),
        disk in proptest::collection::btree_map(0u8..6, proptest::option::of(0u8..3), 0..6),
    ) {
        let loaded: Vec<LoadedCandidate> =
            cands.iter().map(|(f, r, est)| candidate(*f, *r, *est)).collect();
        let stated_hashes: Vec<String> = stated_ids.iter().map(|(f, r)| hash(*f, *r)).collect();
        let known: BTreeSet<&str> = stated_hashes.iter().map(String::as_str).collect();
        let recorded: BTreeSet<(String, String)> = recorded_ids
            .iter()
            .filter(|id| stated_ids.contains(id))
            .map(|(f, r)| (format!("src/f{f}.rs"), hash(*f, *r)))
            .collect();
        let current: BTreeMap<String, Current> = disk
            .iter()
            .map(|(f, r)| {
                let now = r.map_or(Current::Unverified(CurrentRefusal::Missing), |r| {
                    Current::Verified(hash(*f, r))
                });
                (format!("src/f{f}.rs"), now)
            })
            .collect();
        let evidence = Evidence::from_parts(recorded, current);
        let on_disk = |f: u8, r: u8| disk.get(&f) == Some(&Some(r));
        let usable = |f: u8, r: u8| stated_ids.contains(&(f, r)) && recorded_ids.contains(&(f, r));
        let unchanged: u32 = cands
            .iter()
            .filter(|(f, r, _)| usable(*f, *r) && on_disk(*f, *r))
            .fold(0u32, |acc, (_, _, est)| acc.saturating_add(*est));
        let changed: u32 = cands
            .iter()
            .filter(|(f, r, _)| {
                !stated_ids.contains(&(*f, *r))
                    && on_disk(*f, *r)
                    && (0u8..3).any(|old| old != *r && usable(*f, old))
            })
            .fold(0u32, |acc, (_, _, est)| acc.saturating_add(*est));
        prop_assert_eq!(evidence.list_baseline(&known, &loaded), unchanged);
        prop_assert_eq!(evidence.pack_baseline(&known, &loaded), unchanged.saturating_add(changed));
        prop_assert_eq!(evidence.list_baseline(&BTreeSet::new(), &loaded), 0);
        prop_assert_eq!(evidence.pack_baseline(&BTreeSet::new(), &loaded), 0);
    }

    /// Through the real dispatcher, every verb's figure stays under the cost
    /// of what the caller stated and the checkout still holds, minus what the
    /// answer cost; a pull with no stated hash emits zero, and a file edited
    /// on disk after the projection earns nothing.
    #[test]
    fn every_verb_is_bounded_by_what_the_caller_stated_and_the_checkout_holds(
        est in proptest::collection::vec(0u32..3000, 1..10),
        held in proptest::collection::vec(proptest::bool::ANY, 10),
        moved in proptest::collection::vec(proptest::bool::ANY, 10),
        foreign in proptest::bool::ANY,
    ) {
        let (store, files, checkout) = corpus(&est);
        for ((path, _), on) in files.iter().zip(&moved) {
            if *on {
                checkout.write(path, b"// edited after the last checkpoint\n");
            }
        }
        let mut hashes: Vec<String> = files
            .iter()
            .zip(&held)
            .filter(|(_, h)| **h)
            .map(|((_, sha), _)| sha.clone())
            .collect();
        if foreign {
            hashes.push("f".repeat(64));
        }
        let credit = |i: usize| held.get(i) == Some(&true) && moved.get(i) != Some(&true);
        let credited = est
            .iter()
            .enumerate()
            .filter(|(i, _)| credit(*i))
            .fold(0u32, |acc, (_, e)| acc.saturating_add(*e));
        let first_credit = if credit(0) { est.first().copied().unwrap_or(0) } else { 0 };
        let mut req = stated(hashes.clone());
        req.target = files.first().map(|(p, _)| p.clone());
        for (tool, bound) in [
            (ToolName::KnownState, credited),
            (ToolName::DiffSince, credited),
            (ToolName::ContextPack, credited),
            (ToolName::FileIfChanged, first_credit),
        ] {
            let resp = match try_serve(&store, &checkout.reader(), tool, &req) {
                Ok(resp) => resp,
                // Only the pack fails loud on a budget, and it claims nothing.
                Err(ToolError::PackBudgetTooSmall { .. }) => continue,
                Err(other) => return Err(TestCaseError::fail(format!("{other:?}"))),
            };
            check_bound(&resp, bound)?;
            prop_assert_eq!(
                resp.estimated_tokens_avoided,
                bound.saturating_sub(resp.estimated_tokens_returned)
            );
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
    let (store, files, checkout) = corpus(&est);
    let reader = checkout.reader();
    let resp = serve(
        &store,
        &reader,
        ToolName::DiffSince,
        &ToolRequest::default(),
    );
    assert_eq!(
        resp.payload["changed"].as_array().map_or(0, Vec::len)
            + resp.payload["omitted"].as_u64().unwrap_or(0) as usize,
        files.len(),
        "the answer still lists every moved file: {}",
        resp.payload
    );
    assert_eq!(resp.estimated_tokens_avoided, 0);
    assert_eq!(resp.avoided_basis, AvoidedBasis::NoneStated);

    // Stating two of them credits exactly those two.
    let two: Vec<String> = files.iter().take(2).map(|(_, s)| s.clone()).collect();
    let req = ToolRequest {
        known_hashes: two,
        ..Default::default()
    };
    let resp = serve(&store, &reader, ToolName::DiffSince, &req);
    assert_eq!(
        resp.estimated_tokens_avoided,
        20_000u32.saturating_sub(resp.estimated_tokens_returned)
    );
}

/// A file that moved spared no read of the bytes the caller holds.
#[test]
fn a_file_that_moved_books_nothing() {
    let (store, files, checkout) = corpus(&[900]);
    let req = ToolRequest {
        target: files.first().map(|(p, _)| p.clone()),
        known_hashes: vec!["e".repeat(64)],
        ..Default::default()
    };
    let resp = serve(&store, &checkout.reader(), ToolName::FileIfChanged, &req);
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
    let (store, reader, _checkout) = two_revisions("src/a.rs", &old, &new, 400);
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
/// the answer, and the cost is that of the bytes on disk.
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
    assert_eq!(resp.avoided_basis, AvoidedBasis::StatedRecorded);
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
    let (store, _, checkout) = two_revisions("src/a.rs", &old, &new, 400);
    checkout.write("src/a.rs", &old);
    let reader = checkout.reader();
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

/// A pack carries the changed lines of a file whose older revision the caller
/// holds: when this family recorded reading that older revision, the pack is
/// credited the current file's cost, and the lines it shipped leave through
/// the one subtraction; when it did not, the file earns nothing.
#[test]
fn a_pack_credits_a_changed_file_only_against_a_recorded_older_revision() {
    let old = source_file("");
    let new = source_file("9");
    let figure = |recorded: bool| {
        let (store, reader, _checkout) = two_revisions("src/a.rs", &old, &new, 5000);
        let sha = sha256_hex(&old);
        seed_revision(&store, "rev_old", "src/a.rs", &sha, &sha, 10);
        if recorded {
            seed_checkpoint(&store, "c0", 0);
            seed_read(&store, "c0", "rev_old", 4000, 1);
        }
        let req = ToolRequest {
            task: Some("fix a.rs".into()),
            files_in_focus: vec!["src/a.rs".into()],
            token_budget: 4000,
            known_hashes: vec![sha],
            ..Default::default()
        };
        serve(&store, &reader, ToolName::ContextPack, &req)
    };
    let credited = figure(true);
    assert_eq!(
        credited.estimated_tokens_avoided,
        5000u32.saturating_sub(credited.estimated_tokens_returned),
        "{}",
        credited.payload
    );
    assert_eq!(figure(false).estimated_tokens_avoided, 0);
}

/// The ranked hunk shape answers about no file the caller said it held.
#[test]
fn the_ranked_hunk_shape_books_nothing() {
    let (store, _, checkout) = corpus(&[900, 800]);
    let resp = serve(
        &store,
        &checkout.reader(),
        ToolName::ChangedHunks,
        &stated(Vec::new()),
    );
    assert_eq!(resp.estimated_tokens_avoided, 0);
    assert_eq!(resp.avoided_basis, AvoidedBasis::NotCredited);
}

/// The basis word is decided by the verb, the request and the checkout, and
/// each of the five words is reached.
#[test]
fn the_basis_word_names_the_rule_the_figure_was_booked_under() {
    let held = stated(vec!["a".repeat(64)]);
    let targeted = ToolRequest {
        target: Some("src/a.rs".into()),
        ..held.clone()
    };
    let cases = [
        (ToolName::KnownState, &held, true, "stated_recorded"),
        (ToolName::KnownState, &held, false, "unverified"),
        (
            ToolName::DiffSince,
            &stated(Vec::new()),
            false,
            "none_stated",
        ),
        (ToolName::ChangedHunks, &targeted, true, "stated_recorded"),
        (ToolName::ChangedHunks, &held, true, "not_credited"),
        (ToolName::ReadSummary, &targeted, true, "stated_recorded"),
        (
            ToolName::ContextPack,
            &stated(Vec::new()),
            true,
            "none_stated",
        ),
        (ToolName::FileIfChanged, &targeted, false, "unverified"),
        (ToolName::RetrieveExact, &held, true, "not_credited"),
        (ToolName::CostHotspots, &held, true, "not_credited"),
        (ToolName::DocRead, &held, false, "declared"),
        (ToolName::Search, &held, true, "declared"),
    ];
    for (tool, req, checkout, word) in cases {
        assert_eq!(
            AvoidedBasis::of(tool, req, checkout).as_str(),
            word,
            "{tool:?}"
        );
    }
}

/// A stated entry says what the checkout and the record found: a current
/// file never recorded as read, a file that moved on disk, and a file that
/// could not be read each carry their tag; a recorded current one and an
/// unstated one carry nothing.
#[test]
fn a_stated_entry_is_tagged_with_what_the_checkout_and_the_record_found() {
    let entries = [
        candidate(1, 0, 10),
        candidate(2, 0, 10),
        candidate(3, 0, 10),
        candidate(4, 0, 10),
    ];
    let stated_hashes: Vec<String> = entries.iter().map(|lc| lc.content_sha256.clone()).collect();
    let known: BTreeSet<&str> = stated_hashes.iter().map(String::as_str).collect();
    let evidence = Evidence::from_parts(
        BTreeSet::from([("src/f4.rs".to_owned(), hash(4, 0))]),
        BTreeMap::from([
            ("src/f1.rs".to_owned(), Current::Verified(hash(1, 0))),
            ("src/f2.rs".to_owned(), Current::Verified(hash(2, 1))),
            (
                "src/f3.rs".to_owned(),
                Current::Unverified(CurrentRefusal::Missing),
            ),
            ("src/f4.rs".to_owned(), Current::Verified(hash(4, 0))),
        ]),
    );
    let tagged = |lc: &LoadedCandidate, known: &BTreeSet<&str>| {
        let mut entry = serde_json::json!({ "path": lc.candidate.path });
        evidence.tag(known, lc, &mut entry);
        entry
    };
    assert_eq!(
        tagged(&entries[0], &known)["avoided_basis"],
        serde_json::json!(STATED_UNRECORDED)
    );
    assert_eq!(
        tagged(&entries[1], &known)["worktree_sha256"],
        serde_json::json!(hash(2, 1))
    );
    assert_eq!(
        tagged(&entries[2], &known)["unverified"],
        serde_json::json!("missing")
    );
    assert_eq!(
        tagged(&entries[3], &known),
        serde_json::json!({ "path": "src/f4.rs" })
    );
    assert_eq!(
        tagged(&entries[0], &BTreeSet::new()),
        serde_json::json!({ "path": "src/f1.rs" })
    );
}

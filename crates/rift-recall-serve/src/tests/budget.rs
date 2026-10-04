//! The per-turn budget: the call ceiling, the anchor, the single clamped
//! charge — and the accounting property over arbitrary call sequences.

use proptest::prelude::{prop_assert, prop_assert_eq, Strategy, TestCaseError};
use rift_recall_policy::{PolicyEngine, TurnBudget};
use rift_recall_substrate::MemorySubstrate;
use rift_store::SqliteStore;

use super::fixtures::*;
use crate::*;

#[test]
fn budget_never_exceeded_over_a_turn() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    // The skeleton edge: a ceiling so tiny even the empty
    // manifest skeleton exceeds it. Every entry drops (kept == 0), the
    // truncation is honest, the skeleton still ships (the anchor is never
    // stranded), and the clamped charge drains the ceiling without wrapping.
    // The property test below grows this into an arbitrary call sequence.
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::new(4, 3);
    let resp = block(s.dispatch(
        ToolName::KnownState,
        &ranking_req(),
        &ctx(&store, &reader),
        &mut budget,
    ))
    .unwrap();
    assert_eq!(resp.payload["envelopes"].as_array().unwrap().len(), 0);
    assert_eq!(resp.payload["truncated"], true);
    assert_eq!(resp.payload["omitted"], 2, "both seeded entries dropped");
    assert_eq!(
        resp.estimated_tokens_returned,
        rift_tokens::count(&resp.payload.to_string()),
        "returned is the measured (skeleton) payload count"
    );
    assert_eq!(
        budget.tokens_remaining(),
        0,
        "the clamped charge drains the ceiling, never wraps past it"
    );
}

#[test]
fn known_state_anchors_and_refills_the_turn_budget() {
    // A known_state call resets the turn budget; a mid-turn call
    // draws it down, and the next known_state refills it.
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::new(2, 10_000);
    let c = ctx(&store, &reader);
    // Turn 1: known_state anchors (1 call used), then a draw-down call (2 used).
    assert!(block(s.dispatch(ToolName::KnownState, &ranking_req(), &c, &mut budget)).is_ok());
    assert!(block(s.dispatch(
        ToolName::CostHotspots,
        &ToolRequest::default(),
        &c,
        &mut budget
    ))
    .is_ok());
    // The call ceiling now binds within the turn.
    assert_eq!(
        block(s.dispatch(
            ToolName::CostHotspots,
            &ToolRequest::default(),
            &c,
            &mut budget
        )),
        Err(ToolError::BudgetCallsExhausted)
    );
    // Turn 2: a fresh known_state anchor refills the allowance.
    assert!(block(s.dispatch(ToolName::KnownState, &ranking_req(), &c, &mut budget)).is_ok());
    assert_eq!(
        budget.calls_remaining(),
        1,
        "anchor reset the call draw-down"
    );
}

#[test]
fn per_turn_call_ceiling_is_enforced() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::new(2, 10_000);
    let c = ctx(&store, &reader);
    assert!(block(s.dispatch(
        ToolName::CostHotspots,
        &ToolRequest::default(),
        &c,
        &mut budget
    ))
    .is_ok());
    assert!(block(s.dispatch(
        ToolName::CostHotspots,
        &ToolRequest::default(),
        &c,
        &mut budget
    ))
    .is_ok());
    assert_eq!(
        block(s.dispatch(
            ToolName::CostHotspots,
            &ToolRequest::default(),
            &c,
            &mut budget
        )),
        Err(ToolError::BudgetCallsExhausted)
    );
}

/// Seed a store with a drawn corpus: one checkpoint, one revision + one read
/// per file, `est[i]` as the captured `input_tokens`, and a checkout holding
/// every file as recorded. Returns `(store, [(path, sha)], checkout)` in the
/// deterministic id order candidates load in.
fn corpus_store(est: &[u32]) -> (SqliteStore, Vec<(String, String)>, Checkout) {
    let s = open_store();
    seed_checkpoint(&s, "c1", 1);
    let checkout = Checkout::holding(&[]);
    let mut corpus = Vec::new();
    for (i, tokens) in est.iter().enumerate() {
        let path = format!("src/file_{i:02}.rs");
        let text = corpus_text(i);
        let sha = rift_recall_substrate::worktree::sha256_hex(text.as_bytes());
        let rev = format!("rev_{i:02}");
        seed_revision(&s, &rev, &path, &sha, &sha, 10);
        seed_read(&s, "c1", &rev, i64::from(*tokens), 1);
        checkout.write(&path, text.as_bytes());
        corpus.push((path, sha));
    }
    (s, corpus, checkout)
}

/// The bytes of corpus file `i`.
fn corpus_text(i: usize) -> String {
    format!("// corpus file {i}\n")
}

/// The baseline of a verb this sequence never draws. Named rather than written
/// as a bare zero so a reader can see it is a hole that is never entered, not a
/// claim that the verb avoids nothing.
fn unreachable_baseline() -> u32 {
    0
}

/// One drawn call: which of the eight projection verbs, and whether it
/// carries the query params, the first file's hash, and a reason.
type Draw = (usize, bool, bool, bool);

/// The strategy behind a turn: between one and seven draws.
fn call_sequence() -> impl Strategy<Value = Vec<Draw>> {
    proptest::collection::vec(
        (
            0usize..8,
            proptest::bool::ANY,
            proptest::bool::ANY,
            proptest::bool::ANY,
        ),
        1..8,
    )
}

/// The verbs a scope contains — every one whose answer comes out of the
/// projection. Indexed rather than sliced so the sequence generator's range
/// and this list cannot drift into two different eights.
fn projection_verbs() -> Vec<ToolName> {
    TOOL_SURFACE
        .iter()
        .copied()
        .filter(|t| t.is_scope_bound())
        .collect()
}

/// The drawn corpus, as the shadow model prices it.
struct Corpus<'a> {
    est: &'a [u32],
    files: &'a [(String, String)],
}

impl Corpus<'_> {
    fn est_of(&self, path: &str) -> u32 {
        self.files
            .iter()
            .position(|(p, _)| p == path)
            .and_then(|i| self.est.get(i).copied())
            .unwrap_or(0)
    }

    fn first(&self) -> (String, String) {
        self.files.first().cloned().unwrap()
    }
}

/// The shadow model: what anchoring and the accounting seam say MUST happen.
/// The REAL `TurnBudget` must never depart from it — and the shadow
/// structurally cannot exceed its ceilings.
struct Shadow {
    max_calls: u32,
    max_tokens: u32,
    calls_used: u32,
    tokens_used: u32,
}

impl Shadow {
    fn new(max_calls: u32, max_tokens: u32) -> Self {
        Self {
            max_calls,
            max_tokens,
            calls_used: 0,
            tokens_used: 0,
        }
    }

    fn remaining(&self) -> u32 {
        self.max_tokens.saturating_sub(self.tokens_used)
    }

    /// One drawn call against the real dispatcher, with every invariant the
    /// shadow model states about it.
    fn step(
        &mut self,
        s: &ServeDispatcher<'_>,
        c: &RecallContext<'_, SqliteStore, MemorySubstrate>,
        budget: &mut TurnBudget,
        corpus: &Corpus<'_>,
        req_budget: u32,
        draw: Draw,
    ) -> Result<(), TestCaseError> {
        let (tool_idx, with_params, know_first, _) = draw;
        let tool = projection_verbs()[tool_idx];
        let req = request_for(tool, corpus, req_budget, draw);
        // Shadow: the anchor resets the draw-down.
        if matches!(tool, ToolName::KnownState) {
            self.calls_used = 0;
            self.tokens_used = 0;
        }
        // Shadow: the per-turn call ceiling.
        if self.calls_used >= self.max_calls {
            prop_assert_eq!(
                block(s.dispatch(tool, &req, c, budget)),
                Err(ToolError::BudgetCallsExhausted)
            );
            return Ok(());
        }
        self.calls_used += 1;
        // Shadow: the query-aware requirement, and the one call that is
        // exempt from it — a hunk pull that named both a path and a
        // revision has asked about one file and never ranks.
        let pull = matches!(tool, ToolName::ChangedHunks) && know_first;
        if tool.is_ranking_sensitive() && !pull && !with_params {
            prop_assert_eq!(
                block(s.dispatch(tool, &req, c, budget)),
                Err(ToolError::QueryParamsRequired)
            );
            return Ok(());
        }
        let remaining_before = self.remaining();
        match block(s.dispatch(tool, &req, c, budget)) {
            Ok(resp) => {
                check_figures(&resp, tool, corpus, know_first)?;
                if matches!(tool, ToolName::KnownState | ToolName::DiffSince) {
                    check_manifest_bounds(&resp, tool, req_budget, remaining_before, corpus)?;
                }
                // Shadow: the seam's single clamped charge.
                self.tokens_used = self
                    .tokens_used
                    .saturating_add(resp.estimated_tokens_returned.min(remaining_before));
            }
            Err(ToolError::PackBudgetTooSmall { .. }) => {
                prop_assert!(
                    matches!(tool, ToolName::ContextPack),
                    "only the pack fail-louds on budget"
                );
            }
            Err(other) => prop_assert!(false, "unexpected error: {other:?}"),
        }
        prop_assert_eq!(budget.tokens_remaining(), self.remaining());
        prop_assert_eq!(
            budget.calls_remaining(),
            self.max_calls.saturating_sub(self.calls_used)
        );
        prop_assert!(self.tokens_used <= self.max_tokens);
        Ok(())
    }
}

/// The request one draw makes: the first file as its target — as a handle
/// for exact retrieval — its hash when the draw says the caller holds it,
/// and the query params and the reason when the draw says so.
fn request_for(tool: ToolName, corpus: &Corpus<'_>, req_budget: u32, draw: Draw) -> ToolRequest {
    let (_, with_params, know_first, with_reason) = draw;
    let (first_path, first_sha) = corpus.first();
    ToolRequest {
        task: if with_params {
            Some("fix things".to_string())
        } else {
            None
        },
        files_in_focus: if with_params {
            vec![first_path.clone()]
        } else {
            Vec::new()
        },
        token_budget: req_budget,
        known_hashes: if know_first {
            vec![first_sha.clone()]
        } else {
            Vec::new()
        },
        target: Some(match tool {
            ToolName::RetrieveExact => format!("file:{first_path}@sha256:{first_sha}"),
            _ => first_path.clone(),
        }),
        reason: if with_reason {
            Some("debugging the crash".to_string())
        } else {
            None
        },
        ..Default::default()
    }
}

/// Every Ok response satisfies: returned == the measured payload; event
/// figures == response figures; avoided == baseline − returned.
fn check_figures(
    resp: &ToolResponse,
    tool: ToolName,
    corpus: &Corpus<'_>,
    know_first: bool,
) -> Result<(), TestCaseError> {
    let measured = rift_tokens::count(&resp.payload.to_string());
    prop_assert_eq!(resp.estimated_tokens_returned, measured);
    prop_assert_eq!(resp.event.tokens_returned(), resp.estimated_tokens_returned);
    prop_assert_eq!(resp.event.tokens_avoided(), resp.estimated_tokens_avoided);
    let baseline = expected_baseline(tool, corpus, know_first);
    prop_assert_eq!(
        resp.estimated_tokens_avoided,
        baseline.saturating_sub(resp.estimated_tokens_returned)
    );
    Ok(())
}

/// Net avoided is measured against the tool's gross baseline: the one file a
/// draw can state is the first, every file in the corpus is recorded as read,
/// so a crediting verb's baseline is that file's cost when it was stated and
/// nothing otherwise. The hunk pull that stated it finds the file unchanged;
/// the ranked shape states nothing it can be credited for.
fn expected_baseline(tool: ToolName, corpus: &Corpus<'_>, know_first: bool) -> u32 {
    let stated = if know_first {
        corpus.est_of(&corpus.first().0)
    } else {
        0
    };
    match tool {
        ToolName::KnownState
        | ToolName::DiffSince
        | ToolName::ContextPack
        | ToolName::FileIfChanged => stated,
        // A pull against the current revision is credited the cost of the
        // bytes on disk, not the captured estimate.
        ToolName::ChangedHunks if know_first => rift_tokens::count(&corpus_text(0)),
        ToolName::ChangedHunks
        | ToolName::ReadSummary
        | ToolName::RetrieveExact
        | ToolName::CostHotspots => 0,
        // Not drawn by this sequence (`projection_verbs`), and an arm that
        // guessed a figure for either would be a property this fixture cannot
        // see fail.
        ToolName::DocRead | ToolName::Search => unreachable_baseline(),
    }
}

/// Bounded manifests fit the effective cap or ship the empty skeleton, with
/// an honest `truncated ⇔ dropped`.
fn check_manifest_bounds(
    resp: &ToolResponse,
    tool: ToolName,
    req_budget: u32,
    remaining_before: u32,
    corpus: &Corpus<'_>,
) -> Result<(), TestCaseError> {
    let cap = if req_budget == 0 {
        DEFAULT_MANIFEST_TOKENS
    } else {
        req_budget.clamp(1, MAX_MANIFEST_TOKENS)
    }
    .min(remaining_before);
    let kept = match tool {
        ToolName::KnownState => resp.payload["envelopes"].as_array().map_or(0, Vec::len),
        _ => {
            resp.payload["changed"].as_array().map_or(0, Vec::len)
                + resp.payload["unchanged"].as_array().map_or(0, Vec::len)
        }
    };
    let omitted = resp.payload["omitted"].as_u64().unwrap_or(0) as usize;
    prop_assert_eq!(kept + omitted, corpus.files.len());
    prop_assert_eq!(resp.payload["truncated"].as_bool(), Some(omitted > 0));
    prop_assert!(
        resp.estimated_tokens_returned <= cap || kept == 0,
        "manifest over cap without full truncation: returned={} cap={} kept={kept}",
        resp.estimated_tokens_returned,
        cap
    );
    Ok(())
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(32))]
    /// The accounting property: over arbitrary
    /// corpora, ceilings, request budgets, and call sequences across all 8
    /// projection-backed tools (the document verb answers from the family's
    /// sidecar store, which no params object can reach, and carries its own
    /// figures fixture), every Ok response satisfies — returned == the measured payload;
    /// event figures == response figures; avoided == baseline − returned;
    /// bounded tools fit the effective cap or ship the empty skeleton with an
    /// honest `truncated ⇔ dropped`; and the REAL `TurnBudget` never departs
    /// from a shadow model that can never exceed its ceilings.
    #[test]
    fn turn_budget_and_figures_hold_over_arbitrary_call_sequences(
        est in proptest::collection::vec(0u32..2000, 1..12),
        max_calls in 1u32..6,
        max_tokens in 0u32..3000,
        req_budget in 0u32..=4000,
        seq in call_sequence(),
    ) {
        let cfg = cfg();
        let (store, files, checkout) = corpus_store(&est);
        let reader = checkout.reader();
        let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
        let c = ctx(&store, &reader);
        let corpus = Corpus { est: &est, files: &files };
        prop_assert_eq!(projection_verbs().len(), 8);
        let mut budget = TurnBudget::new(max_calls, max_tokens);
        let mut shadow = Shadow::new(max_calls, max_tokens);
        for draw in seq {
            shadow.step(&s, &c, &mut budget, &corpus, req_budget, draw)?;
        }
    }
}

/// Every tool's emitted figure IS the measured payload —
/// `returned == rift_tokens::count(serialized payload)` — and the sealed event
/// carries the same pair wherever its variant has the fields (`Refused`/
/// `Invalidated` audit the decision, not figures — their accessors return 0 by
/// design).
#[test]
fn every_tool_emits_the_measured_payload_count() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local().with_blob(SHA_A, b"fn main() {}\n");
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let c = ctx(&store, &reader);
    let calls: Vec<(ToolName, ToolRequest)> = vec![
        (ToolName::KnownState, ranking_req()),
        (ToolName::DiffSince, ToolRequest::default()),
        (
            ToolName::FileIfChanged,
            ToolRequest {
                target: Some("src/a.rs".into()),
                ..Default::default()
            },
        ),
        (ToolName::ChangedHunks, ranking_req()),
        (
            ToolName::ReadSummary,
            ToolRequest {
                target: Some("src/a.rs".into()),
                ..Default::default()
            },
        ),
        (
            ToolName::RetrieveExact,
            ToolRequest {
                target: Some(format!("file:src/a.rs@sha256:{SHA_A}")),
                reason: Some("debugging the crash".into()),
                ..Default::default()
            },
        ),
        (ToolName::CostHotspots, ToolRequest::default()),
        (ToolName::ContextPack, ranking_req()),
    ];
    for (tool, req) in calls {
        let mut budget = TurnBudget::new(8, 10_000);
        let resp = block(s.dispatch(tool, &req, &c, &mut budget)).unwrap();
        let measured = rift_tokens::count(&resp.payload.to_string());
        assert!(
            measured > 0,
            "{tool:?}: a serialized response is never free"
        );
        assert_eq!(
            resp.estimated_tokens_returned, measured,
            "{tool:?}: returned == measured payload count"
        );
        assert_eq!(
            resp.event.tokens_returned(),
            resp.estimated_tokens_returned,
            "{tool:?}: event figure == response figure (every variant)"
        );
        assert_eq!(
            resp.event.tokens_avoided(),
            resp.estimated_tokens_avoided,
            "{tool:?}: event avoided == response avoided"
        );
    }
}

//! The wire surface: the closed tool list, the decoder, the two spellings of
//! one surface, context planning, and the trust and audit vocabulary.

use rift_recall_policy::{PolicyEngine, TurnBudget};
use rift_recall_substrate::MemorySubstrate;
use rift_recall_types::Trust;
use serde_json::json;

use super::fixtures::*;
use crate::*;

#[test]
fn context_planning_has_one_transport_independent_priority_order() {
    let candidate = |kind, text: &str| ContextCandidate {
        kind,
        text: text.to_owned(),
        avoided_tokens: 0,
        fallback: false,
    };
    let plan = ServeDispatcher::plan_context([
        candidate(ContextKind::DeltaManifest, "delta"),
        candidate(ContextKind::SessionPointer, "pointer"),
        candidate(ContextKind::ReadReplacement, "replacement"),
        candidate(ContextKind::RecoveryMap, "recovery"),
    ]);
    assert_eq!(plan.kind(), Some(ContextKind::ReadReplacement));
    assert_eq!(plan.text(), Some("replacement"));
    let ContextPlan::Deliver {
        delivered_tokens, ..
    } = plan
    else {
        panic!("a non-empty candidate is delivered");
    };
    assert_eq!(delivered_tokens, rift_tokens::count("replacement"));

    assert_eq!(
        ServeDispatcher::plan_context([
            candidate(ContextKind::DeltaManifest, ""),
            candidate(ContextKind::RecoveryMap, "recovery"),
        ])
        .kind(),
        Some(ContextKind::RecoveryMap)
    );
    assert_eq!(
        ServeDispatcher::plan_context(std::iter::empty::<ContextCandidate>()).kind(),
        None
    );
}

/// The surface is closed, and closed at a list rather than at a number: a
/// length alone would let a tool be swapped for another without a word.
///
/// Eight verbs answer out of the projection; the other two do not, and the list
/// is where that is visible. `doc-read` answers out of the family's sidecar
/// store; `search` answers out of the member's own path index and work tree.
///
/// **What "no search" still means, now that there is one.** The closed surface
/// has never held a *transcript* search or an open-ended *session* search, and
/// it still does not: both would be a query over what was said, answered out of
/// the projection, with no bound anybody could state. `search` is a query over
/// the paths in this checkout and the lines in the tracked ones — the call an
/// agent would otherwise make with a glob and a grep — and it is bounded by the
/// turn budget like every other verb here. The two claims are pinned separately
/// below so the second cannot quietly absorb the first.
#[test]
fn the_tool_surface_is_the_closed_list_and_holds_no_transcript_search() {
    assert_eq!(
        TOOL_SURFACE,
        &[
            ToolName::KnownState,
            ToolName::DiffSince,
            ToolName::FileIfChanged,
            ToolName::ChangedHunks,
            ToolName::ReadSummary,
            ToolName::RetrieveExact,
            ToolName::CostHotspots,
            ToolName::ContextPack,
            ToolName::DocRead,
            ToolName::Search,
        ]
    );
    assert_eq!(
        TOOL_SURFACE.iter().filter(|t| t.is_scope_bound()).count(),
        8,
        "the document and search verbs answer out of neither the projection nor \
         the object database"
    );
    assert!(!TOOL_SURFACE
        .iter()
        .any(|t| t.wire_name().contains("transcript") || t.wire_name().contains("session")));
    assert_eq!(
        ToolName::from_wire("rift_known_state"),
        Some(ToolName::KnownState)
    );
    assert_eq!(ToolName::from_wire("rift_search"), Some(ToolName::Search));
    assert_eq!(ToolName::from_wire("rift_transcript_search"), None);
}

#[test]
fn decode_request_is_total_and_rejects_unknown_tools() {
    // unknown tool → protocol error
    assert_eq!(
        decode_request("rift_transcript_search", &serde_json::json!({})),
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

/// **The lookup half of the one `/` spelling.** The decoder every serving
/// channel goes through re-spells the paths a caller states, and leaves the
/// two targets that are not paths exactly as typed.
#[test]
fn the_decoder_respells_stated_paths_and_leaves_the_others_alone() {
    let (_, req) = decode_request(
        "rift_file_if_changed",
        &serde_json::json!({ "target": r"bins\rift\src\hooks.rs" }),
    )
    .unwrap();
    assert_eq!(req.target.as_deref(), Some("bins/rift/src/hooks.rs"));

    let (_, req) = decode_request(
        "rift_known_state",
        &serde_json::json!({ "files_in_focus": [r"crates\rift-store\src\schema.rs", "a.rs"] }),
    )
    .unwrap();
    assert_eq!(
        req.files_in_focus,
        vec![
            "crates/rift-store/src/schema.rs".to_owned(),
            "a.rs".to_owned()
        ]
    );

    // A pack citation and a handle are Rift's own composed addresses, and a
    // search target is a query the caller meant: none is a path to re-spell.
    for (wire, target) in [
        ("rift_retrieve_exact", r"log:a\b"),
        ("rift_search", r"fn read\(.*\)"),
    ] {
        let (_, req) = decode_request(wire, &serde_json::json!({ "target": target })).unwrap();
        assert_eq!(
            req.target.as_deref(),
            Some(target),
            "{wire} target is not a path and must arrive as typed"
        );
    }

    // …and the split is stated once, over the whole surface, so a new verb
    // has to decide which side it is on.
    for tool in TOOL_SURFACE {
        assert_eq!(
            tool.target_is_path(),
            matches!(
                tool,
                ToolName::FileIfChanged
                    | ToolName::ChangedHunks
                    | ToolName::ReadSummary
                    | ToolName::DocRead
            ),
            "{tool:?}"
        );
    }
}

/// The defect, end to end: rows are `/`-spelled, the caller typed `\`, and
/// the answer is about the file rather than `invalid / path_not_found`.
#[test]
fn a_backslash_spelled_argument_resolves_against_a_slash_spelled_row() {
    let cfg = cfg();
    let store = seeded_store();
    let reader = MemorySubstrate::local();
    let s = ServeDispatcher::new(PolicyEngine::new(&cfg));
    let mut budget = TurnBudget::new(10, 10_000);
    let c = ctx(&store, &reader);

    let (tool, req) = decode_request(
        "rift_file_if_changed",
        &serde_json::json!({ "target": r"src\a.rs", "known_hashes": [SHA_A] }),
    )
    .unwrap();
    let resp = block(s.dispatch(tool, &req, &c, &mut budget)).unwrap();
    assert_eq!(resp.payload["result"]["state"], "unchanged");
    assert_eq!(
        resp.payload["result"]["path"], "src/a.rs",
        "the answer names the file the way every row and every hint spells it"
    );
}

proptest::proptest! {
    /// Fuzz: the request decoder never panics on arbitrary JSON shapes for a
    /// known tool, and ill-typed fields default rather than abort.
    ///
    /// TWO ARMS, because the first one on its own was reaching almost nothing.
    /// `.{0,200}` of noise is a valid JSON *document* essentially never, so the
    /// guarded block below ran on close to no case and the property's real
    /// subject — the decoder — was barely executed. The second arm ASSEMBLES a
    /// params object around the drawn text, so the decoder is reached on every
    /// case, and its output is EXAMINED rather than discarded into `let _`.
    ///
    /// `(?s)` matters here specifically: a field's text is carried through
    /// verbatim, and a decoder that truncated at a newline would be invisible
    /// to a single-line draw.
    #[test]
    fn decode_never_panics(s in "(?s).{0,200}") {
        // Arm 1 — totality over raw input, kept as it was.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
            let _ = decode_request("rift_known_state", &v);
            let _ = decode_request("rift_retrieve_exact", &v);
            let _ = decode_request("bogus_tool", &v);
        }
        // also exercise raw non-JSON via a wrapped value
        let _ = decode_request("rift_diff_since", &serde_json::Value::String(s.clone()));

        // Arm 2 — assembled, so the decoder actually runs, with one ill-typed
        // field of each shape beside the well-typed ones.
        let params = serde_json::json!({
            "task": s,
            "files_in_focus": [s, 7, null],
            "token_budget": "not a number",
            "top_n": -1,
            "target": s,
        });
        let (tool, req) = decode_request("rift_known_state", &params).unwrap();
        proptest::prop_assert_eq!(tool, ToolName::KnownState);
        // Text survives byte-exact — newlines, controls and all. `known-state`
        // has no path-shaped target, so its `target` is carried through too.
        proptest::prop_assert_eq!(req.task.as_deref(), Some(s.as_str()));
        proptest::prop_assert_eq!(req.target.as_deref(), Some(s.as_str()));
        // Non-strings in a string array are dropped, not aborted on — and the
        // strings that survive are the ONE thing the decoder rewrites: a
        // stated path arrives `/`-spelled, whatever separator the caller typed
        // (`the_decoder_respells_stated_paths_and_leaves_the_others_alone`).
        let respelled = s.replace('\\', "/");
        proptest::prop_assert_eq!(req.files_in_focus, vec![respelled]);
        // Ill-typed numerics default rather than fail the whole request.
        proptest::prop_assert_eq!(req.token_budget, 0);
        proptest::prop_assert_eq!(req.top_n, None);
        // And the tool name is still the only thing that can refuse a decode.
        proptest::prop_assert_eq!(
            decode_request("bogus_tool", &params).unwrap_err(),
            DecodeError::UnknownTool
        );
    }
}

/// The CLI spelling is the wire spelling, mechanically: drop the `rift_`
/// prefix and swap `_` for `-`. Pinning the relation (rather than just the
/// literals) is what stops a future tool from being added to one surface and
/// forgotten on the other.
#[test]
fn verb_spelling_is_the_wire_spelling_kebabed() {
    for tool in TOOL_SURFACE.iter().copied() {
        let derived = tool
            .wire_name()
            .strip_prefix("rift_")
            .expect("every wire name is rift_-prefixed")
            .replace('_', "-");
        assert_eq!(tool.verb_name(), derived, "{tool:?}");
    }
}

/// The verb surface is a closed bijection: every tool has exactly one verb,
/// every verb round-trips, and nothing outside the eight decodes.
#[test]
fn verb_surface_is_a_closed_bijection() {
    let mut seen = std::collections::BTreeSet::new();
    for tool in TOOL_SURFACE.iter().copied() {
        assert!(seen.insert(tool.verb_name()), "duplicate verb spelling");
        assert_eq!(ToolName::from_verb(tool.verb_name()), Some(tool));
    }
    assert_eq!(seen.len(), TOOL_SURFACE.len());
    for unknown in [
        "known_state",       // the wire spelling is not a CLI verb
        "rift_known_state",  // nor is the fully-qualified wire name
        "why",               // the legacy backend's provenance verb is not ours
        "transcript-search", // searching what was said is not on this surface
        "",
    ] {
        assert_eq!(
            ToolName::from_verb(unknown),
            None,
            "{unknown:?} must not decode"
        );
        assert_eq!(
            decode_verb(unknown, &json!({})).unwrap_err(),
            DecodeError::UnknownTool
        );
    }
}

/// A verb call and a wire call with the same params decode to the same typed
/// request — the CLI is a second caller of one surface, never a second
/// implementation.
#[test]
fn decode_verb_agrees_with_decode_request() {
    let params = json!({
        "task": "fix the flush settle",
        "files_in_focus": ["crates/rift-shadow/src/lib.rs"],
        "error_signatures": ["E0432"],
        "token_budget": 900,
        "known_hashes": [SHA_A],
        "target": "file:a.rs@sha256:aa",
        "reason": "debugging",
        "top_n": 3,
    });
    for tool in TOOL_SURFACE.iter().copied() {
        let (via_verb, verb_req) = decode_verb(tool.verb_name(), &params).unwrap();
        let (via_wire, wire_req) = decode_request(tool.wire_name(), &params).unwrap();
        assert_eq!(via_verb, tool);
        assert_eq!(via_verb, via_wire);
        assert_eq!(verb_req, wire_req);
    }
}

/// The verbs that can need bytes are exactly the handlers that reach for the
/// reader. A caller uses this to decide whether answering requires opening a
/// repository at all, so a handler that starts fetching blobs under a verb
/// this says `false` for would be handed an empty substrate and answer
/// "unavailable" for a reason no one could see. Asserted against the whole
/// surface, both directions.
#[test]
fn substrate_readers_are_the_verbs_that_reach_for_bytes() {
    let reaching: Vec<ToolName> = crate::TOOL_SURFACE
        .iter()
        .copied()
        .filter(|t| t.may_read_the_substrate())
        .collect();
    assert_eq!(
        reaching,
        vec![
            ToolName::ChangedHunks,
            ToolName::RetrieveExact,
            ToolName::ContextPack
        ],
        "the three handlers that call ctx.reader, and no others"
    );
    // The push tier's own verb is on the other side of the line: a manifest
    // is computed from the projection, so nothing it asks opens a repository.
    assert!(!ToolName::DiffSince.may_read_the_substrate());
    assert!(!ToolName::KnownState.may_read_the_substrate());
}

#[test]
fn trust_is_provenance_derived_not_caller_declared() {
    // A file handle is a code fact; a log handle is untrusted; junk fails closed.
    assert_eq!(
        provenance_trust(&format!("file:src/a.rs@sha256:{SHA_A}")),
        Trust::CodeFact
    );
    assert_eq!(provenance_trust("log:tool-9"), Trust::ToolOutputUntrusted);
    assert_eq!(provenance_trust("not a handle"), Trust::ToolOutputUntrusted);
    assert_eq!(
        handle_display_path(&format!("file:src/a.rs@sha256:{SHA_A}")),
        "src/a.rs"
    );
}

/// The grant's two halves land in the two places their contracts allow: the
/// FIXED tag in the frozen neutral-tag column, the person's words nowhere near
/// it. Nothing a caller supplies can reach `memd_audit_event.reason` through
/// this envelope, because the value is a constant of this crate.
#[test]
fn a_grant_tags_the_audit_envelope_and_carries_no_free_text() {
    let event = rift_recall_events::MemdEvent::ManifestServed {
        estimated_tokens_returned: 10,
        estimated_tokens_avoided: 90,
    };
    let tagged = grant_tagged_envelope(&event, "invocation-1");
    let doc: serde_json::Value = serde_json::from_str(&tagged).unwrap();
    assert_eq!(doc["verb"], serde_json::json!("memd.manifest_served"));
    assert_eq!(
        doc["payload"]["reason"],
        serde_json::json!("cross-branch-grant")
    );
    assert_eq!(CROSS_BRANCH_GRANT_TAG, "cross-branch-grant");
    // The untagged envelope is the same document without that one member.
    let plain: serde_json::Value =
        serde_json::from_str(&event.to_hook_json_keyed("invocation-1")).unwrap();
    assert!(plain["payload"].get("reason").is_none());
    assert_eq!(doc["idempotency_key"], plain["idempotency_key"]);
}

/// **The floor is the frame's break-even, measured — not a number someone
/// liked.**
///
/// A decline hands the model a reason frame instead of a range, so below some
/// length the frame costs more than the read it replaces. The two crates that
/// ship the floor say so in prose, with an arithmetic nobody was re-running:
/// the frame's cost against roughly four tokens a line. This is where the
/// arithmetic actually runs, because this crate is the one that has both the
/// sealed renderer and the real tokenizer — the validity crate has no
/// tokenizer, and the policy crate has no renderer.
///
/// So: measure the frame with the encoder that counts every other figure, and
/// assert the shipped floor sits at or just above the break-even it implies. A
/// reword that grows the frame reds this test, which is the point — the floor
/// is a consequence of the wording, and a floor left behind a grown frame is a
/// lever declining reads it makes more expensive.
///
/// The fixture is the renderer.s own golden pair — `src/lib.rs` and the hash
/// its goldens use — so the number here and the bytes pinned beside the
/// renderer are measurements of the same frame. It is worth saying what that
/// corrects: the prose quoted **88** tokens, which is what this frame costs
/// with an unusually cheap-to-tokenize hash. On the canonical fixture the same
/// frame cost 98 before this change, so the shipped floor of 24 had already
/// drifted a line under its own break-even. A real path in this workspace is
/// longer than `src/lib.rs` and a real hash is random, so production frames sit
/// at or above the figure asserted here — the floor is conservative in the one
/// direction that matters.
#[test]
fn the_reason_frame_pays_for_itself_at_the_floor() {
    /// What a line of source costs, the figure both floor doc comments quote.
    const TOKENS_PER_LINE: u32 = 4;
    const PATH: &str = "src/lib.rs";
    const HASH: &str = "3f9a1c2e8b7d4f60a1c2e8b7d4f603f9a1c2e8b7d4f60a1c2e8b7d4f603f9a1c";

    let plain = rift_tokens::count(&render_deny_frame(PATH, HASH, 1, 40, Some(3)));
    assert_eq!(
        plain, 111,
        "the plain frame measured on the canonical fixture; both floor doc \n         comments quote it"
    );
    // The delta frame is the more expensive of the two and is measured beside
    // it, so a reword of either is visible here.
    let delta = rift_tokens::count(
        &render_delta_frame(PATH, HASH, &[(12, 18), (44, 52)], 3, 400, 3, Some(7))
            .expect("two exact sites are a delta frame"),
    );
    assert!(delta >= plain, "plain {plain}, delta {delta}");

    // Break-even: the first whole line count whose read costs more than the
    // frame. Integer arithmetic, so it is the same figure the prose states.
    let break_even = plain / TOKENS_PER_LINE + 1;
    assert_eq!(
        break_even, 28,
        "frame {plain} tokens at {TOKENS_PER_LINE} a line, delta frame {delta}"
    );
    {
        let floor = rift_recall_policy::DEFAULT_MIN_SLICE_LINES;
        assert!(
            floor >= break_even,
            "the floor ({floor}) sits below the frame's break-even ({break_even}): \
             a decline under it costs more than the read"
        );
        assert!(
            floor <= break_even + 4,
            "the floor ({floor}) sits far above the break-even ({break_even}): \
             the pool it forgoes is reads a decline would have paid for"
        );
    }
}

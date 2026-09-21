//! The budget fit: placement order, the effective cap, the hard ceiling, the
//! anchors that outlive pressure, the per-kind layout table, and what the pack
//! says about the entries a budget took.

use super::fixtures::*;
use crate::*;

#[test]
fn sections_are_in_placement_order() {
    let pack = compile(&req(), u32::MAX).unwrap();
    let kinds: Vec<_> = pack.sections.iter().map(|s| s.kind).collect();
    assert_eq!(
        kinds,
        vec![
            SectionKind::CriticalTop,
            SectionKind::Changed,
            SectionKind::Unchanged,
            SectionKind::Recommended,
            SectionKind::CostWarning,
            SectionKind::CriticalTail,
        ]
    );
}

#[test]
fn critical_facts_appear_top_and_tail() {
    let pack = compile(&req(), u32::MAX).unwrap();
    let first = &pack.sections.first().unwrap();
    let last = &pack.sections.last().unwrap();
    assert_eq!(first.kind, SectionKind::CriticalTop);
    assert_eq!(last.kind, SectionKind::CriticalTail);
    assert_eq!(
        first.lines, last.lines,
        "critical facts duplicated at the edges"
    );
}

/// A budget too small for even the cheapest coherent content
/// fails LOUD — never a silently empty or truncated pack.
#[test]
fn budget_too_small_fails_loud() {
    let mut r = PackRequest::new(1);
    r.push_changed(env_entry(
        &exact_env("src/f.rs", "changed file with lots of content here"),
        Sensitivity::Normal,
    ))
    .unwrap();
    match compile(&r, u32::MAX) {
        Err(PackError::BudgetTooSmall { needed, cap }) => {
            assert_eq!(cap, 1);
            assert!(needed > cap);
        }
        other => panic!("expected BudgetTooSmall, got {other:?}"),
    }
    // an fully empty request has nothing to hold — an empty pack, not an error
    assert!(compile(&PackRequest::new(1), u32::MAX).is_ok());
}

/// The pack budget composes with the remaining turn budget —
/// the effective cap is `min(target, hard ceiling, turn_remaining)`.
#[test]
fn effective_cap_is_min_with_turn_remaining() {
    let mut r = PackRequest::new(2000);
    for i in 0..40 {
        r.push_changed(env_entry(
            &exact_env(
                &format!("src/f{i}.rs"),
                "a changed block with a good amount of content in it",
            ),
            Sensitivity::Normal,
        ))
        .unwrap();
    }
    let unbounded = compile(&r, u32::MAX).unwrap();
    let turn_capped = compile(&r, 300).unwrap();
    assert!(
        unbounded.total_tokens > 300,
        "fixture must exceed the turn cap"
    );
    assert!(
        turn_capped.total_tokens <= 300,
        "turn remaining is a hard component of the cap: {}",
        turn_capped.total_tokens
    );
    // and a zero-token turn cannot be blown either
    assert!(matches!(
        compile(&r, 0),
        Err(PackError::BudgetTooSmall { cap: 0, .. })
    ));
}

#[test]
fn never_exceeds_hard_ceiling() {
    let mut r = PackRequest::new(1_000_000); // absurd request
    for i in 0..5000 {
        r.push_changed(env_entry(
            &exact_env(&format!("src/f{i}.rs"), "big changed item blah blah blah"),
            Sensitivity::Normal,
        ))
        .unwrap();
    }
    let pack = compile(&r, u32::MAX).unwrap();
    assert!(pack.total_tokens <= MAX_PACK_TOKENS);
}

/// The total is measured over the RENDERED text by the pinned tokenizer —
/// never a sum of per-entry estimates — and the provenance rides the pack.
#[test]
fn measure_is_over_serialized_bytes() {
    let pack = compile(&req(), u32::MAX).unwrap();
    assert_eq!(pack.total_tokens, rift_tokens::count(&pack.render()));
    assert_eq!(pack.tokenizer, rift_tokens::provenance());
}

/// Anchors survive budget pressure: with a cap far below the content volume,
/// the critical top (and its tail twin when ≥ the floor) outlive the fill.
#[test]
fn anchors_reserved_under_pressure() {
    let mut r = PackRequest::new(220);
    r.push_critical(SectionEntry::critical(&CriticalFact::RevertedProvenance {
        evidence: EvidenceRef::at(1),
    }))
    .unwrap();
    for i in 0..60 {
        r.push_changed(env_entry(
            &exact_env(
                &format!("src/f{i}.rs"),
                "bulk changed content that will not all fit in the pack",
            ),
            Sensitivity::Normal,
        ))
        .unwrap();
    }
    let pack = compile(&r, u32::MAX).unwrap();
    let top = pack
        .sections
        .iter()
        .find(|s| s.kind == SectionKind::CriticalTop)
        .unwrap();
    assert!(
        top.lines
            .iter()
            .any(|l| l.contains("evidence:1") && l.contains("last provenance was reverted")),
        "the critical anchor must survive pressure: {:?}",
        pack.sections
    );
    assert!(pack.total_tokens <= 220);
}

/// The tail degrades gracefully: under pressure it is CUT toward the floor
/// and a rump below [`TAIL_FLOOR_TOKENS`] is dropped whole — but a naturally
/// small (uncut) tail is kept: 100–250 is the band for cut tails, not a
/// minimum for healthy ones.
#[test]
fn tail_degrades_to_floor_then_drops() {
    // a healthy small tail is kept (uncut)
    let pack = compile(&req(), u32::MAX).unwrap();
    assert!(pack
        .sections
        .iter()
        .any(|s| s.kind == SectionKind::CriticalTail));
    // under heavy pressure the tail (and never the top) disappears first
    let mut r = PackRequest::new(90);
    for i in 0..3 {
        r.push_critical(SectionEntry::critical(&CriticalFact::RevertedProvenance {
            evidence: EvidenceRef::at(i + 1),
        }))
        .unwrap();
    }
    for i in 0..30 {
        r.push_changed(env_entry(
            &exact_env(&format!("src/f{i}.rs"), "bulk content"),
            Sensitivity::Normal,
        ))
        .unwrap();
    }
    let tight = compile(&r, u32::MAX).unwrap();
    assert!(tight.total_tokens <= 90);
    let has_top = tight
        .sections
        .iter()
        .any(|s| s.kind == SectionKind::CriticalTop && !s.lines.is_empty());
    assert!(has_top, "the top anchor is the last thing standing");
}

#[test]
fn deterministic_for_identical_inputs() {
    let a = compile(&req(), u32::MAX).unwrap();
    let b = compile(&req(), u32::MAX).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.render(), b.render());
}

/// The context layout IS the order the compiler hardcoded before it was data —
/// including the degrade chain (recommendations, then the cost note, then
/// unchanged, then changed). Pinned so the generalization can never quietly
/// re-rank the pack every turn depends on.
#[test]
fn context_layout_is_the_historical_order() {
    assert_eq!(
        placement(PackKind::Context),
        &[
            SectionKind::CriticalTop,
            SectionKind::Changed,
            SectionKind::Unchanged,
            SectionKind::Recommended,
            SectionKind::CostWarning,
            SectionKind::CriticalTail,
        ]
    );
    assert_eq!(
        degrade_order(PackKind::Context),
        &[
            SectionKind::Recommended,
            SectionKind::CostWarning,
            SectionKind::Unchanged,
            SectionKind::Changed,
        ]
    );
}

/// Structural laws every kind obeys: the critical edges anchor the layout, no
/// section is listed twice, and the degrade order covers **exactly** the body —
/// so no section can be unreachable by degradation (an un-degradable section
/// would make the loop fail loud instead of shrinking).
#[test]
fn every_layout_is_edge_anchored_and_degrades_its_whole_body() {
    for kind in ALL_KINDS {
        let p = placement(kind);
        assert_eq!(p.first(), Some(&SectionKind::CriticalTop), "{kind:?}");
        assert_eq!(p.last(), Some(&SectionKind::CriticalTail), "{kind:?}");
        for s in p {
            assert_eq!(
                p.iter().filter(|x| *x == s).count(),
                1,
                "{kind:?} lists {s:?} twice"
            );
        }
        let body: Vec<SectionKind> = p.iter().copied().filter(|k| !is_edge(*k)).collect();
        let d = degrade_order(kind);
        assert_eq!(
            d.len(),
            body.len(),
            "{kind:?} degrade order misses a section"
        );
        for s in &body {
            assert!(d.contains(s), "{kind:?} cannot degrade {s:?}");
        }
    }
}

/// One ceiling for every kind: a kind's default target is a target, and the
/// hard ceiling is not a per-kind knob.
#[test]
fn kind_defaults_are_targets_under_the_one_shared_ceiling() {
    for kind in ALL_KINDS {
        assert!(
            kind.default_tokens() <= MAX_PACK_TOKENS,
            "{kind:?} default exceeds the shared ceiling"
        );
    }
    let tags: Vec<&str> = ALL_KINDS.iter().map(|k| k.tag()).collect();
    assert_eq!(tags, vec!["context", "review", "investigation"]);
    assert_eq!(PackKind::default(), PackKind::Context);
    assert_eq!(PackRequest::new(10).kind(), PackKind::Context);
}

/// A kind refuses the sections its layout does not have — before any placement
/// check, and without mutating the request.
#[test]
fn a_kind_refuses_a_section_it_does_not_have() {
    let summary = || env_entry(&summary_env("src/a.rs", "s"), Sensitivity::Normal);
    // Neither evidence kind carries the context pack's `Unchanged` section.
    for kind in [PackKind::Review, PackKind::Investigation] {
        let mut r = PackRequest::for_kind(kind, 1200);
        let err = r.push_unchanged(summary()).unwrap_err().to_string();
        assert!(err.contains("layout invariant"), "{err}");
        assert!(err.contains(kind.tag()), "{err}");
        assert_eq!(r.kind(), kind);
        // and nothing was recorded
        assert!(compile(&r, u32::MAX)
            .unwrap()
            .sections
            .iter()
            .all(|s| s.lines.is_empty()));
    }
}

/// The shared sections work under every kind, and the compiled pack renders in
/// that kind's placement order and carries its kind.
#[test]
fn shared_sections_compile_under_every_kind() {
    for kind in ALL_KINDS {
        let mut r = PackRequest::for_kind(kind, kind.default_tokens());
        r.push_critical(SectionEntry::critical(&CriticalFact::RevertedProvenance {
            evidence: EvidenceRef::at(1),
        }))
        .unwrap();
        r.push_changed(env_entry(
            &exact_env("src/retry.rs", "fn retry() {}"),
            Sensitivity::Normal,
        ))
        .unwrap();
        r.push_recommended(SectionEntry::from_handle(
            &file_handle("src/retry.rs"),
            ordinal_of("src/retry.rs"),
        ))
        .unwrap();
        r.set_cost_note(SectionEntry::cost_note(CostNote::RepeatedReadAreas))
            .unwrap();
        let pack = compile(&r, u32::MAX).unwrap();
        assert_eq!(pack.kind, kind);
        // the rendered sections are a subsequence of the kind's placement
        let mut expected = placement(kind).iter();
        for s in &pack.sections {
            assert!(
                expected.any(|p| *p == s.kind),
                "{kind:?} rendered {:?} out of placement order",
                s.kind
            );
        }
        assert!(pack.total_tokens <= MAX_PACK_TOKENS);
    }
}

/// Setting the cost note twice keeps one note; a REFUSED note leaves the note
/// already set intact (the gate runs before the slot is cleared).
#[test]
fn cost_note_slot_holds_at_most_one_and_a_refusal_is_inert() {
    let mut r = PackRequest::new(1200);
    r.set_cost_note(SectionEntry::cost_note(CostNote::RepeatedReadAreas))
        .unwrap();
    r.set_cost_note(SectionEntry::cost_note(CostNote::RepeatedReadAreas))
        .unwrap();
    assert!(r
        .set_cost_note(SectionEntry::from_handle(
            &file_handle("src/a.rs"),
            ordinal_of("src/a.rs")
        ))
        .is_err());
    let pack = compile(&r, u32::MAX).unwrap();
    let notes: Vec<_> = pack
        .sections
        .iter()
        .filter(|s| s.kind == SectionKind::CostWarning)
        .flat_map(|s| s.lines.clone())
        .collect();
    assert_eq!(notes, vec![COST_NOTE.to_string()]);
}

/// A5: an entry that fits in neither its own form nor its handle is gone, and
/// the pack says how many went. Before this, `pack_tight` quietly served two
/// hunks out of six offered entries and looked complete.
#[test]
fn a_dropped_entry_is_announced_not_lost() {
    let tight = compile(&degrading_req(150), u32::MAX).unwrap();
    let announced = announced_drops(&tight).expect("a squeezed pack announces its drops");
    assert_eq!(
        body_entries(&tight) + announced as usize,
        DEGRADING_BODY_ENTRIES,
        "every offered entry is either in the pack or in the count:\n{}",
        tight.render()
    );
    assert!(tight.total_tokens <= 150);
    // and a pack that lost nothing says nothing — the line is not decoration
    let comfortable = compile(&degrading_req(1200), u32::MAX).unwrap();
    assert_eq!(announced_drops(&comfortable), None);
    assert_eq!(body_entries(&comfortable), DEGRADING_BODY_ENTRIES);
}

/// The announcement is strictly additive at the floor: it is given up before
/// the tail shrinks or the top truncates, and only once the body is empty —
/// a pack that is nothing but its anchors, where there is no content left to
/// annotate. `pack_tiny` is therefore byte-identical to what it was before A5.
#[test]
fn the_announcement_is_given_up_before_any_anchor() {
    let tiny = compile(&degrading_req(40), u32::MAX).unwrap();
    assert_eq!(announced_drops(&tiny), None, "{}", tiny.render());
    assert_eq!(body_entries(&tiny), 0, "the body is empty at this floor");
    let rendered = tiny.render();
    let anchors: Vec<&str> = rendered.lines().collect();
    assert_eq!(
        anchors.len(),
        2,
        "top and tail anchors both survive: {anchors:?}"
    );
    assert!(anchors.iter().all(|l| l.starts_with("CRITICAL: ")));
}

/// The trade, stated instead of hidden: at a budget with no slack the
/// announcement costs an entry — and then counts it. Six tokens buy the reader
/// the one thing they cannot recover on their own, which is the knowledge that
/// the pack is not the whole story.
#[test]
fn the_announcement_counts_the_entry_it_costs() {
    // 175/174 and not 190/180: ordinals made the two critical entries
    // CHEAPER (an ordinal is fewer tokens than a path), so the whole review
    // pack shifted down and the knife-edge moved with it. Re-measured rather
    // than nudged until green — the probe walked 150..=200 and the transition
    // is at 175 exactly.
    let roomy = compile(&review_req(175), u32::MAX).unwrap();
    let edge = compile(&review_req(174), u32::MAX).unwrap();
    let signals = |p: &Pack| -> usize {
        p.sections
            .iter()
            .filter(|s| s.kind == SectionKind::ReviewSignals)
            .map(|s| s.lines.len())
            .sum()
    };
    assert_eq!(signals(&roomy), 5, "with slack, nothing is displaced");
    assert_eq!(signals(&edge), 4, "at the edge, the line costs a signal");
    assert_eq!(
        announced_drops(&edge),
        announced_drops(&roomy).map(|n| n + 1),
        "the displaced entry is counted by the line that displaced it"
    );
}

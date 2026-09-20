//! The two evidence kinds — review and investigation — and the serialized
//! shape they share: which sections each admits, what a signal or a finding
//! renders as, and the count the edge fact states.

use rift_recall_validity::trust::serialize_untrusted;

use super::fixtures::*;
use crate::*;

/// The review pack's priority law, stated rather than merely snapshotted:
/// under pressure it keeps the numbers only Rift can compute (attribution,
/// failures, spend, savings) and gives up the diff, which the reviewing agent
/// can read out of the repo itself.
///
/// Squeezed to 190 rather than 180 since A5: the drop announcement is a line,
/// and at 180 there is no slack for it, so it displaces the last signal. That
/// trade is a law of its own and is pinned where it belongs, in
/// `the_announcement_counts_the_entry_it_costs` — this test is about signals
/// outliving the diff, so it is stated at a budget where that is what it is
/// measuring.
///
/// **175 rather than 190 since the switch to ordinals**, re-measured and not nudged: the
/// citation is cheaper than the path it replaced, so the whole pack shifted
/// down and 190 now holds the diff as well. The probe walked 120..=220 and
/// this is the widest budget at which all five signals ride and the diff does
/// not (175..=185; 190 admits one changed entry).
#[test]
fn a_squeezed_review_keeps_its_signals_and_gives_up_the_diff() {
    let tight = compile(&review_req(175), u32::MAX).unwrap();
    let lines = |kind: SectionKind| -> usize {
        tight
            .sections
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.lines.len())
            .sum()
    };
    assert_eq!(lines(SectionKind::ReviewSignals), 5, "all signals survive");
    assert_eq!(lines(SectionKind::ReviewScope), 2, "scope survives");
    assert_eq!(lines(SectionKind::Changed), 0, "the diff is what gives");
    assert!(tight.total_tokens <= 175);
    // and it is a squeeze, not a shape: with room, the diff is there
    let comfortable = compile(&review_req(PackKind::Review.default_tokens()), u32::MAX).unwrap();
    assert!(comfortable
        .sections
        .iter()
        .any(|s| s.kind == SectionKind::Changed && !s.lines.is_empty()));
}

/// The review sections are closed both ways: a scope fact is not a signal, a
/// signal is not a critical fact, and neither is admissible in a context pack.
#[test]
fn review_sections_are_closed_both_ways() {
    let scope = || {
        SectionEntry::review_scope(&ReviewScopeFact::Volume {
            commits: 1,
            sessions: 1,
            files: 1,
        })
    };
    let signal =
        || SectionEntry::review_signal(ReviewSignal::AttributionConfidence { percent: 90 });
    assert_eq!(scope().class(), EntryClass::ScopeFact);
    assert_eq!(signal().class(), EntryClass::Signal);

    let mut r = PackRequest::for_kind(PackKind::Review, 1200);
    assert!(
        r.push_review_signal(scope()).is_err(),
        "scope is not a signal"
    );
    assert!(
        r.push_review_scope(signal()).is_err(),
        "signal is not scope"
    );
    assert!(r.push_critical(signal()).is_err(), "a signal is not a fact");
    assert!(r.push_changed(scope()).is_err(), "scope is not a change");
    assert!(r.push_recommended(signal()).is_err());

    // and a context pack has no review sections at all
    let mut ctx = PackRequest::new(1200);
    for e in [scope(), signal()] {
        let err = ctx
            .push_review_scope(e.clone())
            .or_else(|_| ctx.push_review_signal(e))
            .unwrap_err()
            .to_string();
        assert!(err.contains("layout invariant"), "{err}");
        assert!(err.contains("context"), "{err}");
    }
}

/// Signals render as arithmetic, and the two places a percentage appears are
/// clamped — a caller cannot put "180% confident" in front of a reviewer.
#[test]
fn signals_are_arithmetic_and_percentages_clamp() {
    let e = SectionEntry::review_signal(ReviewSignal::AttributionConfidence { percent: 255 });
    assert_eq!(e.text(), "signal: attribution confidence 100%");
    let e = SectionEntry::critical(&CriticalFact::LowAttributionConfidence { percent: 200 });
    assert!(e.text().contains("100% confident"), "{}", e.text());
    // savings are reported net, and the net is signed: injection can lose
    let e = SectionEntry::review_signal(ReviewSignal::RecallSavings {
        tokens_avoided: 100,
        tokens_injected: 900,
    });
    assert_eq!(
        e.text(),
        "signal: recall savings 100 avoided - 900 injected = -800 net"
    );
}

/// A4: the edge fact states what the pack HOLDS, not what the caller offered.
///
/// The advisory: the tight investigation snapshot read `CRITICAL: 2 finding(s)`
/// over a pack carrying ONE finding, because the caller supplied the count
/// before the budget dropped the other. The count is now the compiler's, and
/// the fact carries none for a caller to get wrong.
#[test]
fn the_findings_count_is_what_the_pack_holds() {
    // Before assembly there is no honest number, so the line states none.
    let uncompiled = SectionEntry::critical(&CriticalFact::UnverifiedFindingsPresent);
    assert_eq!(
        uncompiled.text(),
        "CRITICAL: findings here are unverified notes, not facts - corroborate before acting"
    );

    let mut saw_a_squeeze = false;
    for budget in [120u32, 140, 160, 200, 400, 2000] {
        let Ok(pack) = compile(&investigation_req(budget), u32::MAX) else {
            continue;
        };
        let render = pack.render();
        let held = render
            .lines()
            .filter(|l| l.starts_with("finding: unverified "))
            .count() as u32;
        let claims: Vec<u32> = render.lines().filter_map(claimed_findings).collect();
        for claimed in &claims {
            assert_eq!(
                *claimed, held,
                "budget {budget}: claimed {claimed}, holds {held}\n{render}"
            );
        }
        if held == 0 {
            assert!(
                claims.is_empty(),
                "budget {budget}: a pack with no notes warned about notes\n{render}"
            );
        } else {
            assert!(
                !claims.is_empty(),
                "budget {budget}: a pack with notes did not say they are notes\n{render}"
            );
        }
        if held < 2 {
            saw_a_squeeze = true;
        }
    }
    assert!(
        saw_a_squeeze,
        "the sweep must include a budget that actually drops a finding, \
         or it proves nothing about staleness"
    );
}

/// Structurally: an unverified note is admissible in exactly one
/// section and is refused by the critical edges — so an uncorroborated claim
/// cannot be duplicated top-and-tail as though it were decisive.
#[test]
fn a_finding_can_never_become_a_critical_fact() {
    let note = finding(FindingKind::Decision, "we should ship it", "log:x");
    assert_eq!(note.class(), EntryClass::UnverifiedNote);

    let mut r = PackRequest::for_kind(PackKind::Investigation, 1200);
    assert!(
        r.push_critical(note.clone()).is_err(),
        "not a critical fact"
    );
    assert!(r.push_changed(note.clone()).is_err(), "not a change");
    assert!(r.push_seed(note.clone()).is_err(), "not a seed");
    assert!(r.push_recommended(note.clone()).is_err(), "not a handle");
    assert!(r.push_finding(note).is_ok());

    // and the reverse: a fact is not a finding
    let fact = SectionEntry::critical(&CriticalFact::UnverifiedFindingsPresent);
    assert!(r.push_finding(fact).is_err());

    // nor can a finding exist in a context or review pack at all
    for kind in [PackKind::Context, PackKind::Review] {
        let mut other = PackRequest::for_kind(kind, 1200);
        let err = other
            .push_finding(finding(FindingKind::Intent, "x", "log:x"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("layout invariant"), "{err}");
    }
}

/// Every finding rendering opens with `finding: unverified <kind>` — there is
/// no constructor that yields a "verified" note, so the pack cannot be made to
/// launder a claim into a fact by wording.
#[test]
fn every_finding_renders_as_unverified() {
    for kind in [
        FindingKind::Decision,
        FindingKind::Lesson,
        FindingKind::Intent,
        FindingKind::ErrorSignature,
    ] {
        let e = finding(kind, "some proposed claim", "log:x");
        // `cites <scheme> evidence:<n>` since the switch to ordinals: the SHAPE of the
        // citation is the assertion, because the handle's own text is what it
        // replaced.
        let prefix = format!("finding: unverified {} (cites file evidence:", kind.tag());
        assert!(e.text().starts_with(&prefix), "{}", e.text());
        // The digest's caller handle is not rendered (the caller-handle leak, closed): the finding's
        // only retrievable name is the ordinal it cites.
        assert!(
            !e.text().contains("log:x"),
            "the caller handle rode the finding line: {}",
            e.text()
        );
    }
}

/// The seed vocabulary is identifiers only: each variant renders under the
/// `seed:` tag, and the ONLY way text reaches the seed section is the
/// chokepoint's digest (which carries none of it).
#[test]
fn seeds_are_identifiers_and_untrusted_text_is_a_digest() {
    for (descriptor, expect) in [
        (
            // The KIND rides, the path does not — a seed has no
            // content address to cite it by and `investigate` has no
            // human-only surface to map it on.
            SeedDescriptor::Path,
            "seed: path",
        ),
        (
            SeedDescriptor::Checkpoint { id: "01H".into() },
            "seed: checkpoint 01H",
        ),
        (
            SeedDescriptor::ErrorSignature {
                sha256: "c".repeat(64),
            },
            "seed: error signature @sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        ),
    ] {
        let e = SectionEntry::seed(&descriptor);
        assert_eq!(e.class(), EntryClass::SeedRef);
        assert_eq!(e.text(), expect);
    }
    // an issue body arrives as a digest: counts and a handle, no prose
    let body = "Please fix the retry bug. Steps: run the failing test.";
    let digest = serialize_untrusted("transcript", body.as_bytes(), "log:issue42");
    let e = SectionEntry::from_neutral(&digest);
    assert_eq!(e.class(), EntryClass::NeutralDigest);
    assert!(!e.text().contains("Please fix"), "{}", e.text());
    let mut r = PackRequest::for_kind(PackKind::Investigation, 1200);
    assert!(r.push_seed(e).is_ok());
    // exact content is refused outright, flag or no flag
    assert!(r
        .push_seed(env_entry(
            &exact_env("src/a.rs", "raw"),
            Sensitivity::Normal
        ))
        .is_err());
}

/// The wire vocabulary is closed and unique: every section tag is distinct,
/// lowercase snake_case, and every kind tag likewise — a reader can switch on
/// them without a catch-all.
#[test]
fn wire_tags_are_closed_unique_and_snake_case() {
    let mut tags: Vec<&str> = Vec::new();
    for kind in ALL_KINDS {
        tags.extend(placement(kind).iter().map(|s| s.tag()));
    }
    tags.sort_unstable();
    tags.dedup();
    for tag in &tags {
        assert!(
            tag.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "{tag} is not snake_case"
        );
    }
    // all ten sections are reachable from some kind's layout
    assert_eq!(tags.len(), 10, "{tags:?}");
}

/// Program-wide requirement 3: the artifact carries its schema version and its
/// kind, and the key is the INPUT key for that kind — not a hash of the bytes
/// being serialized.
#[test]
fn evidence_pack_carries_its_version_kind_and_input_key() {
    for kind in ALL_KINDS {
        let mut r = PackRequest::for_kind(kind, kind.default_tokens());
        r.push_critical(SectionEntry::critical(&CriticalFact::RevertedProvenance {
            evidence: EvidenceRef::at(1),
        }))
        .unwrap();
        let evidence = compile(&r, u32::MAX).unwrap().to_evidence(&key_inputs());
        assert_eq!(evidence.schema_version, EVIDENCE_PACK_SCHEMA_VERSION);
        assert_eq!(evidence.kind, kind.tag());
        assert_eq!(evidence.pack_key, pack_key_for(kind, &key_inputs()));
        assert_eq!(evidence.tokenizer, rift_tokens::provenance());
        assert!(evidence.total_tokens <= MAX_PACK_TOKENS);
        // the same pack under a different kind is a different artifact
        assert_ne!(
            evidence.pack_key,
            pack_key_for(PackKind::Review, &{
                let mut other = key_inputs();
                other.query = "different".into();
                other
            })
        );
    }
}

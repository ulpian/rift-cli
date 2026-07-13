use rift_recall_types::{ArtifactState, Envelope, Handle, Trust};
use rift_recall_validity::trust::serialize_untrusted;

use super::*;

fn sha() -> String {
    "a".repeat(64)
}

fn handle_str(path: &str) -> String {
    format!("file:{path}@sha256:{}", sha())
}

fn env(path: &str, state: ArtifactState) -> Envelope {
    Envelope {
        path: path.to_string(),
        trust: Trust::CodeFact,
        state,
        estimated_tokens_returned: 0,
    }
}

fn exact_env(path: &str, content: &str) -> Envelope {
    env(
        path,
        ArtifactState::ExactIncluded {
            handle: handle_str(path),
            basis_sha256: sha(),
            content: content.to_string(),
            range: None,
        },
    )
}

fn summary_env(path: &str, summary: &str) -> Envelope {
    env(
        path,
        ArtifactState::SummaryIncluded {
            handle: handle_str(path),
            basis_sha256: sha(),
            summary: summary.to_string(),
            summary_key: "k".into(),
        },
    )
}

fn unchanged_env(path: &str) -> Envelope {
    env(
        path,
        ArtifactState::KnownUnchanged {
            handle: handle_str(path),
            basis_sha256: sha(),
            last_seen_turn: None,
        },
    )
}

fn file_handle(path: &str) -> Handle {
    Handle::File {
        path: path.to_string(),
        sha256: sha(),
        range: None,
    }
}

fn req() -> PackRequest {
    let mut r = PackRequest::new(1200);
    r.push_critical(SectionEntry::critical(&CriticalFact::RevertedProvenance {
        path: "src/retry.rs".into(),
    }))
    .unwrap();
    r.push_changed(SectionEntry::from_envelope(
        &exact_env("src/retry.rs", "fn retry() { /* 3 hunks */ }"),
        Sensitivity::Normal,
    ))
    .unwrap();
    r.push_unchanged(SectionEntry::from_envelope(
        &summary_env(
            "src/billing.rs",
            "BillingClient.charge() signature unchanged",
        ),
        Sensitivity::Normal,
    ))
    .unwrap();
    r.push_recommended(SectionEntry::from_handle(&file_handle("src/retry.rs")))
        .unwrap();
    r.set_cost_note(SectionEntry::cost_note(CostNote::RepeatedReadAreas))
        .unwrap();
    r
}

#[test]
fn sections_are_in_placement_order() {
    let pack = compile(&req());
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
    let pack = compile(&req());
    let first = &pack.sections.first().unwrap();
    let last = &pack.sections.last().unwrap();
    assert_eq!(first.kind, SectionKind::CriticalTop);
    assert_eq!(last.kind, SectionKind::CriticalTail);
    assert_eq!(
        first.lines, last.lines,
        "critical facts duplicated at the edges"
    );
}

#[test]
fn never_exceeds_budget() {
    let mut r = PackRequest::new(10);
    for i in 0..100 {
        r.push_changed(SectionEntry::from_envelope(
            &exact_env(
                &format!("src/f{i}.rs"),
                "changed file with lots of content here",
            ),
            Sensitivity::Normal,
        ))
        .unwrap();
    }
    let pack = compile(&r);
    assert!(
        pack.total_tokens <= 10,
        "pack must not exceed budget: {}",
        pack.total_tokens
    );
}

#[test]
fn never_exceeds_hard_ceiling() {
    let mut r = PackRequest::new(1_000_000); // absurd request
    for i in 0..5000 {
        r.push_changed(SectionEntry::from_envelope(
            &exact_env(&format!("src/f{i}.rs"), "big changed item blah blah blah"),
            Sensitivity::Normal,
        ))
        .unwrap();
    }
    let pack = compile(&r);
    assert!(pack.total_tokens <= MAX_PACK_TOKENS);
}

#[test]
fn deterministic_for_identical_inputs() {
    let a = compile(&req());
    let b = compile(&req());
    assert_eq!(a, b);
    assert_eq!(a.render(), b.render());
}

// --- placement invariants (PRD-M6 stories 4/5/6; ADR-0016) -------------------

/// Story 5: `UnchangedButRelevant` is summaries + handles + hashes only — an
/// exact-content entry is REFUSED at admission, so a violating pack is
/// unrepresentable rather than merely untested.
#[test]
fn unchanged_refuses_exact_content() {
    let mut r = PackRequest::new(1200);
    let exact = SectionEntry::from_envelope(
        &exact_env("src/a.rs", "raw bytes here"),
        Sensitivity::Normal,
    );
    assert_eq!(exact.class(), EntryClass::ExactContent);
    assert!(r.push_unchanged(exact).is_err());
}

/// Story 6: a sensitive path's summary downgrades to its handle at
/// construction — exact-or-handle, never a lossy summary.
#[test]
fn sensitive_summary_downgrades_to_handle() {
    let e = SectionEntry::from_envelope(
        &summary_env("secrets/config.rs", "gist of a sensitive file"),
        Sensitivity::Sensitive,
    );
    assert_eq!(e.class(), EntryClass::Handle);
    assert!(
        !e.text().contains("gist of a sensitive file"),
        "the summary text must be withheld: {}",
        e.text()
    );
    assert!(e.text().contains("summary withheld"));
}

/// Recommendations admit parse-validated handles only.
#[test]
fn recommended_refuses_non_handles() {
    let mut r = PackRequest::new(1200);
    let summary = SectionEntry::from_envelope(&summary_env("src/a.rs", "s"), Sensitivity::Normal);
    assert!(r.push_recommended(summary).is_err());
    assert!(r
        .push_recommended(SectionEntry::from_handle(&file_handle("src/a.rs")))
        .is_ok());
}

/// The cost slot takes only the closed [`CostNote`] vocabulary (W7:
/// directive-free by construction — there is no free-text constructor).
#[test]
fn cost_note_vocabulary_is_closed() {
    let mut r = PackRequest::new(1200);
    let crit = SectionEntry::critical(&CriticalFact::RevertedProvenance { path: "p".into() });
    assert!(r.set_cost_note(crit).is_err());
    let note = SectionEntry::cost_note(CostNote::RepeatedReadAreas);
    assert_eq!(note.text(), COST_NOTE);
    assert!(r.set_cost_note(note).is_ok());
}

/// A `known_unchanged` envelope is a hash claim with zero content — admissible
/// in `Unchanged` and rendered as identifiers only.
#[test]
fn known_unchanged_is_hashes_only() {
    let e = SectionEntry::from_envelope(&unchanged_env("src/a.rs"), Sensitivity::Normal);
    assert_eq!(e.class(), EntryClass::KnownUnchanged);
    assert!(e.text().contains("not re-inlined"));
    let mut r = PackRequest::new(1200);
    assert!(r.push_unchanged(e).is_ok());
}

/// Critical sections admit only the closed [`CriticalFact`] vocabulary.
#[test]
fn critical_refuses_free_content() {
    let mut r = PackRequest::new(1200);
    let handle = SectionEntry::from_handle(&file_handle("src/a.rs"));
    assert!(r.push_critical(handle).is_err());
}

proptest::proptest! {
    /// Story 4: untrusted bytes routed through the chokepoint enter the pack
    /// as a neutral digest — no payload line ever appears in the rendered
    /// entry, even (especially) directive-shaped ones.
    #[test]
    fn untrusted_payloads_never_render_verbatim(
        lines in proptest::collection::vec("[ -~]{1,40}", 1..10),
    ) {
        let payload = lines.join("\n");
        let digest = serialize_untrusted("tool_output", payload.as_bytes(), "log:toolu_x");
        let entry = SectionEntry::from_neutral(&digest);
        proptest::prop_assert_eq!(entry.class(), EntryClass::NeutralDigest);
        for line in &lines {
            // counts-only: a payload line may never survive into the entry
            // (single characters aside, which any rendering shares).
            if line.len() > 3 {
                proptest::prop_assert!(
                    !entry.text().contains(line.as_str()),
                    "payload line leaked into the pack: {line}"
                );
            }
        }
    }

    /// The tail is always a subset (here: a copy) of the top facts — the
    /// Lost-in-the-Middle remedy cannot be accidentally violated.
    #[test]
    fn tail_is_subset_of_top(paths in proptest::collection::vec("[a-z]{1,12}", 1..6), budget in 60u32..2000) {
        let mut r = PackRequest::new(budget);
        for p in &paths {
            r.push_critical(SectionEntry::critical(&CriticalFact::RevertedProvenance {
                path: p.clone(),
            })).unwrap();
        }
        let pack = compile(&r);
        let top: Vec<_> = pack
            .sections
            .iter()
            .find(|s| s.kind == SectionKind::CriticalTop)
            .map(|s| s.lines.clone())
            .unwrap_or_default();
        let tail: Vec<_> = pack
            .sections
            .iter()
            .find(|s| s.kind == SectionKind::CriticalTail)
            .map(|s| s.lines.clone())
            .unwrap_or_default();
        for line in &top {
            proptest::prop_assert!(tail.contains(line), "top fact missing from tail");
        }
    }
}

// --- pack key (ADR-0020) ------------------------------------------------------

fn key_inputs() -> PackKeyInputs {
    PackKeyInputs {
        query: "task: fix retry".into(),
        ranked: vec![
            ("id1".into(), "hashA".into()),
            ("id2".into(), "hashB".into()),
        ],
        policy_version: 1,
        token_budget: 1200,
    }
}

#[test]
fn pack_key_is_input_keyed_and_sensitive_to_every_input() {
    // ADR-0020: computable before any render, stable for identical inputs.
    let base = pack_key(&key_inputs());
    assert_eq!(base.len(), 64);
    assert_eq!(base, pack_key(&key_inputs()), "identical inputs, same key");

    let mut other_query = key_inputs();
    other_query.query = "task: other".into();
    assert_ne!(
        base,
        pack_key(&other_query),
        "query is part of the identity"
    );

    let mut reordered = key_inputs();
    reordered.ranked.swap(0, 1);
    assert_ne!(
        base,
        pack_key(&reordered),
        "rank ORDER is part of the identity"
    );

    let mut rehashed = key_inputs();
    rehashed.ranked[0].1 = "hashA2".into();
    assert_ne!(
        base,
        pack_key(&rehashed),
        "content hash is part of the identity"
    );

    let mut repoliced = key_inputs();
    repoliced.policy_version = 2;
    assert_ne!(
        base,
        pack_key(&repoliced),
        "policy version is part of the identity"
    );

    let mut rebudgeted = key_inputs();
    rebudgeted.token_budget = 800;
    assert_ne!(
        base,
        pack_key(&rebudgeted),
        "budget is part of the identity"
    );
}

proptest::proptest! {
    /// Byte-identity across seeds (ADR-0020): equal inputs → equal key AND
    /// equal rendered pack; the key never depends on the rendered output.
    #[test]
    fn same_inputs_reserve_byte_identically(
        names in proptest::collection::vec("[a-z]{1,8}", 0..6),
        budget in 1u32..2000,
    ) {
        let ranked: Vec<(String, String)> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (format!("id{i}"), format!("h_{n}")))
            .collect();
        let inputs = PackKeyInputs {
            query: "q".into(),
            ranked: ranked.clone(),
            policy_version: 1,
            token_budget: budget,
        };
        proptest::prop_assert_eq!(pack_key(&inputs), pack_key(&inputs));
        let mut pr = PackRequest::new(budget);
        for (_, h) in &ranked {
            let handle = Handle::File {
                path: "x".into(),
                sha256: h.clone(),
                range: None,
            };
            pr.push_recommended(SectionEntry::from_handle(&handle)).unwrap();
        }
        proptest::prop_assert_eq!(compile(&pr).render(), compile(&pr).render());
    }

    /// Distinct ranked sets produce distinct keys (no concatenation-boundary
    /// collisions thanks to length prefixing).
    #[test]
    fn boundary_shifts_change_the_key(a in "[a-z]{1,6}", b in "[a-z]{1,6}") {
        let one = PackKeyInputs {
            ranked: vec![(format!("{a}{b}"), "h".into())],
            ..key_inputs()
        };
        let two = PackKeyInputs {
            ranked: vec![(a.clone(), format!("{b}h"))],
            ..key_inputs()
        };
        // same concatenated bytes, different field boundaries → different keys
        proptest::prop_assert_ne!(pack_key(&one), pack_key(&two));
    }
}

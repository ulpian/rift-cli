//! Every property test the crate states, in one file: proptest keys its
//! regression seeds by the source file that runs them, so the seeds recorded
//! for these blocks stay live only while the blocks stay together.

use rift_recall_types::Handle;
use rift_recall_validity::trust::serialize_untrusted;

use super::fixtures::*;
use crate::*;

proptest::proptest! {
    /// The review kind is budgeted by the same law as every other: the measured
    /// total never exceeds `min(target, MAX_PACK_TOKENS, turn_remaining)`, and
    /// identical inputs compile byte-identically.
    #[test]
    fn review_pack_obeys_the_shared_ceiling_and_is_deterministic(
        budget in 1u32..6000,
        turn in 1u32..6000,
        ai in 0u32..5000,
        human in 0u32..5000,
        blocks in proptest::collection::vec("[a-z ]{5,120}", 0..12),
    ) {
        let mut r = PackRequest::for_kind(PackKind::Review, budget);
        r.push_review_scope(SectionEntry::review_scope(&ReviewScopeFact::Volume {
            commits: blocks.len() as u32, sessions: 1, files: blocks.len() as u32,
        })).unwrap();
        r.push_review_signal(SectionEntry::review_signal(ReviewSignal::AttributionSplit {
            ai_lines: ai, mixed_lines: 0, human_lines: human,
        })).unwrap();
        for (i, b) in blocks.iter().enumerate() {
            r.push_changed(env_entry(
                &exact_env(&format!("src/f{i}.rs"), b),
                Sensitivity::Normal,
            )).unwrap();
        }
        let cap = budget.clamp(1, MAX_PACK_TOKENS).min(turn);
        match (compile(&r, turn), compile(&r, turn)) {
            (Ok(a), Ok(b)) => {
                proptest::prop_assert!(a.total_tokens <= cap, "measured {} > cap {cap}", a.total_tokens);
                proptest::prop_assert_eq!(a.render(), b.render());
                proptest::prop_assert_eq!(a.kind, PackKind::Review);
            }
            (Err(a), Err(b)) => proptest::prop_assert_eq!(a, b),
            (a, b) => proptest::prop_assert!(false, "nondeterministic outcome: {a:?} vs {b:?}"),
        }
    }
}

proptest::proptest! {
    /// The same law over every budget: a pack never claims a count it does not
    /// hold, and never claims one at all when it holds nothing.
    #[test]
    fn a_pack_never_miscounts_its_findings(budget in 60u32..3000) {
        if let Ok(pack) = compile(&investigation_req(budget), u32::MAX) {
            let render = pack.render();
            let held = render.lines().filter(|l| l.starts_with("finding: unverified ")).count() as u32;
            for claimed in render.lines().filter_map(claimed_findings) {
                proptest::prop_assert_eq!(claimed, held, "budget {}: {}", budget, render);
            }
            proptest::prop_assert!(
                held > 0 || render.lines().filter_map(claimed_findings).count() == 0
            );
        }
    }
}

proptest::proptest! {
    /// The investigation kind obeys the same ceiling and the same determinism
    /// law, and no finding's words survive into the render at any budget.
    #[test]
    fn investigation_pack_is_bounded_deterministic_and_wordless(
        budget in 1u32..6000,
        turn in 1u32..6000,
        claims in proptest::collection::vec("[a-z ]{8,60}", 1..8),
    ) {
        let mut r = PackRequest::for_kind(PackKind::Investigation, budget);
        r.push_seed(SectionEntry::seed(&SeedDescriptor::Path)).unwrap();
        for (i, c) in claims.iter().enumerate() {
            r.push_finding(finding(FindingKind::Lesson, c, &format!("log:t{i}"))).unwrap();
        }
        let cap = budget.clamp(1, MAX_PACK_TOKENS).min(turn);
        match (compile(&r, turn), compile(&r, turn)) {
            (Ok(a), Ok(b)) => {
                proptest::prop_assert!(a.total_tokens <= cap, "measured {} > cap {cap}", a.total_tokens);
                proptest::prop_assert_eq!(a.render(), b.render());
                let rendered = a.render();
                for c in &claims {
                    if c.len() > 3 {
                        proptest::prop_assert!(
                            !rendered.contains(c.as_str()),
                            "a finding's words reached the pack: {c}"
                        );
                    }
                }
            }
            (Err(a), Err(b)) => proptest::prop_assert_eq!(a, b),
            (a, b) => proptest::prop_assert!(false, "nondeterministic outcome: {a:?} vs {b:?}"),
        }
    }
}

proptest::proptest! {
    /// Determinism all the way to the wire: identical inputs serialize
    /// byte-identically, so a review artifact regenerated tomorrow diffs empty.
    #[test]
    fn evidence_pack_serializes_byte_identically(
        budget in 200u32..4000,
        blocks in proptest::collection::vec("[a-z ]{5,60}", 0..8),
    ) {
        let build = || {
            let mut r = PackRequest::for_kind(PackKind::Review, budget);
            r.push_review_scope(SectionEntry::review_scope(&ReviewScopeFact::Volume {
                commits: blocks.len() as u32, sessions: 1, files: blocks.len() as u32,
            })).unwrap();
            for (i, b) in blocks.iter().enumerate() {
                r.push_changed(env_entry(
                    &exact_env(&format!("src/f{i}.rs"), b),
                    Sensitivity::Normal,
                )).unwrap();
            }
            r
        };
        let one = serde_json::to_string(&compile(&build(), u32::MAX).unwrap().to_evidence(&key_inputs())).unwrap();
        let two = serde_json::to_string(&compile(&build(), u32::MAX).unwrap().to_evidence(&key_inputs())).unwrap();
        proptest::prop_assert_eq!(one, two);
    }
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
        for (i, _) in paths.iter().enumerate() {
            r.push_critical(SectionEntry::critical(&CriticalFact::RevertedProvenance {
                evidence: EvidenceRef::at(u32::try_from(i).unwrap_or(0) + 1),
            })).unwrap();
        }
        let pack = compile(&r, u32::MAX).unwrap();
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
        // tail ⊆ top: the tail is a (possibly empty, floor-degraded) prefix
        // of the top — it can never carry a fact the top does not.
        for line in &tail {
            proptest::prop_assert!(top.contains(line), "tail fact not present at top");
        }
        proptest::prop_assert!(tail.len() <= top.len());
    }
}

proptest::proptest! {
    /// The spec's hard-ceiling law, for ALL inputs: a compiled pack's measured
    /// total never exceeds `min(target, MAX_PACK_TOKENS, turn_remaining)`,
    /// and compile always terminates (the degrade loop strictly shrinks).
    #[test]
    fn measured_total_never_exceeds_effective_cap(
        budget in 1u32..6000,
        turn in 1u32..6000,
        blocks in proptest::collection::vec("[a-z ]{5,120}", 0..25),
        facts in 0usize..4,
    ) {
        let mut r = PackRequest::new(budget);
        for i in 0..facts {
            r.push_critical(SectionEntry::critical(&CriticalFact::RevertedProvenance {
                evidence: EvidenceRef::at(u32::try_from(i).unwrap_or(0) + 1),
            })).unwrap();
        }
        for (i, b) in blocks.iter().enumerate() {
            r.push_changed(env_entry(
                &exact_env(&format!("src/f{i}.rs"), b),
                Sensitivity::Normal,
            )).unwrap();
        }
        let cap = budget.clamp(1, MAX_PACK_TOKENS).min(turn);
        match compile(&r, turn) {
            Ok(pack) => {
                proptest::prop_assert!(
                    pack.total_tokens <= cap,
                    "measured {} > cap {cap}", pack.total_tokens
                );
                proptest::prop_assert_eq!(
                    pack.total_tokens,
                    rift_tokens::count(&pack.render())
                );
            }
            Err(PackError::BudgetTooSmall { .. }) => {} // loud, legitimate
        }
    }

    /// No lossy degradation, sensitive or otherwise: every surviving line is
    /// a WHOLE entry rendering — the original text or its full handle spill —
    /// never a truncated slice of inline content (sensitive-safe).
    #[test]
    fn degradation_swaps_whole_entries_never_truncates(
        budget in 30u32..400,
        blocks in proptest::collection::vec("[a-z ]{10,150}", 1..15),
    ) {
        let mut r = PackRequest::new(budget);
        let mut originals = Vec::new();
        for (i, b) in blocks.iter().enumerate() {
            let e = env_entry(
                &exact_env(&format!("src/f{i}.rs"), b),
                Sensitivity::Normal,
            );
            originals.push(e.text().to_string());
            r.push_changed(e).unwrap();
        }
        if let Ok(pack) = compile(&r, u32::MAX) {
            for sec in pack.sections.iter().filter(|s| s.kind == SectionKind::Changed) {
                for line in &sec.lines {
                    // The spill form since the switch to ordinals: the handle it degraded
                    // to used to be `available: file:<path>@sha256:<hash>`, and
                    // the citation replaced it — so the whole-entry test is
                    // "it is an original, or it is the citation's spill".
                    let whole = originals.contains(line)
                        || (line.starts_with(FILE_TAG)
                            && line.ends_with(" available (budget_exhausted)"));
                    proptest::prop_assert!(whole, "a truncated slice leaked: {line}");
                }
            }
        }
    }
}

proptest::proptest! {
    /// The accounting law over every budget: what the caller offered is either
    /// in the pack or in the count. The one exemption is the anchors-only
    /// floor, where the announcement itself has been given up — and there the
    /// body is empty, which the pack shows on its face.
    #[test]
    fn nothing_is_dropped_without_being_counted(budget in 30u32..2500) {
        if let Ok(pack) = compile(&degrading_req(budget), u32::MAX) {
            let held = body_entries(&pack);
            match announced_drops(&pack) {
                Some(n) => proptest::prop_assert_eq!(
                    held + n as usize,
                    DEGRADING_BODY_ENTRIES,
                    "budget {}: {} held + {} announced\n{}", budget, held, n, pack.render()
                ),
                None => proptest::prop_assert!(
                    held == DEGRADING_BODY_ENTRIES || held == 0,
                    "budget {}: {} entries held, none announced missing\n{}",
                    budget, held, pack.render()
                ),
            }
        }
    }
}

proptest::proptest! {
    /// The law behind the enumeration: for ANY caller string in an identifier
    /// field, the rendered entry is exactly one line and carries no character
    /// the sanitizer claims to neutralize.
    #[test]
    fn any_identifier_renders_as_one_visible_line(
        chars in proptest::collection::vec(proptest::char::any(), 0..24),
    ) {
        let hostile: String = chars.into_iter().collect();
        for (position, entry) in entries_carrying(&hostile) {
            let (frame, content) = frame_and_content(&entry);
            proptest::prop_assert!(
                content.is_none_or(|c| c == TRUSTED_BLOCK),
                "{} rendered lines of its own: {:?}",
                position,
                entry.text()
            );
            proptest::prop_assert!(
                !frame.chars().any(crate::entry::is_invisible_or_line_breaking),
                "{} kept a neutralized char: {:?}",
                position,
                frame
            );
        }
    }
}

proptest::proptest! {
    /// Byte-identity across seeds: equal inputs → equal key AND
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
            pr.push_recommended(SectionEntry::from_handle(&handle, 1)).unwrap();
        }
        // Determinism holds across the whole contract: identical inputs give
        // identical outcomes — byte-identical packs OR the identical fail-loud
        // error (tiny budgets are legitimately too small now).
        let (a, b) = (compile(&pr, u32::MAX), compile(&pr, u32::MAX));
        match (a, b) {
            (Ok(x), Ok(y)) => proptest::prop_assert_eq!(x.render(), y.render()),
            (Err(x), Err(y)) => proptest::prop_assert_eq!(x, y),
            (x, y) => proptest::prop_assert!(false, "nondeterministic outcome: {x:?} vs {y:?}"),
        }
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

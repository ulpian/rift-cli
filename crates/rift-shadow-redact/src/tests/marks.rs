//! Overlap resolution and settling: no loser's remainder is published, a
//! parse read off blanked text displaces nothing, a settling round finds what
//! the first pass could not, and a pass stays linear in the document.

use rift_core::SecretBytes;

use super::fixtures::masked;
use crate::marks::{blank, coalesce, propose, select_spans, winners, work, ByteMarks};
use crate::span::Span;
use crate::{redact, redact_with, DetectorClass, RulePackSet};

/// Regression: fuzz crash-265daa39b7149f97854ec8fa27d4753c0e7d3e6e.
///
/// The input holds a high-entropy token that *ends inside* a PII email
/// (`…abcdef0p_011=vendor.githu@b.pat`). The email wins coalescing, and the
/// overlapping entropy span used to be dropped whole — leaving the
/// unmasked high-entropy prefix `KOSFODNN7E…0p_011=` in the output, which
/// a second redaction then masked (non-idempotent, and a first-pass leak).
#[test]
fn overlap_dropped_span_remainder_is_still_masked() {
    let raw: &[u8] = &[
        134, 0, 73, 91, 65, 0, 0, 255, 255, 48, 49, 50, 51, 0, 0, 0, 118, 101, 110, 100, 111, 114,
        46, 103, 105, 116, 104, 117, 0, 75, 79, 83, 70, 79, 68, 78, 78, 55, 69, 50, 51, 52, 53, 54,
        55, 56, 57, 97, 98, 99, 100, 101, 102, 48, 112, 95, 48, 49, 49, 61, 118, 101, 110, 100,
        111, 114, 46, 103, 105, 116, 104, 117, 64, 98, 46, 112, 97, 116, 10,
    ];
    let first = redact(&SecretBytes::new(raw.to_vec())).unwrap();
    let cleared = first.redacted.as_slice().to_vec();
    let text = String::from_utf8_lossy(&cleared);
    assert!(
        !text.contains("KOSFODNN7E23456789abcdef0p_011="),
        "high-entropy fragment survived the first pass: {text}"
    );
    let second = redact(&SecretBytes::new(cleared.clone())).unwrap();
    assert_eq!(
        second.redacted.as_slice(),
        cleared.as_slice(),
        "redaction must be idempotent on its own output"
    );
}

/// The reject, verbatim. It was found while [`coalesce`] still dropped the
/// losing side of an overlap **whole**, so a numeric-PII span landing inside
/// a longer high-entropy token published the rest of it — 15-19 characters,
/// one short of the [`ENTROPY_MIN_LEN`] floor the detect→mask fixpoint would
/// have used to recover them. Every row here was found by a random
/// differential search against *that* pipeline, not constructed by hand;
/// [`coalesce`] carves the loser now, and these rows are what says so.
///
/// The last row is the one that decides how this reads: the mechanism was
/// **already reachable on main** through `pii.ipv4`, so the new detectors
/// multiplied its reach rather than invented it, and closing it makes this
/// change a net improvement in redaction rather than a restoration.
#[test]
fn a_numeric_span_never_carves_a_high_entropy_token() {
    for (input, published) in [
        ("Zx9Qw3eR7tY2uI8oPa-378282246310005", "Zx9Qw3eR7tY2uI8oPa"),
        ("nQDK5E0rRNAsr3Ceh8=4838532243606", "nQDK5E0rRNAsr3Ceh8"),
        ("eJh0f2qowNanaLl-4014685929926", "eJh0f2qowNanaLl"),
        ("3cJm8OK6eR8t07tTO+4072631099565948514", "3cJm8OK6eR8t07tTO"),
        ("iAoAoQtaz2joBcMn=2658673162067016", "iAoAoQtaz2joBcMn"),
        ("KEOZAaMeeLWNVjkd8u=+1-581-545-9267", "KEOZAaMeeLWNVjkd8u"),
        ("ujz2rZDDCiPFxvQR/+28735304574", "ujz2rZDDCiPFxvQR"),
        // Live on main today, with no new detector involved at all.
        ("Zx9Qw3eR7tY2uI8oPa-192.168.1.1", "Zx9Qw3eR7tY2uI8oPa"),
    ] {
        let m = masked(input);
        assert!(!m.contains(published), "{input} published {published}: {m}");
    }
}

/// **The published-remainder reject, verbatim.** Its mechanism survived
/// the first fix because
/// that fix keyed on whole-match detectors and `pii.us_taxpayer_id_labelled`
/// masks a capture *group*: the labelled value won the overlap, the entropy
/// span was dropped whole, and the rest of the token shipped — 22 characters
/// of it. These detectors do not exist on main, so this was a relaxation the
/// branch introduced. Carving the loser closes it without knowing anything
/// about groups.
#[test]
fn a_labelled_value_never_carves_the_token_it_sits_in() {
    for (input, published) in [
        (
            "tax_id 325531976+eSorngoNobNA7WT07TOWk",
            "eSorngoNobNA7WT07TOWk",
        ),
        ("SSN: 260954757=riLaBefpXlEAzxzV", "riLaBefpXlEAzxzV"),
        ("national_id 738486159/vLsVecq8aOJPc4GO", "vLsVecq8aOJPc4GO"),
        (
            "ssn=123456789+eSorngoNobNA7WT07TOWk",
            "eSorngoNobNA7WT07TOWk",
        ),
    ] {
        let m = masked(input);
        assert!(!m.contains(published), "{input} published {published}: {m}");
    }
}

/// **The grown-span reject, verbatim.** Growing a winner's span to
/// swallow the guesses it
/// touches reproduced the same failure one level up: the grown span reached
/// a *competitor*, coalescing dropped that competitor whole, and its
/// remainder — a URI username and the first bytes of its password — shipped.
/// Nothing grows now, so no span can reach a competitor it never matched.
#[test]
fn a_winner_never_reaches_across_a_competitor() {
    let m =
        masked("https://alice:se!cretTOKENVALUE@h.com/xghp_77nq5VH8bb0MrD5Ag0ro6zXAPtyGa8jyLqY1");
    assert!(!m.contains("alice"), "username published: {m}");
    assert!(!m.contains("se!"), "password head published: {m}");
    assert!(m.contains("[REDACTED:uri]"), "{m}");
    assert!(m.contains("[REDACTED:vendor]"), "{m}");
}

/// Carving a loser costs nothing a neighbour needed. A detector anchored on
/// a word boundary cannot see a shape that abuts a token, and the old
/// pipeline found it on the *next* pass once a placeholder supplied the
/// boundary. A carved remainder takes that second chance away — masking the
/// entropy tail `161` leaves `.214.203.186`, and three octets are not an
/// address — so the winner set settles inside the pass instead. These three
/// inputs were the whole remainder of the 30 000-record differential.
#[test]
fn a_carved_remainder_never_costs_a_neighbouring_shape() {
    for (input, published) in [
        (
            "ITIN ghp_mspSGeibkh8D79F9XLgrKLBoMIvNjBW3h5wg161.214.203.186",
            ".214.203.186",
        ),
        (
            "tax id: AIzaAGI3XwY-E-guQwL6B30OZS9tmNuQwDe8fh8221.254.5.0",
            ".254.5.0",
        ),
        ("\"ssn\":\"AKIA6SJHYDE45M8ZX0B739.196.174.74", ".196.174.74"),
    ] {
        let m = masked(input);
        assert!(!m.contains(published), "{input} published {published}: {m}");
    }
}

/// **The relaxation the published-remainder reject named, at its
/// mechanism.** A `uri.credentials` parse
/// that exists only because settling blanked the bytes its regex could not
/// cross is not a reading of the input, so it displaces nobody — and the
/// competitor it used to displace, `pii.email` over `6789@h.co`, keeps the
/// host bytes that drop-the-loser masked and this pipeline published.
///
/// Each half is asserted structurally rather than trusted: the parse is
/// absent from the input's own proposals and present in the blanked copy's,
/// which is what makes the input a member of the class instead of a literal
/// that happens to reproduce it.
#[test]
fn a_parse_that_only_a_blanked_copy_can_see_displaces_nothing() {
    let doc = "http://user:123 45 6789@h.co/path";
    // Asked of the pipeline rather than of one table, and by choice: what
    // this test needs is whether `propose` reads a URI out of the document,
    // which is a verdict over the whole input and not a fact any table
    // holds. It is *not* that the table is out of reach. Reaching the
    // pass-1 table from outside `builtin_tables` goes through
    // `builtin_shape_tables`, exactly as the sibling sentence on
    // `numeric_pii_shape_detectors` already says for the PII table, and
    // `the_reference_pipeline_is_still_the_one_main_runs` reads all nine of
    // that table's detectors — pattern, group and gate — that way, without
    // calling `propose` at all.
    let uri_spans = |t: &str| {
        propose(t, &RulePackSet::builtin())
            .into_iter()
            .filter(|s| s.id == "uri.credentials")
            .count()
    };
    assert_eq!(
        uri_spans(doc),
        0,
        "the input itself must not parse as a URI"
    );
    let hidden = winners(propose(doc, &RulePackSet::builtin()));
    assert_eq!(
        uri_spans(&blank(doc, &hidden)),
        1,
        "the blanked copy is what makes the parse appear"
    );
    let m = masked(doc);
    for published in ["@h.co", "h.co/path", "6789"] {
        assert!(!m.contains(published), "{doc} published {published}: {m}");
    }
    // The intended case parses in the input as written, so the exception
    // still fires and the host still survives.
    let intended = masked("postgres://admin:s3cr3tpw@db.example.com");
    assert_eq!(intended, "postgres://[REDACTED:uri]@db.example.com");
}

/// The other sign of [`select_spans`]'s doc: a settling round genuinely
/// finds a shape the first proposal could not, so "settling is additive" is
/// a statement with content and not a description of a no-op.
#[test]
fn a_settling_round_finds_a_shape_the_first_pass_could_not() {
    let doc = "ITIN ghp_mspSGeibkh8D79F9XLgrKLBoMIvNjBW3h5wg161.214.203.186";
    let packs = RulePackSet::builtin();
    let covered = |spans: &[Span]| {
        (0..doc.len())
            .filter(|b| spans.iter().any(|s| s.start <= *b && *b < s.end))
            .count()
    };
    let settled = covered(&select_spans(doc, &packs));
    let flat = covered(&coalesce(propose(doc, &packs)));
    assert!(
        settled > flat,
        "settling covered {settled} bytes, one pass covered {flat}"
    );
}

/// The span arithmetic the whole guarantee rests on, exercised directly on
/// the structure that now performs it. Removing a middle splits, removing an
/// end trims, removing everything empties, and a piece is never wider than
/// what it came from.
///
/// Every row here is a row `subtract` carried before [`ByteMarks`] replaced
/// it, re-expressed — *plus* the rows a packed representation can get wrong
/// and a list of intervals cannot: a range inside one word, a range
/// straddling a word boundary, a range longer than a word, a whole word
/// claimed, and a single free byte sitting on the boundary itself.
#[test]
fn subtracting_a_claimed_range_never_widens_what_is_left() {
    let runs = |range: (usize, usize), taken: &[(usize, usize)]| {
        let mut m = ByteMarks::new(4096);
        for &(s, e) in taken {
            m.mark(s, e);
        }
        m.free_runs(range.0, range.1)
    };
    // The rows `subtract` carried.
    assert_eq!(runs((0, 10), &[(3, 6)]), vec![(0, 3), (6, 10)]);
    assert_eq!(runs((0, 10), &[(0, 4)]), vec![(4, 10)]);
    assert_eq!(runs((0, 10), &[(4, 10)]), vec![(0, 4)]);
    assert_eq!(runs((0, 10), &[(0, 10)]), Vec::new());
    assert_eq!(runs((0, 10), &[(20, 30)]), vec![(0, 10)]);
    assert_eq!(runs((2, 8), &[(0, 3), (7, 9)]), vec![(3, 7)]);
    assert_eq!(runs((0, 9), &[(1, 2), (4, 5), (7, 8)]).len(), 4);
    for piece in runs((5, 25), &[(9, 11), (0, 7), (24, 40)]) {
        assert!(5 <= piece.0 && piece.1 <= 25, "widened: {piece:?}");
    }
    // The rows the packing adds.
    assert_eq!(runs((70, 80), &[(72, 74)]), vec![(70, 72), (74, 80)]);
    assert_eq!(runs((60, 70), &[(62, 66)]), vec![(60, 62), (66, 70)]);
    assert_eq!(runs((0, 200), &[(64, 128)]), vec![(0, 64), (128, 200)]);
    assert_eq!(runs((0, 64), &[(0, 64)]), Vec::new());
    assert_eq!(runs((0, 128), &[(0, 64), (65, 128)]), vec![(64, 65)]);
    assert_eq!(runs((63, 65), &[(63, 64)]), vec![(64, 65)]);
    assert_eq!(runs((0, 192), &[(0, 64), (128, 192)]), vec![(64, 128)]);
    // …and the same boundaries read through `any`, which is the other scan
    // the marks replaced.
    let mut m = ByteMarks::new(4096);
    m.mark(64, 128);
    assert!(!m.any(0, 64), "a word before the marked one read as marked");
    assert!(m.any(63, 65), "a straddling range missed the marked byte");
    assert!(m.any(127, 200), "the last marked byte was missed");
    assert!(
        !m.any(128, 4096),
        "the word after the marked one read as marked"
    );
    assert!(!m.any(70, 70), "an empty range cannot touch anything");
}

/// A deterministic 3-3-4 phone column (`dense`), or the same geometry in
/// digits no detector claims. The generator is inline and seeded so that
/// every run draws the same document — a ratio over two different corpora is
/// not a ratio.
fn growth_corpus(bytes: usize, dense: bool) -> String {
    let mut seed = 0x5EED_1234_ABCD_0001_u64;
    let mut next = move || {
        seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let mut doc = String::with_capacity(bytes + 64);
    while doc.len() < bytes {
        if dense {
            let (a, b, c) = (200 + next() % 800, 200 + next() % 800, next() % 10_000);
            doc.push_str(&format!("  {a:03} {b:03} {c:04}\n"));
        } else {
            let a = next() % 1_000_000_000;
            doc.push_str(&format!("  {a:09}00\n"));
        }
    }
    doc
}

/// The steps [`work`] counted while redacting `doc`, and how many spans came
/// out — the second half is what keeps the ratios below from being green
/// because an arm stopped matching.
fn redact_counted(doc: &str) -> (u64, u32) {
    let raw = SecretBytes::from(doc);
    work::take();
    let out = redact(&raw).unwrap();
    (work::take(), out.report.total_spans)
}

/// **The asymptote, asserted**, so it cannot come back in silence.
///
/// [`winners`] and [`coalesce`] used to scan the whole chosen set once per
/// span, which is O(spans²); span count is linear in document length, so the
/// redactor was quadratic in bytes — measured at exponent **1.94**, 8.5 s
/// for a single pass over 1 MiB of this corpus, against 0.03 s for the same
/// bytes with nothing in them.
///
/// **It counts steps, not seconds.** The clock measured the machine, the
/// build and whoever else was on the box at least as much as the algorithm,
/// and its ratio-of-ratios form still reddened under load and passed alone.
/// [`work`] counts, in the test build only, every step of the loops the
/// quadratic lived in and could return to: each word (and, in a partly
/// marked word, each bit) the [`ByteMarks`] queries `mark`, `any` and
/// `free_runs` walk; each span [`winners`], [`coalesce`], [`blank`] and
/// `mask` visit and each piece `coalesce` carves; and, in `select_spans`'
/// settle loop, each proposal per round, each re-stamped find and the full
/// proposal set per find for the duplicate check. The count is the same on
/// every machine and under any load, so the bounds are pinned to measured
/// figures rather than to a guess at the worst box.
///
/// **What it catches:** a rescan of the chosen set per span in `winners` or
/// `coalesce` that runs through those loops — the marks asked once per
/// chosen span, or a span loop that carries its own count — turns the
/// eight-times step below into a sixty-four-times one. **What it cannot
/// see:** a new loop in this module that nobody counted; the precedence sort
/// (n log n, outside the count); and the work inside the detectors — the
/// regex searches, whose engine guarantees time linear in the haystack, the
/// entropy tokenizer's single forward scan, and the gates that read one
/// match's bytes each. Those are linear by construction, not by this test.
///
/// * **Size.** Eight times the document — 2 185 spans to 17 477 — counted
///   **21 169 → 169 309** steps, a ratio of **7.998**. The ceiling is
///   8 × 1.25 = **10.0**: linear sits at 8, a quadratic at 64, and the 25 %
///   leaves room for a counted step a later change adds per span without
///   letting a super-linear one through at this size.
/// * **Density.** The span-free control costs this module **zero** steps at
///   both sizes — nothing proposed, nothing to resolve — so the old
///   dense-over-flat ratio has no denominator here; the bound becomes steps
///   per span at the large size, measured **9.69** and held at **12.0**
///   (9.69 × 1.25, rounded up). A per-span constant that grows is what the
///   density arm was for: same bytes, more cost from the spans in them.
///
/// Calibrated by mutation, not argued: one loop inserted at the head of
/// `coalesce`'s, asking the marks about every span already chosen, reddens
/// the size arm at **63.5x** (2 407 189 → 152 883 335 steps); reverted, it
/// passes.
#[test]
fn redaction_cost_does_not_grow_faster_than_the_document() {
    let (w_small, n_small) = redact_counted(&growth_corpus(32 * 1024, true));
    let (w_large, n_large) = redact_counted(&growth_corpus(256 * 1024, true));
    // The control: the same two sizes with nothing any detector claims.
    let (w_flat_small, n_flat_small) = redact_counted(&growth_corpus(32 * 1024, false));
    let (w_flat, n_flat) = redact_counted(&growth_corpus(256 * 1024, false));
    assert!(
        n_small > 2_000 && n_large > 16_000,
        "the dense arms stopped matching ({n_small}, {n_large} spans), so the ratios say nothing"
    );
    assert_eq!(
        (n_flat_small, n_flat),
        (0, 0),
        "a control arm started matching: {n_flat_small}, {n_flat} spans"
    );
    assert_eq!(
        (w_flat_small, w_flat),
        (0, 0),
        "a span-free document cost the span arithmetic steps"
    );
    let by_size = w_large as f64 / w_small as f64;
    let per_span = w_large as f64 / f64::from(n_large);
    assert!(
        by_size <= 8.0 * 1.25,
        "eight times the document cost {by_size:.3}x the steps \
         ({w_small} -> {w_large} steps, {n_small} -> {n_large} spans)"
    );
    assert!(
        per_span <= 12.0,
        "a span cost {per_span:.2} steps at the large size ({w_large} steps, {n_large} spans)"
    );
}

/// The hostile span-count shape, as a **clock-free** assertion about what
/// the pipeline does with it.
///
/// Every token here is both a vendor match and a high-entropy run, so each
/// one proposes two coextensive spans of different precedence — the shape
/// where the span count is the input author's parameter rather than the
/// document's. The parked branch carried a commit bounding a per-absorbing-
/// span fixpoint over every heuristic span, written for an
/// `absorb_overlapped_heuristics` that no longer exists — merge #19 replaced
/// the scanning union with byte marks, and [`coalesce`] is still very much
/// here. This asserts the property that commit was protecting,
/// which is that resolving N overlaps costs one span each and not N each.
/// Two proposals per token in, one winner per token out, and nothing
/// unmasked between them.
#[test]
fn coextensive_overlaps_resolve_to_one_span_each() {
    let unit = "ghp_QWERTYUIOPASDFGHJKLZXCVBNM0123456789 ";
    let n = 2_000usize;
    let doc = unit.repeat(n);
    let proposed = propose(&doc, &RulePackSet::builtin());
    assert_eq!(
        proposed
            .iter()
            .filter(|s| s.class == DetectorClass::VendorSecret)
            .count(),
        n
    );
    assert_eq!(
        proposed
            .iter()
            .filter(|s| s.class == DetectorClass::Entropy)
            .count(),
        n
    );
    assert_eq!(
        select_spans(&doc, &RulePackSet::builtin()).len(),
        n,
        "N coextensive overlaps must collapse to N winners"
    );
    let out = redact(&SecretBytes::from(doc.as_str())).unwrap();
    let text = String::from_utf8(out.redacted.as_slice().to_vec()).unwrap();
    assert!(
        !text.contains("QWERTYUIOPASDFGHJKLZXCVBNM"),
        "a token survived"
    );
}

/// The timing half of the row above, kept `#[ignore]`d because it is a
/// measurement instrument and not a gate — a fifth wall-clock assertion in
/// the required suite buys less than it costs in flake.
///
/// **Measured 2026-08-08, `--release`, 2019 Intel i5-8279U, min of three**,
/// over a 16x range of span counts (32 000 → 512 000 proposals, 656 KB →
/// 10.5 MB): 0.0537 s → 0.9728 s, i.e. **18.11x for 16x the spans, an
/// exponent of 1.045**; the four step-to-step exponents were 1.114, 1.061,
/// 0.967, 1.036. Quadratic would predict 256x. So the class the parked
/// bound was written against does not survive the byte-mark rewrite, and
/// the bound was not ported — this is the measurement that decided it, and
/// it is re-runnable rather than a sentence.
#[test]
#[ignore = "measurement instrument, not a gate: see the doc for the recorded run"]
fn probe_hostile_span_count_growth() {
    use std::time::Instant;
    let unit = "ghp_QWERTYUIOPASDFGHJKLZXCVBNM0123456789 ";
    let mut prev: Option<(usize, f64)> = None;
    for n in [16_000usize, 32_000, 64_000, 128_000, 256_000] {
        let doc = unit.repeat(n);
        let proposed = propose(&doc, &RulePackSet::builtin()).len();
        let mut best = f64::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            let o = redact(&SecretBytes::from(doc.as_str())).unwrap();
            let secs = t.elapsed().as_secs_f64();
            assert!(o.report.total_spans > 0);
            best = best.min(secs);
        }
        let exp = prev.map_or(f64::NAN, |(pn, pt)| {
            (best / pt).log2() / ((n as f64) / (pn as f64)).log2()
        });
        println!(
            "PROBE tokens={n} bytes={} proposed={proposed} min_secs={best:.4} \
             exponent_vs_prev={exp:.3}",
            doc.len()
        );
        prev = Some((n, best));
    }
}

/// Two heuristic spans overlapping each other: the card wins on width and
/// the email is dropped whole, so before the union `4111 1111 1111 1111@a.io`
/// masked the email and published fourteen digits of the card on main, and
/// masked the card and published `@a.io` here. Neither now.
#[test]
fn two_overlapping_heuristic_spans_seal_their_union() {
    let m = masked("4111 1111 1111 1111@a.io");
    assert!(!m.contains("4111 1111 1111"), "card published: {m}");
    assert!(!m.contains("@a.io"), "email published: {m}");
}

/// The same carve with no PII in sight: a built-in vendor pattern matching
/// the *middle* of a high-entropy token published both ends — 15 characters
/// each, under the length floor, so the fixpoint could not recover them
/// either. Found by the probe written for the pack case, which is why the
/// rule is keyed on whole-match detectors and not on pack rules.
#[test]
fn a_builtin_whole_match_detector_never_carves_a_heuristic_span() {
    let m = masked("Zx9Qw3eR7tY2uI8AKIAIOSFODNN7EXAMPLEoP1aS5dF6gH0j");
    assert!(!m.contains("Zx9Qw3eR7tY2uI8"), "head published: {m}");
    assert!(!m.contains("oP1aS5dF6gH0j"), "tail published: {m}");
}

/// A pack rule may only ever seal *more*. Both rows carve a heuristic span
/// from the inside; the email row is the plain leak (`ample.com` survives,
/// and nothing re-detects a bare domain), the entropy row the quiet one
/// (the tail left behind is under the length floor).
#[test]
fn a_pack_rule_never_carves_a_heuristic_span() {
    for (rule, input, remainder) in [
        ("alice@ex", "mail alice@example.com end", "ample.com"),
        (
            "Zx9Qw3eR7tY2",
            "tok Zx9Qw3eR7tY2uI8oP1aS5dF6gH0j end",
            "uI8oP1aS5dF6gH0j",
        ),
    ] {
        let packs = RulePackSet::builtin()
            .with_vendor_pattern("acme.rule", rule, "acme@1")
            .unwrap();
        let o = redact_with(&SecretBytes::from(input), &packs).unwrap();
        let m = String::from_utf8(o.redacted.as_slice().to_vec()).unwrap();
        assert!(!m.contains(remainder), "rule {rule} carved {input}: {m}");
    }
}

/// A capture-group detector keeps its context — and exactly how far that
/// goes, because it is the one place [`coalesce`] does not seal the union.
///
/// It reaches a **competing parse**: `pii.email` reads
/// `Zx9…@db.example.com` out of the URI, loses to the detector that knows
/// the `@` is an authority delimiter, and goes whole rather than leaving its
/// remainder — which is what keeps the host, the thing that makes a captured
/// transcript diagnosable (`uri.host_preserved`).
///
/// It does **not** reach the entropy layer, and that is stated rather than
/// discovered: when the entropy tokenizer swallows the key name into the
/// same run as the value (`=` is a token character), the key goes with it.
/// Over-redaction, in the direction the legacy backend's whole-match connection-string
/// regions already take, and the price of never publishing a guess's
/// remainder.
#[test]
fn a_group_detector_keeps_its_context_and_only_its_context() {
    let m = masked("postgres://admin:Zx9Qw3eR7tY2uI8oP1aS5dF@db.example.com/app");
    assert!(m.contains("db.example.com"), "host must survive: {m}");
    assert!(!m.contains("Zx9Qw3eR7tY2uI8oP1aS5dF"), "{m}");
    // the key survives when nothing else claims it
    let short = masked("password=hunter2;");
    assert_eq!(short, "password=[REDACTED:jdbc];");
    // and goes when the entropy layer claimed `password=…` as one token
    let d = masked("password=Zx9Qw3eR7tY2uI8oP1aS5dF6gH;");
    assert_eq!(d, "[REDACTED:entropy][REDACTED:jdbc];");
    assert!(!d.contains("Zx9Qw3eR7tY2uI8oP1aS5dF6gH"), "{d}");
}

//! Overlap resolution and settling: no loser's remainder is published, a
//! parse read off blanked text displaces nothing, a settling round finds what
//! the first pass could not, and a pass stays linear in the document.

use rift_core::SecretBytes;

use super::fixtures::masked;
use crate::marks::{blank, coalesce, propose, select_spans, winners, ByteMarks};
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
/// every machine draws the same document — a timing ratio over two different
/// corpora is not a ratio.
fn timing_corpus(bytes: usize, dense: bool) -> String {
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

/// Seconds to redact `doc`, and how many spans came out — the second half is
/// what keeps the ratios below from being green because an arm stopped
/// matching.
fn redact_timed(doc: &str) -> (f64, u32) {
    let raw = SecretBytes::from(doc);
    let started = std::time::Instant::now();
    let out = redact(&raw).unwrap();
    (started.elapsed().as_secs_f64(), out.report.total_spans)
}

/// **The asymptote, asserted**, so it cannot come back in silence.
///
/// [`winners`] and [`coalesce`] used to scan the whole chosen set once per
/// span, which is O(spans²); span count is linear in document length, so the
/// redactor was quadratic in bytes — measured at exponent **1.94**, 8.5 s
/// for a single pass over 1 MiB of this corpus, against 0.03 s for the same
/// bytes with nothing in them.
///
/// **Both bounds are ratios on purpose.** A wall-clock ceiling measures the
/// machine and the build at least as much as the algorithm — this suite runs
/// unoptimized, and again under coverage instrumentation, on a box shared
/// with other work — so a ceiling loose enough never to flake there is far
/// too loose to catch what it is for. A ratio cancels all three.
///
/// * **Size.** Eight times the document may not cost more than **1.5 times
///   what eight times the same document costs the SAME pipeline with nothing
///   in it to find**. Linear predicts 1.0; the scanning implementation's
///   recorded 24.16 against a scanning arm the span-free corpus leaves at ~8
///   is ~3.0. The old form of this arm compared 8x the document against a
///   bare **12.0** — a raw wall-clock ratio, with the flake that implies;
///   see the paragraph below.
/// * **Density.** The same byte count with a phone number every fifteen of
///   them may not cost six times the same byte count with nothing any
///   detector claims. This is the variable the composition measurement
///   actually found — same size, same read, same archive, and the cost was
///   span density — and it measures **3.33** here against **18.16** for the
///   scanning implementation.
///
/// **The size arm is a ratio of ratios, and that is a repair with its own
/// measurements.** It reddened at **14.26x** under fleet load 53 and passed
/// at load 14 — a guard whose verdict depends on who else is on the box. A
/// raw `t_large / t_small` has no machine-independent expected value, so its
/// ceiling has to be chosen loose enough for the worst machine and is then
/// too loose for the best. Dividing by the *same run's* span-free control
/// over the *same* two sizes gives a dimensionless number whose expected
/// value is 1.0 on any machine — the house rule being to quote a ratio to
/// its own control measured in the same run.
///
/// **Mutation-calibrated on this tree, not argued.** One line inserted at
/// the head of [`coalesce`]'s loop —
/// `chosen.iter().filter(|c| c.start <= s.start).count()`, an O(spans²) scan
/// and nothing else — reddens this arm at **excess 2.658** (`by_size` 22.86
/// against a control of 8.60), at fleet load 92; the line reverted, it
/// passes. So the ceiling of 1.5 sits between a measured ~1.0 and a measured
/// 2.66 with 1.47x of margin below and 1.77x above, and the plant is the
/// quadratic class this arm exists for rather than a proxy for it. Note what
/// the two numbers show together: the raw ratio inflated to 22.86 while the
/// control inflated to 8.60, and dividing attributed the excess correctly —
/// which is the whole reason the control is measured rather than assumed
/// to be 8.
///
/// Measured on this box (2026-08-24, 8-core, dev profile, interleaved,
/// `min` per arm) across a deliberately produced ~5x swing in machine speed:
///
/// | load | `t_large/t_small` | control | **ratio of ratios** |
/// |---|---|---|---|
/// | ~8, quiet | 8.09 | 7.97 | **1.015** |
/// | ~30, 16 synthetic burners | 7.97 | 8.25 | **0.966** |
/// | ~87, fleet | 7.22 | 8.04 | **0.898** |
///
/// **If you reproduce the burner row, the load generator runs under
/// `trap 'kill 0' EXIT` — never a kill step after the measurement**
/// (a rule this run paid for). The original run's post-measurement kill
/// never executed because the measurement wedged under the load the
/// burners created, and 16 of them spun orphaned for 19 hours at ~5
/// cores, corrupting every wall-clock number on the machine. A wedged
/// measurement must take its load down with it; only an EXIT trap
/// guarantees that.
///
/// Absolute times moved 4.5x across those rows; the ratio of ratios stayed
/// within 6% of 1.0. **Stated because it is the honest limit of the claim:
/// the 14.26x reading was NOT reproduced at any of these three loads**, so
/// what is demonstrated is that the new statistic is dimensionless and
/// cancelled a controlled 4.5x slowdown — not that the observed flake is
/// cured. If it recurs, the reading to record is the ratio of ratios and its
/// control, which is a number a later reader can compare with these three.
///
/// Each arm is the `min` of **five** interleaved rounds (three before), the
/// estimator that survives a shared machine: contention can only ever make a
/// sample slower, so more rounds can only improve it.
///
/// **One basis for all four figures**, stated because three different pairs
/// were in circulation for the scanning arm and no reader could tell which
/// machine any of them came from: 2026-08-08, `cargo test` in the dev
/// profile (which is how this suite runs), 2019 Intel i5-8279U, min of three.
/// The scanning pair is this same tree with [`winners`] and [`coalesce`]
/// replaced by the transcriptions kept in `winners_dropping_the_loser` and
/// `coalesce_by_scanning`; both bounds fail under it, by 2.0x and 3.0x,
/// which is what makes asserting them worth the wall clock. An independent
/// run of the same four measurements on a different machine (recorded in the
/// C5 verdict) read 7.30 and 32.46 on size, 3.32 and 20.61 on density —
/// every passing arm *lower* there and every failing arm *higher* — so the
/// ratios move with the machine in both directions at once, and only the
/// sign of each comparison travels.
///
/// **The headroom is 1.50x on size and 1.80x on density.** On size that is
/// now 1.5 against a measured ~1.0 in a quantity that does not depend on the
/// machine, where it used to be 12.0 against a measured 7.99 in one that
/// does — the same factor, bought in a unit that travels. It is the
/// number to watch, it is smaller than the ratios alone suggest, and it is
/// the only wall-clock-derived quantity **in this crate** — one
/// `Instant::now`, in [`redact_timed`], and nothing else here reads a
/// clock. It is *not* the only one in the workspace test job the required
/// suite runs: `rift-shadow-hooks`' flush test, `rift-config`'s glob walk
/// and (until that crate was deleted) the wrapper's runner
/// tests all asserted on elapsed time as well. So if anything in *this file* flakes, this is it — which is the
/// claim this sentence used to make about a suite it had not measured.
/// The answer is not a looser bound, which
/// would stop catching the thing it is for; it is that a regression here is
/// real until measured otherwise.
#[test]
fn redaction_cost_does_not_grow_faster_than_the_document() {
    let small = timing_corpus(32 * 1024, true);
    let large = timing_corpus(256 * 1024, true);
    // The control: the SAME two sizes with nothing any detector claims, so
    // its ratio is this machine's cost of *scanning* eight times the bytes.
    let flat_small = timing_corpus(32 * 1024, false);
    let flat = timing_corpus(256 * 1024, false);
    let (mut t_small, mut t_large) = (f64::MAX, f64::MAX);
    let (mut t_flat_small, mut t_flat) = (f64::MAX, f64::MAX);
    let (mut n_small, mut n_large) = (0, 0);
    let (mut n_flat_small, mut n_flat) = (0, 0);
    for _ in 0..5 {
        let (t, n) = redact_timed(&small);
        t_small = t_small.min(t);
        n_small = n;
        let (t, n) = redact_timed(&large);
        t_large = t_large.min(t);
        n_large = n;
        let (t, n) = redact_timed(&flat_small);
        t_flat_small = t_flat_small.min(t);
        n_flat_small = n;
        let (t, n) = redact_timed(&flat);
        t_flat = t_flat.min(t);
        n_flat = n;
    }
    assert!(
        n_small > 2_000 && n_large > 16_000,
        "the dense arms stopped matching ({n_small}, {n_large} spans), so the ratios say nothing"
    );
    assert_eq!(
        (n_flat_small, n_flat),
        (0, 0),
        "a control arm started matching: {n_flat_small}, {n_flat} spans"
    );
    let by_size = t_large / t_small;
    // The same eight-times step measured on the same machine in the same
    // run with no spans to resolve. Linear predicts the two agree.
    let control_by_size = t_flat / t_flat_small;
    let excess = by_size / control_by_size;
    let by_density = t_large / t_flat;
    assert!(
        excess <= 1.5,
        "eight times the document cost {by_size:.2}x the time against {control_by_size:.2}x \
         for the same step with nothing in it — {excess:.3}x more than scanning alone \
         ({t_small:.4}s -> {t_large:.4}s, control {t_flat_small:.4}s -> {t_flat:.4}s)"
    );
    assert!(
        by_density <= 6.0,
        "a span every fifteen bytes cost {by_density:.2}x the same bytes with none \
         ({t_flat:.4}s -> {t_large:.4}s)"
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

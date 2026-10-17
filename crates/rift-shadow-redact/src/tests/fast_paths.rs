//! The short-token fast path as a proven invariant rather than a comment.
//!
//! `propose` returns nothing, without running a detector, for a text that
//! `is_short_benign_token` accepts: shorter than `ENTROPY_MIN_LEN` bytes, with
//! no ASCII digit and none of `: = @ - .`. That is a security claim — a skip
//! that a detector could have matched is a secret sealed unredacted — so it is
//! held from both sides: structurally, by reading every built-in pattern's
//! syntax tree, and behaviourally, by running every detector and the entropy
//! scanner on texts the gate accepts.

use proptest::prelude::*;
use regex_syntax::hir::{Class, Hir, HirKind};

use super::fixtures::out;
use crate::builtin_tables::builtin_shape_tables;
use crate::entropy::{entropy_spans, ENTROPY_MIN_LEN};
use crate::marks::{is_short_benign_token, select_spans};
use crate::shapes::{shape_spans, table_spans};
use crate::RulePackSet;

/// The bytes the gate refuses. The structural test assumes exactly this set,
/// and `the_gate_refuses_exactly_the_digits_and_delimiters` pins the gate to it.
fn is_gate_byte(b: u8) -> bool {
    b.is_ascii_digit() || matches!(b, b':' | b'=' | b'@' | b'-' | b'.')
}

/// Whether every match of `hir` must contain a byte the gate refuses.
///
/// Conservative in the direction that matters: `false` only ever means "not
/// proven", so a pattern this cannot read fails the test rather than passing it.
fn every_match_holds_a_gate_byte(hir: &Hir) -> bool {
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => false,
        HirKind::Literal(lit) => lit.0.iter().copied().any(is_gate_byte),
        HirKind::Class(class) => class_is_within_gate_bytes(class),
        HirKind::Repetition(rep) => rep.min >= 1 && every_match_holds_a_gate_byte(&rep.sub),
        HirKind::Capture(cap) => every_match_holds_a_gate_byte(&cap.sub),
        HirKind::Concat(subs) => subs.iter().any(every_match_holds_a_gate_byte),
        HirKind::Alternation(subs) => subs.iter().all(every_match_holds_a_gate_byte),
    }
}

/// Whether every character a class admits is a gate byte. A Unicode digit
/// such as `٣` is not one: the gate reads ASCII only, so a `\d` would fail here.
fn class_is_within_gate_bytes(class: &Class) -> bool {
    match class {
        Class::Unicode(c) => c
            .ranges()
            .iter()
            .all(|r| r.end().is_ascii() && (r.start()..=r.end()).all(|ch| is_gate_byte(ch as u8))),
        Class::Bytes(c) => c
            .ranges()
            .iter()
            .all(|r| (r.start()..=r.end()).all(is_gate_byte)),
    }
}

/// **Every built-in detector needs what the gate refuses.** Each pattern is
/// parsed as the `regex` crate parses it, and must either match at least
/// `ENTROPY_MIN_LEN` bytes or hold a digit or delimiter in every match. A
/// detector that does neither makes the gate wrong; the answer is to shrink
/// the gate, not this test.
#[test]
fn every_builtin_detector_needs_a_byte_or_a_length_the_gate_refuses() {
    let mut unproven = Vec::new();
    let mut checked = 0;
    for table in builtin_shape_tables() {
        for d in table {
            checked += 1;
            let hir = regex_syntax::parse(d.re.as_str()).expect("a built-in pattern parses");
            let long_enough = hir
                .properties()
                .minimum_len()
                .is_some_and(|n| n >= ENTROPY_MIN_LEN);
            if !long_enough && !every_match_holds_a_gate_byte(&hir) {
                unproven.push(d.id.to_string());
            }
        }
    }
    assert!(checked >= 25, "the walk read only {checked} detectors");
    assert!(
        unproven.is_empty(),
        "the short-token gate skips these detectors' matches: {unproven:?}"
    );
}

/// The reader the structural test leans on, checked against patterns whose
/// answer is known, so a reader that proves everything cannot pass it green.
#[test]
fn the_pattern_reader_proves_only_patterns_that_need_a_gate_byte() {
    let proves = |re: &str| every_match_holds_a_gate_byte(&regex_syntax::parse(re).expect(re));
    for needs in [
        "[0-9]",
        "a@b",
        "x(?:1|:)y",
        "(?i)password=",
        r"\b[0-9]{1,5}",
        "-{1,}",
    ] {
        assert!(proves(needs), "{needs} needs a gate byte");
    }
    for skips in [
        r"\d", "[a-z]{5}", "[0-9]?", "[0-9]*", "a|1", r"\bxox", "", "[:a]", "(?i)k",
    ] {
        assert!(!proves(skips), "{skips} can match without a gate byte");
    }
}

/// The structural test's premise, pinned: one ASCII byte at a time, the gate
/// refuses exactly the digits and `: = @ - .`, and refuses any text of
/// `ENTROPY_MIN_LEN` bytes or more whatever it holds.
#[test]
fn the_gate_refuses_exactly_the_digits_and_delimiters() {
    for b in 0u8..=127 {
        let text = char::from(b).to_string();
        assert_eq!(
            is_short_benign_token(&text),
            !is_gate_byte(b),
            "byte {b:#04x}"
        );
    }
    assert!(is_short_benign_token(&"a".repeat(ENTROPY_MIN_LEN - 1)));
    assert!(!is_short_benign_token(&"a".repeat(ENTROPY_MIN_LEN)));
    assert!(!is_short_benign_token(&"é".repeat(ENTROPY_MIN_LEN / 2)));
}

/// The fallback `propose` takes for a table whose `RegexSet` did not build:
/// the direct scan, so losing the prefilter costs time and never a span.
#[test]
fn a_table_without_a_regex_set_is_scanned_detector_by_detector() {
    let text = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 alice@example.com";
    for table in builtin_shape_tables() {
        let fallback = table_spans(text, table, None);
        let direct = shape_spans(text, table);
        let key = |s: &crate::span::Span| (s.start, s.end, s.id.clone(), s.parse);
        assert_eq!(
            fallback.iter().map(key).collect::<Vec<_>>(),
            direct.iter().map(key).collect::<Vec<_>>()
        );
    }
}

/// One generator piece: a prefix literal a detector opens on, an ASCII letter
/// or punctuation byte, a multi-byte character (including the case-fold
/// partners `ſ` and `K` and the non-ASCII digit `٣`), or any character at all.
/// Pieces that hold a gate byte are dropped by the filter.
fn piece() -> impl Strategy<Value = String> {
    let prefixes = prop::sample::select(vec![
        "AKIA",
        "ghp_",
        "sk_",
        "sk_live_",
        "sk",
        "xox",
        "xoxb",
        "AIza",
        "BEGIN",
        "PRIVATE KEY",
        "sb_secret_",
        "github_pat_",
        "password",
        "pwd",
        "jdbc",
        "Authorization",
        "bearer",
        "ssn",
        "Street",
        "api_key",
    ]);
    let multibyte = prop::sample::select(vec![
        "\u{17f}",
        "\u{212a}",
        "\u{663}",
        "\u{ff1a}",
        "\u{ff1d}",
        "\u{ff20}",
        "é",
        "日本",
        "\u{0}",
        "\u{1f600}",
    ]);
    prop_oneof![
        prefixes.prop_map(str::to_owned),
        "[ -~]{1,4}",
        multibyte.prop_map(str::to_owned),
        any::<char>().prop_map(String::from),
    ]
    .prop_filter("a gate byte", |p| !p.bytes().any(is_gate_byte))
}

/// Concatenated pieces, cut at a character boundary below `ENTROPY_MIN_LEN`.
fn short_benign_text() -> impl Strategy<Value = String> {
    prop::collection::vec(piece(), 0..10).prop_map(|pieces| {
        let mut text = String::new();
        for ch in pieces.concat().chars() {
            if text.len() + ch.len_utf8() >= ENTROPY_MIN_LEN {
                break;
            }
            text.push(ch);
        }
        text
    })
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(2048))]

    /// **A text the gate accepts is one no detector proposes on.** Every
    /// built-in detector is run alone, without the gate or the `RegexSet`, and
    /// must not even match; the entropy scanner must propose nothing; and the
    /// whole pipeline must leave the text unmarked.
    #[test]
    fn a_text_the_short_token_gate_skips_is_one_no_detector_would_mark(text in short_benign_text()) {
        prop_assert!(is_short_benign_token(&text), "generator escaped the gate: {:?}", text);
        for table in builtin_shape_tables() {
            for d in table {
                prop_assert!(!d.re.is_match(&text), "{} matches {:?}", d.id, text);
                prop_assert!(shape_spans(&text, std::slice::from_ref(d)).is_empty());
            }
        }
        prop_assert!(entropy_spans(&text).is_empty(), "entropy.high on {:?}", text);
        prop_assert!(select_spans(&text, &RulePackSet::builtin()).is_empty());
        // The report, not the bytes: `normalize` drops zero-width characters
        // such as U+FEFF before any detector runs, which is not a mark.
        let report = out(&text).report;
        prop_assert_eq!((report.total_spans, report.bytes_masked), (0, 0));
        prop_assert!(report.by_detector.is_empty());
    }
}

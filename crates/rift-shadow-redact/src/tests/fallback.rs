//! The per-line fallback: a line whose fixpoint does not converge is sealed
//! flat, or replaced by a closed-vocabulary placeholder record, and the rest of
//! the document keeps its normal-path bytes. Nothing is dropped, and the
//! fallback never seals less than the gate.

use rift_core::SecretBytes;

use crate::{
    count_fallback_placeholders, is_fallback_placeholder, redact_with, RedactionOutput, RulePackSet,
};

/// A rule that fires on a word and inside its own placeholder, so a line
/// carrying the word re-grows on every pass and the fixpoint never arrives.
/// It does not fire inside any other class's placeholder, so a neighbour line
/// whose secret is masked still settles.
fn oscillating() -> RulePackSet {
    RulePackSet::builtin()
        .with_vendor_pattern("osc", "WEDGE|REDACTED:vendor", "acme@1")
        .unwrap()
}

/// The same wedge anchored to the start of the text: it fires on a string
/// leaf that begins with the word and never on a JSON line, which begins with
/// a brace — so the walk diverges and the flat line converges.
fn oscillating_at_the_start() -> RulePackSet {
    RulePackSet::builtin()
        .with_vendor_pattern("osc_start", r"^\[?REDACTED", "acme@1")
        .unwrap()
}

fn sealed(text: &str, packs: &RulePackSet) -> RedactionOutput {
    redact_with(&SecretBytes::from(text), packs).unwrap()
}

fn text_of(out: &RedactionOutput) -> String {
    String::from_utf8(out.redacted.as_slice().to_vec()).unwrap()
}

#[test]
fn original_context_of_a_labelled_id_survives_a_wedged_neighbor() {
    let packs = oscillating();
    let control = text_of(&sealed("ssn=\n123456789", &packs));
    assert!(
        !control.contains("123456789"),
        "labelled control: {control}"
    );
    let out = sealed("WEDGE ssn=\n123456789", &packs);
    let text = text_of(&out);
    assert!(
        !text.contains("123456789"),
        "fallback lost label context: {text}"
    );
    assert_eq!(text.lines().count(), 2, "{text}");
    assert_eq!(out.report.redaction_flat_fallback, 2);
    assert_eq!(out.report.redaction_fallback_placeholders, 2);
    assert_eq!(count_fallback_placeholders(text.as_bytes()), 2);
    let again = sealed(&text, &packs);
    assert_eq!(text_of(&again), text);
    assert_eq!(again.report.redaction_flat_fallback, 0);
}

#[test]
fn original_context_of_a_key_block_survives_a_wedged_interior_line() {
    let packs = RulePackSet::builtin()
        .with_vendor_pattern("osc_key", "WEDGE|REDACTED:vendor|REDACTED:key", "acme@1")
        .unwrap();
    let key = "-----BEGIN RSA PRIVATE KEY-----\nABCDEF0123\nWEDGE\nGHIJKL4567\n-----END RSA PRIVATE KEY-----";
    let control = text_of(&sealed(key, &RulePackSet::builtin()));
    assert!(control.contains("[REDACTED:key]"));
    let doc = format!("safe before\n{key}\nsafe after");
    let out = sealed(&doc, &packs);
    let text = text_of(&out);
    for value in ["ABCDEF0123", "GHIJKL4567"] {
        assert!(
            !text.contains(value),
            "fallback lost key-block context: {text}"
        );
    }
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines.len(), 7);
    assert_eq!(lines.first(), Some(&"safe before"));
    assert_eq!(lines.last(), Some(&"safe after"));
    assert_eq!(out.report.redaction_flat_fallback, 7);
    assert_eq!(out.report.redaction_fallback_placeholders, 5);
    assert_eq!(count_fallback_placeholders(text.as_bytes()), 5);
    let again = sealed(&text, &packs);
    assert_eq!(text_of(&again), text);
}

#[test]
fn later_pass_context_created_by_masking_is_preserved_too() {
    let packs = RulePackSet::builtin()
        .with_vendor_pattern("osc", "TRIGGER|REDACTED:vendor", "acme@1")
        .unwrap()
        .with_vendor_pattern("after_mask", r"\[REDACTED:vendor\]\n[0-9]{9}", "acme@1")
        .unwrap();
    let out = sealed("TRIGGER\n123456789\nunaffected", &packs);
    let text = text_of(&out);
    assert!(
        !text.contains("123456789"),
        "later-pass protection lost: {text}"
    );
    assert_eq!(text.lines().count(), 3);
    assert_eq!(text.lines().last(), Some("unaffected"));
    assert_eq!(out.report.redaction_flat_fallback, 3);
    assert_eq!(out.report.redaction_fallback_placeholders, 2);
}

#[test]
fn decoded_context_is_not_lost_when_a_failed_walk_can_converge_flat() {
    let doc = serde_json::json!({"text": "REDACTED ssn=\n123456789"}).to_string();
    let normal = text_of(&sealed(&doc, &RulePackSet::builtin()));
    assert!(
        !normal.contains("123456789"),
        "normal decoded control: {normal}"
    );
    let out = sealed(&doc, &oscillating_at_the_start());
    let text = text_of(&out);
    assert!(
        !text.contains("123456789"),
        "decoded protection lost: {text}"
    );
    assert_eq!(text.lines().count(), 1);
}

#[test]
fn decoded_context_of_a_failed_pretty_document_cannot_be_split_away() {
    let value = serde_json::json!({"text": "REDACTED ssn=\n123456789"});
    let doc = format!("{}\n", serde_json::to_string_pretty(&value).unwrap());
    let normal = text_of(&sealed(&doc, &RulePackSet::builtin()));
    assert!(!normal.contains("123456789"));
    let out = sealed(&doc, &oscillating_at_the_start());
    let text = text_of(&out);
    assert!(
        !text.contains("123456789"),
        "pretty decoded protection lost: {text}"
    );
    assert_eq!(text.split('\n').count(), doc.split('\n').count());
    let nonempty =
        u32::try_from(doc.lines().filter(|line| !line.trim().is_empty()).count()).unwrap();
    assert_eq!(out.report.redaction_flat_fallback, nonempty);
    assert_eq!(out.report.redaction_fallback_placeholders, nonempty);
    assert!(text.ends_with('\n'));
}

/// The acceptance fixture: one line of three cannot converge under any path.
/// The transcript is still sealed, that line is the placeholder record naming
/// its line number, the others are byte-identical to the normal path, and the
/// count is one.
#[test]
fn a_line_that_cannot_converge_is_replaced_and_its_neighbours_are_untouched() {
    let packs = oscillating();
    let first = r#"{"type":"user","text":"mail alice@example.com please"}"#;
    let stuck = r#"{"type":"assistant","text":"start WEDGE end"}"#;
    let last = r#"{"type":"user","text":"thanks"}"#;
    let out = sealed(&format!("{first}\n{stuck}\n{last}"), &packs);
    let text = text_of(&out);
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert_eq!(lines[0], text_of(&sealed(first, &packs)));
    assert_eq!(lines[2], last);
    assert!(is_fallback_placeholder(lines[1]), "{}", lines[1]);
    assert_eq!(
        lines[1],
        r#"{"line":2,"reason":"did_not_converge","rift_redaction":"placeholder"}"#
    );
    assert!(!text.contains("start"), "the stuck line's bytes are gone");
    assert_eq!(out.report.redaction_flat_fallback, 1);
    assert_eq!(out.report.redaction_fallback_placeholders, 1);
}

/// Raw JSON convergence cannot certify decoded leaves after their walk fails.
/// The former flat-output assertion allowed unprovable decoded coverage; close
/// that record instead, retaining both independently safe neighbours.
#[test]
fn a_failed_decoded_walk_closes_its_record_even_if_raw_json_converges() {
    let packs = oscillating_at_the_start();
    let line = r#"{"text":"REDACTED notes","to":"alice@example.com"}"#;
    let ok = r#"{"type":"user","text":"thanks"}"#;
    let out = sealed(&format!("{ok}\n{line}\n{ok}"), &packs);
    let text = text_of(&out);
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert_eq!(lines[0], ok);
    assert_eq!(lines[2], ok);
    assert_eq!(lines[1], crate::fallback::placeholder_record(2));
    assert_eq!(out.report.redaction_flat_fallback, 1);
    assert_eq!(out.report.redaction_fallback_placeholders, 1);
}

/// A run of flat lines that cannot converge together keeps the lines that
/// converge alone, redacted together so a detector reading across lines still
/// sees them, and replaces only the one that cannot.
#[test]
fn a_stuck_line_in_a_flat_run_costs_only_itself() {
    let packs = oscillating();
    let doc = "plain one alice@example.com\nplain two\nstart WEDGE end\nplain four";
    let out = sealed(doc, &packs);
    let text = text_of(&out);
    let lines: Vec<&str> = text.split('\n').collect();
    assert_eq!(lines.len(), 4, "{text}");
    assert_eq!(lines[0], "plain one [REDACTED:pii.email]");
    assert_eq!(lines[1], "plain two");
    assert!(is_fallback_placeholder(lines[2]), "{}", lines[2]);
    assert!(lines[2].contains(r#""line":3"#), "{}", lines[2]);
    assert_eq!(lines[3], "plain four");
    assert_eq!(out.report.redaction_flat_fallback, 4);
    assert_eq!(out.report.redaction_fallback_placeholders, 1);
}

/// Lines that settle one at a time but wedge once joined (the rule reads
/// across the line break) are replaced together: no sub-run of them is
/// sealed, so nothing the joined text would have masked reaches the output.
#[test]
fn lines_that_settle_apart_but_wedge_together_are_all_replaced() {
    let packs = RulePackSet::builtin()
        .with_vendor_pattern("osc_join", "WEDGE\nX|REDACTED:vendor", "acme@1")
        .unwrap();
    let out = sealed("a WEDGE\nX b", &packs);
    let text = text_of(&out);
    assert_eq!(count_fallback_placeholders(text.as_bytes()), 2, "{text}");
    assert_eq!(out.report.redaction_flat_fallback, 2);
    assert_eq!(out.report.redaction_fallback_placeholders, 2);
}

/// The recount reads only whole records, wherever they sit in a document.
#[test]
fn the_recount_counts_whole_records_only() {
    let record = r#"{"line":3,"reason":"did_not_converge","rift_redaction":"placeholder"}"#;
    let doc = format!("x\n{record}\n {record}\n{record}");
    assert_eq!(count_fallback_placeholders(doc.as_bytes()), 2);
    assert_eq!(count_fallback_placeholders(b""), 0);
}

/// A failed whole-value walk cannot be certified by raw JSON field lines.
/// Each nonempty physical line becomes one record; the trailing slot remains.
#[test]
fn a_whole_document_that_cannot_be_walked_counts_every_line() {
    let packs = oscillating_at_the_start();
    let doc = "{\n  \"text\": \"REDACTED x\",\n  \"to\": \"alice@example.com\"\n}\n";
    let out = sealed(doc, &packs);
    let text = text_of(&out);
    assert!(!text.contains("alice@example.com"), "{text}");
    assert_eq!(text.split('\n').count(), doc.split('\n').count());
    assert_eq!(out.report.redaction_flat_fallback, 4);
    assert_eq!(out.report.redaction_fallback_placeholders, 4);
}

/// The normal path never touches the counters, and a counter at zero is not
/// serialized, so every report written before this fallback reads the same.
#[test]
fn a_converging_document_counts_nothing_and_serializes_as_before() {
    let out = sealed(
        r#"{"text":"mail alice@example.com"}"#,
        &RulePackSet::builtin(),
    );
    assert_eq!(out.report.redaction_flat_fallback, 0);
    assert_eq!(out.report.redaction_fallback_placeholders, 0);
    let json = serde_json::to_string(&out.report).unwrap();
    assert!(!json.contains("fallback"), "{json}");
}

/// The recogniser is exact: only the record the gate writes is one, so a
/// transcript line that merely mentions the words is not counted.
#[test]
fn only_the_exact_placeholder_record_is_recognised() {
    let record = r#"{"line":7,"reason":"did_not_converge","rift_redaction":"placeholder"}"#;
    assert!(is_fallback_placeholder(record));
    for other in [
        r#"{"line":7,"reason":"other","rift_redaction":"placeholder"}"#,
        r#"{"line":"7","reason":"did_not_converge","rift_redaction":"placeholder"}"#,
        r#"{"line":7,"reason":"did_not_converge","rift_redaction":"placeholder","x":1}"#,
        r#" {"line":7,"reason":"did_not_converge","rift_redaction":"placeholder"}"#,
        "did_not_converge placeholder",
        "",
    ] {
        assert!(!is_fallback_placeholder(other), "{other}");
    }
}

/// Lines a generated document is built from: ordinary JSONL records, records
/// and plain lines carrying a seeded secret, and lines that wedge either pack.
const PIECES: &[&str] = &[
    r#"{"type":"user","text":"hello"}"#,
    r#"{"type":"user","text":"key AKIAIOSFODNN7EXAMPLE here"}"#,
    r#"{"to":"alice@example.com","text":"REDACTED first"}"#,
    "plain text with alice@example.com inside",
    "start WEDGE end",
    r#"{"text":"WEDGE","to":"alice@example.com"}"#,
    r#"{"text":"REDACTED and AKIAIOSFODNN7EXAMPLE"}"#,
    "REDACTED at the start AKIAIOSFODNN7EXAMPLE",
    "",
    "not json {",
];

const SEEDED: &[&str] = &["AKIAIOSFODNN7EXAMPLE", "alice@example.com"];

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(256))]

    /// For any document built from those lines and under either wedge, no
    /// seeded secret reaches the sealed bytes and the line count is the
    /// input's: the fallback over-redacts and drops nothing.
    #[test]
    fn no_seeded_secret_survives_and_every_line_is_kept(
        picks in proptest::collection::vec(0usize..PIECES.len(), 1..12),
        anchored in proptest::bool::ANY,
    ) {
        let packs = if anchored { oscillating_at_the_start() } else { oscillating() };
        let doc: Vec<&str> = picks.iter().map(|i| PIECES[*i]).collect();
        let doc = doc.join("\n");
        let out = sealed(&doc, &packs);
        let text = text_of(&out);
        for secret in SEEDED {
            proptest::prop_assert!(!text.contains(secret), "{} in {}", secret, text);
        }
        proptest::prop_assert_eq!(text.split('\n').count(), doc.split('\n').count());
        proptest::prop_assert!(
            out.report.redaction_fallback_placeholders <= out.report.redaction_flat_fallback
        );
    }
}

#[test]
fn a_failed_json_record_does_not_cut_an_enclosing_key_block() {
    let doc = "safe before\n-----BEGIN RSA PRIVATE KEY-----\nABCDEF0123\n{\"text\":\"REDACTED notes\"}\nGHIJKL4567\n-----END RSA PRIVATE KEY-----\nsafe after";
    // The built-in PEM grammar accepts base64 body lines, not a JSON record.
    // An additive organization detector can protect an arbitrary block body.
    let block = r"(?s)-----BEGIN RSA PRIVATE KEY-----.*?-----END RSA PRIVATE KEY-----";
    let normal = RulePackSet::builtin()
        .with_vendor_pattern("block", block, "acme@1")
        .unwrap();
    let control = crate::redact_fragment(doc, &normal, &mut crate::jsonl::blank_report()).unwrap();
    assert!(!control.contains("ABCDEF0123"), "{control}");
    let packs = oscillating_at_the_start()
        .with_vendor_pattern("block", block, "acme@1")
        .unwrap();
    let out = sealed(doc, &packs);
    let text = text_of(&out);
    assert!(
        !text.contains("ABCDEF0123"),
        "record cut key context: {text}"
    );
    assert!(
        !text.contains("GHIJKL4567"),
        "record cut key context: {text}"
    );
    assert_eq!(text.lines().count(), 7);
    assert_eq!(text.lines().next(), Some("safe before"));
    assert_eq!(text.lines().last(), Some("safe after"));
    assert_eq!(out.report.redaction_fallback_placeholders, 5);
}

#[test]
fn collapsed_newline_endpoints_keep_safe_blank_and_trailing_slots() {
    let packs = oscillating()
        .with_vendor_pattern("joins", r"JOIN\n|LEFT\nMIDDLE\nRIGHT", "acme@1")
        .unwrap();
    let doc = "WEDGE\nJOIN\nneighbor\nLEFT\nMIDDLE\nRIGHT\n\n  \nuntouched\n";
    let out = sealed(doc, &packs);
    let text = text_of(&out);
    let lines: Vec<_> = text.split('\n').collect();
    assert_eq!(lines.len(), doc.split('\n').count());
    assert!(lines[..6].iter().all(|line| is_fallback_placeholder(line)));
    assert_eq!(&lines[6..], &["", "  ", "untouched", ""]);
    assert_eq!(out.report.redaction_fallback_placeholders, 6);
    assert_eq!(count_fallback_placeholders(text.as_bytes()), 6);
}

#[test]
fn dense_independent_spans_do_not_force_their_safe_line() {
    let packs = oscillating();
    let dense = vec!["alice@example.com"; 64].join(" ");
    let expected = text_of(&sealed(&dense, &packs));
    let doc = format!("WEDGE\n{dense}\nsafe\n");
    let out = sealed(&doc, &packs);
    let text = text_of(&out);
    assert_eq!(text.split('\n').nth(1), Some(expected.as_str()));
    assert_eq!(out.report.redaction_fallback_placeholders, 1);
    assert_eq!(
        out.report.total_spans, 64,
        "failed probes cannot inflate actual masks"
    );
}

#[test]
fn allowlisted_context_is_not_promoted_to_forced_protection() {
    let packs = oscillating().with_allowlisted_value("123456789");
    let doc = "WEDGE ssn=\n123456789\nsafe";
    let out = sealed(doc, &packs);
    let text = text_of(&out);
    assert_eq!(text.lines().nth(1), Some("123456789"));
    assert_eq!(out.report.redaction_fallback_placeholders, 1);
}

#[test]
fn a_vendor_can_transform_a_fallback_record_on_a_safe_second_seal() {
    let packs = oscillating_at_the_start()
        .with_vendor_pattern("record_line", r#""line":[0-9]+"#, "acme@1")
        .unwrap();
    let doc = serde_json::json!({"text":"REDACTED ssn=\n123456789"}).to_string();
    let first = sealed(&doc, &packs);
    let first_text = text_of(&first);
    assert!(is_fallback_placeholder(&first_text));
    let second = sealed(&first_text, &packs);
    let second_text = text_of(&second);
    assert!(!second_text.contains("123456789"));
    assert_ne!(second_text, first_text);
    assert_eq!(second_text.lines().count(), 1);
    assert_eq!(second.report.redaction_flat_fallback, 0);
}

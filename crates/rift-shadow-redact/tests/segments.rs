//! The segment guard: a transcript sealed one turn at a time must seal exactly
//! as the whole transcript would.
//!
//! L-5 seals a growing transcript as per-turn segments, each redacted alone,
//! and the record's leaf is the hash of their concatenation. That is only
//! honest if the gate cannot tell the difference, so the rule this file holds
//! is: at every cut the gate admits ([`cut_is_safe`]), the concatenation of the
//! segments' sealed bytes is the whole document's sealed bytes, byte for byte,
//! and every counter of the report sums. At a cut it refuses, extending to the
//! next admitted cut (or the end of the file) always restores that equality, so
//! a secret split across two turns inside a flat run is never judged in halves.
//! Round 30 Q2 orders this guard to land before any segment is written.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::collections::BTreeMap;

use proptest::prelude::*;
use rift_core::SecretBytes;
use rift_shadow_redact::{
    count_fallback_placeholders, cut_is_safe, redact_with, redact_with_at, RedactionOutput,
    RedactionReport, RulePackSet,
};

const PEM_LINES: [&str; 4] = [
    "MIIBOgIBAAJBAKj34GkxFhD90vcNLYLInFEX6Ppy1tPf9Cnzj4p4WGeKLs1Pt8Qu",
    "KUpRKfFLfRYC9AIKjbJTWit+CqvjWYzvQwECAwEAAQJAIJLixBy2qpFoS4DSmoEm",
    "o3qGy0t6z09AIJtH+5OeRV1be+N4cDYJKffGzDa88vQENZiRm0GRq6a+HPGQMd2k",
    "TQIhAKMSvzIBnni7ot/OSie2TmJLY4SwTQAevXysE2RbFDYdAiEBCUEaRQnMnbp7",
];
const ALLOWED: &str = "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// The rule-pack sets the property ranges over: the built-in pack, a rule that
/// wedges any line carrying `WEDGE` (a flat line that never settles), a rule
/// that makes a walk diverge on a leaf starting `REDACTED` (a `Diverged` JSON
/// line), and an allowlisted value present in the documents.
fn packs(which: u8) -> RulePackSet {
    let builtin = RulePackSet::builtin();
    match which % 4 {
        0 => builtin,
        1 => builtin
            .with_vendor_pattern("osc", "WEDGE|REDACTED:vendor", "acme@1")
            .unwrap(),
        2 => builtin
            .with_vendor_pattern("osc_start", r"^\[?REDACTED", "acme@1")
            .unwrap(),
        _ => builtin.with_allowlisted_value(ALLOWED),
    }
}

fn leaf() -> impl Strategy<Value = (String, String)> {
    prop_oneof![
        Just(("text", "the build is green again".to_string())),
        Just(("aws", "AKIAQ3EGRX5HZ7VJ2KLM".to_string())),
        Just(("gh", "ghp_R8x2Qm4Lp7Zt1Wn6Vb3Kc9Jd5Hf0Gs2Ay4Eu".to_string())),
        Just((
            "jwt",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1rwW1gFWFOEjXk"
                .to_string()
        )),
        Just((
            "pem",
            format!(
                "-----BEGIN RSA PRIVATE KEY-----\n{}\n-----END RSA PRIVATE KEY-----",
                PEM_LINES[0]
            )
        )),
        Just(("ssn", "123456789".to_string())),
        Just(("tok", "Zx9qL2vB8nR4tY7wK1mP3sD6fG0hJ5cXq8Wv".to_string())),
        Just(("uni", "caf\u{e9} pass\u{200B}word \u{FEFF}here".to_string())),
        Just(("allowed", ALLOWED.to_string())),
        Just(("wedge", "a WEDGE in a leaf".to_string())),
        Just(("start", "REDACTED at the start".to_string())),
    ]
    .prop_map(|(k, v)| (k.to_string(), v))
}

fn record() -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(leaf(), 1..=4).prop_map(|leaves| {
        let map: serde_json::Map<String, serde_json::Value> = leaves
            .into_iter()
            .enumerate()
            .map(|(i, (k, v))| (format!("{k}{i}"), serde_json::Value::String(v)))
            .collect();
        vec![serde_json::Value::Object(map).to_string()]
    })
}

fn flat() -> impl Strategy<Value = Vec<String>> {
    prop_oneof![
        Just(vec!["plain prose about the build".to_string()]),
        Just(vec!["ssn=".to_string(), "123456789".to_string()]),
        (3usize..=6).prop_map(|n| {
            let mut lines = vec!["-----BEGIN RSA PRIVATE KEY-----".to_string()];
            lines.extend(
                PEM_LINES
                    .iter()
                    .cycle()
                    .take(n - 2)
                    .map(ToString::to_string),
            );
            lines.push("-----END RSA PRIVATE KEY-----".to_string());
            lines
        }),
        Just(vec!["42".to_string()]),
        Just(vec!["a WEDGE line of prose".to_string()]),
        Just(vec![String::new()]),
    ]
}

fn item() -> impl Strategy<Value = Vec<String>> {
    prop_oneof![3 => record(), 2 => flat()]
}

/// A document of 1–40 lines and whether it ends in a newline.
fn document() -> impl Strategy<Value = (Vec<String>, bool)> {
    (proptest::collection::vec(item(), 1..=20), any::<bool>()).prop_map(|(items, trailing)| {
        let mut lines: Vec<String> = items.into_iter().flatten().collect();
        lines.truncate(40);
        (lines, trailing)
    })
}

fn seal_whole(text: &str, packs: &RulePackSet) -> RedactionOutput {
    redact_with(&SecretBytes::from(text), packs).unwrap()
}

/// The two segments of a document cut before line `k`: each line keeps its
/// `\n`, so the segments concatenate back to the document byte for byte.
fn split(lines: &[String], trailing: bool, k: usize) -> (String, String) {
    let first = format!("{}\n", lines[..k].join("\n"));
    let mut second = lines[k..].join("\n");
    if trailing {
        second.push('\n');
    }
    (first, second)
}

fn whole_text(lines: &[String], trailing: bool) -> String {
    let mut text = lines.join("\n");
    if trailing {
        text.push('\n');
    }
    text
}

fn sum_maps<K: Ord + Clone>(a: &BTreeMap<K, u32>, b: &BTreeMap<K, u32>) -> BTreeMap<K, u32> {
    let mut out = a.clone();
    for (k, v) in b {
        *out.entry(k.clone()).or_insert(0) += v;
    }
    out
}

/// Every counter of `whole` is the sum of the two parts'; pack versions equal.
fn assert_counts_sum(whole: &RedactionReport, a: &RedactionReport, b: &RedactionReport) {
    assert_eq!(whole.total_spans, a.total_spans + b.total_spans);
    assert_eq!(whole.bytes_masked, a.bytes_masked + b.bytes_masked);
    assert_eq!(whole.allowlisted, a.allowlisted + b.allowlisted);
    assert_eq!(whole.by_detector, sum_maps(&a.by_detector, &b.by_detector));
    assert_eq!(whole.by_class, sum_maps(&a.by_class, &b.by_class));
    assert_eq!(
        whole.redaction_flat_fallback,
        a.redaction_flat_fallback + b.redaction_flat_fallback
    );
    assert_eq!(
        whole.redaction_fallback_placeholders,
        a.redaction_fallback_placeholders + b.redaction_fallback_placeholders
    );
    assert_eq!(whole.rule_pack_versions, a.rule_pack_versions);
    assert_eq!(whole.rule_pack_versions, b.rule_pack_versions);
}

/// Seal the document whole and as two segments cut before line `k`, and hold
/// them equal; returns the concatenated segment output.
fn assert_segments_match(
    lines: &[String],
    trailing: bool,
    k: usize,
    packs: &RulePackSet,
) -> Vec<u8> {
    let whole = seal_whole(&whole_text(lines, trailing), packs);
    let (first, second) = split(lines, trailing, k);
    let a = redact_with_at(&SecretBytes::from(first.as_str()), packs, 0).unwrap();
    let b = redact_with_at(&SecretBytes::from(second.as_str()), packs, k).unwrap();
    let mut joined = a.redacted.as_slice().to_vec();
    joined.extend_from_slice(b.redacted.as_slice());
    assert_eq!(
        String::from_utf8_lossy(&joined),
        String::from_utf8_lossy(whole.redacted.as_slice()),
        "cut before line {k}"
    );
    assert_counts_sum(&whole.report, &a.report, &b.report);
    assert_eq!(
        count_fallback_placeholders(&joined),
        count_fallback_placeholders(whole.redacted.as_slice())
    );
    joined
}

/// The first cut at or after `k` the gate admits, or `None` when the flat run
/// reaches the end of the file and the turn seals up to its last structural line.
fn extended_cut(lines: &[String], k: usize, packs: &RulePackSet) -> Option<usize> {
    (k..lines.len()).find(|&j| cut_is_safe(&lines[j - 1], &lines[j], packs))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    #[test]
    fn a_segmented_seal_equals_the_whole_seal_at_every_admitted_cut(
        (lines, trailing) in document(),
        which in 0u8..4,
        at in any::<proptest::sample::Index>(),
    ) {
        prop_assume!(lines.len() >= 2);
        let packs = packs(which);
        let k = 1 + at.index(lines.len() - 1);
        if let Some(j) = extended_cut(&lines, k, &packs) {
            let joined = assert_segments_match(&lines, trailing, j, &packs);
            if which == 0 {
                let again = seal_whole(&String::from_utf8_lossy(&joined), &packs);
                prop_assert_eq!(again.redacted.as_slice(), joined.as_slice());
            }
        }
    }
}

fn lines_of(text: &[&str]) -> Vec<String> {
    text.iter().map(ToString::to_string).collect()
}

#[test]
fn a_stop_between_two_records_is_an_admitted_cut() {
    let lines = lines_of(&[
        r#"{"type":"user","message":"key AKIAQ3EGRX5HZ7VJ2KLM please"}"#,
        r#"{"type":"assistant","message":"done"}"#,
    ]);
    let packs = RulePackSet::builtin();
    assert!(cut_is_safe(&lines[0], &lines[1], &packs));
    let joined = assert_segments_match(&lines, true, 1, &packs);
    assert!(!String::from_utf8_lossy(&joined).contains("AKIAQ3EGRX5HZ7VJ2KLM"));
}

#[test]
fn a_cut_beside_a_diverged_record_is_refused() {
    let lines = lines_of(&[
        r#"{"a":"fine"}"#,
        r#"{"b":"REDACTED at the start"}"#,
        r#"{"c":"fine"}"#,
    ]);
    let packs = packs(2);
    assert!(!cut_is_safe(&lines[0], &lines[1], &packs));
    assert!(!cut_is_safe(&lines[1], &lines[2], &packs));
    assert_eq!(extended_cut(&lines, 1, &packs), None);
}

#[test]
fn a_private_key_pasted_across_the_cut_is_never_judged_in_halves() {
    let mut lines = lines_of(&[r#"{"type":"user"}"#, "-----BEGIN RSA PRIVATE KEY-----"]);
    lines.extend(lines_of(&PEM_LINES));
    lines.push("-----END RSA PRIVATE KEY-----".to_string());
    lines.push(r#"{"type":"assistant"}"#.to_string());
    lines.push(r#"{"type":"user"}"#.to_string());
    let packs = RulePackSet::builtin();
    // The cut just past the run is refused too: R1 asks both sides to be
    // records, so a run never ends a segment.
    for k in 2..lines.len() - 1 {
        assert!(!cut_is_safe(&lines[k - 1], &lines[k], &packs), "cut {k}");
    }
    let j = extended_cut(&lines, 3, &packs);
    assert_eq!(j, Some(lines.len() - 1));
    let joined = assert_segments_match(&lines, false, j.unwrap(), &packs);
    let text = String::from_utf8_lossy(&joined).into_owned();
    assert!(text.contains("[REDACTED:key]"), "{text}");
    assert!(PEM_LINES.iter().all(|body| !text.contains(body)), "{text}");
}

#[test]
fn a_labelled_identifier_on_the_next_line_keeps_its_label() {
    let lines = lines_of(&[
        r#"{"type":"user"}"#,
        "ssn=",
        "123456789",
        r#"{"z":1}"#,
        r#"{"z":2}"#,
    ]);
    let packs = RulePackSet::builtin();
    assert!(!cut_is_safe(&lines[1], &lines[2], &packs));
    let j = extended_cut(&lines, 2, &packs).unwrap();
    let joined = assert_segments_match(&lines, true, j, &packs);
    assert!(!String::from_utf8_lossy(&joined).contains("123456789"));
}

#[test]
fn a_placeholder_in_the_second_segment_carries_its_document_line() {
    let lines = lines_of(&[
        r#"{"a":"one"}"#,
        r#"{"b":"two"}"#,
        r#"{"c":"three"}"#,
        "WEDGE",
    ]);
    let packs = packs(1);
    assert!(cut_is_safe(&lines[1], &lines[2], &packs));
    let joined = assert_segments_match(&lines, false, 2, &packs);
    let text = String::from_utf8_lossy(&joined).into_owned();
    assert!(
        text.ends_with(r#"{"line":4,"reason":"did_not_converge","rift_redaction":"placeholder"}"#),
        "{text}"
    );
}

#[test]
fn a_blank_line_at_the_cut_is_refused() {
    let lines = lines_of(&[r#"{"a":1}"#, "", r#"{"b":2}"#]);
    let packs = RulePackSet::builtin();
    assert!(!cut_is_safe(&lines[0], &lines[1], &packs));
    assert!(!cut_is_safe(&lines[1], &lines[2], &packs));
}

#[test]
fn a_zero_width_character_is_judged_as_the_gate_normalises_it() {
    let lines = lines_of(&["\u{FEFF}{\"a\":\"x\u{200B}y\"}", r#"{"b":2}"#]);
    let packs = RulePackSet::builtin();
    assert!(cut_is_safe(&lines[0], &lines[1], &packs));
    assert_segments_match(&lines, true, 1, &packs);
}

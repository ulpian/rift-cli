//! Loaded and programmatic rule packs: additive only, never fatal on the
//! capture path, and bounded when a rule oscillates.

use rift_core::SecretBytes;

use super::fixtures::{masked, pack};
use crate::{redact_with, PackRule, PackSpec, PackWarning, RulePackSet};

#[test]
fn a_loaded_pack_adds_a_detector_and_names_itself() {
    let (packs, warnings) = RulePackSet::builtin().with_pack(&pack(
        "internal_token",
        r"ACME-[A-Z0-9]{8}",
        vec![("ACME-AB12CD34", true), ("acme-lowercase", false)],
    ));
    assert!(warnings.is_empty(), "{warnings:?}");
    let o = redact_with(&SecretBytes::from("secret ACME-AB12CD34 here"), &packs).unwrap();
    let m = String::from_utf8(o.redacted.as_slice().to_vec()).unwrap();
    assert!(!m.contains("ACME-AB12CD34"), "{m}");
    assert_eq!(o.report.by_detector.get("acme.internal_token"), Some(&1));
    assert!(o
        .report
        .rule_pack_versions
        .iter()
        .any(|v| v == "acme@2026.08.0"));
}

#[test]
fn a_pack_cannot_disable_a_builtin_detector() {
    let (packs, _) = RulePackSet::builtin().with_pack(&pack("noop", r"ZZZZ-NOTHING", vec![]));
    let o = redact_with(
        &SecretBytes::from("key AKIAIOSFODNN7EXAMPLE and alice@example.com"),
        &packs,
    )
    .unwrap();
    let m = String::from_utf8(o.redacted.as_slice().to_vec()).unwrap();
    assert!(!m.contains("AKIAIOSFODNN7EXAMPLE"), "{m}");
    assert!(!m.contains("alice@example.com"), "{m}");
}

/// An uncompilable pattern is one dropped rule, not a failed load: the rest
/// of the pipeline has to keep running on the capture path.
#[test]
fn an_uncompilable_rule_is_dropped_not_raised() {
    let mut spec = pack("good", r"ACME-[A-Z0-9]{8}", vec![]);
    spec.rules.push(PackRule {
        id: "bad".to_string(),
        regex: "([unclosed".to_string(),
        samples: Vec::new(),
    });
    let (packs, warnings) = RulePackSet::builtin().with_pack(&spec);
    assert!(
        matches!(
            warnings.as_slice(),
            [PackWarning::RegexRejected { rule, .. }] if rule == "acme.bad"
        ),
        "{warnings:?}"
    );
    let o = redact_with(&SecretBytes::from("ACME-AB12CD34"), &packs).unwrap();
    assert_eq!(o.report.by_detector.get("acme.good"), Some(&1));
}

/// A rule that disagrees with its own sample is kept — a wrong self-test is
/// not a reason to stop redacting — and the warning carries the sample's
/// length, never its text.
#[test]
fn a_failing_sample_warns_keeps_the_rule_and_leaks_nothing() {
    let secret = "ACME-AB12CD34";
    let (packs, warnings) = RulePackSet::builtin().with_pack(&pack(
        "internal_token",
        r"ACME-[A-Z0-9]{8}",
        vec![(secret, false)],
    ));
    let rendered = format!("{warnings:?}");
    assert!(rendered.contains("SampleMismatch"), "{rendered}");
    assert!(
        !rendered.contains(secret),
        "a warning quoted a sample: {rendered}"
    );
    assert!(rendered.contains(&secret.len().to_string()), "{rendered}");
    let o = redact_with(&SecretBytes::from("ACME-AB12CD34"), &packs).unwrap();
    assert_eq!(
        o.report.by_detector.get("acme.internal_token"),
        Some(&1),
        "the rule must survive its own bad sample"
    );
}

/// A pattern that matches the empty string matches at every position: it
/// seals nothing and would rewrite the text into placeholders, then do it
/// again to its own output. Dropped with a warning, like any other rule.
#[test]
fn a_rule_matching_the_empty_string_is_dropped() {
    let (packs, warnings) = RulePackSet::builtin().with_pack(&pack("greedy", r"X*", vec![]));
    assert!(
        matches!(
            warnings.as_slice(),
            [PackWarning::MatchesEmptyString { rule }] if rule == "acme.greedy"
        ),
        "{warnings:?}"
    );
    let o = redact_with(&SecretBytes::from("plain text"), &packs).unwrap();
    assert_eq!(
        String::from_utf8(o.redacted.as_slice().to_vec()).unwrap(),
        "plain text"
    );
}

/// A pack rule may not take a built-in detector's id: a count in
/// `by_detector` has to name exactly one detector. Both halves of
/// `is_builtin_detector_id` are exercised — a shape-table id and the entropy
/// layer's, which has no `ShapeDetector` to be found in.
#[test]
fn a_rule_may_not_take_a_builtin_detector_id() {
    for (name, id) in [("vendor", "aws.access_key_id"), ("entropy", "high")] {
        let spec = PackSpec {
            name: name.to_string(),
            version: "1".to_string(),
            rules: vec![PackRule {
                id: id.to_string(),
                regex: r"ZZZZ".to_string(),
                samples: Vec::new(),
            }],
        };
        let (_, warnings) = RulePackSet::builtin().with_pack(&spec);
        assert!(
            matches!(
                warnings.as_slice(),
                [PackWarning::IdCollidesWithBuiltin { .. }]
            ),
            "{name}.{id}: {warnings:?}"
        );
    }
}

/// A pack rule is a detector like any other, so the JSON walker runs it on
/// every leaf and every key — there is no field a key name can put beyond
/// its reach.
#[test]
fn pack_rules_run_on_json_leaves_and_keys() {
    let (packs, _) =
        RulePackSet::builtin().with_pack(&pack("internal_token", r"ACME-[A-Z0-9]{8}", vec![]));
    let line = r#"{"signature":"ACME-AB12CD34","ACME-EF56GH78":"v"}"#;
    let o = redact_with(&SecretBytes::from(line), &packs).unwrap();
    let m = String::from_utf8(o.redacted.as_slice().to_vec()).unwrap();
    assert!(!m.contains("ACME-AB12CD34"), "{m}");
    assert!(!m.contains("ACME-EF56GH78"), "{m}");
    assert_eq!(o.report.by_detector.get("acme.internal_token"), Some(&2));
}

#[test]
fn org_pack_adds_detector() {
    let packs = RulePackSet::builtin()
        .with_vendor_pattern(
            "acme.internal",
            r"ACME-[A-Z0-9]{8}",
            "acme-internal@2026.06.0",
        )
        .unwrap();
    let o = redact_with(&SecretBytes::from("secret ACME-AB12CD34 here"), &packs).unwrap();
    let m = String::from_utf8(o.redacted.as_slice().to_vec()).unwrap();
    assert!(!m.contains("ACME-AB12CD34"));
    assert!(o
        .report
        .rule_pack_versions
        .iter()
        .any(|v| v.contains("acme")));
    // without the pack, the custom token is NOT redacted
    let plain = masked("secret ACME-AB12CD34 here");
    assert!(plain.contains("ACME-AB12CD34"));
}

/// A rule that matches the empty string matches at *every* position, so
/// honouring its matches would rewrite the text into placeholders and
/// re-grow its own output on the next pass. The constructor refuses the
/// ones it can see; the span builder is the guarantee, because a pattern can
/// match empty only *in context* and still fail `is_match("")`.
#[test]
fn a_pattern_that_matches_nothing_seals_nothing() {
    assert!(RulePackSet::builtin()
        .with_vendor_pattern("bomb", r"x*", "acme@1")
        .is_err());
    let packs = RulePackSet::builtin()
        .with_vendor_pattern("edge", r"\bZ*", "acme@1")
        .unwrap();
    let o = redact_with(&SecretBytes::from("plain words here"), &packs).unwrap();
    let m = String::from_utf8(o.redacted.as_slice().to_vec()).unwrap();
    assert_eq!(m, "plain words here");
}

/// The module doc says a hostile input cannot wedge the pipeline. That rests
/// fully on [`MAX_PASSES`], whose failure branch nothing exercised — so
/// the sentence was a claim no test could falsify.
///
/// A rule matching a *substring* of a placeholder is the wedge: masking
/// `REDACTED` writes `[REDACTED:vendor]`, which contains `REDACTED`, so the
/// next pass masks it again and the text grows without bound. The pipeline
/// stops after eight passes, and what it then seals is the line's
/// placeholder record — never partially redacted bytes, since a partial
/// result here is a leak and a loop is a hang, and never an error, since an
/// error loses the whole document the line sat in.
#[test]
fn an_oscillating_rule_is_bounded_and_seals_a_placeholder() {
    let packs = RulePackSet::builtin()
        .with_vendor_pattern("osc", "REDACTED", "acme@1")
        .unwrap();
    let out = redact_with(&SecretBytes::from("start REDACTED end"), &packs).unwrap();
    assert_eq!(
        out.redacted.as_slice(),
        br#"{"line":1,"reason":"did_not_converge","rift_redaction":"placeholder"}"#
    );
    assert_eq!(out.report.redaction_fallback_placeholders, 1);
    // and the ordinary case still converges through the same loop
    let ok = redact_with(&SecretBytes::from("start plain end"), &packs).unwrap();
    assert_eq!(ok.redacted.as_slice(), b"start plain end");
    assert_eq!(ok.report.redaction_flat_fallback, 0);
}

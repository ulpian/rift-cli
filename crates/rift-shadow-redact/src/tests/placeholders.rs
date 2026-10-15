//! The placeholder table by value: one label per class and per PII detector,
//! all delimited the same way, each a fixpoint of the pipeline that emits it,
//! and never counted twice by `mask`.

use super::fixtures::{masked_with, pack};
use crate::marks::mask;
use crate::span::{MatchedAgainst, Span};
use crate::{pii_placeholders, DetectorClass, RulePackSet, PLACEHOLDER_CLOSE, PLACEHOLDER_OPEN};

/// Every placeholder this crate can emit, longest first.
fn every_placeholder() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = [
        DetectorClass::VendorSecret,
        DetectorClass::CredentialedUri,
        DetectorClass::JdbcDsn,
        DetectorClass::PrivateKey,
        DetectorClass::Entropy,
        DetectorClass::Pii,
    ]
    .into_iter()
    .map(DetectorClass::placeholder)
    .chain(pii_placeholders().values().map(String::as_str))
    .collect();
    all.sort_by_key(|p| std::cmp::Reverse(p.len()));
    all
}

/// Where in `text` the pipeline's own placeholders sit.
///
/// **A survival check by substring search cannot tell a surviving document
/// byte from a byte the pipeline itself wrote**, and with per-detector PII
/// labels that is
/// no longer hypothetical: the PII placeholders spell detector ids, and
/// those ids are made of the same words the detectors match *on*. Enumerated
/// rather than argued — every six-byte window of the label vocabulary that
/// the placeholder alphabet also spells:
///
/// | label | windows | placeholder that spells them |
/// |---|---|---|
/// | `taxpayer_id` | `taxpay` `axpaye` `xpayer` `payer_` `ayer_i` `yer_id` | `[REDACTED:pii.us_taxpayer_id]`, `…_labelled` |
/// | `national_id` | `nation` `ationa` `tional` | `[REDACTED:pii.phone_uk_national]` |
/// | `street` | `street` | `[REDACTED:pii.street_address]` |
///
/// The suite found this itself rather than being told: on the placeholder
/// commit `nothing_survives_here_that_dropping_the_loser_masked` reddened at
/// `taxpayer_id=001-05-2034.AAAAAAAA` on the window `taxpay`, with nothing
/// whatever surviving.
///
/// **Positions rather than a sentinel, and that is a correction made by
/// measurement.** The first fix here blanked each placeholder to a NUL and
/// asserted no generator could spell one; the assertion reddened on its
/// first run, on the document `"\0"`. Ranges need no such assumption:
/// [`contains_run_outside`] accepts a match only where it does not touch
/// one, so every window stays evidence and none of the haystack is rewritten.
pub(super) fn placeholder_ranges(text: &[u8]) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for p in every_placeholder() {
        let pb = p.as_bytes();
        let mut i = 0;
        while i + pb.len() <= text.len() {
            if &text[i..i + pb.len()] == pb {
                ranges.push((i, i + pb.len()));
                i += pb.len();
            } else {
                i += 1;
            }
        }
    }
    ranges
}

/// `contains_run`, counting only matches that lie outside every range in
/// `ranges` — see [`placeholder_ranges`] for why a plain search stopped
/// meaning what it says.
pub(super) fn contains_run_outside(
    haystack: &[u8],
    ranges: &[(usize, usize)],
    needle: &[u8],
) -> bool {
    haystack
        .windows(needle.len())
        .enumerate()
        .any(|(i, w)| w == needle && !ranges.iter().any(|(s, e)| i < *e && i + needle.len() > *s))
}

/// **`mask` by value.** The differential reference calls this function, so
/// `the_reference_pipeline_is_still_the_one_main_runs` listing it as pinned
/// is a claim, and until this test existed the claim was false — that doc
/// asserted nothing whatever about `mask`, in the direction that makes the
/// guard look stronger than it is. Pinned: the placeholder table, the
/// splice, and the rule that re-masking an already-placed placeholder is not
/// a new redaction — the last of those now in its **family** form, which is
/// what per-detector labels forced. The probe therefore carries two re-match arms and
/// not one: the same detector meeting its own placeholder (the case that
/// existed) and a *different* PII detector meeting a sibling's (the case
/// that only exists once the label names the detector). Without the family
/// check the second arm relabels — `[REDACTED:pii.email]` leaves as
/// `[REDACTED:pii.street_address]` and is counted a second time — so the
/// output of the pipeline would depend on which detector happened to fire
/// last, and `redact(redact(x)) == redact(x)` would be false.
#[test]
fn mask_is_pinned_by_value_because_the_reference_calls_it() {
    assert_eq!(
        [
            DetectorClass::VendorSecret.placeholder(),
            DetectorClass::CredentialedUri.placeholder(),
            DetectorClass::JdbcDsn.placeholder(),
            DetectorClass::PrivateKey.placeholder(),
            DetectorClass::Entropy.placeholder(),
            DetectorClass::Pii.placeholder(),
        ],
        [
            "[REDACTED:vendor]",
            "[REDACTED:uri]",
            "[REDACTED:jdbc]",
            "[REDACTED:key]",
            "[REDACTED:entropy]",
            "[REDACTED:pii]",
        ],
        "a placeholder moved, so the two pipelines no longer emit the same bytes"
    );
    // The PII family, by value and in full: eleven detectors, eleven labels,
    // and the family root reachable by no detector at all. A twelfth
    // detector reddens here rather than shipping an unreviewed label.
    let family: Vec<&str> = pii_placeholders().values().map(String::as_str).collect();
    assert_eq!(
        family,
        [
            "[REDACTED:pii.card_pan]",
            "[REDACTED:pii.card_pan_grouped]",
            "[REDACTED:pii.email]",
            "[REDACTED:pii.ipv4]",
            "[REDACTED:pii.phone_e164]",
            "[REDACTED:pii.phone_intl_zero_prefix]",
            "[REDACTED:pii.phone_nanp]",
            "[REDACTED:pii.phone_uk_national]",
            "[REDACTED:pii.street_address]",
            "[REDACTED:pii.us_taxpayer_id]",
            "[REDACTED:pii.us_taxpayer_id_labelled]",
        ],
        "the PII placeholder family moved"
    );
    let mut report = crate::jsonl::blank_report();
    let span = |start: usize, end: usize, id: &str| Span {
        start,
        end,
        class: DetectorClass::Pii,
        id: id.to_string(),
        parse: None,
        matched: MatchedAgainst::Input,
    };
    // `a <email> b <the family root> c <a sibling's label> d`
    let probe = "a alice@example.com b [REDACTED:pii] c [REDACTED:pii.email] d";
    assert_eq!(
        mask(
            probe,
            &[
                span(2, 19, "pii.email"),
                span(22, 36, "pii.email"),
                span(39, 59, "pii.street_address"),
            ],
            &mut report
        ),
        "a [REDACTED:pii.email] b [REDACTED:pii] c [REDACTED:pii.email] d",
        "mask's splice moved"
    );
    assert_eq!(
        (report.total_spans, report.bytes_masked),
        (1, 17),
        "mask counted a re-masked placeholder as a new redaction"
    );
}

/// **Every placeholder is delimited the same way, and every placeholder is a
/// fixpoint of the pipeline that emits it — over this enumeration.**
///
/// Two properties, one test, because each is the other's precondition. The
/// delimiters are constants that [`is_known_placeholder`] takes a
/// placeholder apart with, so a class placeholder spelled any other way
/// would be re-masked forever instead of suppressed. And a placeholder whose
/// own text a detector matches is the same wedge from the other side — this
/// is not a shape argument: `pii.us_taxpayer_id_labelled` is a **23-byte
/// token** to the entropy layer, three bytes over [`ENTROPY_MIN_LEN`], and
/// clears the length floor. It survives only because it does not clear
/// [`ENTROPY_MIN_BITS`], which is a fact about that particular word and not
/// about the naming scheme, so the twelfth detector gets this test rather
/// than the reasoning.
///
/// **What "fixpoint" means here, exactly** (an earlier review found the old
/// docstring claiming more than two contexts): every placeholder in
/// [`every_placeholder`] plus a loaded pack's own emission, each in the ten
/// contexts enumerated in the body — bare, padded, doubled, on its own
/// lines, after `key=` and `password=`, as a bearer token, as a JSON
/// string value, as the userinfo of a URL, before a period — under both
/// the builtin pack set and one with a user pack loaded. That is the
/// claim; a context not in the list is not covered. A failing context is
/// a finding to fix, never a context to delete.
#[test]
fn every_placeholder_is_a_fixpoint_and_is_delimited_the_same_way() {
    let contexts: [fn(&str) -> String; 10] = [
        |p| p.to_owned(),
        |p| format!("x {p} y"),
        |p| format!("{p}{p}"),
        |p| format!("{p}\n{p}\n"),
        |p| format!("key={p}"),
        |p| format!("password={p}"),
        |p| format!("Authorization: Bearer {p}"),
        |p| format!("{{\"token\": \"{p}\"}}"),
        |p| format!("https://{p}@host/"),
        |p| format!("{p}."),
    ];
    let (with_user_pack, warnings) = RulePackSet::builtin().with_pack(&pack(
        "internal_token",
        r"ACME-[A-Z0-9]{8}",
        vec![("ACME-AB12CD34", true), ("acme-lowercase", false)],
    ));
    assert!(warnings.is_empty(), "{warnings:?}");
    // The pack's own emission, taken from the pipeline rather than spelled
    // here, so a renamed pack placeholder cannot leave this list stale.
    let emitted = masked_with("ACME-AB12CD34", &with_user_pack);
    assert!(
        emitted.starts_with(PLACEHOLDER_OPEN) && emitted.ends_with(PLACEHOLDER_CLOSE),
        "a pack rule's emission is delimited like every other placeholder: {emitted}"
    );
    let mut placeholders: Vec<String> =
        every_placeholder().into_iter().map(str::to_owned).collect();
    placeholders.push(emitted);

    let sets: [(&str, RulePackSet); 2] = [
        ("builtin", RulePackSet::builtin()),
        ("with a user pack", with_user_pack),
    ];
    for p in &placeholders {
        assert!(
            p.starts_with(PLACEHOLDER_OPEN) && p.ends_with(PLACEHOLDER_CLOSE),
            "{p} is not delimited like the family predicate takes it apart"
        );
        for (label, packs) in &sets {
            for context in &contexts {
                let text = context(p);
                assert_eq!(
                    masked_with(&text, packs),
                    text,
                    "the pipeline ({label}) re-masks its own placeholder in context: {text:?}"
                );
            }
        }
    }
}

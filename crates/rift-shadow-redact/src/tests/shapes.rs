//! The shape gates directly: they fail closed, never widen, trim where a
//! greedy match cannot, and the numeric ones fire on nothing in this
//! repository's own text.

use std::collections::BTreeSet;

use super::fixtures::{masked, visit_text_files, workspace_root};
use crate::builtin_tables::builtin_shape_tables;
use crate::entropy::entropy_spans;
use crate::marks::{is_short_benign_token, propose};
use crate::shapes::{
    is_card_pan, is_e164_phone, is_labelled_taxpayer_id, is_nanp_phone, is_us_taxpayer_id, luhn_ok,
    shape_spans, split_number, stands_alone,
};
use crate::span::{ShapeDetector, Span, SpanGate};
use crate::RulePackSet;

/// Every gate fails closed on an input it cannot read or was never shaped
/// for. A gate is a security decision, so "cannot tell" resolves to "not a
/// redaction" — never a panic, and never a mask on a guess. Reached directly
/// because the regexes make these shapes unreachable through `redact`, which
/// is exactly why the gates must not depend on them.
#[test]
fn a_gate_fails_closed_on_input_it_cannot_read() {
    let gates: [SpanGate; 5] = [
        is_us_taxpayer_id,
        is_labelled_taxpayer_id,
        is_e164_phone,
        is_nanp_phone,
        is_card_pan,
    ];
    for gate in gates {
        assert!(
            gate("123-45-6789", 0, 999).is_none(),
            "range past the end of the text"
        );
        assert!(gate("", 0, 0).is_none(), "empty text");
    }
    // shapes the regexes cannot produce, handed to the gates anyway
    assert!(
        is_us_taxpayer_id("12-34", 0, 5).is_none(),
        "two runs, not three"
    );
    assert!(
        is_us_taxpayer_id("99999999999-45-6789", 0, 19).is_none(),
        "a field too wide to be a field"
    );
    assert!(
        is_nanp_phone("415-555", 0, 7).is_none(),
        "two runs, not three"
    );
    assert!(
        is_nanp_phone("100-200-3000", 0, 12).is_none(),
        "area code starts with 1"
    );
    assert!(
        is_labelled_taxpayer_id("12345678", 0, 8).is_none(),
        "eight digits"
    );
    // UnionPay, the one issuer range no corpus row reaches
    assert!(!masked("card 6212345678901232").contains("6212345678901232"));
}

/// The one over-redaction the phone shapes carry, stated rather than
/// dropped: three columns of a numeric table whose widths are 3-3-4 and
/// whose first two start 2-9 are a valid NANP number written with spaces,
/// and nothing in the text says which it is. The legacy backend's bare branch masks the
/// same input. `N`XX still removes the sequential cases, and a first column
/// of any other width removes the rest.
#[test]
fn a_bare_numeric_triple_is_read_as_a_phone_number() {
    assert!(!masked("table 256 512 1024 end").contains("256 512 1024"));
    assert!(masked("table 100 200 3000 end").contains("100 200 3000"));
    assert!(masked("table 4096 512 1024 end").contains("4096 512 1024"));
}

/// The second over-redaction the phone shapes carry, and the reason it is
/// kept rather than narrowed. In a unified diff an added line begins with
/// `+` at column 0, so a line of digits reads as a country code and a
/// number — and Rift captures diffs and hunks, so this visibly costs a
/// numeric diff line.
///
/// Measured before deciding, over `git log -p` for 600 commits excluding
/// this crate's own sources (11 099 971 bytes, 261 067 lines, 174 396 added
/// lines): **one** `pii.phone_e164` span at column 0, inside a binary blob,
/// and **zero** added lines that are exactly 8-15 digits. A `+` at column 0
/// cannot be told from a country code by any rule — in a diff a real
/// `+44…` number appears as `++44…`, so the marker case is the one with a
/// single `+` — and refusing it would lose a bare international number at a
/// line start for a cost this repository does not pay. Kept, stated, pinned.
#[test]
fn a_diff_added_line_of_digits_is_read_as_a_phone_number() {
    for over in ["+9876543210", "+1700000000000", "version bump +1000000000"] {
        assert!(masked(over).contains("[REDACTED:pii.phone_e164]"), "{over}");
    }
    // The diff of a real `+`-prefixed number keeps its own `+`, so the
    // number is still detected at column 1.
    assert!(!masked("++44 20 7123 4567").contains("+44 20 7123 4567"));
    // Under the ceiling and over it: 8 digits is a number, 19 is not.
    assert!(masked("+12345678").contains("[REDACTED:pii.phone_e164]"));
    assert!(masked("PR +1234567890123456789").contains("+1234567890123456789"));
}

/// A gate may narrow the span it was given; it may never widen it. That is
/// the whole of what makes a gated detector safe to add, and it is checked
/// against every gate rather than argued from the type.
#[test]
fn a_gate_never_widens_the_span_it_was_given() {
    let gates: [SpanGate; 5] = [
        is_us_taxpayer_id,
        is_labelled_taxpayer_id,
        is_e164_phone,
        is_nanp_phone,
        is_card_pan,
    ];
    let texts = [
        "call +44 20 7123 4567 9999 and 123-45-6789 or 4111 1111 1111 1111",
        "SSN=123456789 (415) 555-0199 +1.415.555.0199",
        "",
        "+++++",
    ];
    for gate in gates {
        for t in texts {
            for start in 0..=t.len() {
                for end in start..=t.len() {
                    if !t.is_char_boundary(start) || !t.is_char_boundary(end) {
                        continue;
                    }
                    if let Some((s, e)) = gate(t, start, end) {
                        assert!(
                            start <= s && e <= end && s < e,
                            "gate widened {start}..{end} to {s}..{e} in {t:?}"
                        );
                    }
                }
            }
        }
    }
}

/// A greedy match cannot tell where one number ends and the next begins, so
/// four digits after a non-US number used to disarm the only detector that
/// covers it: the match ran to sixteen digits, failed E.164's ceiling, and
/// the phone number was published in full. The gate now trims to the last
/// group that fits and the neighbour survives beside the mask.
#[test]
fn trailing_digits_do_not_disarm_an_international_number() {
    let m = masked("call +44 20 7123 4567 9999");
    assert!(!m.contains("+44 20 7123 4567"), "{m}");
    assert!(m.ends_with("9999"), "neighbour must survive: {m}");
    for kept in ["call +44 20 7123 4567 tomorrow", "ring +44 20 7123 4567."] {
        assert!(!masked(kept).contains("+44 20 7123 4567"), "{kept}");
    }
}

/// `split_number`'s doc says a leading or trailing prefix is not a
/// separator, so `seps.len() + 1 == runs.len()` whenever there is a run at
/// all. Every numeric gate reads `runs`/`seps` positionally, so that is a
/// load-bearing sentence — and it was believed rather than checked.
#[test]
fn split_number_pairs_every_separator_with_a_following_run() {
    for s in [
        "",
        "-",
        "12",
        "-12",
        "12-",
        "-12-",
        "(415) 555-0199",
        "+1.415.555.0199",
        "1-415-555-0199",
        "4111 1111 1111 1111",
        "12-345-45-6789-01",
        "a1b2c3",
        "1\n2",
    ] {
        let (runs, seps) = split_number(s);
        if runs.is_empty() {
            assert!(seps.is_empty(), "{s:?} -> {runs:?} {seps:?}");
        } else {
            assert_eq!(seps.len() + 1, runs.len(), "{s:?} -> {runs:?} {seps:?}");
        }
    }
}

/// The Luhn check is the gate, not decoration: flipping one digit of a
/// published test number is the whole difference between masked and kept.
#[test]
fn luhn_is_the_gate_for_payment_cards() {
    assert!(!masked("card 4111111111111111").contains("4111111111111111"));
    assert!(masked("card 4111111111111112").contains("4111111111111112"));
    // the `\b` window claim: a card beside another number still matches at
    // its own length instead of swallowing the neighbour and failing on it
    let with_year = masked("card 4111 1111 1111 1111 2026");
    assert!(!with_year.contains("4111 1111 1111 1111"), "{with_year}");
    assert!(with_year.ends_with("2026"), "{with_year}");
    assert!(luhn_ok("4111111111111111"));
    assert!(!luhn_ok("4111111111111112"));
    assert!(!luhn_ok("41111111111111x1"));
}

/// A sentence-ending period does not continue a number, but a separator with
/// a digit beyond it does. This is the rule that lets the numeric detectors
/// keep tight boundaries without losing the common prose form.
#[test]
fn numeric_shapes_are_bounded_by_their_neighbours() {
    assert!(!masked("his SSN is 123-45-6789.").contains("123-45-6789"));
    assert!(masked("run 12-345-45-6789-01 now").contains("12-345-45-6789-01"));
    assert!(masked("v1.123-45-6789").contains("123-45-6789"));
    assert!(!stands_alone("a123-45-6789", 1, 12));
    assert!(stands_alone("(123-45-6789)", 1, 12));
}

/// Ids of the detectors whose subject is a bare digit run — the ones whose
/// false-positive cost has to be *measured*, not asserted.
const NUMERIC_PII: [&str; 8] = [
    "pii.us_taxpayer_id",
    "pii.us_taxpayer_id_labelled",
    "pii.phone_e164",
    "pii.phone_nanp",
    "pii.phone_uk_national",
    "pii.phone_intl_zero_prefix",
    "pii.card_pan",
    "pii.card_pan_grouped",
];

/// The detectors under measurement — email and IPv4 are older behaviour and
/// their hits are not this item's cost. Picked out of
/// [`builtin_shape_tables`], the one value [`propose`] runs, because that is
/// the only way to reach the PII table from outside [`builtin_tables`].
fn numeric_pii_shape_detectors() -> Vec<&'static ShapeDetector> {
    builtin_shape_tables()
        .into_iter()
        .flatten()
        .filter(|d| NUMERIC_PII.contains(&d.id.as_ref()))
        .collect()
}

/// The measured false-positive cost of the numeric PII detectors, run over
/// this repository's own text — the false-positive budget the security
/// design asks for, as an executable probe rather than a number in a
/// report.
///
/// The redaction crate's own sources are excluded: they *are* the seeded
/// corpus. Everything else — sources, manifests, `Cargo.lock`, snapshots,
/// fixtures, docs — is content a real capture would carry, and a hit here is
/// a version string, a UUID, an epoch timestamp or a numeric table that would
/// be masked out of a developer's transcript. The assertion is zero, so the
/// day a detector starts firing on ordinary repository text, it is this test
/// that says so and not a user.
///
/// This half is what the numeric detectors would mask in one file, as
/// reportable lines.
fn numeric_pii_hits(path: &std::path::Path, contents: &str) -> Vec<String> {
    numeric_pii_shape_detectors()
        .into_iter()
        .flat_map(|d| shape_spans(contents, std::slice::from_ref(d)))
        .map(|span| {
            let text = contents.get(span.start..span.end).unwrap_or_default();
            let line = contents
                .get(..span.start)
                .unwrap_or_default()
                .lines()
                .count();
            format!("{}:{line}: {} would mask {text:?}", path.display(), span.id)
        })
        .collect()
}

#[test]
fn numeric_pii_detectors_do_not_fire_on_this_repositorys_own_text() {
    let root = workspace_root();
    let own_crate = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    assert_eq!(
        numeric_pii_shape_detectors().len(),
        NUMERIC_PII.len(),
        "detector id renamed"
    );
    // The instrument is calibrated before it is trusted: a document with a
    // known identifier in it must produce exactly one reported hit. A probe
    // that would stay silent on a real hit measures nothing.
    let control = numeric_pii_hits(
        std::path::Path::new("control.env"),
        "harmless 1.2.3\nSSN=123-45-6789\nharmless 2026-08-07\n",
    );
    assert_eq!(control.len(), 1, "probe is blind: {control:?}");
    assert!(control
        .first()
        .is_some_and(|h| h.contains("pii.us_taxpayer_id")
            && h.contains("123-45-6789")
            && h.contains("control.env:2")));

    let mut hits: Vec<String> = Vec::new();
    let mut scanned = 0usize;
    visit_text_files(&root, &mut |path, contents| {
        if path.starts_with(&own_crate) || path.components().any(|c| c.as_os_str() == "fuzz") {
            return;
        }
        scanned += 1;
        hits.extend(numeric_pii_hits(path, contents));
    });
    // The floor is what makes the reported count meaningful. The ledger
    // records "449 files" and the old assertion was `> 100`, so the walk
    // could have collapsed to a fifth of the workspace and still issued a
    // clean bill of health — a scope test's failure mode is a walk that
    // silently stops finding files, which is the same lesson the seal
    // guard's per-root census exists for. 400 is the floor; the census
    // prints on failure so a drop is diagnosable rather than mysterious.
    assert!(
        scanned >= 400,
        "the probe scanned only {scanned} text files (floor 400) — a walk that \
         stops finding files reports zero hits for the wrong reason"
    );
    assert!(
        hits.is_empty(),
        "numeric PII detectors fire on {} places in ordinary repository text \
         ({scanned} files scanned):\n{}",
        hits.len(),
        hits.join("\n")
    );
}

/// **The fast paths change no proposal.** `propose` — with its per-table
/// `RegexSet` prefilter and its short-token gate — must return exactly what the
/// direct scan of every table returns, in the same order: on the hand-written
/// samples, on every text file the census reads (real transcripts and this
/// repository's own text, whole), and on every short benign leaf those files
/// split into, which is where the gate fires.
#[test]
fn regex_set_and_short_token_fast_paths_match_direct_table_scans() {
    let tables = builtin_shape_tables();
    let sets = tables.map(|t| {
        regex::RegexSet::new(t.iter().map(|d| d.re.as_str())).expect("valid built-in RegexSet")
    });
    let samples = [
        "type",
        "assistant",
        "tool_use",
        "content",
        "SSN=123-45-6789 and ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
        "postgres://alice:s3cr3t@db.example.com:5432/app",
        "contact alice@example.com or +1 415 555 2671 at 742 Evergreen Terrace",
    ];
    for sample in samples {
        for (table, set) in tables.into_iter().zip(&sets) {
            let direct = shape_spans(sample, table);
            let prefiltered = crate::shapes::shape_spans_with_set(sample, table, set);
            assert_eq!(
                span_keys(&prefiltered),
                span_keys(&direct),
                "mismatch on {sample:?}"
            );
        }
        assert_propose_matches_the_direct_scan(sample);
    }
    let mut leaves = BTreeSet::new();
    let mut scanned = 0;
    visit_text_files(&workspace_root(), &mut |_, contents| {
        scanned += 1;
        assert_propose_matches_the_direct_scan(contents);
        leaves.extend(
            contents
                .split(|c: char| c.is_whitespace() || "\"',{}[]()`".contains(c))
                .filter(|leaf| is_short_benign_token(leaf))
                .map(str::to_owned),
        );
    });
    assert!(scanned >= 400, "the walk read only {scanned} files");
    assert!(leaves.len() >= 1000, "only {} short leaves", leaves.len());
    for leaf in &leaves {
        assert_propose_matches_the_direct_scan(leaf);
    }
}

type SpanKey = (usize, usize, String, Option<(usize, usize)>);

fn span_keys(spans: &[Span]) -> Vec<SpanKey> {
    spans
        .iter()
        .map(|s| (s.start, s.end, s.id.clone(), s.parse))
        .collect()
}

/// `propose`'s layer order, with neither fast path: every built-in table
/// scanned detector by detector, and the entropy layer before the PII table.
fn direct_propose(text: &str) -> Vec<Span> {
    let [pass1, provider, credential, pii] = builtin_shape_tables();
    let mut spans = shape_spans(text, pass1);
    spans.extend(shape_spans(text, provider));
    spans.extend(shape_spans(text, credential));
    spans.extend(entropy_spans(text));
    spans.extend(shape_spans(text, pii));
    spans
}

fn assert_propose_matches_the_direct_scan(text: &str) {
    let fast = propose(text, &RulePackSet::builtin());
    let direct = direct_propose(text);
    let at = text.get(..text.len().min(80)).unwrap_or_default();
    assert_eq!(span_keys(&fast), span_keys(&direct), "mismatch on {at:?}…");
}

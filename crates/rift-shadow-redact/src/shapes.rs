//! Pass 1 and the PII half of pass 2: the shape detectors' gates — the
//! checksum, the issuing authority's allocation rules and the neighbourhood
//! test a linear-time regex cannot express — and [`shape_spans`], which runs a
//! detector table and clamps every gate's answer inside the regex's own match.
//! Separate from the entropy layer because a shape is evidence about *what* a
//! run of bytes is; entropy is the guess made when no shape claims it.

use crate::span::{MatchedAgainst, ShapeDetector, Span};

pub(crate) fn shape_spans(text: &str, detectors: &[ShapeDetector]) -> Vec<Span> {
    let mut spans = Vec::new();
    for d in detectors {
        shape_spans_for_detector(text, d, &mut spans);
    }
    spans
}

pub(crate) fn shape_spans_with_set(
    text: &str,
    detectors: &[ShapeDetector],
    set: &regex::RegexSet,
) -> Vec<Span> {
    let matches = set.matches(text);
    if !matches.matched_any() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    for (idx, d) in detectors.iter().enumerate() {
        if matches.matched(idx) {
            shape_spans_for_detector(text, d, &mut spans);
        }
    }
    spans
}

fn shape_spans_for_detector(text: &str, d: &ShapeDetector, spans: &mut Vec<Span>) {
    if d.group == 0 {
        for m in d.re.find_iter(text) {
            let Some((start, end)) = gated_range(d, text, m) else {
                continue;
            };
            spans.push(Span {
                start,
                end,
                class: d.class,
                id: d.id.to_string(),
                parse: None,
                matched: MatchedAgainst::Input,
            });
        }
        return;
    }
    for caps in d.re.captures_iter(text) {
        let Some(m) = caps.get(d.group) else {
            continue;
        };
        let Some((start, end)) = gated_range(d, text, m) else {
            continue;
        };
        spans.push(Span {
            start,
            end,
            class: d.class,
            id: d.id.to_string(),
            // A group detector keeps the rest of its match as context.
            parse: caps.get(0).map(|w| (w.start(), w.end())),
            // Every proposal starts as a read of the text it was handed;
            // `select_spans` re-stamps the ones it handed blanked text.
            matched: MatchedAgainst::Input,
        });
    }
}

/// Narrow a credential **value** span past one matching pair of quotes.
///
/// A credential written `password: "hunter 2 space"` has to be matched with
/// its quotes — the value contains a space, and a value class that stops at
/// whitespace publishes the tail (the defect `dsn.quoted_value_tail` records).
/// Matching them and then *masking* them costs the structure instead: the
/// placeholder lands where the quotes were and the JSON, YAML or connection
/// string no longer parses. So the regex takes the quotes and the gate hands
/// them back, which is exactly what a gate is for — it only ever narrows
/// ([`SpanGate`]).
///
/// An empty quoted value narrows to nothing and `shape_spans` drops it: there
/// is no secret in `password: ""`, and a placeholder there would invent one.
pub(crate) fn unquoted_value(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    let value = text.get(start..end)?;
    let mut chars = value.chars();
    let (Some(open), Some(close)) = (chars.next(), chars.next_back()) else {
        return Some((start, end));
    };
    if open == close && matches!(open, '"' | '\'') {
        return Some((start + open.len_utf8(), end - close.len_utf8()));
    }
    Some((start, end))
}

/// US taxpayer identifier — an SSA social-security number or an IRS individual
/// taxpayer identification number, which share the `NNN-NN-NNNN` shape.
/// Separators must match, so `123-45 6789` is not an identifier.
pub(crate) fn is_us_taxpayer_id(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    if !stands_alone(text, start, end) {
        return None;
    }
    let s = text.get(start..end)?;
    let (runs, seps) = split_number(s);
    if runs.len() != 3 || seps.first() != seps.get(1) {
        return None;
    }
    let (Some(area), Some(group), Some(serial)) =
        (field(&runs, 0), field(&runs, 1), field(&runs, 2))
    else {
        return None;
    };
    issued_taxpayer_id(area, group, serial).then_some((start, end))
}

/// The same identifier written as one unpunctuated 9-digit run — `SSN=123456789`
/// in a `.env` or `{"ssn":"123456789"}` in a captured JSON document, which no
/// separator-bearing shape can reach and which nothing else in the pipeline sees
/// (nine digits carry too little entropy and too common a shape to redact on
/// sight).
///
/// The key beside it is the evidence that makes those nine digits an identifier;
/// the allocation rules still decide. The detector masks the value group only,
/// so the key survives and the file stays readable.
///
/// The labels are the ones an issuing authority's number is filed under —
/// `ssn`, `social security`, `itin`, `tax id`, `taxpayer id`, `national id` —
/// and all of them resolve to the same SSA/IRS allocation check, because that
/// is the only authority this gate knows. A `national_id` from another country
/// is a recorded gap, not a covered case.
pub(crate) fn is_labelled_taxpayer_id(
    text: &str,
    start: usize,
    end: usize,
) -> Option<(usize, usize)> {
    if !stands_alone(text, start, end) {
        return None;
    }
    let s = text.get(start..end)?;
    let parse = |r: std::ops::Range<usize>| s.get(r).and_then(|f| f.parse::<u32>().ok());
    let (Some(area), Some(group), Some(serial)) = (parse(0..3), parse(3..5), parse(5..9)) else {
        return None;
    };
    issued_taxpayer_id(area, group, serial).then_some((start, end))
}

/// ITU-T E.164. What the detector actually reads, which is narrower than "a `+`
/// country code and 8-15 digits, punctuated or not": the regex takes a country
/// code and then digit *groups*, and the gate counts groups from the left,
/// stopping before the one that would push the number past E.164's 15-digit
/// ceiling.
///
/// The trim is the whole point. A greedy match cannot tell where one number ends
/// and the next begins, so `call +44 20 7123 4567 9999` used to match sixteen
/// digits, fail the ceiling, and publish the phone number legacy — four digits
/// after a non-US number disarmed the only detector that covers it. Trimming to
/// the last group that fits masks the number and leaves the neighbour, which is
/// the only answer that is neither a leak nor a guess; a regex cannot express
/// "as many groups as keep the total under 15" and a reject-only gate can only
/// choose between publishing all of it and masking all of it.
///
/// The leading `+` is the evidence that makes punctuation safe to accept —
/// The legacy backend's own `redact/pii.go` records the lesson that dotted separators
/// without it collide with version strings and dotted quads. In a unified diff
/// that `+` is also the added-line marker, which is an over-redaction stated and
/// pinned rather than narrowed away — see
/// `a_diff_added_line_of_digits_is_read_as_a_phone_number`.
pub(crate) fn is_e164_phone(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    if !stands_alone(text, start, end) {
        return None;
    }
    let s = text.get(start..end)?;
    let bytes = s.as_bytes();
    let digit_at = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_digit);
    let (mut digits, mut cut, mut i) = (0usize, 0usize, 0usize);
    while i < bytes.len() {
        if digit_at(i) {
            let run_start = i;
            while digit_at(i) {
                i += 1;
            }
            if digits + (i - run_start) > 15 {
                break;
            }
            digits += i - run_start;
            cut = i;
        } else {
            i += 1;
        }
    }
    (digits >= 8).then_some((start, start + cut))
}

pub(crate) fn is_nanp_phone(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    if !stands_alone(text, start, end) {
        return None;
    }
    let s = text.get(start..end)?;
    let (all_runs, all_seps) = split_number(s);
    // A leading `1` is the trunk prefix, not part of the number.
    let trunk = usize::from(all_runs.first().is_some_and(|r| *r == "1"));
    let (Some(runs), Some(seps)) = (all_runs.get(trunk..), all_seps.get(trunk..)) else {
        return None;
    };
    if runs.len() != 3 || seps.len() != 2 {
        return None;
    }
    let lengths_ok = matches!(
        (runs.first(), runs.get(1), runs.get(2)),
        (Some(a), Some(b), Some(c)) if a.len() == 3 && b.len() == 3 && c.len() == 4
    );
    let nxx = |r: Option<&&str>| {
        r.and_then(|r| r.chars().next())
            .is_some_and(|c| ('2'..='9').contains(&c))
    };
    let separators_ok = s.contains('(') || seps.first() == seps.get(1);
    (lengths_ok && nxx(runs.first()) && nxx(runs.get(1)) && separators_ok).then_some((start, end))
}

/// North American Numbering Plan number without a country code: `NXX-NXX-XXXX`,
/// `(NXX) NXX-XXXX`, or the same behind a `1` trunk prefix.
///
/// The area and central-office codes must both start 2-9 (the NANP's `N`), which
/// is what separates a real number from a sequential digit run. Separators must
/// match unless the area code is parenthesised — a bracketed area code is
/// evidence on its own, mixed separators in a bare digit run are not.
///
/// The condition worth stating rather than dropping: a bare *space*-separated
/// triple is accepted, so three columns of a numeric table whose widths are
/// 3-3-4 and whose first two columns start 2-9 (`256 512 1024`) is masked as a
/// phone number. `N`XX removes the sequential cases (`100 200 3000`) but cannot
/// remove that one. It is over-redaction, it is what the legacy backend's own bare branch
/// does with the same input, and it is pinned by
/// `a_bare_numeric_triple_is_read_as_a_phone_number` so the trade is visible
/// rather than discovered.
/// A national number written behind its **trunk prefix** — the UK's `020 7123
/// 4567` shape, and the one every native phone detector missed because it is
/// neither NANP's 3-3-4 nor E.164 behind a `+`.
///
/// The leading `0` is the evidence, and it is what makes this affordable: the
/// row that recorded this gap refused it on the reading that a 3-4-4 triple
/// would claim numeric table columns like `256 5121 1024`, and that is true of
/// a *bare* triple. Anchored on the trunk `0` the same widening measured **zero**
/// hits over this repository's tracked text and 3 over its whole diff history,
/// all three the parity row's own literal.
///
/// A UK national number is 10 or 11 digits including the trunk digit, in three
/// groups with one separator style; nothing here is UK-specific beyond that,
/// which is why the id says `uk_national` and the note says what it checks.
pub(crate) fn is_trunked_national_phone(
    text: &str,
    start: usize,
    end: usize,
) -> Option<(usize, usize)> {
    if !stands_alone(text, start, end) {
        return None;
    }
    let s = text.get(start..end)?;
    let (runs, seps) = split_number(s);
    if runs.len() != 3 || seps.len() != 2 || seps.first() != seps.get(1) {
        return None;
    }
    if !runs.first().is_some_and(|r| r.starts_with('0')) {
        return None;
    }
    let digits: usize = runs.iter().map(|r| r.len()).sum();
    (10..=11).contains(&digits).then_some((start, end))
}

/// The same number written with the ITU **exit code** `00` instead of `+`.
///
/// `00` is weaker evidence than `+` — it also opens an octal literal, a
/// zero-padded id and a timestamp — so the gate carries what the prefix does
/// not: at least three groups written with explicit separators, and 10 to 15
/// national digits, which is E.164's own range. Measured that way it costs
/// **zero** hits over this repository's tracked text; the unguarded form the
/// row refused (`00` plus any digit run) costs **1 188**, because a hyphenated
/// document id ending `-0046` and a `Cargo.toml` version are exactly that
/// shape.
pub(crate) fn is_exit_code_phone(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    if !stands_alone(text, start, end) {
        return None;
    }
    let s = text.get(start..end)?;
    let (runs, seps) = split_number(s);
    if runs.len() < 3 || seps.len() + 1 != runs.len() {
        return None;
    }
    let digits: usize = runs.iter().map(|r| r.len()).sum();
    // The `00` is the exit code and not part of the number.
    let national = digits.checked_sub(2)?;
    (10..=15).contains(&national).then_some((start, end))
}

/// A payment-card primary account number: 13-19 digits standing alone, an issuer
/// prefix a network actually issues at that length, and a valid Luhn check
/// digit. All three must hold.
///
/// Punctuation is deliberately not constrained beyond the shape. Requiring
/// consistent separators removed no false positive when this was measured over
/// the workspace's own text, so it only ever cost coverage on a sloppily written
/// real number. The funnel, re-measured on this tree with the widened separator
/// (2026-08-07): **488** raw 13-19-digit runs → **45** after the `\b` window
/// rule → **5** Luhn-valid → **0** carrying an issuer prefix at a length that
/// network issues. Luhn alone would have masked 60 of the raw 485 before the
/// separator widened, which is the answer to "is the checksum the gate or
/// decoration". A comma is *not* in the separator class, and that is a measured
/// refusal rather than an omission — see the `4111,1111,1111,1111` corpus row.
///
/// Those are one-off measurements. What is *enforced* is
/// `numeric_pii_detectors_do_not_fire_on_this_repositorys_own_text`, which
/// requires the final total to stay zero over at least 400 scanned files.
pub(crate) fn is_card_pan(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    if !stands_alone(text, start, end) {
        return None;
    }
    let s = text.get(start..end)?;
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    let len = digits.len();
    ((13..=19).contains(&len) && card_network_shape(&digits, len) && luhn_ok(&digits))
        .then_some((start, end))
}

/// A street address — house number, capitalised words, street-type suffix.
///
/// **This is the one PII shape with no checksum and no issuing authority**, so
/// the shape alone decides and the whole question is what it costs. The gate
/// adds the only two things available: the house number must stand alone (so a
/// version string in `v1.2.3 Test Drive` is not a house number — the `.` with a
/// digit beyond it disqualifies it), and the match is bounded, because an
/// address is a phrase and not a paragraph.
///
/// Measured before it was written, over both corpora this crate uses: **zero**
/// hits on 493 files of tracked text, and over the whole diff history only the
/// parity row's own literal. That measurement is weaker than the others in this
/// file and the weakness is stated rather than glossed: this corpus is a Rust
/// workspace, and the text that collides with this shape is English prose with
/// capitalised phrases in it. The collision is real and is recorded as its own
/// parity row rather than left for a user to find.
pub(crate) fn is_street_address(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    if !stands_alone(text, start, end) {
        return None;
    }
    (end.checked_sub(start)? <= 64).then_some((start, end))
}

/// True when `text[start..end]` is a self-contained number rather than a
/// fragment of a longer one.
///
/// A neighbouring digit or word character disqualifies the match outright; a
/// neighbouring `-`/`.`/`/` disqualifies it only when a digit sits beyond that
/// separator, so `345-45-6789` inside `12-345-45-6789-01` is not a taxpayer id
/// while `his SSN is 123-45-6789.` still is — a sentence period is not a
/// continuation of the number.
pub(crate) fn stands_alone(text: &str, start: usize, end: usize) -> bool {
    fn side_ok(adjacent: Option<char>, beyond: Option<char>) -> bool {
        match adjacent {
            Some(c) if c.is_ascii_alphanumeric() || c == '_' => false,
            Some('-' | '.' | '/') => !beyond.is_some_and(|c| c.is_ascii_digit()),
            None | Some(_) => true,
        }
    }
    let mut left = text.get(..start).unwrap_or_default().chars().rev();
    let mut right = text.get(end..).unwrap_or_default().chars();
    side_ok(left.next(), left.next()) && side_ok(right.next(), right.next())
}

/// Split a matched numeric shape into its digit runs and the separator text
/// *between* them: `(415) 555-0199` reads as runs `415`/`555`/`0199` with
/// separators `") "` and `"-"`. A leading (`+`, `(`) or trailing prefix is not a
/// separator, so `seps.len() + 1 == runs.len()` whenever there is a run at all.
pub(crate) fn split_number(s: &str) -> (Vec<&str>, Vec<&str>) {
    let bytes = s.as_bytes();
    let digit_at = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_digit);
    let (mut runs, mut seps): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
    let mut i = 0usize;
    while i < bytes.len() {
        let start = i;
        if digit_at(i) {
            while digit_at(i) {
                i += 1;
            }
            if let Some(run) = s.get(start..i) {
                runs.push(run);
            }
        } else {
            while i < bytes.len() && !digit_at(i) {
                i += 1;
            }
            // Only a stretch with a digit run on both sides is a separator.
            if !runs.is_empty() && i < bytes.len() {
                if let Some(sep) = s.get(start..i) {
                    seps.push(sep);
                }
            }
        }
    }
    (runs, seps)
}

/// The Luhn (ISO/IEC 7812-1) check digit. This is the *gate*, not decoration:
/// a PAN-shaped run that fails it is not masked, which is what keeps the
/// detector off long identifiers and timestamps.
pub(crate) fn luhn_ok(digits: &str) -> bool {
    let mut sum = 0u32;
    for (i, c) in digits.chars().rev().enumerate() {
        let Some(d) = c.to_digit(10) else {
            return false;
        };
        sum += if i % 2 == 1 {
            let doubled = d * 2;
            if doubled > 9 {
                doubled - 9
            } else {
                doubled
            }
        } else {
            d
        };
    }
    sum.is_multiple_of(10)
}

/// The bytes a detector's match seals once its gate has spoken, or `None`
/// when the gate refuses the candidate or nothing would be sealed.
///
/// A gate can drop or narrow a candidate, never widen one: an ungated
/// detector behaves exactly as it did before gates existed, and a gated one
/// never seals a byte its regex did not match. The clamp is what makes that
/// structural rather than a promise checked against four fixed texts — a gate
/// returning a wider range is narrowed back to its match here, so no gate can
/// ever seal a byte the regex did not select. Both bounds come from a
/// `Match`, so clamping cannot land off a `char` boundary.
///
/// An empty match seals nothing: it can only insert a placeholder where there
/// is no content. Skipping them is what makes the empty-match rejection in
/// `with_vendor_pattern` a diagnostic rather than the guarantee — a pattern
/// that matches empty only *in context* (`\b`) never reaches `is_match("")`,
/// and would otherwise rewrite the whole text into placeholders, then do it
/// again to its own output on the next pass.
fn gated_range(d: &ShapeDetector, text: &str, m: regex::Match<'_>) -> Option<(usize, usize)> {
    let (start, end) = match d.gate {
        Some(gate) => {
            let (s, e) = gate(text, m.start(), m.end())?;
            (s.max(m.start()), e.min(m.end()))
        }
        None => (m.start(), m.end()),
    };
    if start < end {
        Some((start, end))
    } else {
        None
    }
}

fn field(runs: &[&str], idx: usize) -> Option<u32> {
    runs.get(idx).and_then(|r| r.parse::<u32>().ok())
}

/// The issuing authority's *published allocation rules* for a US taxpayer
/// identifier, which is what keeps the 3-2-4 shape from firing on arbitrary
/// digit runs: the SSA has never issued area 000 or 666, group 00, or serial
/// 0000, and the 900-999 area belongs to the IRS, which issues ITINs only inside
/// published group ranges.
fn issued_taxpayer_id(area: u32, group: u32, serial: u32) -> bool {
    if group == 0 || serial == 0 {
        return false;
    }
    match area {
        0 | 666 => false,
        // IRS ITIN: the 9NN area is only ever issued with these group ranges.
        900..=999 => matches!(group, 50..=65 | 70..=88 | 90..=92 | 94..=99),
        _ => true,
    }
}

/// Issuer-identification-number prefix + length, per the networks' published
/// numbering ranges (ISO/IEC 7812). Pairing the prefix with the length is what
/// keeps the detector away from the digit runs a repository actually carries:
/// epoch milliseconds and nanoseconds start with `1`, which no network issues.
fn card_network_shape(digits: &str, len: usize) -> bool {
    let p = |n: usize| digits.get(..n).and_then(|d| d.parse::<u32>().ok());
    let in_range = |v: Option<u32>, lo: u32, hi: u32| v.is_some_and(|v| (lo..=hi).contains(&v));
    if p(1) == Some(4) {
        return matches!(len, 13 | 16 | 19); // Visa
    }
    if matches!(p(2), Some(34 | 37)) {
        return len == 15; // American Express
    }
    if in_range(p(2), 51, 55) || in_range(p(4), 2221, 2720) {
        return len == 16; // Mastercard
    }
    if p(4) == Some(6011) || p(2) == Some(65) || in_range(p(3), 644, 649) {
        return (16..=19).contains(&len); // Discover
    }
    if in_range(p(4), 3528, 3589) {
        return (16..=19).contains(&len); // JCB
    }
    if p(2) == Some(62) {
        return (16..=19).contains(&len); // UnionPay
    }
    if in_range(p(3), 300, 305) || p(4) == Some(3095) || matches!(p(2), Some(36 | 38 | 39)) {
        return len == 14; // Diners Club
    }
    false
}

//! The seeded PII corpus: every entry derived from a public format
//! specification or a network's published test number, masked; and the shapes
//! a repository legitimately carries, kept.

use super::fixtures::masked;

/// The defect this closes, reproduced end to end: the capture inclusion
/// controls make a `.worktreeinclude`d `.env` durable, and a verifier
/// measured this exact file landing in a checkpoint shard with its AWS key and
/// email masked but `SSN=` and `PHONE=` verbatim.
#[test]
fn an_included_env_file_leaves_no_pii_verbatim() {
    let env = "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\n\
               SSN=123-45-6789\n\
               PHONE=+1-415-555-0199\n\
               CARD=4111 1111 1111 1111\n\
               EMAIL=alice@example.com\n\
               API_TOKEN=Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ\n";
    let m = masked(env);
    for verbatim in [
        "AKIAIOSFODNN7EXAMPLE",
        "123-45-6789",
        "+1-415-555-0199",
        "4111 1111 1111 1111",
        "alice@example.com",
        "Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ",
    ] {
        assert!(!m.contains(verbatim), "{verbatim} survived redaction: {m}");
    }
    // the keys survive: a redacted .env is still a readable .env
    for key in ["SSN=", "PHONE=", "CARD=", "EMAIL="] {
        assert!(m.contains(key), "key {key} must be preserved: {m}");
    }
}

/// The seeded PII corpus, by class. Every entry is derived from a public
/// format specification (SSA area/group/serial allocation, IRS ITIN group
/// ranges, the NANP `N`XX rule, ITU-T E.164 length, ISO/IEC 7812 issuer
/// ranges + Luhn) or is a network's own published test number, and every
/// entry is masked — zero false negatives.
const SEEDED_TAXPAYER_IDS: [&str; 3] = [
    "123-45-6789",
    "123 45 6789",
    "900-70-1234", // IRS ITIN: 9NN area with an issued group
];

const SEEDED_PHONE_NUMBERS: [&str; 15] = [
    "+1-415-555-0199",
    "+14155550199",
    "+44 20 7123 4567",
    // international groupings a 2-4-digit group rule could not spell:
    // a 1-digit group, and a 5-digit one
    "+33 1 42 68 53 00",
    "+81 3 1234 5678",
    "+55 11 91234-5678",
    "+1 (415) 555-0199",
    "415-555-0199",
    "+1.415.555.0199", // dots are separators behind a `+`, and only there
    "(415) 555-0199",
    "1-415-555-0199",
    "415 555 0199",
    // national spellings behind a trunk prefix or the ITU exit code,
    // which no `+`-anchored or NANP-anchored shape can reach
    "020 7123 4567",
    "0800 123 4567",
    "0044 20 7123 4567",
];

/// The networks' published test numbers.
const SEEDED_PAYMENT_CARDS: [&str; 12] = [
    "4111111111111111",
    "4111 1111 1111 1111",
    "4111-1111-1111-1111",
    "4111-1111 1111-1111", // punctuation may be inconsistent; the number is not
    "4111  1111  1111  1111", // a doubled separator is still a separator
    "4111.1111.1111.1111",
    "378282246310005",
    "5555555555554444",
    "6011111111111117",
    "30569309025904",
    "3530111333300000",
    // the comma form, in the uniform grouping that makes it a card
    // rather than a coordinate list
    "4111,1111,1111,1111",
];

const SEEDED_LABELLED_VALUES_AND_ADDRESSES: [&str; 4] = [
    // a labelled identifier whose value sits on the next line
    "ssn=\n123456789",
    // street addresses
    "1600 Pennsylvania Ave",
    "221B Baker Street",
    "350 Fifth Avenue",
];

/// The other half of the same claim: the shapes a repository legitimately
/// carries, every one of them kept.
const KEPT_LOOKALIKES: [&str; 27] = [
    // never-issued taxpayer ranges
    "000-12-3456",
    "666-12-3456",
    "123-00-4567",
    "123-45-0000",
    "901-11-1234", // 9NN area outside every ITIN group range
    "123-45 6789", // mixed separators
    // digit runs that only look like numbers
    "12-345-45-6789-01", // a fragment of a longer run
    "100-200-3000",      // area code cannot start with 0 or 1
    "415-555 0199",      // mixed separators
    "415.555.0199",      // dots without a `+`: a dotted triple is not a phone
    "1.2.3",             // version strings
    "2.10.4",
    "1735689600001",                            // epoch milliseconds
    "550e8400-e29b-41d4-a716-446655440000",     // a UUID
    "356a192b7913b04c54574d18c28d46e6395428ab", // a commit hash
    "1234567890123452",                         // Luhn-valid, but no network issues a 1-prefix
    "1735689600000000005",                      // Luhn-valid epoch nanoseconds
    "4111111111111112",                         // Visa prefix, correct length, Luhn fails
    "12345678901234567890123",                  // a 23-digit identifier
    // The shapes the two trunk-prefixed phone rules must NOT claim, and
    // the reason each is here: a bare numeric triple with no trunk `0`
    // is what those rows refused to widen for, and a zero-padded
    // ordinary number is what `00` in front of any digit run costs --
    // 1 188 hits over this repository's own text — a hyphenated
    // document id ending `-0046` and a Cargo version among them.
    "256 5121 1024",
    "0046",
    "0044",
    "0012 34", // an exit code needs three groups, not two
    // The comma separator is still refused in GENERAL, and the reason
    // is still measured: adding `,` to the card detector's separator
    // class takes it from 0 to 281 hits over this repository's tracked
    // text (and 73 to 361 over its whole diff history), because a
    // comma-separated coordinate list (`407,71,46 414,71,46 420,57` in
    // a canopy snapshot) forms Luhn-valid 16-digit windows. Luhn is not
    // a sufficient gate against dense numeric data, which is what
    // pii.epoch_nanoseconds has said all along.
    //
    // What changed is that the comma is now accepted in ONE writing --
    // four groups of exactly four digits (pii.card_pan_grouped) -- so
    // the coordinate list below must still survive, and it is here to
    // say so.
    "407,71,46 414,71,46 420,57",
    "1234,5678,9012,3456", // uniform grouping, but Luhn fails
    // A house number is a number that stands alone: the `.` with a
    // digit beyond it makes this a version string, not an address.
    "v1.2.3 Test Drive",
    // No capitalised word between the number and the street type, so
    // there is no name for an address to be made of.
    "4 Way Handshake",
];

/// Every row, set in a sentence, leaves no byte of itself in the output.
fn assert_every_row_masked(rows: &[&str]) {
    for s in rows {
        let m = masked(&format!("value {s} end"));
        assert!(!m.contains(s), "seeded PII survived redaction: {s} -> {m}");
    }
}

#[test]
fn seeded_taxpayer_ids_are_masked() {
    assert_every_row_masked(&SEEDED_TAXPAYER_IDS);
}

#[test]
fn seeded_phone_numbers_are_masked() {
    assert_every_row_masked(&SEEDED_PHONE_NUMBERS);
}

#[test]
fn seeded_payment_cards_are_masked() {
    assert_every_row_masked(&SEEDED_PAYMENT_CARDS);
}

#[test]
fn seeded_labelled_values_and_street_addresses_are_masked() {
    assert_every_row_masked(&SEEDED_LABELLED_VALUES_AND_ADDRESSES);
}

#[test]
fn the_shapes_a_repository_carries_are_kept() {
    for s in KEPT_LOOKALIKES {
        let m = masked(&format!("value {s} end"));
        assert!(m.contains(s), "false positive: {s} was masked -> {m}");
    }
}

/// Nine bare digits are only an identifier when a key says so. The key is
/// context, not subject, so it survives the masking — and without it the same
/// nine digits are left alone, because a bare 9-digit run is an ordinary
/// shape in ordinary text.
#[test]
fn an_unpunctuated_taxpayer_id_needs_its_key() {
    for line in [
        "SSN=123456789",
        "ssn: 123456789",
        "USER_SSN = 123456789",
        "SOCIAL_SECURITY_NUMBER=123456789",
    ] {
        let m = masked(line);
        assert!(!m.contains("123456789"), "{line} survived redaction: {m}");
        assert!(
            m.contains("[REDACTED:pii.us_taxpayer_id_labelled]"),
            "{line} -> {m}"
        );
    }
    // the key is preserved, the value is not
    assert_eq!(
        masked("SSN=123456789"),
        "SSN=[REDACTED:pii.us_taxpayer_id_labelled]"
    );
    // no key, no redaction — and an unissued area is not an identifier even
    // with one
    assert!(masked("order 123456789 shipped").contains("123456789"));
    assert!(masked("SSN=000000000").contains("000000000"));
    assert!(masked("SSN=666123456").contains("666123456"));
}

/// A quote is not a disqualifying neighbour, and this is the adjacency that
/// most needed pinning: the capture document is JSON end to end, so a quote
/// is the commonest thing sitting beside a value. `stands_alone` reads those
/// neighbours, and a one-character change there — treating `"` like a digit
/// — leaves every JSON-quoted identifier verbatim while the whole suite
/// stays green. Measured before this test existed: 39 tests, 0 failures,
/// with that mutation in place.
/// **A value alone on its line was judged alone, and the label above it
/// could not reach it** — the walker's run boundary, not the detector.
///
/// `123456789` is a valid JSON document, so the line went structural, the
/// flat run ended at the label, and nine digits with nothing beside them
/// are not an identifier. The label reached the value only when the line
/// happened to carry something unparseable beside it, which is a coverage
/// difference no rule of this crate intended and nothing stated.
#[test]
fn a_labelled_value_alone_on_its_line_is_not_judged_alone() {
    assert!(!masked("ssn=\n123456789\n").contains("123456789"));
    assert!(!masked("SSN\n123456789").contains("123456789"));
    // The same shape where the value line was never parseable, which is how
    // this was covered before and must stay covered.
    assert!(!masked("value ssn=\n123456789 end").contains("123456789"));
    // A bare scalar line still redacts on its own merits.
    assert!(!masked("{\"a\":1}\nAKIAIOSFODNN7EXAMPLE").contains("AKIAIOSFODNN7EXAMPLE"));
    // …and an object line still goes structural: key order, spacing and
    // quoting are the input's.
    assert_eq!(
        masked("{\"text\":\"key AKIAIOSFODNN7EXAMPLE here\",\"keep\":\"ok\"}"),
        "{\"text\":\"key [REDACTED:vendor] here\",\"keep\":\"ok\"}"
    );
}

#[test]
fn a_quote_is_not_a_disqualifying_neighbour() {
    for (doc, secret) in [
        (r#"{"ssn":"123-45-6789"}"#, "123-45-6789"),
        (r#"{"phone":"+1-415-555-0199"}"#, "+1-415-555-0199"),
        (r#"{"phone":"(415) 555-0199"}"#, "(415) 555-0199"),
        (r#"{"card":"4111111111111111"}"#, "4111111111111111"),
        (r#"{"card":"4111 1111 1111 1111"}"#, "4111 1111 1111 1111"),
        (r#"{"ssn":"123456789"}"#, "123456789"),
        (r#"{"social_security":"123456789"}"#, "123456789"),
        (r#"{"social_security_number": "123456789"}"#, "123456789"),
        ("{'ssn': '123456789'}", "123456789"),
    ] {
        let m = masked(doc);
        assert!(!m.contains(secret), "{doc} left {secret} verbatim: {m}");
    }
    // NOT covered, and recorded rather than widened: an XML/HTML element
    // body (`<field name="ssn">123456789</field>`) puts a `>` between the
    // label and the value. Adding `>` to the separator would also claim
    // `if ssn_count > 123456789`, and the capture document is JSON, so the
    // narrow rule is the sound one here. Asserted so the gap is a fact
    // rather than a belief.
    assert!(masked(r#"<field name="ssn">123456789</field>"#).contains("123456789"));
}

/// The label does not need punctuation, and it is not only `ssn`. Every one
/// of these resolves to the same SSA/IRS allocation check — the label is
/// evidence that nine digits are an identifier, never evidence about which
/// authority issued them.
#[test]
fn a_taxpayer_id_label_may_be_any_of_its_names() {
    for line in [
        "SSN 123456789",
        "itin = 900701234",
        "tax_id: 123456789",
        "TAX ID 123456789",
        "taxpayer_id=123456789",
        "national_id: 123456789",
    ] {
        let m = masked(line);
        assert!(
            m.contains("[REDACTED:pii.us_taxpayer_id_labelled]"),
            "{line} left its value verbatim: {m}"
        );
    }
    // The label must be a label: `ssn` inside a longer word is not one, and
    // the allocation rules still decide even when it is.
    assert!(masked("xssn=123456789").contains("123456789"));
    assert!(masked("XSSN=123456789").contains("123456789"));
    assert!(masked("tax_id=000000000").contains("000000000"));
}

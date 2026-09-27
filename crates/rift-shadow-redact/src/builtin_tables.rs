//! **The built-in shape-detector tables, behind one door.**
//!
//! `pass1_detectors`, `provider_detectors`, `credential_detectors` and
//! `pii_detectors` are private to
//! this module and [`builtin_shape_tables`] is the only item that leaves it —
//! the same value the differential oracle's census reads. So the way a further
//! table gets added is the way these three are already reached: into that
//! array, where the census enumerates it. `provider_detectors` is that sentence
//! taken up, twice: layers 3, 5 and 7 arrived as tables rather than as more
//! lines in `pass1_detectors`, and the array is where they joined. Clippy's
//! 100-line function cap is what insisted the second time, which is a better
//! reason than taste — the module doc had said it first.
//!
//! **This is a default, not a lock, and the difference is measured rather than
//! assumed.** Eight plants, each one a detector `propose` runs that no entry in
//! the census accounts for:
//!
//! | # | plant | compiles? | census | differential | rest of the package |
//! |---|---|---|---|---|---|
//! | 1 | private table here, called from `propose` | **no** — `E0603` | — | — | — |
//! | 2 | same table, `pub(super)` added by hand | yes | green | **red** | — |
//! | 3 | table written at the crate root instead | yes | green | **red** | — |
//! | 4 | crate-root table, undeclared id, matches nothing here | yes | green | green | **green**, 61 + 5 |
//! | 5 | crate-root table, id `pii.email`, matches bare hosts | yes | green | green | one red: the anchor |
//! | 6 | second `pii.email` pattern added *here*, after the genuine one | yes | green | green | one red: the anchor |
//! | 7 | row 6's detector moved *before* the genuine one | yes | green | green | **two red**: the pin and the anchor |
//! | 8 | second `pii.ipv4` pattern added *here*, after the genuine one | yes | green | green | **green**, 61 + 5 |
//!
//! Rows 1–3 were measured two cycles earlier, rows 4–7 in the one after it, and
//! row 8 here.
//! Rows 4–6 were measured **twice** — at `767ef03`, where all three left that
//! tree's whole package green at its own 60 lib + 5 parity with nothing red
//! anywhere, and again on this tree, where the answer changes for two of them
//! and the last column is that second reading. Two of the first-tree runs
//! carried one extra temporary assertion apiece, which is how the values below
//! were read off; it recorded them and was removed.
//!
//! **Row 7 is row 6 with one line moved, and it is there because the ordering
//! is otherwise something a reader has to take on trust.** The pin looks each
//! declared id up with `find`, so it compares whichever detector answers to that
//! id *first* and never learns there is a second. Put the collision after the
//! genuine detector and the pin is green (row 6); put the same detector before
//! it and the pin reddens naming `pii.email's pattern moved` — 59 passed, 2
//! failed, the pin and the anchor. So the pin is not vacuous, and the order is
//! the whole of what decides whether it sees the collision at all.
//!
//! Row 1 is what this module buys: the accidental route, the one that
//! follows the shape of the two tables already here, does not build. Rows 2 and
//! 3 are what it cannot buy — Rust privacy runs one way, a child module
//! sees its parent's private items and never the reverse, so nothing arrangeable
//! here stops the crate root from writing its own `&[ShapeDetector]`, and
//! nothing stops an adder from widening a visibility. Both still compile, and
//! **the census stays green through both**, because a detector that does not
//! fire on `CENSUS_PROBE` is a detector the census never meets. That is exactly
//! the hole a verifier demonstrated.
//!
//! What reddens rows 2 and 3 is not here but at the oracle: `main_proposes`
//! refuses to pass an id no frozen list declares, so such a detector reddens the
//! differential on the first document it fires on instead of being carried into
//! the reference as one `main` proposes.
//!
//! **Rows 4, 5 and 6 are the price of that being a check on the id.** Row 4
//! never reaches the predicate at all: nothing here matches it, so nothing
//! observes it, and it is green on both trees. Rows 5 and 6 do reach it and are
//! *passed*, because its two inputs are an id and the pack set and `pii.email`
//! is declared — it has no way to ask which table the span came from. Row 6 is
//! the one to read twice: the collision needs no crate-root table and no
//! widened visibility, only a second pattern under an id this file already
//! carries, placed after the genuine one so the pin's `find` still meets the
//! genuine one first. `report.by_detector` is keyed by id, so that is an
//! ordinary refactor and not an attack. Both leave the reference masking
//! `x h.com y` — as `[REDACTED:vendor]` from the crate root, `[REDACTED:pii]`
//! from here — where `main` masks nothing, and the subject masks it too, so the
//! two cancel: the swallowed-token mechanism, reached through the id rather
//! than the table.
//!
//! The one red in the last column is
//! `neither_side_masks_the_bare_host_the_collision_plants_moved`, added in the
//! same commit as these three rows, and **it is narrow on purpose and should be
//! read as narrow**: it records what this tree does with `x h.com y`, so it
//! reddens the two plants that moved that string and nothing else — 60 passed,
//! 1 failed, both times. It is a baseline that makes rows 5 and 6 reproducible,
//! not a fix for them.
//!
//! **Row 8 is that last sentence turned from a prediction into a measurement.**
//! It is row 6 under a different id — a second `pii.ipv4` after the genuine one,
//! matching `h2.com` rather than the `h.com` the anchor holds — and it leaves
//! **the whole package green: 61 lib passed, 0 failed, 5 parity, 0 failed,
//! nothing red anywhere**, while both sides mask `x h2.com y` to the same
//! `x [REDACTED:pii] y`. Rows 5 and 6 at least moved a string one test had
//! written down; row 8 moves none, so no test in the crate observes it. Any
//! other id split into two patterns goes past in the same way, which is why the
//! anchor is a record of these plants and not an answer to them.
//!
//! Read this module as the thing that makes the mistake hard to make, that
//! predicate as the thing that makes it hard to keep, and rows 4–6 and 8 as the
//! demonstrated part of what neither does. The list is open — it holds what has
//! been planted, not what exists.
//!
//! **One spelling note that covers every `[REDACTED:pii]` above, and the same
//! renderings in the `lib.rs` texts that cite this table** (`coalesce`'s
//! examples, `the_reference_pipeline_is_still_the_one_main_runs`,
//! `masked_dropping_the_loser` and
//! `nothing_survives_here_that_dropping_the_loser_masked`): those outputs are
//! recorded **as they were measured**, before the owner gave the PII
//! family a label per detector. Re-run today, rows 5 and 6 read
//! `[REDACTED:pii.email]` and row 8 reads `[REDACTED:pii.ipv4]`. The finding is
//! untouched and does not depend on the spelling: both sides call the one
//! `mask`, so both emit the same label whatever it is, and it is that identity —
//! not the string — that makes the relaxation cancel. The counts (`61` lib
//! passed, `5` parity) belong to the tree they were taken on and are not
//! re-derived here.

use std::sync::OnceLock;

use std::borrow::Cow;

use regex::Regex;

use crate::{
    is_card_pan, is_e164_phone, is_exit_code_phone, is_labelled_taxpayer_id, is_nanp_phone,
    is_street_address, is_trunked_national_phone, is_us_taxpayer_id, DetectorClass, ShapeDetector,
    SpanGate,
};

/// **The credential key vocabulary, chosen by measurement and not by taste.**
///
/// Every candidate word was counted in the colon form over this repository's
/// own tracked text (493 files, the same corpus
/// `benchmarks/redaction-fp-baseline.md` records) before any of it was written,
/// and three words that a taste-driven list would obviously contain are
/// **excluded** because of what that count said:
///
/// * bare `token` — **44** hits, every sampled one a Rust type annotation
///   (`token: &str`, `{token:?}`). A field named `token` is ordinary code;
/// * bare `secret` — **7** hits, every one a type annotation
///   (`secret: &[u8]`, `SECRET: &str`);
/// * `pwd` — **1** hit, and it is `$PWD:/repo` in a CI workflow. `PWD` is a
///   shell variable before it is a password.
///
/// The words that survive cost **zero** hits on the same corpus. The compound
/// forms (`secret_key`, `client_secret`, `api_key`) are kept because the
/// compound is unambiguous where the bare word is not.
///
/// This is the colon form only. The `=` form for this wider vocabulary was
/// measured too — **4** hits, two of them the ceiling lines of the baseline
/// artifact itself (`dsn.password = 10`) — and is deliberately not claimed
/// here: `dsn.password` owns the `=` separator for `password`/`pwd`, and
/// widening it is a separate decision with a separate number.
const CREDENTIAL_KEYS: &str = "password|passwd|api[_-]?key|access[_-]?key|secret[_-]?key|\
                               auth[_-]?token|access[_-]?token|refresh[_-]?token|\
                               client[_-]?secret|api[_-]?secret";

/// The one pattern-compiling closure of `pass1_detectors`, as its families
/// receive it: id, class, pattern and capture group.
///
/// Handing the closure down keeps the `expect` that compiles a built-in
/// pattern, and the allow that covers it, in the table function alone.
type Pass1Detector = dyn Fn(&'static str, DetectorClass, &str, usize) -> ShapeDetector;

/// The one pattern-compiling closure of `pii_detectors`, as its families
/// receive it: id, pattern, capture group and optional gate.
type PiiDetector = dyn Fn(&'static str, &str, usize, Option<SpanGate>) -> ShapeDetector;

/// The gated pattern-compiling closure of `credential_detectors`, as its
/// key/value family receives it: id, pattern, capture group and gate.
type GatedCredentialDetector = dyn Fn(&'static str, &str, usize, SpanGate) -> ShapeDetector;

// The only `expect`s here compile compile-time-*constant* regexes; a bad
// built-in pattern is a build-time bug, surfaced loudly on first use, never a
// runtime crash on user input.
#[allow(clippy::expect_used)]
fn pass1_detectors() -> &'static [ShapeDetector] {
    static CELL: OnceLock<Vec<ShapeDetector>> = OnceLock::new();
    CELL.get_or_init(|| {
        // `expect` here is at init of compile-time-constant patterns; a bad
        // built-in regex is a build-time bug, surfaced loudly on first use.
        let d = |id: &'static str, class: DetectorClass, re: &str, group: usize| ShapeDetector {
            id: Cow::Borrowed(id),
            class,
            re: Regex::new(re).expect("valid built-in regex"),
            group,
            gate: None,
        };
        let mut detectors = vendor_charset_detectors(&d);
        detectors.extend(connection_credential_detectors(&d));
        // PEM private-key block (DOTALL)
        detectors.push(d(
            "key.pem_block",
            DetectorClass::PrivateKey,
            r"(?s)-----BEGIN[^-]*PRIVATE KEY-----.*?-----END[^-]*PRIVATE KEY-----",
            0,
        ));
        detectors
    })
}

/// The vendor tokens pass 1 knows by a fixed prefix and charset, each masked
/// whole.
///
/// They open pass 1 in their reviewed order, so `pass1_detectors` calls this
/// family before any other.
fn vendor_charset_detectors(d: &Pass1Detector) -> Vec<ShapeDetector> {
    vec![
        d(
            "vendor.aws.access_key_id",
            DetectorClass::VendorSecret,
            r"AKIA[0-9A-Z]{16}",
            0,
        ),
        d(
            "vendor.github.pat",
            DetectorClass::VendorSecret,
            r"ghp_[A-Za-z0-9]{36}",
            0,
        ),
        d(
            "vendor.stripe.secret",
            DetectorClass::VendorSecret,
            r"sk_(?:live|test)_[A-Za-z0-9]{16,}",
            0,
        ),
        d(
            "vendor.slack.token",
            DetectorClass::VendorSecret,
            r"xox[baprs]-[A-Za-z0-9-]{10,}",
            0,
        ),
        d(
            "vendor.openai.key",
            DetectorClass::VendorSecret,
            r"sk-[A-Za-z0-9]{20,}",
            0,
        ),
        d(
            "vendor.google.api_key",
            DetectorClass::VendorSecret,
            r"AIza[0-9A-Za-z_\-]{35}",
            0,
        ),
    ]
}

/// The passwords a URI or a DSN carries, in their reviewed order: the
/// credentialed URI, its empty-user form, then the `=` password.
///
/// `uri.credentials` and `dsn.password` are the reference pipeline's, which is
/// why [`credential_detectors`] is a table of its own proposed after them.
fn connection_credential_detectors(d: &Pass1Detector) -> Vec<ShapeDetector> {
    vec![
        // credentialed URI: mask only the user:pass group, keep scheme+host
        d(
            "uri.credentials",
            DetectorClass::CredentialedUri,
            r"://([^/\s@:]+:[^/\s@]+)@",
            1,
        ),
        // The same URI with no user part -- `scheme://:password@host`, which
        // is how a Redis or an AMQP URL carries a password. The rule above
        // requires at least one non-colon user character and therefore
        // structurally cannot reach it; the two are disjoint, since this one
        // needs a `:` immediately after the `//`.
        //
        // **It masks its whole match, and that is a measurement rather than
        // a preference.** Written the way `uri.credentials` is -- capture
        // the password, keep the delimiters -- the span carries a `parse`,
        // and a parse displaces the `pii.email` span that reads
        // `password@host` out of the same bytes. On a dotted host that
        // email span is the only thing masking the host on `main`, so the
        // capture-group form *published a host main masked* and
        // `nothing_survives_here_that_dropping_the_loser_masked` reddened on
        // `https://:AAAAAAAA@db.example.com`. Masking the whole match keeps
        // this branch strengthening-only: the loser is carved instead of
        // displaced, the scheme and the host after `@` survive on a dotless
        // host, and on a dotted one the email detector still claims what it
        // claimed before.
        d(
            "uri.credentials_empty_user",
            DetectorClass::CredentialedUri,
            r"://:[^/\s@]+@",
            0,
        ),
        // JDBC/DSN password value
        d(
            "dsn.password",
            DetectorClass::JdbcDsn,
            r"(?i)(?:password|pwd)=([^;&\s]+)",
            1,
        ),
    ]
}

#[allow(clippy::expect_used)] // compile-time-constant regexes only (see above)
fn pii_detectors() -> &'static [ShapeDetector] {
    static CELL: OnceLock<Vec<ShapeDetector>> = OnceLock::new();
    CELL.get_or_init(|| {
        let d = |id: &'static str, re: &str, group: usize, gate: Option<SpanGate>| ShapeDetector {
            id: Cow::Borrowed(id),
            class: DetectorClass::Pii,
            re: Regex::new(re).expect("valid built-in regex"),
            group,
            gate,
        };
        let mut detectors = vec![
            d(
                "pii.email",
                r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}",
                0,
                None,
            ),
            d("pii.ipv4", r"\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b", 0, None),
        ];
        detectors.extend(taxpayer_id_detectors(&d));
        detectors.extend(phone_detectors(&d));
        // The street address, layer 8's third shape and the only one here
        // with neither a checksum nor an issuing authority behind it. The
        // suffix vocabulary is the wide one BECAUSE it is the one that was
        // measured: zero hits over 493 files of this repository's tracked
        // text, and over the whole diff history only the parity row's own
        // literal. What that measurement cannot see is prose, which is
        // recorded as a parity row of its own rather than implied here.
        detectors.push(d(
            "pii.street_address",
            r"\b[0-9]{1,5}[a-zA-Z]?[ ](?:[A-Z][A-Za-z.'\-]{1,15}[ ]){1,3}(?:Street|St|Avenue|Ave|Road|Rd|Boulevard|Blvd|Lane|Ln|Drive|Dr|Court|Ct|Place|Pl|Terrace|Ter|Parkway|Pkwy|Highway|Hwy|Square|Sq|Way)\b\.?",
            0,
            Some(is_street_address),
        ));
        detectors.extend(card_pan_detectors(&d));
        detectors
    })
}

/// The US taxpayer id in its two writings: grouped digits on their own, and
/// nine bare digits that only a label makes an identifier.
fn taxpayer_id_detectors(d: &PiiDetector) -> Vec<ShapeDetector> {
    vec![
        d(
            "pii.us_taxpayer_id",
            r"\b[0-9]{3}[- ][0-9]{2}[- ][0-9]{4}\b",
            0,
            Some(is_us_taxpayer_id),
        ),
        // Value-only masking, key preserved: the key is what makes nine bare
        // digits an identifier, so it is context and not subject.
        //
        // The three parts that decide what this reaches, each measured:
        // the label may be preceded only by a non-alphanumeric (so `ssn`
        // inside a longer word is not a label, while `USER_SSN` still is);
        // an optional quote sits on either side of the separator, because
        // the capture document is JSON and `{"ssn":"123456789"}` is the
        // shape it actually carries; and the separator itself is optional,
        // because `SSN 123456789` labels its value with a space.
        //
        // The separator may also cross **one** line break, which this rule
        // refused until it was measured. The refusal reasoned that `\n` in
        // that class lets a heading claim the first number of the table
        // under it; the label vocabulary is narrow enough that it does not.
        // Allowing one newline finds exactly the same hits over this
        // repository's tracked text (zero) and over its whole diff history
        // (the seeded fixtures only). One, not many: `[\r\n]*` would let a
        // heading reach a number fifty lines away, and a markdown table
        // cell cannot be reached at all because `|` is in neither class.
        d(
            "pii.us_taxpayer_id_labelled",
            r#"(?i)(?:\A|[^a-z0-9])(?:ssn|social[ _\-]?security|itin|tax[ _\-]?id|taxpayer[ _\-]?id|national[ _\-]?id)[a-z_ ]{0,12}["']?[ \t]*(?:[:=][ \t]*)?\r?\n?[ \t]*["']?([0-9]{9})\b"#,
            1,
            Some(is_labelled_taxpayer_id),
        ),
    ]
}

/// The phone numbers, in their reviewed order: E.164, the two national
/// spellings, then NANP.
///
/// Each gate checks what its pattern cannot, such as group counts and digit
/// totals.
fn phone_detectors(d: &PiiDetector) -> Vec<ShapeDetector> {
    vec![
        d(
            "pii.phone_e164",
            r"\+[1-9][0-9]{0,2}(?:[ .\-]?\(?[0-9]{1,5}\)?){1,7}",
            0,
            Some(is_e164_phone),
        ),
        // The two national spellings no `+`-anchored or NANP-anchored shape
        // can reach, each anchored on the prefix that is its evidence: a
        // trunk `0` and the ITU exit code `00`. Both measured zero over
        // this repository's own text before they were written; the gates
        // carry what the prefixes cannot (group counts, digit totals).
        d(
            "pii.phone_uk_national",
            r"\b0[1-9][0-9]{1,2}[ \-][0-9]{3,4}[ \-][0-9]{4}\b",
            0,
            Some(is_trunked_national_phone),
        ),
        d(
            "pii.phone_intl_zero_prefix",
            r"\b00[1-9][0-9]{0,2}(?:[ .\-][0-9]{1,5}){2,6}\b",
            0,
            Some(is_exit_code_phone),
        ),
        d(
            "pii.phone_nanp",
            r"(?:1[ \-])?(?:\([2-9][0-9]{2}\)[ ]?|[2-9][0-9]{2}[ \-])[2-9][0-9]{2}[ \-][0-9]{4}",
            0,
            Some(is_nanp_phone),
        ),
    ]
}

/// The card numbers, both behind the checksum gate `is_card_pan`: the
/// uniformly grouped comma writing first, then the general window.
fn card_pan_detectors(d: &PiiDetector) -> Vec<ShapeDetector> {
    vec![
        // The comma separator this detector refuses in general, allowed in
        // the ONE writing where it is not ambiguous: four groups of exactly
        // four digits. The refusal is right and is measured -- adding `,`
        // to the separator class below takes this detector from 0 to 281
        // hits over this repository's tracked text, because a
        // comma-separated coordinate list forms Luhn-valid 16-digit
        // windows. What the checksum cannot supply, the GROUPING can: a
        // card is written in uniform groups and a coordinate list is not.
        // Measured at the uniform grouping: zero hits over the tracked
        // text, and over the whole diff history only this row's own
        // literal.
        d(
            "pii.card_pan_grouped",
            r"\b[0-9]{4},[0-9]{4},[0-9]{4},[0-9]{4}\b",
            0,
            Some(is_card_pan),
        ),
        d(
            "pii.card_pan",
            // `\b` on both ends selects the *window* -- inside a longer digit
            // run no 13-19 window has a boundary, so a 25-digit identifier
            // never becomes a candidate, and a card followed by a further
            // number still matches at its own length rather than greedily
            // swallowing the neighbour and failing the length check.
            r"\b[0-9](?:[ \-.]{0,2}[0-9]){12,18}\b",
            0,
            Some(is_card_pan),
        ),
    ]
}

/// **Layers 5 and 7 — connection strings and bounded credential key/values.**
///
/// Its own table for the same reason [`provider_detectors`] is: these are a
/// layer, not more lines in `pass1_detectors`. `dsn.password` and
/// `uri.credentials` stay where they are — they are the reference pipeline's
/// and their order relative to each other is pinned by review — so everything
/// here is proposed *after* all of them and the existing tie-breaks do not move.
#[allow(clippy::expect_used)] // compile-time-constant regexes only (see above)
fn credential_detectors() -> &'static [ShapeDetector] {
    static CELL: OnceLock<Vec<ShapeDetector>> = OnceLock::new();
    CELL.get_or_init(|| {
        let g = |id: &'static str, re: &str, group: usize, gate: SpanGate| ShapeDetector {
            id: Cow::Borrowed(id),
            class: DetectorClass::JdbcDsn,
            re: Regex::new(re).expect("valid built-in regex"),
            group,
            gate: Some(gate),
        };
        let d = |id: &'static str, re: &str, group| ShapeDetector {
            id: Cow::Borrowed(id),
            class: DetectorClass::JdbcDsn,
            re: Regex::new(re).expect("valid built-in regex"),
            group,
            gate: None,
        };
        let mut detectors = vec![
            // A DSN password value when it is QUOTED. `dsn.password` stops at
            // the first space, so `Password="hunter 2 space"` shipped
            // everything after `hunter`; this takes the quotes and the gate
            // narrows past them, so the whole value goes.
            g(
                "dsn.password_quoted",
                r#"(?i)(?:password|pwd)=("[^"\n]+"|'[^'\n]+')"#,
                1,
                crate::unquoted_value,
            ),
            // Layer 5's whole-string half: once a real password is present, the
            // host+user+password triple is the secret, not just the password.
            //
            // Two shapes, both anchored on something only a connection string
            // has, because the anchor is what keeps this from swallowing
            // ordinary text: a driver scheme (`jdbc:`/`odbc:`) carried through
            // to its password, and a semicolon-delimited `key=value` list that
            // OPENS on a connection-string keyword and reaches a password.
            // That opening keyword is PRECISION, and it is not speed: it stops
            // an arbitrary `a=b;` run from being a candidate, and it was added
            // in the belief that it would also cut the scan cost. Measured, it
            // does not -- 27.9 s anchored against 28.6 s unanchored over the
            // same 7.5 MB in the same session, which is inside the noise. The
            // anchor stays for what it does do; the claim it was added for is
            // struck rather than left standing. Neither can match `scheme://user:pass@host` -- that
            // stays `uri.credentials`, which keeps the host on purpose -- and
            // neither matches a bare `password=x`, which stays `dsn.password`.
            // Every value class is newline-bounded so a stray `key=value;` and
            // a password fifty lines later cannot form one match, and every
            // password value must be non-empty: `Password=;` hides nothing, so
            // masking around it would only cost the reader the rest of the line.
            //
            // Measured over this repository's own 493 files before it was
            // written: zero hits for both shapes.
            d(
                "dsn.connection_string",
                r#"(?i)(?:\b(?:jdbc|odbc):[^\s"']*(?:password|pwd)=[^\s"';]+|\b(?:server|data ?source|initial catalog|integrated security|database|driver|host|port|uid|user ?id|dsn)=[^;\n]*;(?:[A-Za-z][A-Za-z ]*=[^;\n]*;){0,7}[ \t]*(?:password|pwd)=(?:"[^"\n]+"|'[^'\n]+'|[^;\s]+);?)"#,
                0,
            ),
        ];
        detectors.extend(credential_key_value_detectors(&g));
        detectors
    })
}

/// Layer 7: a credential written as a key and its value.
///
/// Both carry the `unquoted_value` gate, so a quoted value is masked without
/// its quotes.
fn credential_key_value_detectors(g: &GatedCredentialDetector) -> Vec<ShapeDetector> {
    vec![
        // Layer 7, the colon half: a bounded credential key/value written
        // the way YAML, a log line and ordinary prose write it. The `=`
        // half is `dsn.password`, which is the reference pipeline's and is
        // frozen; this is the separator it never had. The vocabulary is
        // measured rather than guessed -- see `CREDENTIAL_KEYS`.
        g(
            "kv.credential_colon",
            &format!(
                r#"(?i)(?:\A|[^a-z0-9])(?:{CREDENTIAL_KEYS})["']?[ \t]*:[ \t]*("[^"]*"|'[^']*'|[^\s;,]+)"#
            ),
            1,
            crate::unquoted_value,
        ),
        // The same layer, in the one place a credential carries its own
        // word instead of a key name. The scheme word is kept: it is what
        // makes the line diagnosable, and it is also what makes this rule
        // precise enough to cost nothing.
        g(
            "kv.authorization_header",
            r"(?i)(?:proxy-)?authorization[ \t]*:[ \t]*(?:basic|bearer|digest|token)[ \t]+(\S+)",
            1,
            crate::unquoted_value,
        ),
    ]
}

/// **Layer 3 — deterministic provider token prefixes.**
///
/// Its own table because it is its own layer, and because the way a third table
/// gets added is the way the other two are already reached: into the array
/// [`builtin_shape_tables`] returns, where the census enumerates it.
///
/// A prefix+length rule needs neither entropy nor a surrounding key name, which
/// is the whole reason the layer exists — every shape here is a real credential
/// that both the vendor charsets above and the entropy floor structurally miss.
/// `sk-ant-` is the sharpest case: `vendor.openai.key` is `sk-` followed by a
/// character class that the `ant-` infix ends after three characters, so an
/// Anthropic key passed this pipeline untouched, in a tool for Claude Code
/// sessions.
///
/// Each pattern is the vendor's own published prefix; none is copied from a
/// foreign rule corpus (recorded as blocked on corpus licensing). Only
/// *secret* prefixes are here: Supabase's publishable key is public by design
/// and masking it would cost a diagnosable transcript for nothing.
#[allow(clippy::expect_used)] // compile-time-constant regexes only (see above)
fn provider_detectors() -> &'static [ShapeDetector] {
    static CELL: OnceLock<Vec<ShapeDetector>> = OnceLock::new();
    CELL.get_or_init(|| {
        let d = |id: &'static str, re: &str| ShapeDetector {
            id: Cow::Borrowed(id),
            class: DetectorClass::VendorSecret,
            re: Regex::new(re).expect("valid built-in regex"),
            group: 0,
            gate: None,
        };
        vec![
            d(
                "provider.supabase.secret_key",
                r"sb_secret_[A-Za-z0-9_\-]{16,}",
            ),
            d("provider.anthropic.api_key", r"sk-ant-[A-Za-z0-9_\-]{16,}"),
            d(
                "provider.github.fine_grained_pat",
                r"github_pat_[A-Za-z0-9_]{22,}",
            ),
        ]
    })
}

/// **Every built-in shape-detector table `propose` runs**, as one value.
///
/// The census reads this function and `propose` destructures what it returns,
/// so the two cannot disagree about a table that is *in* it. The agreement is at
/// the granularity the census has, which is the **id**: it reads ids off these
/// tables and deduplicates them, so a *second* detector under an id already
/// present is not a disagreement it is able to have — rows 6 and 8 of the module
/// doc's plant table, both of them inside a table this function returns. What
/// keeps a table from staying *out* of the array
/// is the module boundary above, not this signature — see the module doc for
/// both halves neither covers.
///
/// The hole that motivates the coupling is demonstrable rather than
/// hypothetical: the oracle's `main_proposes` used to answer **true** for any id
/// the census did not contain (an org pack's rule is not a built-in), so a
/// detector living in a table the census forgot to read was carried into the
/// reference as if `main` proposed it — the drift
/// `the_reference_pipeline_is_still_the_one_main_runs` exists to prevent, at the
/// one place that test could not see.
pub(super) fn builtin_shape_tables() -> [&'static [ShapeDetector]; 4] {
    [
        pass1_detectors(),
        provider_detectors(),
        credential_detectors(),
        pii_detectors(),
    ]
}

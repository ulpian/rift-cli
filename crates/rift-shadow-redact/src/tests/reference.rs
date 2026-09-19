//! The differential's reference half: the detector set `main` runs, frozen
//! by id, pattern, group and gate; and the drop-the-loser and scanning
//! transcriptions of the loop, kept as literal second implementations so a
//! rewrite of the pipeline cannot move its own oracle.

use std::collections::BTreeMap;

use super::fixtures::masked;
use crate::builtin_tables::builtin_shape_tables;
use crate::entropy::{entropy_spans, shannon_bits, ENTROPY_MIN_BITS, ENTROPY_MIN_LEN};
use crate::marks::{by_precedence, mask, precedence, propose};
use crate::span::{MatchedAgainst, ShapeDetector, Span};
use crate::{normalize, sha256, DetectorClass, RedactionReport, RulePackSet, MAX_PASSES};

/// Every built-in detector id this tree can propose, read off
/// [`builtin_shape_tables`] — the same value [`propose`] destructures — and
/// off the entropy layer, by making it fire. Naming the tables a second time
/// here is what left a table `propose` reads able to stay out of the census;
/// reading the one value is what keeps this function and `propose` from
/// disagreeing about a table that is *in* it. Nothing here can see a table
/// that is not — the module boundary makes reaching one the accidental way a
/// compile error, and [`main_proposes`] catches the deliberate ways **whose
/// ids no frozen list declares, on the first document such a detector fires
/// on**. That qualifier is load-bearing and was left off here while being
/// stated correctly twice elsewhere: a detector that fires on nothing the
/// suite contains is never proposed, so it never reaches that predicate and
/// nothing observes it at all — row 4 of the [`builtin_tables`] plant table,
/// green on both trees. A detector bearing an id a frozen list *does* declare
/// is passed on the id alone, so this census cannot see that either — rows 5,
/// 6 and 8. Both are entries in the open blind-spot list on
/// `the_census_matches_what_propose_emits_on_the_probe`.
pub(super) fn builtin_detector_ids() -> Vec<String> {
    let mut ids: Vec<String> = builtin_shape_tables()
        .into_iter()
        .flat_map(|table| table.iter().map(|d| d.id.to_string()))
        .collect();
    ids.extend(
        entropy_spans("Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4l")
            .into_iter()
            .map(|s| s.id),
    );
    ids.sort();
    ids.dedup();
    ids
}

/// **The detector ids `main` proposes**, transcribed from
/// `f14e61e:crates/rift-shadow-redact/src/lib.rs` and frozen here.
const MAIN_DETECTOR_IDS: [&str; 12] = [
    "vendor.aws.access_key_id",
    "vendor.github.pat",
    "vendor.stripe.secret",
    "vendor.slack.token",
    "vendor.openai.key",
    "vendor.google.api_key",
    "uri.credentials",
    "dsn.password",
    "key.pem_block",
    "entropy.high",
    "pii.email",
    "pii.ipv4",
];

/// The detector ids this branch adds. Declared rather than inferred: with
/// [`MAIN_DETECTOR_IDS`] these two lists must **partition** the built-in
/// census, so a *new id* cannot join it without a decision about which side
/// of the differential it belongs on
/// (`the_reference_pipeline_is_still_the_one_main_runs`).
///
/// **The partition is over ids, and the census is the ids [`CENSUS_PROBE`]
/// makes fire, deduplicated** — so this is weaker than "a detector cannot
/// join the pipeline without a decision", which is what this doc used to
/// say. Four planted detectors joined without one and left this assertion
/// green: rows 4, 5, 6 and 8 of the [`builtin_tables`] plant table. Row 4
/// contributes no id at all, because it fires on nothing here; rows 5, 6 and
/// 8 contribute no *new* id, because `pii.email` and `pii.ipv4` are already
/// elements — whether the collision sits inside the enumerated tables (6, 8)
/// or in a table outside them (5).
const DETECTOR_IDS_ADDED_HERE: [&str; 17] = [
    "pii.us_taxpayer_id",
    "pii.us_taxpayer_id_labelled",
    "pii.phone_e164",
    "pii.phone_nanp",
    "pii.card_pan",
    "pii.street_address",
    "kv.credential_colon",
    "kv.authorization_header",
    "provider.supabase.secret_key",
    "provider.anthropic.api_key",
    "provider.github.fine_grained_pat",
    "uri.credentials_empty_user",
    "dsn.password_quoted",
    "dsn.connection_string",
    "pii.phone_uk_national",
    "pii.phone_intl_zero_prefix",
    "pii.card_pan_grouped",
];

/// `(id, pattern, group, gated)` for every shape detector `main` runs, from
/// the same source. Ids alone do not pin the reference: a *shared*
/// detector whose pattern, capture group or gate moved here would move the
/// reference with it, silently, which is the failure this whole apparatus
/// exists to catch. It catches the shared detector *this list can find*:
/// the check below looks each id up with `find`, so it meets the first
/// detector answering to that id and pins that one. A **second** detector
/// under the same id, added after the first, is not compared with anything —
/// row 6 of the [`builtin_tables`] plant table, and the second entry in the
/// blind-spot list on `the_census_matches_what_propose_emits_on_the_probe`. Rows 6
/// and 7 are that one line moved: after the genuine detector this check is
/// green, before it this check reddens with `pii.email's pattern moved`.
const MAIN_SHAPE_DETECTORS: [(&str, &str, usize, bool); 11] = [
    ("vendor.aws.access_key_id", r"AKIA[0-9A-Z]{16}", 0, false),
    ("vendor.github.pat", r"ghp_[A-Za-z0-9]{36}", 0, false),
    (
        "vendor.stripe.secret",
        r"sk_(?:live|test)_[A-Za-z0-9]{16,}",
        0,
        false,
    ),
    (
        "vendor.slack.token",
        r"xox[baprs]-[A-Za-z0-9-]{10,}",
        0,
        false,
    ),
    ("vendor.openai.key", r"sk-[A-Za-z0-9]{20,}", 0, false),
    ("vendor.google.api_key", r"AIza[0-9A-Za-z_\-]{35}", 0, false),
    ("uri.credentials", r"://([^/\s@:]+:[^/\s@]+)@", 1, false),
    ("dsn.password", r"(?i)(?:password|pwd)=([^;&\s]+)", 1, false),
    (
        "key.pem_block",
        r"(?s)-----BEGIN[^-]*PRIVATE KEY-----.*?-----END[^-]*PRIVATE KEY-----",
        0,
        false,
    ),
    (
        "pii.email",
        r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}",
        0,
        false,
    ),
    ("pii.ipv4", r"\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b", 0, false),
];

/// Would `main`'s pipeline propose this span?
///
/// Built-ins only when [`MAIN_DETECTOR_IDS`] names them. An org pack's rule
/// is not a built-in and `main` runs whatever packs it is handed, so a pack
/// id passes through — but it passes through because **the pack set handed
/// in says it is one**, not because the census failed to mention it.
///
/// That difference is the whole of the finding. The old form answered *true*
/// for any id the census did not contain, so a built-in the census could not
/// see — a detector in a table nothing enumerates — was carried into the
/// reference as one `main` proposes, and every relaxation it caused
/// cancelled out on both sides of the differential. It is the half of the
/// census the module boundary cannot reach: privacy can stop `propose` from
/// naming a table *inside* [`builtin_tables`], and nothing can stop the
/// crate root from writing one outside it. Failing closed here catches both
/// **on the id**, on the first document such a detector fires on, wherever
/// its table lives.
///
/// **On the id is also the limit, and it was planted rather than reasoned
/// to.** The inputs are an id and the pack set, so there is nothing here to
/// ask which table a span came from: a second detector carrying an id a
/// frozen list already declares is passed for that reason alone, and the
/// reference adopts it. Rows 5 and 6 of the [`builtin_tables`] plant table
/// are that, planted twice — once from the crate root and once as a second
/// pattern inside the enumerated table — each leaving every guard in the
/// crate green **at `767ef03`** while the reference masks a bare host `main`
/// masks nothing in. On this tree the one thing they redden is
/// `neither_side_masks_the_bare_host_the_collision_plants_moved`, which
/// holds the baseline they moved off and is a record of them, not an answer
/// to them.
///
/// Output-neutral for every caller that exists: with an empty pack set every
/// id [`propose`] emits is in one of the two frozen lists, so the assertion
/// never fires and the answer is `MAIN_DETECTOR_IDS.contains(id)` either
/// way. Held by `an_undeclared_builtin_stops_the_reference` and
/// `a_pack_rule_passes_through_because_the_pack_set_says_so`.
fn main_proposes(id: &str, packs: &RulePackSet) -> bool {
    if packs.extra.iter().any(|d| d.id == id) {
        return true;
    }
    assert!(
        MAIN_DETECTOR_IDS.contains(&id) || DETECTOR_IDS_ADDED_HERE.contains(&id),
        "{id} is proposed by this tree, is not a rule of the packs handed in, \
         and no frozen list declares it — the reference cannot say whether main \
         proposes it and would have carried it as one main does"
    );
    MAIN_DETECTOR_IDS.contains(&id)
}

/// The reference stops rather than guessing. An id that no pack supplied and
/// no frozen list declares is exactly what a table nobody enumerated
/// produces, and the old filter answered *true* for it — which made every
/// relaxation that detector caused cancel out on both sides.
#[test]
#[should_panic(expected = "no frozen list declares it")]
fn an_undeclared_builtin_stops_the_reference() {
    let _ = main_proposes("pii.declared_by_nobody", &RulePackSet::builtin());
}

/// The other side of the same predicate, so the pack branch is exercised on
/// purpose rather than only by the census miss that used to reach it.
#[test]
fn a_pack_rule_passes_through_because_the_pack_set_says_so() {
    let packs = RulePackSet::builtin()
        .with_vendor_pattern("acme.rule", r"ACME-[0-9]{4}", "acme@1")
        .unwrap();
    assert!(
        main_proposes("acme.rule", &packs),
        "a pack's own rule is one main runs"
    );
    assert!(main_proposes("pii.email", &packs), "a detector main has");
    assert!(
        !main_proposes("pii.card_pan", &packs),
        "a detector this branch added is not one main proposes"
    );
}

/// **The baseline the two collision plants moved off**, recorded so that
/// rows 5 and 6 of the [`builtin_tables`] table can be re-derived against a
/// value rather than a memory. On this tree neither the reference nor the
/// pipeline touches `x h.com y`: no detector either one runs matches a bare
/// host. Under row 5 both sides return `x [REDACTED:vendor] y` and under row
/// 6 both return `x [REDACTED:pii] y` — each measured on both sides, and in
/// each case the *same* value, which is exactly why the differential stays
/// green and the relaxation cancels instead of showing up.
///
/// The last assertion is the mechanism in one line: `pii.email` is passed
/// because a frozen list declares that id, and nothing in the call says
/// which table proposed it. This is a calibration anchor and a tripwire — it
/// reddens if this crate grows a bare-host detector, and if the predicate
/// ever learns to tell two detectors under one id apart it reddens too and
/// the blind-spot list gets rewritten rather than left stale. It is not a
/// proof that the list is closed; nothing here is.
#[test]
fn neither_side_masks_the_bare_host_the_collision_plants_moved() {
    let packs = RulePackSet::builtin();
    assert_eq!(
        masked_dropping_the_loser("x h.com y", &packs),
        "x h.com y",
        "the reference masked a bare host, which is what the plants made it do"
    );
    assert_eq!(
        masked("x h.com y"),
        "x h.com y",
        "the pipeline masked a bare host"
    );
    assert!(
        main_proposes("pii.email", &packs),
        "a declared id is passed on the id alone, whatever proposed the span"
    );
}

/// One line per built-in detector, so [`propose`] can be *asked* what it
/// emits instead of being described. Every id the census declares has to
/// come out of this document, which is what keeps the check below from
/// passing because the probe was blind.
const CENSUS_PROBE: &str = concat!(
    "aws AKIAIOSFODNN7EXAMPLE\n",
    "gh ghp_abcdefghijklmnopqrstuvwxyz0123456789\n",
    "stripe sk_live_abcdefghijklmnop\n",
    "slack xoxb-1234567890-abcdef\n",
    "openai sk-abcdefghijklmnopqrstuvwx\n",
    "google AIzaabcdefghijklmnopqrstuvwxyz0123456789\n",
    "postgres://admin:s3cr3tpw@db.example.com\n",
    "redis://:s3cr3tpw@localhost:6379/0\n",
    "jdbc:mysql://h/db?user=u&password=hunter2secret\n",
    "Server=db;User Id=sa;Password=\"hunter 2 space\";\n",
    "password=hunter2hunter2hunter2\n",
    "password: hunter2hunter2hunter2\n",
    "authorization: Basic YWRtaW46aHVudGVyMg==\n",
    "supabase sb_secret_aBcDaBcDaBcDaBcDaBcDaBcD\n",
    "anthropic sk-ant-aBcDaBcDaBcDaBcDaBcDaBcD\n",
    "gh github_pat_aBcDaBcDaBcDaBcDaBcDaBcD\n",
    "-----BEGIN PRIVATE KEY-----\nMIIBxyz\n-----END PRIVATE KEY-----\n",
    "token Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4l\n",
    "mail alice@example.com\n",
    "addr 10.0.0.1\n",
    "ssn 123-45-6789\n",
    "SSN=123456789\n",
    "phone +14155550199\n",
    "phone 415 555 0199\n",
    "phone 020 7123 4567\n",
    "phone 0044 20 7123 4567\n",
    "card 4111111111111111\n",
    "card 4111,1111,1111,1111\n",
    "visit 1600 Pennsylvania Ave before Friday\n",
    "ssn=\n123456789\n",
);

/// **The census and what [`propose`] emits on [`CENSUS_PROBE`] are the same
/// set** — which is what this asserts, and it is one third of the statement
/// its name used to make.
///
/// The whole statement is *the census is the id set `propose` can emit*, and
/// three separate things hold it, none of them this test alone:
///
/// 1. **[`builtin_tables`]** makes the accidental route a compile error: the
///    tables are private, [`builtin_shape_tables`] is the only item that
///    leaves the module, and a third table wired straight into [`propose`]
///    does not build. That is a default and not a lock — one hand-written
///    `pub(super)`, or a table declared at the crate root instead, compiles
///    again.
/// 2. **[`main_proposes`]** is what both of those run into. It refuses to
///    pass an id no frozen list declares, so the differential goes red on the
///    first document such a detector fires on. Measured rather than assumed:
///    the three plants are tabulated in the `builtin_tables` module doc, and
///    **this test stayed green through the two of them that compile** — a
///    detector that misses [`CENSUS_PROBE`] is one the census never meets.
/// 3. **This test** checks the remaining direction — that every id the
///    census declares is one `propose` actually emits, and *only* those. It
///    asserts equality rather than containment so that a detector which
///    stops firing fails here rather than shrinking the evidence in silence.
///
/// **Among the cases none of these three covers** — an open list, and every
/// entry in it was planted and run rather than argued for. One phrasing is
/// ruled out first because it is the one that comes to mind and it is wrong:
/// **not** a declared detector that stops firing — this test is an equality
/// and that reddens it.
///
/// * **A detector the census does not enumerate that fires on nothing the
///   suite contains.** It misses the probe, so (3) is blind; it is in no
///   table the census reads, so it is not in the set (3) compares against;
///   and it never reaches [`main_proposes`], so (2) never sees it. It is
///   invisible, and so is everything it does. Planted as row 4 of the
///   [`builtin_tables`] table: an undeclared crate-root detector whose
///   pattern occurs nowhere here — package green on both trees, 60 lib + 5
///   parity at `767ef03` and 61 + 5 here, the added test included.
/// * **A detector whose id collides with one a frozen list already
///   declares**, which [`main_proposes`] passes on the id alone: its inputs
///   are an id and the pack set, so it cannot ask which table proposed the
///   span. (1) does not apply — the id needs no new table — and this test
///   compares a *deduplicated* id set, so a second detector under a present
///   id changes nothing it looks at. Planted twice, rows 5 and 6: a
///   crate-root `pii.email` matching bare hosts, and the same pattern added
///   as a second `pii.email` *inside* the enumerated table, after the
///   genuine one — the order matters and row 7 is the control for it: moved
///   *before* the genuine one, the pin reddens with `pii.email's pattern
///   moved`. At `767ef03` both left the whole package green — the
///   suite's own 60 lib + 5 parity — while the reference masked `x h.com y`
///   that `main` masks nothing in, and the pipeline masked it to the same
///   value, so the two cancelled: the published-remainder mechanism,
///   restored through the id.
///   Both values were read off, `[REDACTED:vendor]` for the crate-root plant
///   and `[REDACTED:pii]` for the in-table one. On this tree they still leave
///   this test, the pin and the differential green, and redden only
///   `neither_side_masks_the_bare_host_the_collision_plants_moved`, which
///   holds that one string and is not a fix for the entry. Row 6 is why this
///   is not exotic: `report.by_detector` is keyed by id, so splitting one
///   detector into two patterns under one id is an ordinary refactor.
///   **Row 8 is why the anchor is a record and not an answer**: the same
///   shape under `pii.ipv4`, matching `h2.com` instead of the `h.com` the
///   anchor holds, leaves the whole package green — 61 lib passed / 0
///   failed, 5 parity / 0, nothing red anywhere — with both sides masking
///   `x h2.com y` to the same `x [REDACTED:pii] y`. Rows 5 and 6 at least
///   moved a string one test had written down; row 8 moves none.
///
/// The list is open. It says what has been demonstrated to get past all
/// three, not that nothing else does.
#[test]
fn the_census_matches_what_propose_emits_on_the_probe() {
    let mut emitted: Vec<String> = propose(CENSUS_PROBE, &RulePackSet::builtin())
        .into_iter()
        .map(|s| s.id)
        .collect();
    emitted.sort();
    emitted.dedup();
    assert_eq!(
        emitted,
        builtin_detector_ids(),
        "the ids propose emits and the ids the census holds are different sets"
    );
}

/// **The reference is only a reference while it is pinned to `main`**, and
/// the ids alone do not pin it. This checks three ways it could drift
/// without anyone noticing: a new detector nobody declared, a renamed one,
/// and a shared detector whose pattern/group/gate moved. The entropy layer
/// is included by making it fire rather than by naming its id, so a rename
/// there fails the census rather than quietly shrinking it.
///
/// **They are three ways, not the three ways**, which is what this doc used
/// to claim. A *second* detector under an id already declared drifts the
/// reference without touching any of them: it declares no new id, renames
/// nothing, and the pattern check below meets only the first detector
/// answering to each id (see [`MAIN_SHAPE_DETECTORS`], which states the same
/// bound about its own `find`). Row 8 of the [`builtin_tables`] plant table
/// is that, measured: a second `pii.ipv4` after the genuine one leaves the
/// whole package green — 61 lib passed / 0 failed, 5 parity / 0 — while both
/// sides mask `x h2.com y` to the same `x [REDACTED:pii] y`. Rows 5 and 6
/// are the same shape under `pii.email`. The list of ways is open.
///
/// What the reference still shares with the pipeline under test, and what is
/// done about each. `normalize`, `shannon_bits`, `precedence` and the pass
/// cap are pinned here by *value*; `mask` is pinned by value too, in
/// `mask_is_pinned_by_value_because_the_reference_calls_it` — that test is
/// new, and it exists because this doc used to list `mask` as pinned while
/// nothing in the crate asserted anything about it, which is the direction
/// that makes the guard look stronger than it was. `winners` is no longer
/// shared at all
/// ([`winners_dropping_the_loser`]). What is left shared and pinned only by
/// pattern is the detection itself — `propose`, `shape_spans`,
/// `entropy_spans` — which the frozen detector table pins by pattern, group
/// and gate, and `by_precedence`'s tie-break beyond `precedence` itself,
/// which is pinned only by review of the diff.
#[test]
fn the_reference_pipeline_is_still_the_one_main_runs() {
    let census = builtin_detector_ids();
    assert!(
        census.iter().any(|id| id == "entropy.high"),
        "the entropy probe stopped firing, so the census silently lost a layer: {census:?}"
    );
    for id in &census {
        let in_main = MAIN_DETECTOR_IDS.contains(&id.as_str());
        let added = DETECTOR_IDS_ADDED_HERE.contains(&id.as_str());
        assert!(
            in_main ^ added,
            "{id} is in neither frozen list or in both; until it is declared, \
             the differential cannot say whether main proposes it"
        );
    }
    for id in MAIN_DETECTOR_IDS
        .iter()
        .chain(DETECTOR_IDS_ADDED_HERE.iter())
    {
        assert!(
            census.iter().any(|c| c == id),
            "{id} is declared but no detector answers to it — a rename would \
             leave the reference running a detector main does not have"
        );
    }
    // Read through the one value `propose` runs, not through the tables by
    // name: naming them here is what let a table `propose` reads stay out of
    // this check, and after `builtin_tables` there is no other way to say it.
    let shapes: Vec<&ShapeDetector> = builtin_shape_tables().into_iter().flatten().collect();
    for (id, pattern, group, gated) in MAIN_SHAPE_DETECTORS {
        let d = shapes
            .iter()
            .find(|d| d.id == id)
            .unwrap_or_else(|| panic!("{id} is gone"));
        assert_eq!(d.re.as_str(), pattern, "{id}'s pattern moved");
        assert_eq!(d.group, group, "{id}'s capture group moved");
        assert_eq!(d.gate.is_some(), gated, "{id}'s gate moved");
    }
    assert_eq!(ENTROPY_MIN_LEN, 20, "the entropy floor moved");
    assert!(
        (ENTROPY_MIN_BITS - 4.0).abs() < f64::EPSILON,
        "the entropy floor moved"
    );
    assert_eq!(MAX_PASSES, 8, "the pass cap moved");
    assert_eq!(normalize("a\u{200b}b".as_bytes()), "ab");
    assert!(
        (shannon_bits("AKIAIOSFODNN7EXAMPLE") - 3.684_183_719_779_188_7).abs() < 1e-12,
        "shannon_bits moved: {}",
        shannon_bits("AKIAIOSFODNN7EXAMPLE")
    );
    assert_eq!(
        [
            precedence(DetectorClass::PrivateKey),
            precedence(DetectorClass::CredentialedUri),
            precedence(DetectorClass::JdbcDsn),
            precedence(DetectorClass::VendorSecret),
            precedence(DetectorClass::Pii),
            precedence(DetectorClass::Entropy),
        ],
        [5, 4, 4, 4, 2, 1],
        "precedence moved"
    );
}

/// The pipeline as it stood before a losing span was carved: identical in
/// every respect except that an overlap drops the loser **whole**. It is a
/// deliberate second implementation of the loop — a differential whose
/// reference is a copy of the thing under test measures nothing.
///
/// **It proposes `main`'s detector set, not this tree's**, and that is the
/// correction the drifted-reference finding forced. Calling [`propose`]
/// whole made the reference carry
/// the five detectors this branch adds, so its own fixpoint settled the
/// same `://user:123 45 6789@h.co` the pipeline under test settled, and
/// `nothing_survives_here_that_dropping_the_loser_masked` was structurally
/// unable to see any relaxation the new detector set caused — which was the
/// whole of the published-remainder class. The oracle had drifted with the
/// code it was the reference
/// for. The filter is a frozen list ([`MAIN_DETECTOR_IDS`]), held to the
/// live tables by `the_reference_pipeline_is_still_the_one_main_runs`.
///
/// One divergence is deliberate: `main` filters the chosen set through the
/// allowlist before masking and this does not. Every caller here passes
/// [`RulePackSet::builtin`], whose allowlist is empty, so the branch would
/// be untested code rather than fidelity.
///
/// **It no longer calls [`winners`]**, and that is this cycle's correction.
/// The winner selection was the one part of the reference borrowed from the
/// code under test, so rewriting `winners` would have moved reference and
/// subject together — the exact shape of that reject, one function to the
/// left. It
/// has its own now ([`winners_dropping_the_loser`]).
pub(super) fn masked_dropping_the_loser(raw: &str, packs: &RulePackSet) -> String {
    let mut text = normalize(raw.as_bytes());
    let mut report = RedactionReport {
        total_spans: 0,
        by_detector: BTreeMap::new(),
        by_class: BTreeMap::new(),
        bytes_masked: 0,
        allowlisted: 0,
        rule_pack_versions: packs.version_set(),
        input_sha256: sha256(raw.as_bytes()),
    };
    for _ in 0..MAX_PASSES {
        let mut proposed = propose(&text, packs);
        proposed.retain(|s| main_proposes(&s.id, packs));
        let mut chosen = winners_dropping_the_loser(proposed);
        chosen.sort_by_key(|s| s.start);
        let out = mask(&text, &chosen, &mut report);
        if out == text {
            break;
        }
        text = out;
    }
    text
}

/// **`main`'s winner selection, transcribed** — the reference's own, so that
/// a rewrite of [`winners`] cannot move the reference with it. Keep it a
/// literal second implementation: the overlap predicate is spelled out here
/// rather than calling [`overlaps`], and nothing in it may be replaced by a
/// call into the pipeline under test.
pub(super) fn winners_dropping_the_loser(mut spans: Vec<Span>) -> Vec<Span> {
    by_precedence(&mut spans);
    let mut chosen: Vec<Span> = Vec::new();
    for s in spans {
        if !chosen.iter().any(|c| c.start < s.end && s.start < c.end) {
            chosen.push(s);
        }
    }
    chosen
}

/// **`subtract`, transcribed** — the interval arithmetic [`ByteMarks`]
/// replaced, kept here as the reference half of the differential below and
/// nowhere else.
fn subtract_by_scanning(range: (usize, usize), taken: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut pieces = vec![range];
    for &(ts, te) in taken {
        let mut next = Vec::with_capacity(pieces.len() + 1);
        for (start, end) in pieces {
            if te <= start || ts >= end {
                next.push((start, end));
                continue;
            }
            if start < ts {
                next.push((start, ts));
            }
            if te < end {
                next.push((te, end));
            }
        }
        pieces = next;
        if pieces.is_empty() {
            break;
        }
    }
    pieces
}

/// **[`coalesce`], transcribed as it was before the marks**: scan the chosen
/// set for an overlap, scan it again for a parse, subtract all of it out of
/// the loser. Byte for byte the algorithm the marks replaced, kept as the
/// reference half of `the_marks_coalesce_the_way_the_scan_did` — the
/// in-tree, permanent form of the out-of-tree differential this rewrite was
/// measured with. Like [`winners_dropping_the_loser`], nothing in it may be
/// replaced by a call into the pipeline under test.
pub(super) fn coalesce_by_scanning(mut spans: Vec<Span>) -> Vec<Span> {
    let overlaps = |a: &Span, b: &Span| a.start < b.end && b.start < a.end;
    by_precedence(&mut spans);
    let mut chosen: Vec<Span> = Vec::new();
    for s in spans {
        if !chosen.iter().any(|c| overlaps(c, &s)) {
            chosen.push(s);
            continue;
        }
        let displaced_by_a_parse = chosen
            .iter()
            .any(|c| c.parse.is_some() && c.matched == MatchedAgainst::Input && overlaps(c, &s));
        if displaced_by_a_parse && s.class != DetectorClass::Entropy {
            continue;
        }
        let taken: Vec<(usize, usize)> = chosen.iter().map(|c| (c.start, c.end)).collect();
        for (start, end) in subtract_by_scanning((s.start, s.end), &taken) {
            chosen.push(Span {
                start,
                end,
                class: s.class,
                id: s.id.clone(),
                parse: None,
                matched: s.matched,
            });
        }
    }
    chosen.sort_by_key(|s| s.start);
    chosen
}

/// Every field of a span set, so two implementations can be compared without
/// [`Span`] growing a derive it does not otherwise need. Attribution is part
/// of the answer: a set with the same coverage and a different `id` or
/// `class` masks the same bytes under a different placeholder.
pub(super) fn shape_of(
    spans: &[Span],
) -> Vec<(usize, usize, DetectorClass, String, bool, MatchedAgainst)> {
    spans
        .iter()
        .map(|s| {
            (
                s.start,
                s.end,
                s.class,
                s.id.clone(),
                s.parse.is_some(),
                s.matched,
            )
        })
        .collect()
}

//! The layer-parity sweep, as an executable table.
//!
//! The legacy backend's redactor runs nine layers over a JSON-aware walker. This file is the
//! sweep of those layers against the native pipeline: one row per behaviour, each
//! row pinning **what the native pipeline does today** (asserted) next to **the
//! sweep's verdict** on that behaviour (documentation the later security work consumes).
//!
//! It is a characterisation test on purpose. A row cannot drift silently in
//! either direction: closing a gap flips the asserted outcome and the test fails
//! until the verdict is updated too, so the table stays an honest ledger of where
//! parity actually stands. [`OPEN_GAPS`] pins the count, which is the handle "no `Gap` row
//! remains" needs.
//!
//! Source consulted read-only at the pinned tag
//! `v0.9.1-nightly.202608050700.76dff6a81`: `redact/redact.go`, `redact/pii.go`,
//! `redact/providers.go`, `redact/custom.go`, `redact/packs.go`. Behaviour and
//! lessons only — no Go code and no foreign rule corpus is reproduced here. The
//! ~260-format vendor corpus lives in an external module whose licence could not
//! be established from the permitted sources (recorded as blocked on corpus licensing), so
//! every native pattern is derived from the vendor's own
//! published key-format documentation instead.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use rift_core::SecretBytes;
use rift_shadow_redact::redact;

/// Which of the legacy backend's layers a row exercises. Numbers are the layer indices in
/// `redact.go`'s `detectAllLayers`; [`Layer::JsonWalker`] is the structural
/// substrate those layers run inside (`JSONLContent`), not a detector.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layer {
    /// 1 — Shannon entropy over token runs.
    Entropy,
    /// 2 — the vendor pattern corpus (~260 formats).
    VendorCorpus,
    /// 3 — deterministic provider token prefixes.
    ProviderPrefix,
    /// 4 — credentialed URIs.
    CredentialedUri,
    /// 5 — JDBC / DSN / semicolon connection strings.
    ConnectionString,
    /// 6 — user-defined custom rules and rule packs.
    CustomRules,
    /// 7 — bounded credential key/value pairs.
    CredentialKv,
    /// 8 — opt-in PII (email / phone / address).
    Pii,
    /// 9 — the batched heavy-model privacy pass.
    HeavyModelPass,
    /// The JSON-aware walker the eight regex layers run inside.
    JsonWalker,
}

const ALL_LAYERS: [Layer; 10] = [
    Layer::Entropy,
    Layer::VendorCorpus,
    Layer::ProviderPrefix,
    Layer::CredentialedUri,
    Layer::ConnectionString,
    Layer::CustomRules,
    Layer::CredentialKv,
    Layer::Pii,
    Layer::HeavyModelPass,
    Layer::JsonWalker,
];

/// What the native pipeline does with the row's `needle` **today**.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    /// The needle does not appear in the redacted output.
    Removed,
    /// The needle appears verbatim in the redacted output.
    Present,
}

/// The sweep's finding for the row.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    /// Native matches the legacy backend's observable behaviour.
    AtParity,
    /// Native is deliberately stronger or more useful than the legacy backend.
    Stronger,
    /// Native behaves differently in a way that over-redacts (safe direction)
    /// or diverges on policy; relaxing it is an owner decision, not a contributor's.
    Divergence,
    /// A behaviour the legacy backend has and native does not. Must be closed by later security
    /// work.
    Gap,
    /// A behaviour Rift will never have, by design.
    NotPorted,
}

struct Case {
    id: &'static str,
    layer: Layer,
    input: &'static str,
    /// The substring whose presence/absence in the output the row pins.
    needle: &'static str,
    outcome: Outcome,
    verdict: Verdict,
    note: &'static str,
}

/// Rows currently classified [`Verdict::Gap`]. Pinned so that closing one
/// without recording it fails the sweep.
///
/// 11 → 12 at the PII work, which is bookkeeping rather than a
/// regression: `pii.phone_and_address` bundled three deficiencies (no phone, no
/// address, no labelled tokens) into one row. Phone is closed; the two that
/// remain are now a row each, so a later item can close one without a note
/// claiming credit for the others.
///
/// 12 → 13 at the published-remainder remediation, and this one is a *newly recorded* gap
/// rather
/// than a newly opened one: `pii.card_pan_comma_separated` was always missing
/// and was never written down. It is recorded as a Gap because refusing it was a
/// measured decision (a comma separator multiplies this detector's hits over the
/// repository's own diff history by 108), and a decision nobody can see is
/// indistinguishable from an oversight.
///
/// 13 → 16 for the same reason, three more times. A verifier named the two UK
/// phone spellings and the newline-split label as behaviours this crate is
/// **silent** about, and silence is what the claim rule forbids: none of the
/// three is newly broken, all three were unwritten. Each row says what refusing
/// it costs, so closing one is a decision someone can take rather than a gap
/// someone has to rediscover.
///
/// 16 → 13 at the JSON walker, and the three do not close the same way. One is
/// genuinely closed: `entropy.json_escape_boundary`, where the leaf is now
/// judged decoded so the escape letter is never part of the token. The other two
/// — `json.thinking_signature`, `json.identifier_leaf` — stop being gaps because
/// the owner **withdrew the requirement** on 2026-08-08: Rift does not skip by
/// key name, so a behaviour the legacy backend has and Rift deliberately
/// refuses is a Divergence, not a hole to close. Reclassifying rather than deleting them is
/// the point; the cost stays on the table where a reader meets it.
///
/// 13 → 11 at the first closure, and both are credential survivals rather than
/// judgement calls: `kv.colon_form` and `vendor.basic_auth_base64`, closed by
/// one root cause — layer 7's vocabulary was a single regex with a single
/// separator (`(?:password|pwd)=`), so neither the colon form nor the
/// `Authorization:` form had anything to match. Measured cost of both new
/// detectors over this repository's own 493 files: **zero** hits
/// (`benchmarks/redaction-fp-baseline.md`).
///
/// 11 → 10 with layer 3, `provider.supabase_secret_key`. Same shape of closure
/// and the same measured cost of zero; the layer went in carrying three
/// prefixes rather than the one this row names, for the reason its note gives.
///
/// 10 → 9 at `uri.empty_userinfo_user`. Its neighbour
/// `uri.empty_userinfo_accidental_pii_catch` predicted it would flip to
/// `Present` when this closed — the host surviving — and it did not: the shape
/// that preserves the host publishes bytes the reference pipeline masks, which
/// a property caught and which the row now records. The prediction was worth
/// writing down even though it was wrong, because being wrong in public is what
/// made the measurement happen.
///
/// 9 → 7 at layer 5, `dsn.whole_string` and `dsn.quoted_value_tail`. One root
/// cause with two halves — the connection-string layer was a single value-only
/// group detector whose value class stopped at the first space — so one commit
/// closes both, and the value-class half is fixed on its own so it holds
/// outside a connection string too.
///
/// 7 → 5 at the two phone spellings, and neither is closed the way its own row
/// refused. Both refusals were right about the shape being refused — a bare
/// 3-4-4 triple, and `00` in front of any digit run, which measures **1 188**
/// hits over this repository's own tracked text. Both close on the prefix that
/// is the evidence (a trunk `0`, the ITU exit code `00`) plus a gate carrying
/// the group count and digit total the prefix cannot: **zero** hits over the
/// tracked text, three over the whole diff history, and all three are the rows'
/// own literals.
///
/// 5 → 3, and both of these were refusals this table had already priced. The
/// comma PAN closes on the sentence its own row ended with — "closing this
/// needs something the checksum cannot supply": that something is the
/// **grouping**, since a card is written in uniform groups and the coordinate
/// list that defeats Luhn is not. The newline label closes because the fear
/// that kept it open was never measured; measured, allowing one line break
/// finds exactly the same hits as allowing none.
///
/// 3 → 2 at `pii.street_address`, the last of the thirteen closable on this crate's own
/// evidence. It is also the weakest measurement in this file and says
/// so: zero hits over 493 files of tracked text is what a corpus of Rust would
/// report whether or not the shape is common in the English prose it collides
/// with, so the collision is a row (`pii.street_address_capitalised_phrase`)
/// rather than a sentence.
///
/// **2 → 1 at `pii.labelled_tokens`, and it is the one closure in this table
/// that this crate could not have made.** It changes the redacted BYTES, which
/// the owner reserves; the owner took that decision, and the
/// row is now `AtParity` with the label naming the firing detector. What made it
/// safe to take was already recorded here as the reason it was refused —
/// `report.by_detector` has carried the granularity all along, so the
/// decision moved an existing distinction into the output rather than inventing one.
///
/// **1 → 0 at `custom.pack_rule_unloadable`, and it closes from outside this
/// file fully.** Nothing this table can call could ever have closed it:
/// `redact()` builds `RulePackSet::builtin()`, and a `with_pack` composition
/// assembled here would be this crate testing itself, which the owner refuses by name. It
/// closes through the shipped binary —
/// `bins/rift/tests/redactor_pack_e2e.rs`, cited from the row — and the row's
/// old note records what that cost: the capability landed on 2026-08-08 and
/// the row went on asserting its absence for two weeks, because no test in this
/// crate could see it either way.
///
/// **Zero is not the end of the sweep and this constant must not be read as
/// one.** The table's other verdicts still carry live obligations — five
/// `Divergence` rows are over-redactions an owner may want back, and
/// `heavy.prose_pii` is `NotPorted` by design. What zero means is exactly what
/// it says: no behaviour of the reference pipeline that native
/// lacks is recorded here as still missing.
const OPEN_GAPS: usize = 0;

const CASES: &[Case] = &[
    // ---- layer 1: entropy -------------------------------------------------
    Case {
        id: "entropy.long_blob",
        layer: Layer::Entropy,
        input: "token: Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ3xC7vB9nM2qWeRt",
        needle: "Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ3xC7vB9nM2qWeRt",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "unstructured high-entropy blobs are caught by both pipelines",
    },
    Case {
        id: "entropy.twenty_char_floor",
        layer: Layer::Entropy,
        input: "key aB3dE6gH9jK2mN5pQ8rS end",
        needle: "aB3dE6gH9jK2mN5pQ8rS",
        outcome: Outcome::Removed,
        verdict: Verdict::Stronger,
        note: "native floor is len >= 20 with Shannon >= 4.0; the legacy backend's is len >= \
        10 with \
               Shannon > 4.5. A 20-char all-distinct token tops out at log2(20) = 4.32, so \
               native catches a class the legacy backend's 4.5 threshold structurally cannot \
               -- and \
               the legacy backend's len >= 10 buys nothing, since log2(19) = 4.25 < 4.5",
    },
    Case {
        id: "entropy.word_shaped_identifier",
        layer: Layer::Entropy,
        input: "ran a_hook_measure_that_cannot_prove_ingest_is_failed_not_published ok",
        needle: "a_hook_measure_that_cannot_prove_ingest_is_failed_not_published",
        outcome: Outcome::Present,
        verdict: Verdict::AtParity,
        note: "a snake_case test name of 63 characters scoring 4.04 bits: over native's 4.0 \
               floor, so it was masked until the word-shape rule (2026-09-11) read it as \
               words, and under the legacy backend's 4.5 floor, which never masked it. \
               Identifiers, CamelCase types and paths are the entropy layer's whole \
               false-positive cost on this repository; the rule and its residual are \
               documented at `word_shaped` and pinned by \
               word_shape_never_excuses_a_secret",
    },
    Case {
        id: "entropy.json_escape_boundary",
        layer: Layer::Entropy,
        input: r#"{"text":"line1\nZx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ"}"#,
        needle: r"line1\[REDACTED:entropy]",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "closed by the JSON walker: the leaf is judged decoded, so the escape letter \
               is never part of the token and the placeholder lands inside the string value \
               -- `line1\\n[REDACTED:entropy]`, still valid JSON. The legacy backend reaches \
               the same \
               place from the other side, advancing the span start past known escape letters \
               and re-checking the floor. The sub-case where the two differ is its own row, \
               entropy.json_escape_carries_the_floor",
    },
    Case {
        id: "entropy.json_escape_carries_the_floor",
        layer: Layer::Entropy,
        input: r#"{"text":"line1\nZx9Qw3eR7tY2uI8oP1a"}"#,
        needle: r"line1\[REDACTED:entropy]",
        outcome: Outcome::Present,
        verdict: Verdict::Divergence,
        note: "the sub-case the row above does NOT close, recorded rather than left to be \
               rediscovered. The decoded token is 19 characters -- under the floor -- and \
               only reaches 20 because the raw bytes spell the escape as a backslash and an \
               'n' and the 'n' is a token character. The legacy backend advances past the \
               escape letter \
               and re-checks, so it KEEPS this token; native masks it, because the walker's \
               closure check sees the flat pipeline would still mask the raw line and hands \
               the line to the flat path, which eats the escape letter. Over-redaction plus \
               a line that no longer parses: safe on bytes, lossy on replay. Narrowing it \
               means teaching the entropy layer about JSON escapes in the flat path, which \
               is a detector change and not a walker one",
    },
    Case {
        id: "entropy.overlapped_span_remainder",
        layer: Layer::Entropy,
        input: "Zx9Qw3eR7tY2uI8oPa-378282246310005",
        needle: "Zx9Qw3eR7tY2uI8oPa",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "the overlap rule itself, and the only row here sourced from the legacy \
        backend's span \
               ARITHMETIC rather than a detector: applyRegions sorts by start (larger first) \
               and UNIONS anything that touches -- `if r.start <= last.end { last.end = \
               max(...) }`, keeping the first label -- so the legacy backend never publishes \
               the \
               remainder of a region that lost an overlap. Native resolved overlaps by \
               precedence and dropped the loser WHOLE, which published 15-19 characters of a \
               high-entropy token whenever a numeric-PII span landed inside one (the \
               published-remainder reject, \
               and reachable on main through pii.ipv4 alone). Native now restores the union \
               the same way the legacy backend gets it -- by span arithmetic and not by a \
               detector rule: \
               coalesce keeps a losing span's non-overlapping REMAINDER as its own span. The \
               first attempt instead GREW every whole-match span over the guesses it touched, \
               which reproduced the defect one level up (a grown span reached a competitor, \
               the competitor was dropped whole, and its remainder shipped) and could not \
               reach a capture-group detector at all",
    },
    Case {
        id: "entropy.overlapped_group_detector_context",
        layer: Layer::Entropy,
        input: "password=Zx9Qw3eR7tY2uI8oP1aS5dF6gH;",
        needle: "password=",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "the key name goes WITH the value here, and it did not always: while a loser \
               was dropped whole, the entropy span covering `password=Zx9...` lost to the \
               DSN group and left `password=` behind. Sealing the loser's remainder instead \
               takes the key with it, because `=` is a token character and the tokenizer \
               never saw two things. The legacy backend's connection-string regions are whole \
               FindAllStringIndex matches and take the key too, so this row moved from \
               Divergence to AtParity by strengthening. The context that DOES survive is a \
               competing parse's -- see uri.host_preserved",
    },
    // ---- layer 2: vendor pattern corpus -----------------------------------
    Case {
        id: "vendor.aws_access_key",
        layer: Layer::VendorCorpus,
        input: "aws_access_key_id = AKIAIOSFODNN7EXAMPLE",
        needle: "AKIAIOSFODNN7EXAMPLE",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "the six built-in vendor shapes (AWS/GitHub/Stripe/Slack/OpenAI/Google) are \
               derived from each vendor's published key-format docs",
    },
    Case {
        id: "vendor.basic_auth_base64",
        layer: Layer::VendorCorpus,
        input: "Authorization: Basic YWRtaW46aHVudGVyMg==",
        needle: "YWRtaW46aHVudGVyMg==",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "CLOSED by kv.authorization_header, and NOT by the thing this row used to ask \
               for. These bytes score Shannon 3.92 over 20 characters, so the entropy floor \
               structurally cannot reach them and no vendor charset describes them: the \
               header word is the only evidence in the line, and reading it is what closes \
               the row. The ~260-format corpus thread stays BLOCKED on licensing and is not \
               claimed here. The gap this leaves is named rather than implied: the same \
               base64 with NO header in front of it still survives, here and in the legacy \
               backend -- \
               its entropy threshold is 4.5, so it misses this class by more than native \
               does. Measured cost of the rule over this repository's own text: ZERO hits \
               (benchmarks/redaction-fp-baseline.md)",
    },
    // ---- layer 3: deterministic provider prefixes -------------------------
    Case {
        id: "provider.supabase_secret_key",
        layer: Layer::ProviderPrefix,
        input: "SUPABASE_KEY sb_secret_aBcDaBcDaBcDaBcDaBcDaBcD",
        needle: "sb_secret_aBcDaBcDaBcDaBcDaBcDaBcD",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "CLOSED: layer 3 exists now. A prefix+length rule needs neither entropy nor a \
               surrounding key name, which is exactly what reaches a low-entropy credential \
               format -- the legacy backend's providers.go documents sb_secret_ passing \
               straight \
               through its corpus when captured alone, because that rule is composite. Two \
               more shapes went in with it rather than after it, because building the layer \
               and then leaving known published prefixes out of it is fixing the report \
               instead of the defect: sk-ant- (which vendor.openai.key structurally cannot \
               reach -- its `sk-` is followed by a class the `ant-` infix ends after three \
               characters, so an Anthropic key survived this pipeline fully, and this is \
               a tool for Claude Code sessions) and github_pat_ (the fine-grained PAT, a \
               different shape from ghp_). All three are in \
               seeded_secret_corpus_zero_false_negatives; measured cost over this \
               repository's own text is ZERO. Named gap, not a claim of completeness: \
               gho_/ghs_/ghu_/ghr_ are still uncovered, because they differ from the FROZEN \
               vendor.github.pat pattern by one character class and that pattern is pinned \
               by the differential oracle",
    },
    // ---- layer 4: credentialed URIs ---------------------------------------
    Case {
        id: "uri.userinfo_password",
        layer: Layer::CredentialedUri,
        input: "postgres://admin:s3cr3tpw@db.example.com:5432/app",
        needle: "s3cr3tpw",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "userinfo credentials are removed by both pipelines",
    },
    Case {
        id: "uri.host_preserved",
        layer: Layer::CredentialedUri,
        input: "postgres://admin:s3cr3tpw@db.example.com:5432/app",
        needle: "db.example.com",
        outcome: Outcome::Present,
        verdict: Verdict::Stronger,
        note: "for the user:pass@host shape native masks only the userinfo group, where \
               the legacy backend redacts the whole URI and destroys the host that makes a \
               captured \
               transcript diagnosable. See uri.empty_userinfo_accidental_pii_catch for the \
               shape where native loses the host anyway",
    },
    Case {
        id: "uri.empty_userinfo_user",
        layer: Layer::CredentialedUri,
        input: "redis://:s3cr3tpw@localhost:6379/0",
        needle: "s3cr3tpw",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "CLOSED by uri.credentials_empty_user, a second detector rather than a widened \
               one: the frozen uri.credentials pattern is pinned by the differential oracle, \
               and the two are disjoint anyway -- this one requires a `:` immediately after \
               the `//`, which the other structurally cannot match. Native still masks the \
               password ONLY, where the legacy backend redacts the whole URI, so the host that \
               makes a \
               transcript diagnosable survives (uri.host_preserved's claim now holds for \
               both userinfo shapes rather than one)",
    },
    Case {
        id: "uri.empty_userinfo_accidental_pii_catch",
        layer: Layer::CredentialedUri,
        input: "redis://:s3cr3tpw@cache.internal:6379/0",
        needle: "cache.internal",
        outcome: Outcome::Removed,
        verdict: Verdict::Divergence,
        note: "STILL Removed after the row above closed, and the reason is a measurement \
               that overruled the plan. This note used to predict the flip: close the \
               empty-user gap, the URI rule claims the password, the host survives. It was \
               written that way, and \
               nothing_survives_here_that_dropping_the_loser_masked reddened on \
               `https://:AAAAAAAA@db.example.com` -- a capture-group span carries a parse, \
               a parse displaces the overlapping pii.email span, and that email span is the \
               ONLY thing masking this host on the reference pipeline. Preserving the host \
               here would have meant publishing bytes the reference masks, so the rule masks \
               its whole match instead: the password is now caught deterministically \
               (dotless hosts included, which is the closure), and on a DOTTED host the \
               email detector still takes the host exactly as it did before. The \
               coincidence is still there; what changed is that nothing depends on it",
    },
    // ---- layer 5: connection strings --------------------------------------
    Case {
        id: "dsn.password_value",
        layer: Layer::ConnectionString,
        input: "jdbc:mysql://h/db?user=u&password=hunter2secret",
        needle: "hunter2secret",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "the password value itself is removed by both pipelines",
    },
    Case {
        id: "dsn.whole_string",
        layer: Layer::ConnectionString,
        input: "jdbc:mysql://h/db?user=u&password=hunter2secret",
        needle: "jdbc:mysql://h/db?user=u",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "CLOSED by dsn.connection_string, on the reading this row already recorded: \
               once a real password is present the host+user+password triple is the secret, \
               not just the password. Two anchored shapes, and the anchor \
               is the whole of what keeps it from swallowing ordinary text -- a driver \
               scheme (jdbc:/odbc:) carried through to its password, and a \
               semicolon-delimited key=value list that reaches one. It deliberately does NOT \
               take `scheme://user:pass@host` (that stays uri.credentials, which keeps the \
               host, and a_connection_string_with_a_real_password_goes_whole pins both \
               exclusions) or a bare `password=x`. Measured over this repository\'s own 493 \
               files before it was written: zero hits",
    },
    Case {
        id: "dsn.quoted_value_tail",
        layer: Layer::ConnectionString,
        input: r#"Server=db;User Id=sa;Password="hunter 2 space";"#,
        needle: "2 space",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "CLOSED twice over, which is worth saying because only one of the two is the \
               general fix. This row\'s literal is a connection string, so \
               dsn.connection_string takes it whole; the under-redaction it recorded is a \
               property of the VALUE CLASS and would have survived outside a DSN, so \
               dsn.password_quoted closes that separately -- it matches the quotes and a \
               gate narrows past them, which is how the legacy backend strips one matching \
               pair before \
               deciding, and it keeps the quotes in the output so the document still parses. \
               `Password=\"hunter 2 space\" trailing` is the case that needed the second \
               rule and it is pinned",
    },
    Case {
        id: "dsn.placeholder_over_redaction",
        layer: Layer::ConnectionString,
        input: "password=${DB_PASSWORD}",
        needle: "${DB_PASSWORD}",
        outcome: Outcome::Removed,
        verdict: Verdict::Divergence,
        note: "no placeholder-shape skip heuristics (${VAR}, <password>, ***, changeme, \
               prior REDACTED output), so templates and docs are masked, inflating report \
               counts. Over-redaction is the safe direction, so this holds \
               until the heuristics land",
    },
    // ---- layer 6: user rules and rule packs -------------------------------
    Case {
        id: "custom.pack_rule_unloadable",
        layer: Layer::CustomRules,
        input: "internal token ACME-AB12CD34",
        needle: "ACME-AB12CD34",
        outcome: Outcome::Present,
        verdict: Verdict::AtParity,
        note: "CLOSED, and NOT by anything in this file -- read the outcome column with that \
               in mind, because `Present` here is correct and says nothing about the row. \
               `redact()` builds RulePackSet::builtin(), so a probe run from this crate \
               cannot see a repository's packs and never will; re-pointing this row at a \
               hand-built with_pack composition would be this crate testing itself, which the \
               owner refuses explicitly. The closing evidence is \
               `bins/rift/tests/redactor_pack_e2e.rs`, which writes a pack into a temp \
               repository's .rift/policies/redactors/, drives the REAL binary through a real \
               post-commit capture hook, and reads content.redacted back out of the \
               checkpoint shard git stores: the pack's token is absent, [REDACTED:vendor] is \
               present, and rule_pack_versions in the sealed report carries acme@2026.08.0, \
               which only with_pack can mint. Its second test is the calibration and is what \
               makes the first mean anything -- same repo, same file, no pack, and the token \
               SURVIVES. \
               THE STALE POINTER THIS REPLACES, because the lesson is bigger than the row: \
               it said closing this meant calling with_pack in rift-shadow/src/condense.rs \
               and bins/rift. That request was serviced on 2026-08-08 (5948eae) \
               and the join has lived in bins/rift/src/redactor_packs.rs ever since -- so \
               `no pack a user writes reaches the capture path` was FALSE for two weeks \
               while this row asserted it, and hooks.rs calls capture_rule_packs at four \
               sites. Nothing reddened, because a parity row whose subject is in another \
               crate cannot be observed by the table that carries it. That is the defect the \
               e2e closes: the row now cites a test that runs",
    },
    // ---- layer 7: bounded credential key/value ----------------------------
    Case {
        id: "kv.env_assignment",
        layer: Layer::CredentialKv,
        input: "export DB_PASSWORD=hunter2secret",
        needle: "hunter2secret",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "value-only masking with the key preserved; native reaches it through the \
               unanchored DSN detector rather than a bounded vendor-prefixed key shape, so \
               the report attributes it to dsn.password",
    },
    Case {
        id: "kv.colon_form",
        layer: Layer::CredentialKv,
        input: "password: hunter2secret",
        needle: "hunter2secret",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "CLOSED by kv.credential_colon. The key survives and the value goes, which is \
               layer 7's contract; a quoted value is matched WITH its quotes and the gate \
               hands them back, so the document still parses and the tail of a value \
               containing a space cannot ship. The vocabulary is measured rather than \
               tasteful: bare `token` (44 hits over this repository, every sampled one a \
               Rust type annotation), bare `secret` (7, all annotations) and `pwd` (1, and \
               it is `$PWD`) are EXCLUDED, and what remains costs ZERO hits over the same \
               493 files. The legacy backend reaches the same place from the walker instead, \
               keyed on \
               the field name plus a credential context",
    },
    Case {
        id: "kv.colon_form_type_annotation",
        layer: Layer::CredentialKv,
        input: "pub password: String,",
        needle: "String",
        outcome: Outcome::Removed,
        verdict: Verdict::Divergence,
        note: "the price of the row above, recorded rather than left for a user to find: a \
               Rust field DECLARATION whose name is in the credential vocabulary has its \
               type masked, because nothing in the line distinguishes `password: String` \
               from `password: hunter2secret` without parsing Rust. It measured ZERO \
               occurrences over this repository's own 493 files, which is why the trade was \
               taken, and it is a real one in source generally -- Rift captures diffs. \
               Over-redaction is the safe direction and relaxing it is a \
               redaction-semantics change, so this stays a Divergence for an owner to \
               overrule with the number in front of them, not a gap this crate can close",
    },
    // ---- layer 8: PII -----------------------------------------------------
    Case {
        id: "pii.email_always_on",
        layer: Layer::Pii,
        input: "contact alice@example.com about the outage",
        needle: "alice@example.com",
        outcome: Outcome::Removed,
        verdict: Verdict::Stronger,
        note: "the legacy backend's PII layer is opt-in and off by default; native redacts \
        email \
               unconditionally. Making it opt-in would weaken the gate: that is a \
               redaction-semantics change and therefore owner-only",
    },
    Case {
        id: "pii.bot_noreply_not_allowlisted",
        layer: Layer::Pii,
        input: "Co-Authored-By: bot <noreply@anthropic.com>",
        needle: "noreply@anthropic.com",
        outcome: Outcome::Removed,
        verdict: Verdict::Divergence,
        note: "no allowlist for bot/noreply addresses, so every commit trailer in a \
               transcript is masked; the legacy backend allowlists noreply@, actions@ and the \
               GitHub \
               noreply domains. Any allowlist here stays subtract-only and counted in \
               report.allowlisted -- seen and kept, never silently missed",
    },
    // The three-in-one `pii.phone_and_address` row is split here: phone is
    // covered (rows below), address and labelled tokens are not, and bundling
    // them hid which third of the row each later item actually closed.
    Case {
        id: "pii.street_address",
        layer: Layer::Pii,
        input: "visit 1600 Pennsylvania Ave before Friday",
        needle: "1600 Pennsylvania Ave",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "CLOSED, and this row is the one whose measurement a reader should trust \
               least -- which is why the weakness is written here rather than left to be \
               inferred. The row asked for its own FP budget before anything landed, and it \
               now has one: benchmarks/redaction-fp-baseline.md, where this detector \
               measures ZERO over 493 files of tracked text, with only this row's own \
               literal over the whole diff history. But an address has no checksum and no \
               issuing authority, so the shape alone decides, and the text that COLLIDES \
               with the shape is English prose with capitalised phrases in it -- which a \
               Rust workspace is thin in. The gate adds the only evidence available (the \
               house number must stand alone, so `v1.2.3 Test Drive` is a version string; \
               the match is bounded, because an address is a phrase). The collision that \
               remains is a row of its own",
    },
    Case {
        id: "pii.street_address_capitalised_phrase",
        layer: Layer::Pii,
        input: "see 2 Hard Drive benchmarks",
        needle: "2 Hard Drive",
        outcome: Outcome::Removed,
        verdict: Verdict::Divergence,
        note: "the price of the row above, named with an example rather than admitted in \
               general. A number followed by capitalised words and a street-type word is an \
               address to this detector and a heading to a reader, and nothing in the line \
               says which. It measured zero occurrences over this repository's own text, \
               which is exactly what a corpus of Rust would say whether or not the shape is \
               common in prose. Over-redaction is the safe direction for PII and relaxing \
               it is a redaction-semantics change, so this is a Divergence for an owner to \
               overrule with an example in front of them -- not a gap this crate can close",
    },
    Case {
        id: "pii.labelled_tokens",
        layer: Layer::Pii,
        input: "mail alice@example.com or call 415-555-0199",
        needle: "[REDACTED:pii.email]",
        outcome: Outcome::Present,
        verdict: Verdict::AtParity,
        note: "CLOSED by the owner, who took the redaction-semantics decision this \
               row was reserved for. WHAT IT SAID WHILE OPEN, kept verbatim because the \
               reasoning is why the closure was safe to take: every PII class collapses to \
               one placeholder, so a redacted transcript no \
               longer says what was removed; the legacy backend emits [REDACTED_EMAIL]/[REDACTED_PHONE]/\
               [REDACTED_ADDRESS]. The report's by_detector map already carries the \
               granularity (pii.email vs pii.phone_nanp); moving it into the placeholder \
               changes the redacted bytes, which is a redaction-semantics change and \
               therefore owner-only. WHAT IS TRUE NOW: a PII \
               placeholder names the detector that fired -- [REDACTED:pii.email] and its \
               ten siblings, at whole-token granularity -- against the reference \
               pipeline's three labels; the five non-PII placeholders are unchanged. \
               AtParity and not Stronger: it is the same capability, and the shapes native \
               covers that the reference does not are claimed by their own rows rather \
               than a second time here. The needle moved with the code in the same commit \
               and had to -- measured both ways rather than reasoned. Code without needle: \
               `mail [REDACTED:pii.email] or call [REDACTED:pii.phone_nanp]` reads Removed \
               against a recorded Present, so the row reddens. Needle without code: `mail \
               [REDACTED:pii] or call [REDACTED:pii]` reads Removed too. NOTE ON THAT DECISION \
               ITSELF: its parenthetical says the old needle is a SUBSTRING of the new \
               placeholder and would therefore pass silently. It is not -- the family root \
               ends in `]` exactly where the label carries `.` -- so the mandate is right \
               for a stronger reason than the one given: neither half lands silently, \
               either half alone reddens",
    },
    Case {
        id: "pii.us_taxpayer_id",
        layer: Layer::Pii,
        input: "SSN=123-45-6789",
        needle: "123-45-6789",
        outcome: Outcome::Removed,
        verdict: Verdict::Stronger,
        note: "the legacy backend has no national-id detector at all (pii.go covers email, \
        phone and \
               address), so an SSN in an .env or a transcript is invisible to it. Native \
               masks the SSA shape and the IRS ITIN that shares it, gated on the issuing \
               authority's published allocation rules. This row is the defect a \
               verifier measured landing verbatim in a durable shard once the \
               inclusion controls made .env capturable",
    },
    Case {
        id: "pii.labelled_id_json_quoted",
        input: "{\"ssn\":\"123456789\"}",
        layer: Layer::Pii,
        needle: "123456789",
        outcome: Outcome::Removed,
        verdict: Verdict::Stronger,
        note: "the shape the capture document actually uses. The legacy backend's pii.go has \
        no \
               national-id detector at all, so this is coverage it does not have; native \
               did not have it either until the labelled detector learned that a quote may \
               sit on either side of the separator and that the separator may be a space. \
               The value goes, the key stays, and the JSON stays parseable",
    },
    Case {
        id: "pii.taxpayer_id_unissued_range",
        layer: Layer::Pii,
        input: "ticket 000-12-3456 and order 123-45-0000",
        needle: "000-12-3456",
        outcome: Outcome::Present,
        verdict: Verdict::AtParity,
        note: "both pipelines keep it, for opposite reasons: the legacy backend has no \
        detector, native \
               has one that knows area 000 (like group 00 and serial 0000) has never been \
               issued. The precision is the point -- a 3-2-4 digit run is a common shape in \
               ordinary text and masking all of them would make the detector unusable",
    },
    Case {
        id: "pii.phone_e164_non_us",
        layer: Layer::Pii,
        input: "ring +44 20 7123 4567 tomorrow",
        needle: "+44 20 7123 4567",
        outcome: Outcome::Removed,
        verdict: Verdict::Stronger,
        note: "the legacy backend's phone regex is North America only -- its three branches \
        all require \
               a +1 prefix, a parenthesised NANP area code, or a bare NANP triple, so every \
               non-US number passes through even with PII enabled. CORRECTED 2026-08-07: \
               this note used to claim native reads E.164 'generally: a + country code and \
               8-15 digits, punctuated or not', and that was false -- the pattern took \
               2-4-digit groups, so +33 1 42 68 53 00 (a 1-digit group) and +55 11 \
               91234-5678 (a 5-digit one) were NOT masked, and this row passed only because \
               its own literal happens to decompose into 2-4-digit groups. That is the \
               corpus-literal failure mode: a detector that catches only the literals in its \
               own corpus passes every test written from that corpus. Groups are now 1-5 \
               digits and the three shapes above are corpus rows",
    },
    Case {
        id: "pii.phone_e164_trailing_group",
        layer: Layer::Pii,
        input: "call +44 20 7123 4567 9999 today",
        needle: "+44 20 7123 4567",
        outcome: Outcome::Removed,
        verdict: Verdict::Stronger,
        note: "four digits after a non-US number used to disarm the only detector that \
               covers it: the greedy match ran to sixteen digits, failed E.164's 15-digit \
               ceiling, and the number was published in full. The legacy backend has no non-US \
               branch at \
               all, so it publishes this unconditionally. The gate now TRIMS to the last \
               group that fits rather than choosing between publishing all of it and masking \
               all of it -- a regex cannot express 'as many groups as keep the total under \
               15', and a reject-only gate has no third answer",
    },
    Case {
        id: "pii.phone_international_zero_prefix",
        layer: Layer::Pii,
        input: "ring 0044 20 7123 4567 tomorrow",
        needle: "0044 20 7123 4567",
        outcome: Outcome::Removed,
        verdict: Verdict::Stronger,
        note: "the same number pii.phone_e164_non_us masks, written with the ITU exit code \
               00 instead of +. The `+` is the evidence that makes punctuated groups safe to \
               read as a number, and `00` is not evidence: `00` also opens an octal literal, \
               a zero-padded id and a timestamp, so accepting it would put the group-walking \
               gate in front of every 00-prefixed digit run in a capture. Recorded rather \
               than closed, because closing it needs a rule the funnel measurement does not \
               yet have. The legacy backend misses it too (its regex is North America only)",
    },
    Case {
        id: "pii.phone_uk_national",
        layer: Layer::Pii,
        input: "ring 020 7123 4567 tomorrow",
        needle: "020 7123 4567",
        outcome: Outcome::Removed,
        verdict: Verdict::Stronger,
        note: "CLOSED without paying the trade this row refused, and the difference is one \
               character. The refusal was right about a BARE 3-4-4 triple: nothing filters \
               `256 5121 1024` out of one, which is why \
               a_bare_numeric_triple_is_read_as_a_phone_number exists. The trunk `0` is \
               evidence a bare triple does not have, and anchored on it the same widening \
               measures ZERO hits over this repository's tracked text and 3 over its whole \
               diff history -- all three this row's own literal. The gate adds what the \
               prefix cannot: three groups, one separator style, 10-11 digits. The legacy \
               backend misses \
               it too, so this is Stronger rather than AtParity",
    },
    Case {
        id: "pii.labelled_id_across_a_newline",
        layer: Layer::Pii,
        input: "ssn=\n123456789\n",
        needle: "123456789",
        outcome: Outcome::Removed,
        verdict: Verdict::Stronger,
        note: "CLOSED, and the reasoning that kept it open turned out to be untested rather \
               than wrong-headed. The fear was that `\\n` in the separator class lets a \
               heading claim the first number of the table under it; measured, it does not, \
               because the label vocabulary is six words and a markdown cell cannot be \
               reached at all (`|` is in neither class). Allowing at most ONE line break \
               finds exactly the same hits as before over this repository's tracked text \
               (zero) and over its whole diff history (the seeded fixtures only). One and \
               not `[\\r\\n]*`, which would let a heading reach a number fifty lines away. \
               The shape this closes is the one the row named: a .env-style capture with a \
               trailing backslash or a here-doc",
    },
    Case {
        id: "pii.card_pan_comma_separated",
        layer: Layer::Pii,
        input: "card 4111,1111,1111,1111 end",
        needle: "4111,1111,1111,1111",
        outcome: Outcome::Removed,
        verdict: Verdict::Stronger,
        note: "CLOSED by taking this row's last sentence literally. The refusal stands and \
               is re-measured on today's tree: adding `,` to the PAN separator class takes \
               this detector from 0 to 281 hits over the repository's tracked text and 73 \
               to 361 over its whole diff history, because a comma-separated coordinate \
               list (407,71,46 414,71,46 420,57 in a canopy snapshot) forms Luhn-valid \
               16-digit windows. The thing the checksum cannot supply turned out to be the \
               GROUPING: a card is written in uniform groups and a coordinate list is not, \
               so pii.card_pan_grouped accepts the comma in exactly one writing -- four \
               groups of exactly four digits -- and measures ZERO hits over the tracked \
               text with only this row's own literal over the whole history. The coordinate \
               list is in the seeded corpus's KEPT side so the refusal it stands for cannot \
               be lost. Named gap: the 15-digit Amex comma form is not uniform and is \
               not covered",
    },
    Case {
        id: "pii.phone_nanp",
        layer: Layer::Pii,
        input: "call 415-555-0199 or 1-415-555-0199",
        needle: "415-555-0199",
        outcome: Outcome::Removed,
        verdict: Verdict::Stronger,
        note: "same shape both pipelines match, but the legacy backend's PII layer is opt-in \
        and off by \
               default, so in the shipped configuration this number survives there and not \
               here",
    },
    Case {
        id: "pii.phone_fictional_exchange",
        layer: Layer::Pii,
        input: "call 555-123-4567 for the demo",
        needle: "555-123-4567",
        outcome: Outcome::Present,
        verdict: Verdict::Divergence,
        note: "the legacy backend's bare branch is shape-only \
        (\\d{3}[-\\s]\\d{3}[-\\s]\\d{4}), so with PII \
               enabled it masks this and every sequential digit triple like 100-200-3000. \
               Native requires the NANP's N: both the area and the central-office code start \
               2-9, which no documentation number (exchange 123) and no counting sequence \
               satisfies. Deliberate precision, not an oversight -- relaxing it would trade \
               a measured zero false positives for a shape that fires on numeric tables",
    },
    Case {
        id: "pii.numeric_triple_space_separated",
        layer: Layer::Pii,
        input: "columns 256 512 1024 end",
        needle: "256 512 1024",
        outcome: Outcome::Removed,
        verdict: Verdict::AtParity,
        note: "the over-redaction the phone shapes carry, recorded rather than left to be \
               discovered: a 3-3-4 digit triple whose first two columns start 2-9 is a valid \
               NANP number written with spaces, and nothing in the text says which it is. \
               the legacy backend's bare branch masks the same input. NXX still removes the \
               sequential \
               cases (100 200 3000) and any other column width removes the rest, so the \
               remainder is narrow -- and it measured zero over this repository's own text, \
               where the shape does not occur at all",
    },
    Case {
        id: "pii.dotted_triple_is_not_a_phone",
        layer: Layer::Pii,
        input: "call 415.555.0199 now",
        needle: "415.555.0199",
        outcome: Outcome::Present,
        verdict: Verdict::AtParity,
        note: "dots are separators only behind a `+` (pii.phone_e164 masks +1.415.555.0199). \
               the legacy backend's pii.go records the same rule and the reason: without an \
               unambiguous \
               prefix a dotted decimal collides with version strings and dotted quads. Both \
               pipelines therefore keep this one",
    },
    Case {
        id: "pii.card_pan",
        layer: Layer::Pii,
        input: "charge 4111 1111 1111 1111 today",
        needle: "4111 1111 1111 1111",
        outcome: Outcome::Removed,
        verdict: Verdict::Stronger,
        note: "the legacy backend's pii.go has no payment-card detector; whether its external \
        vendor \
               corpus carries one cannot be established from the permitted sources \
               (recorded as blocked on corpus licensing), so this is derived \
               from ISO/IEC 7812: \
               issuer prefix + length + Luhn check digit",
    },
    Case {
        id: "pii.card_pan_bad_check_digit",
        layer: Layer::Pii,
        input: "order 4111111111111112 shipped",
        needle: "4111111111111112",
        outcome: Outcome::Present,
        verdict: Verdict::AtParity,
        note: "one digit off the published Visa test number and the Luhn check fails, so it \
               is not masked. The check is the gate, not decoration: over this repository's \
               own text the shape proposed 45 windows, 5 of which pass Luhn and none of \
               which carry an issuer prefix at a length that network issues",
    },
    Case {
        id: "pii.epoch_nanoseconds",
        layer: Layer::Pii,
        input: "recorded at 1735689600000000005 ns",
        needle: "1735689600000000005",
        outcome: Outcome::Present,
        verdict: Verdict::AtParity,
        note: "a 19-digit epoch-nanosecond timestamp that *passes* Luhn -- the false positive \
               a card detector gets wrong if the checksum is its only gate. No network issues \
               a 1-prefix, so the issuer-range rule keeps it, and this row is what fails if \
               that rule is ever dropped for the checksum alone",
    },
    Case {
        id: "pii.dotted_quad_version",
        layer: Layer::Pii,
        input: "upgrade to 1.2.3.4 before Friday",
        needle: "1.2.3.4",
        outcome: Outcome::Removed,
        verdict: Verdict::Divergence,
        note: "native masks every dotted quad, version strings included; the legacy backend \
        ships no IP \
               detector at all. Stricter, but a known false-positive source in changelogs \
               -- the false-positive budget measures it before anything is relaxed",
    },
    // ---- the JSON walker --------------------------------------------------
    Case {
        id: "json.thinking_signature",
        layer: Layer::JsonWalker,
        input: r#"{"type":"thinking","signature":"EqQBCkYIBRgCKkC9x7vJ2mNq4wPzR8sT1uV6yA3bC5dE7fG9hJ0kL2mN"}"#,
        needle: "EqQBCkYIBRgCKkC9x7vJ2mNq4wPzR8sT1uV6yA3bC5dE7fG9hJ0kL2mN",
        outcome: Outcome::Removed,
        verdict: Verdict::Divergence,
        note: "NOT a gap any more, and not because it was closed -- the owner ruled on \
               2026-08-08 that Rift does not skip by key name at all. The legacy backend skips \
               any key ending in 'signature' (its 0x0018 lesson); three \
               verifier rejects in a row found new ways to aim that narrowing, so the \
               machinery went instead of being patched a fourth time. A thinking-block \
               signature is high-entropy base64 and is masked here, which does break replay \
               of thinking blocks. That is the accepted cost, recorded as a divergence \
               because reopening it is an owner decision and no longer this crate's",
    },
    Case {
        id: "json.identifier_leaf",
        layer: Layer::JsonWalker,
        input: r#"{"session_id":"01J8ZKX9QW3ER7TY2UI8OP1AS5"}"#,
        needle: "01J8ZKX9QW3ER7TY2UI8OP1AS5",
        outcome: Outcome::Removed,
        verdict: Verdict::Divergence,
        note: "same decision, same reason: the legacy backend skips *id/*ids, path-like keys and \
               image/base64 payloads; Rift skips nothing by key name. What survives here \
               survives on its own merits instead -- a transcript UUID's Shannon entropy is \
               under the 4.0 floor, so `uuid`/`parentUuid` are untouched with no rule about \
               them at all (jsonl::tests::transcript_uuids_survive_on_the_entropy_floor). A \
               ULID like this one is not: 26 upper-case-and-digit characters clear both the \
               length floor and the entropy floor, so it goes",
    },
    Case {
        id: "json.line_stays_parsable",
        layer: Layer::JsonWalker,
        input: r#"{"text":"key AKIAIOSFODNN7EXAMPLE here","keep":"ok"}"#,
        needle: r#"{"text":"key [REDACTED:vendor] here","keep":"ok"}"#,
        outcome: Outcome::Present,
        verdict: Verdict::AtParity,
        note: "READ THE OUTCOME CAREFULLY -- this is the one row whose needle is the WANTED \
               output and not a secret, so `Present` is the passing state and means the \
               whole redacted line came back byte for byte. It is the walker's remaining \
               purpose once nothing is skipped by key name: the replacement lands inside the string value and \
               every other byte -- quoting, key order, spacing -- is the input's. The legacy \
               backend \
               walks JSONL for the same reason (`JSONLContent` substitutes per leaf); flat \
               scanning is what puts a placeholder across a quote and costs the record \
               instead of the secret",
    },
    // ---- layer 9: the heavy model pass ------------------------------------
    Case {
        id: "heavy.prose_pii",
        layer: Layer::HeavyModelPass,
        input: "my landlord Mrs Kowalski lives two doors from the old bakery",
        needle: "Kowalski",
        outcome: Outcome::Present,
        verdict: Verdict::NotPorted,
        note: "the legacy backend's ninth layer batches prose leaves to a 1.5B privacy-filter \
        model and \
               rewrites pre-push commits when it fires. Rift will never run it: no LLM \
               execution, no network in capture, and no \
               history rewrite ever. If a heavy pass is ever \
               added it runs pre-storage behind the execution seam, never post-commit",
    },
];

fn redacted(input: &str) -> String {
    let out = redact(&SecretBytes::from(input)).expect("redaction must not fail on a probe");
    String::from_utf8_lossy(out.redacted.as_slice()).into_owned()
}

/// Every row's asserted outcome still holds. A row that flips fails here, which
/// is the point: the table cannot rot silently.
#[test]
fn native_pipeline_matches_the_recorded_parity_table() {
    let mut drift = Vec::new();
    for case in CASES {
        let output = redacted(case.input);
        let present = output.contains(case.needle);
        let actual = if present {
            Outcome::Present
        } else {
            Outcome::Removed
        };
        if actual != case.outcome {
            drift.push(format!(
                "{}: recorded {:?}, observed {:?} (verdict {:?})\n  in  : {}\n  out : {}\n  \
                 note: {}",
                case.id, case.outcome, actual, case.verdict, case.input, output, case.note
            ));
        }
    }
    assert!(
        drift.is_empty(),
        "parity table drifted -- update the row's outcome AND its verdict:\n{}",
        drift.join("\n")
    );
}

/// The sweep covers all nine legacy backend layers plus the walker; none is unexamined.
#[test]
fn every_legacy_layer_is_swept() {
    for layer in ALL_LAYERS {
        assert!(
            CASES.iter().any(|c| c.layer == layer),
            "no parity row exercises {layer:?} -- the sweep is incomplete"
        );
    }
}

/// The open-gap count is pinned, so closing a gap forces a table edit (and with
/// it a ledger line): "no `Gap` row remains".
#[test]
fn open_gap_count_is_pinned() {
    let gaps: Vec<&str> = CASES
        .iter()
        .filter(|c| c.verdict == Verdict::Gap)
        .map(|c| c.id)
        .collect();
    assert_eq!(
        gaps.len(),
        OPEN_GAPS,
        "open parity gaps changed; update OPEN_GAPS and the ledger. Current: {gaps:?}"
    );
}

/// Row ids are unique, so a failure message names exactly one behaviour.
#[test]
fn row_ids_are_unique() {
    let mut ids: Vec<&str> = CASES.iter().map(|c| c.id).collect();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(before, ids.len(), "duplicate parity row id");
}

/// The `NotPorted` verdict's executable half —
/// stated at its real strength, which an earlier review found overclaimed. This is an
/// **11-name denylist over this crate's own manifest text**: it reds if one of
/// the names below is spelled in `Cargo.toml`, and it sees nothing else — not
/// a client under another name, not one reached through an intermediary crate.
/// The structural claim ("nothing this crate links can reach a network") lives
/// in `xtask::ci_meta`: `no_network_deps_in_capture_crates` screens every
/// manifest in the `rift` binary's in-workspace closure (this crate is a
/// planted member of that closure), and `no_network_crate_in_the_resolved_graph`
/// screens the default-resolved `cargo tree` of both binaries for the external
/// case. This test keeps the verdict row honest; those two keep the crate
/// honest.
#[test]
fn heavy_model_pass_has_no_dependency_to_stand_on() {
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
    )
    .expect("read own manifest");
    for forbidden in [
        "reqwest",
        "hyper",
        "ureq",
        "curl",
        "tokio",
        "async-openai",
        "llama",
        "onnx",
        "candle",
        "tonic",
        "socket2",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "rift-shadow-redact must never gain `{forbidden}`: the heavy privacy-filter \
             layer is NotPorted by design and capture does no network"
        );
    }
    assert!(
        CASES
            .iter()
            .any(|c| c.verdict == Verdict::NotPorted && c.layer == Layer::HeavyModelPass),
        "the NotPorted verdict must stay recorded against the heavy model pass"
    );
}

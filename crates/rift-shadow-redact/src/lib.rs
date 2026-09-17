//! Two-pass redaction — the one-way gate.
//!
//! Raw, untrusted [`SecretBytes`] in; sealed [`RedactedBytes`] + a counts-only
//! [`RedactionReport`] out. This crate is the **single** constructor of
//! [`RedactedBytes`] (`rift-core` makes the buffer private; the only mint is
//! [`RedactedBytes::seal`], called once, here).
//!
//! * **Pass 1** — high-precision shape detectors: vendor tokens (AWS/GitHub/
//!   Stripe/Slack/OpenAI/Google), credentialed URIs, JDBC/DSN connection
//!   strings, PEM private-key blocks.
//! * **Pass 2** — entropy scoring (catches arbitrary-shape high-entropy blobs)
//!   plus conservative PII (email, IPv4, US taxpayer ids, phone numbers, payment
//!   card numbers). The PII shapes that a digit run can imitate carry a
//!   `SpanGate`: the regex proposes, the issuing authority's allocation rules
//!   and a checksum dispose, so a version string, a UUID, a commit hash and an
//!   epoch timestamp are all left alone (each pinned by a corpus row).
//!
//! **The shapes that are over-redacted**, both stated because a trade a reader
//! can see is a trade someone can overrule, and both pinned by a test:
//!
//! * a bare space-separated 3-3-4 digit triple, indistinguishable from a phone
//!   number written the same way (`is_nanp_phone`);
//! * a `+` followed by 8-15 digits — which in a unified diff is an *added line*
//!   of digits, and Rift captures diffs (`is_e164_phone`). Measured over
//!   261 067 lines of this repository's own diff text: one hit, inside a binary
//!   blob, and zero added lines that are exactly 8-15 digits. Narrowing it (a
//!   `+` at column 0 is a diff marker) would trade a measured-near-zero
//!   over-redaction for a real under-redaction, so the behaviour is kept and
//!   `a_diff_added_line_of_digits_is_read_as_a_phone_number` pins it.
//!
//! Spans coalesce, then mask with a fixed placeholder — one per class, and
//! within the PII family one per *detector*, so a redacted transcript says
//! which shape was removed (`placeholder_for`). Fixed either
//! way: the placeholder is chosen by the detector that fired and never by the
//! bytes it matched, so masked length never reveals secret length. Coalescing
//! resolves an overlap by
//! *attribution* only — the losing span keeps whatever of itself the winner did
//! not claim, so the sealed byte set is the union of everything every layer
//! proposed **save for one named exception**, where a shape span that a parse of
//! the input's own bytes already explains goes whole so the parse's context
//! survives (`coalesce`). The
//! detect→mask loop then repeats to a fixpoint, which is what makes redaction
//! idempotent on its own output. The report holds **only** counts, ids,
//! hashes, and pack versions — it is structurally incapable of carrying a
//! secret. All regexes are linear-time (`regex` crate, which cannot compile a
//! backreference at all), and the detect→mask loop is bounded by `MAX_PASSES`
//! and fails closed rather than looping — so a hostile input cannot wedge the
//! pipeline. The bound is the load-bearing half and it is
//! exercised by `an_oscillating_rule_is_bounded_and_fails_safe`; the regex half
//! is enforced by the compiler, since a backreference pattern never builds.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

mod builtin_tables;
mod jsonl;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use rift_core::{RedactedBytes, RedactionProvenance, Result, RiftError, SecretBytes, Sha256Id};
use sha2::{Digest, Sha256};

use crate::builtin_tables::builtin_shape_tables;

/// The detector family a redaction span belongs to.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, serde::Serialize, serde::Deserialize,
)]
pub enum DetectorClass {
    /// Provider token patterns (AWS, GitHub, Stripe, …).
    VendorSecret,
    /// `scheme://user:password@host` credentials in URLs.
    CredentialedUri,
    /// JDBC / DSN / connection-string passwords.
    JdbcDsn,
    /// PEM / OpenSSH private-key blocks.
    PrivateKey,
    /// High-Shannon-entropy tokens above length+entropy thresholds.
    Entropy,
    /// Email / IP / national-id shapes.
    Pii,
}

impl DetectorClass {
    /// The fixed-length placeholder for this class (does not encode secret size).
    ///
    /// This is the whole answer for five of the six classes. For [`Self::Pii`]
    /// it is the **family root** rather than what a PII span is actually masked
    /// with: a PII span carries its detector's own
    /// placeholder, `[REDACTED:pii.email]` and its ten siblings, and the value
    /// here is what the family falls back to for a PII span whose id the
    /// built-in table does not carry. `placeholder_for` is the function the
    /// pipeline calls; this one is the class's own name and stays public because
    /// the family predicate and the pinned placeholder table are written in
    /// terms of it.
    #[must_use]
    pub fn placeholder(self) -> &'static str {
        match self {
            Self::VendorSecret => "[REDACTED:vendor]",
            Self::CredentialedUri => "[REDACTED:uri]",
            Self::JdbcDsn => "[REDACTED:jdbc]",
            Self::PrivateKey => "[REDACTED:key]",
            Self::Entropy => "[REDACTED:entropy]",
            Self::Pii => "[REDACTED:pii]",
        }
    }
}

/// The two halves of every placeholder this crate emits. They are constants
/// because the family predicate has to take a placeholder apart again, and two
/// spellings of the same delimiter drifting apart is exactly how a suppression
/// check stops suppressing. `every_placeholder_is_delimited_the_same_way` holds
/// the class table to them.
const PLACEHOLDER_OPEN: &str = "[REDACTED:";
const PLACEHOLDER_CLOSE: &str = "]";

/// `detector id → [REDACTED:<id>]` for every built-in PII detector.
///
/// Built **from the table [`propose`] reads**, not from a second list of names:
/// a twelfth PII detector gets its placeholder by existing, and cannot arrive
/// with the family root as a silent default. The ids are compile-time constants
/// (`&'static str` in [`builtin_tables`]) so no byte of any document reaches a
/// placeholder — which is the property that keeps a placeholder from becoming a
/// channel for the text it replaced.
fn pii_placeholders() -> &'static BTreeMap<String, String> {
    static CELL: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    CELL.get_or_init(|| {
        let [_, _, _, pii] = builtin_shape_tables();
        pii.iter()
            .map(|d| {
                (
                    d.id.to_string(),
                    format!("{PLACEHOLDER_OPEN}{}{PLACEHOLDER_CLOSE}", d.id),
                )
            })
            .collect()
    })
}

/// The placeholder a span is masked with: the class's, refined to the firing
/// detector's for the PII family.
///
/// Whole-token granularity: the label replaces the *whole* matched span, so an
/// email becomes one `[REDACTED:pii.email]` and never a labelled fragment
/// beside a published remainder.
fn placeholder_for(class: DetectorClass, id: &str) -> &'static str {
    if class == DetectorClass::Pii {
        if let Some(p) = pii_placeholders().get(id) {
            return p.as_str();
        }
    }
    class.placeholder()
}

/// Whether `text` is already a placeholder this pipeline emits — any class,
/// any PII family member.
///
/// The suppression rule in [`mask`] was an equality against the one placeholder
/// a class emits, then membership in the span's own family, so a
/// *second* PII detector matching a *first* one's placeholder could not rewrite
/// the label. The fixpoint enumeration then found the cross-family case: the
/// DSN password detector matches `password=<value>` whatever the value is, and
/// when the value is `[REDACTED:pii.us_taxpayer_id_labelled]` a family check
/// lets it relabel that as `[REDACTED:jdbc]` — output that depends on which
/// detector fired last, and `redact(redact(x)) == redact(x)` false. A
/// placeholder is prior output whichever detector wrote it, so the question is
/// membership in the whole emitted set, not in one family.
///
/// The set is **closed**: the six class placeholders plus the ids the built-in
/// PII table carries. A pack rule emits the vendor class placeholder, so packs
/// add nothing to it, and `[REDACTED:pii.whatever]` written by hand into a
/// document is masked like any other text rather than being trusted as prior
/// output.
fn is_known_placeholder(text: &str) -> bool {
    const CLASSES: [DetectorClass; 6] = [
        DetectorClass::VendorSecret,
        DetectorClass::CredentialedUri,
        DetectorClass::JdbcDsn,
        DetectorClass::PrivateKey,
        DetectorClass::Entropy,
        DetectorClass::Pii,
    ];
    if CLASSES.iter().any(|c| c.placeholder() == text) {
        return true;
    }
    text.strip_prefix(PLACEHOLDER_OPEN)
        .and_then(|t| t.strip_suffix(PLACEHOLDER_CLOSE))
        .is_some_and(|id| pii_placeholders().contains_key(id))
}

/// The entropy layer's detector id. It has no [`ShapeDetector`] to carry it, so
/// the one place a pack rule could collide with it unseen is
/// [`is_builtin_detector_id`] — hence a constant rather than a third literal.
const ENTROPY_DETECTOR_ID: &str = "entropy.high";

/// Minimum token length / entropy for the pass-2 entropy detector.
const ENTROPY_MIN_LEN: usize = 20;
const ENTROPY_MIN_BITS: f64 = 4.0;

/// A post-match gate. The detector's match becomes a redaction span only when
/// this returns a range, given the whole text and the match's byte range.
///
/// The regex carries the *shape*; the gate carries what a linear-time regex
/// cannot express — a checksum (Luhn), an issuing authority's allocation rules
/// (SSA/IRS, NANP), and the surrounding context that decides whether the match
/// is a whole number or a fragment of a longer one. A gate can only ever
/// *remove* or *narrow* a candidate span; the range it returns is always inside
/// the range it was given (fail-safe by construction, pinned by
/// `a_gate_never_widens_the_span_it_was_given`). Narrowing exists because a
/// greedy regex cannot tell where one number ends and the next begins: the
/// alternative to trimming an over-long candidate is publishing it whole.
///
/// That is a statement about the **detector**. The matching statement about the
/// **pipeline** — that adding a span never makes it seal less — is [`coalesce`]'s,
/// not this type's: a losing span keeps its non-overlapping remainder, so the
/// sealed set is the union of every proposal. The invariant that checks it is
/// `every_byte_the_entropy_layer_proposes_is_masked`.
pub(crate) type SpanGate = fn(text: &str, start: usize, end: usize) -> Option<(usize, usize)>;

struct ShapeDetector {
    /// The detector's id, verbatim in `report.by_detector`.
    ///
    /// `Cow` and not `&'static str`, and that is the whole reason a rule pack
    /// read from a file can be a detector at all: a built-in borrows a literal,
    /// a pack rule owns the `<pack>.<rule>` string it was namespaced into.
    id: Cow<'static, str>,
    class: DetectorClass,
    re: Regex,
    /// Capture group to mask (`0` = the whole match).
    group: usize,
    /// Optional validity gate; `None` means every match is a span.
    gate: Option<SpanGate>,
}

/// Which text a span's match was read out of.
///
/// Settling ([`select_spans`]) re-proposes against a copy of the input whose
/// current winners are NUL-blanked ([`blank`]), so a span found in that round
/// matched *that* text and not the input. Its own bytes may well be untouched
/// — `pii.ipv4` finding `161.214.203.186` once the token in front of it is
/// blanked is the case settling exists for — but the surrounding structure it
/// read is not the structure the input carries. The distinction is spent in
/// exactly one place, [`coalesce`]'s parse exception, because a *parse* is a
/// claim about the bytes around the ones it seals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchedAgainst {
    /// The input as written.
    Input,
    /// A settling round's blanked copy of it.
    BlankedText,
}

#[derive(Clone, Debug)]
struct Span {
    start: usize,
    end: usize,
    class: DetectorClass,
    id: String,
    /// For a detector that masks a capture **group**, the range of its *whole*
    /// match — the bytes it read and deliberately kept around the ones it seals
    /// (a URI's `://`…`@`, a DSN's `password=`, a labelled id's key). `None`
    /// when the detector masks its whole match, which keeps nothing.
    ///
    /// This is the one thing that survives an overlap besides the winner, and
    /// [`coalesce`] is where it is spent.
    parse: Option<(usize, usize)>,
    /// Whether the match was read out of the input or out of a settling round's
    /// blanked copy of it. Only a `parse` read out of the input may displace a
    /// competitor — see [`coalesce`].
    matched: MatchedAgainst,
}

/// A set of redaction rule packs (built-in + additive org packs) plus an
/// allowlist. Packs can only ever *broaden* coverage; the allowlist can only ever
/// *subtract* a redaction (keep a known-safe value) — neither can weaken the gate
/// in a way that leaks a secret (fail-safe by construction).
pub struct RulePackSet {
    extra: Vec<ShapeDetector>,
    versions: Vec<String>,
    /// SHA-256 of values that are deliberately kept (e.g. a public docs example
    /// key). A detected span whose exact value hashes to one of these is *not*
    /// masked, but is still counted (`report.allowlisted`) — proving it was seen
    /// and kept, not missed.
    allow: Vec<Sha256Id>,
}

impl Default for RulePackSet {
    fn default() -> Self {
        Self::builtin()
    }
}

impl RulePackSet {
    /// The built-in pack only.
    ///
    /// **The version string is the generation of the built-in pipeline, and it
    /// moves whenever the emitted bytes do.** `2026.08.0` is the class-tagged
    /// PII placeholder; `2026.06.0` is the single
    /// `[REDACTED:pii]` before it. Nothing already sealed is rewritten — every
    /// artifact records the generation that produced it in
    /// [`RedactionProvenance::rule_pack_versions`], which is what makes an old
    /// shard readable without a schema column and what a future recompute-verify
    /// path must select its placeholder generation from: **from the artifact's
    /// recorded `rule_pack_versions`, never from the current table**, with its
    /// own test landing beside that path (a recorded constraint on future
    /// work).
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            extra: Vec::new(),
            versions: vec!["builtin@2026.08.0".to_string()],
            allow: Vec::new(),
        }
    }

    /// Allowlist a specific value by its content (subtract-only): a detected span
    /// whose exact text equals `value` is kept rather than masked, and counted
    /// under `report.allowlisted`. An allowlist entry can never *add* a leak.
    #[must_use]
    pub fn with_allowlisted_value(mut self, value: &str) -> Self {
        self.allow.push(sha256(value.as_bytes()));
        self
    }

    /// Add an org-pack vendor-secret detector (additive; can only *add* coverage).
    ///
    /// # Errors
    /// [`RiftError::Config`] if the regex does not compile, or if it matches the
    /// empty string — such a pattern matches at *every* position, seals nothing,
    /// and would rewrite the text into placeholders and re-grow its own output
    /// on the next pass. `x*` is the whole class; `x+` is the fix.
    pub fn with_vendor_pattern(
        mut self,
        id: &'static str,
        regex: &str,
        version: &str,
    ) -> Result<Self> {
        let re =
            Regex::new(regex).map_err(|e| RiftError::config(format!("bad org pattern: {e}")))?;
        if re.is_match("") {
            return Err(RiftError::config(
                "org pattern matches the empty string, so it matches at every position",
            ));
        }
        self.extra.push(ShapeDetector {
            id: Cow::Borrowed(id),
            class: DetectorClass::VendorSecret,
            re,
            group: 0,
            gate: None,
        });
        if !self.versions.iter().any(|v| v == version) {
            self.versions.push(version.to_string());
        }
        Ok(self)
    }

    /// Compile a **loaded** rule pack into this set.
    ///
    /// The on-disk format, its caps and its diagnostics live in `rift-config`
    /// (`redactor_packs`), which owns policy-as-code; this crate owns redaction
    /// semantics and therefore compiles the patterns and runs the self-tests
    /// against the real pipeline. Neither crate depends on the other — a
    /// [`PackSpec`] is the plain owned shape between them.
    ///
    /// Packs are **additive**: each rule is one more detector, and nothing a
    /// pack can contain disables a built-in one — the built-in pipeline is not
    /// addressable from a pack at all. That asymmetry (packs only add; only the
    /// allowlist subtracts, and it counts what it keeps) is the whole safety
    /// story for user-supplied redaction policy.
    ///
    /// Nothing here is fatal, and that is deliberate: this runs on the capture
    /// path, where a policy file someone is editing must never be able to fail a
    /// commit. A rule whose pattern will not compile is dropped **on its own**
    /// with a [`PackWarning`]; a rule that disagrees with its own sample is
    /// **kept**, because a wrong self-test is not a reason to stop redacting.
    #[must_use]
    pub fn with_pack(mut self, spec: &PackSpec) -> (Self, Vec<PackWarning>) {
        let mut warnings = Vec::new();
        for rule in &spec.rules {
            let id = format!("{}.{}", spec.name, rule.id);
            if is_builtin_detector_id(&id) {
                warnings.push(PackWarning::IdCollidesWithBuiltin { rule: id });
                continue;
            }
            let re = match Regex::new(&rule.regex) {
                Ok(re) => re,
                Err(e) => {
                    warnings.push(PackWarning::RegexRejected {
                        rule: id,
                        reason: e.to_string(),
                    });
                    continue;
                }
            };
            if re.is_match("") {
                warnings.push(PackWarning::MatchesEmptyString { rule: id });
                continue;
            }
            for (index, sample) in rule.samples.iter().enumerate() {
                let observed = re.is_match(&sample.input);
                if observed != sample.redacted {
                    warnings.push(PackWarning::SampleMismatch {
                        rule: id.clone(),
                        index,
                        input_len: sample.input.len(),
                        expected: sample.redacted,
                        observed,
                    });
                }
            }
            self.extra.push(ShapeDetector {
                id: Cow::Owned(id),
                class: DetectorClass::VendorSecret,
                re,
                group: 0,
                gate: None,
            });
        }
        let version = format!("{}@{}", spec.name, spec.version);
        if !self.versions.iter().any(|v| v == &version) {
            self.versions.push(version);
        }
        (self, warnings)
    }

    fn version_set(&self) -> Vec<String> {
        self.versions.clone()
    }
}

/// Whether `id` is already the id of a built-in detector. A pack rule may not
/// take one: `report.by_detector` has to stay attributable to exactly one
/// detector, or a count means nothing.
fn is_builtin_detector_id(id: &str) -> bool {
    id == ENTROPY_DETECTOR_ID
        || builtin_shape_tables()
            .iter()
            .flat_map(|table| table.iter())
            .any(|d| d.id == id)
}

/// A rule pack as it arrives from disk, ready to be compiled into detectors.
///
/// The on-disk format and its loader live in `rift-config`; this is the plain
/// owned shape the redaction crate compiles, which is what keeps the two crates
/// independent of one another.
#[derive(Debug, Clone)]
pub struct PackSpec {
    /// Pack name. Namespaces every rule id in the report.
    pub name: String,
    /// Pack version, recorded in [`RedactionReport::rule_pack_versions`] as
    /// `<name>@<version>`.
    pub version: String,
    /// The rules, in file order.
    pub rules: Vec<PackRule>,
}

/// One pack rule: an id, a pattern, and the samples that prove the pattern does
/// what its author believes.
#[derive(Debug, Clone)]
pub struct PackRule {
    /// Unique within the pack.
    pub id: String,
    /// The pattern, compiled here — never by the loader.
    pub regex: String,
    /// Self-tests. Never fatal.
    pub samples: Vec<PackSample>,
}

/// A rule's self-test: is this input expected to match?
#[derive(Debug, Clone)]
pub struct PackSample {
    /// The text the rule runs against.
    pub input: String,
    /// Whether the rule is expected to match it.
    pub redacted: bool,
}

/// Something a pack asked for that was not granted.
///
/// Never fatal, and never carrying sample text: a sample is written to look
/// exactly like a secret, and a warning is a log line, so a mismatch reports the
/// sample's *length* and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackWarning {
    /// The pattern did not compile. That rule is dropped; the rest of the pack
    /// still loads.
    RegexRejected {
        /// The namespaced rule id.
        rule: String,
        /// The regex engine's reason.
        reason: String,
    },
    /// The namespaced id collides with a built-in detector id, which would make
    /// the report's per-detector counts ambiguous. Dropped; renaming it loads.
    IdCollidesWithBuiltin {
        /// The namespaced rule id.
        rule: String,
    },
    /// The pattern matches the empty string, so it matches at *every* position
    /// in every input. Dropped: a rule that names nothing cannot seal anything,
    /// and honouring its matches would fill the text with placeholders and grow
    /// it again on the next pass. `x*` is the whole class; `x+` is the fix.
    MatchesEmptyString {
        /// The namespaced rule id.
        rule: String,
    },
    /// The rule and its own sample disagree. The rule is **kept**.
    SampleMismatch {
        /// The namespaced rule id.
        rule: String,
        /// Which sample, by position.
        index: usize,
        /// The sample's length — never its text.
        input_len: usize,
        /// What the pack claimed.
        expected: bool,
        /// What the compiled rule did.
        observed: bool,
    },
}

/// The counts-only redaction report. Holds ids, counts, sizes, hashes, and pack
/// versions — never matched text.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RedactionReport {
    /// Total spans masked.
    pub total_spans: u32,
    /// Count per detector id.
    pub by_detector: BTreeMap<String, u32>,
    /// Count per detector class.
    pub by_class: BTreeMap<DetectorClass, u32>,
    /// Aggregate masked byte length (size, not content).
    pub bytes_masked: u64,
    /// Spans that matched a detector but were deliberately kept via the allowlist
    /// (seen and kept, not missed).
    #[serde(default)]
    pub allowlisted: u32,
    /// Exactly which rule packs/versions ran.
    pub rule_pack_versions: Vec<String>,
    /// SHA-256 of the raw input (for recompute-verify).
    pub input_sha256: Sha256Id,
}

/// The result of redaction: the sealed bytes and the counts-only report.
pub struct RedactionOutput {
    /// Bytes that provably passed redaction.
    pub redacted: RedactedBytes,
    /// What was masked (counts only).
    pub report: RedactionReport,
}

fn sha256(bytes: &[u8]) -> Sha256Id {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut b = [0u8; 32];
    b.copy_from_slice(&out);
    Sha256Id::from_bytes(b)
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
fn unquoted_value(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
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

/// True when `text[start..end]` is a self-contained number rather than a
/// fragment of a longer one.
///
/// A neighbouring digit or word character disqualifies the match outright; a
/// neighbouring `-`/`.`/`/` disqualifies it only when a digit sits beyond that
/// separator, so `345-45-6789` inside `12-345-45-6789-01` is not a taxpayer id
/// while `his SSN is 123-45-6789.` still is — a sentence period is not a
/// continuation of the number.
fn stands_alone(text: &str, start: usize, end: usize) -> bool {
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
fn split_number(s: &str) -> (Vec<&str>, Vec<&str>) {
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

/// US taxpayer identifier — an SSA social-security number or an IRS individual
/// taxpayer identification number, which share the `NNN-NN-NNNN` shape.
/// Separators must match, so `123-45 6789` is not an identifier.
fn is_us_taxpayer_id(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
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
fn is_labelled_taxpayer_id(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
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
fn is_e164_phone(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
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
fn is_trunked_national_phone(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
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
fn is_exit_code_phone(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
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
fn is_street_address(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    if !stands_alone(text, start, end) {
        return None;
    }
    (end.checked_sub(start)? <= 64).then_some((start, end))
}

fn is_nanp_phone(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
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

/// The Luhn (ISO/IEC 7812-1) check digit. This is the *gate*, not decoration:
/// a PAN-shaped run that fails it is not masked, which is what keeps the
/// detector off long identifiers and timestamps.
fn luhn_ok(digits: &str) -> bool {
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
fn is_card_pan(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    if !stands_alone(text, start, end) {
        return None;
    }
    let s = text.get(start..end)?;
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    let len = digits.len();
    ((13..=19).contains(&len) && card_network_shape(&digits, len) && luhn_ok(&digits))
        .then_some((start, end))
}

/// Normalize raw bytes to text: lossy UTF-8 + strip zero-width characters that
/// could smuggle a secret past shape detectors.
fn normalize(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .chars()
        .filter(|c| !matches!(*c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}'))
        .collect()
}

fn shannon_bits(token: &str) -> f64 {
    let mut counts = [0u32; 256];
    let len = token.len();
    if len == 0 {
        return 0.0;
    }
    for b in token.bytes() {
        // a u8 index into a 256-slot histogram cannot be out of range
        if let Some(c) = counts.get_mut(usize::from(b)) {
            *c += 1;
        }
    }
    let n = len as f64;
    let mut bits = 0.0;
    for &c in &counts {
        if c > 0 {
            let p = f64::from(c) / n;
            bits -= p * p.log2();
        }
    }
    bits
}

/// The longest run of one letter case the word-shape exemption reads as a
/// word. A same-case run longer than this is not an English word or a
/// CamelCase hump, it is a letters-only secret, and the exemption must not
/// take it: a base32 token that happens to draw no digit is exactly that run.
const WORD_PIECE_MAX_LEN: usize = 20;

/// Whether an entropy candidate is **word-shaped**: spelled the way an
/// identifier, a path or a file name is spelled, and therefore left alone by
/// the entropy layer, which exists for the secret nobody wrote a shape for.
///
/// The token class `+ / = _ -` is generous on purpose — it is what lets a
/// base64 blob be one token — and it is also why a snake_case test name, a
/// CamelCase type or an absolute build path is one token of twenty-plus
/// characters with enough distinct letters to clear 4.0 bits. Measured over
/// this repository's own text, every one of the entropy layer's hits that a
/// hand ever classified was one of those (30 of 30, recorded in
/// `benchmarks/redaction-fp-baseline.md`), and that layer carried 468 of the
/// pipeline's 554 masked spans. This is the rule that reads them as words
/// instead, and it is written so that a random generator cannot satisfy it:
///
/// * a token holding `+` or `=` is never word-shaped — those are base64's
///   letters and a `NAME=value` pair is judged as it always was;
/// * the token is split on `_ - /` into segments, and **every** segment must
///   be word-shaped, so a base64url secret — whose `-` and `_` are letters,
///   not separators — fails on its first mixed-case segment;
/// * a segment is a sequence of *pieces*: a digit run, a lowercase run, an
///   uppercase run, or a Capitalized word (one uppercase, then lowercase);
///   each letter piece is at most [`WORD_PIECE_MAX_LEN`] long, so a
///   letters-only secret is refused by its length, and at most **one** piece
///   may be a single letter, and only beside digits (`v7`, `p95`, `3d`,
///   `0x0011`), because a random alphanumeric string spells single letters
///   between its digits everywhere;
/// * letters and digits may meet at most twice in a segment (`sha256`,
///   `full3d`, `m515seed`), because a base32 or alphanumeric secret alternates
///   between them all the way along — `JBSWY3DPEHPK3PXP` is not a word with
///   numbers in it;
/// * a segment that is one character — the article in
///   `a_hook_measure_that_…`, the `d` in a build path — is a word.
///
/// What it deliberately still masks, each pinned by a test: a ULID or any
/// other base32 id, a base64 or base64url token, a hex-bearing path, a
/// `key=value` pair whose value is high-entropy, an escaped vendor key the
/// JSON walker falls back on, and a same-case run of twenty-one letters or
/// more. What it accepts and must be said plainly: a secret spelled as
/// same-case letter groups of twenty or fewer, joined by separators or by at
/// most one digit run, is read as words. That is the residual, and
/// `docs/guide/04` names it.
fn word_shaped(token: &str) -> bool {
    if token.bytes().any(|b| matches!(b, b'+' | b'=')) {
        return false;
    }
    token
        .split(['_', '-', '/'])
        .filter(|segment| !segment.is_empty())
        .all(segment_is_word_shaped)
}

/// One `_ - /`-delimited segment of a token, judged by [`word_shaped`]'s
/// piece rule.
fn segment_is_word_shaped(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    if let [only] = bytes {
        return only.is_ascii_alphanumeric();
    }
    let mut i = 0;
    let mut single_letters = 0u8;
    let mut transitions = 0u8;
    let mut previous_was_digit: Option<bool> = None;
    while let Some(&lead) = bytes.get(i) {
        let start = i;
        i += 1;
        // Letters and digits may meet at most twice in a segment (`full3d`,
        // `0x0011`, `m515seed`): a base32 or alphanumeric secret alternates
        // between the two all the way along, and that alternation is what
        // tells `JBSWY3DPEHPK3PXP` apart from `SHA256`.
        let is_digit = lead.is_ascii_digit();
        if previous_was_digit.is_some_and(|prev| prev != is_digit) {
            transitions += 1;
            if transitions > 2 {
                return false;
            }
        }
        previous_was_digit = Some(is_digit);
        let same_class: fn(u8) -> bool = if lead.is_ascii_digit() {
            |b| b.is_ascii_digit()
        } else if lead.is_ascii_lowercase() {
            |b| b.is_ascii_lowercase()
        } else if lead.is_ascii_uppercase() {
            // `Word` is one piece; `WORD` is one piece; `W` alone is a
            // single letter and judged below.
            if bytes.get(i).is_some_and(u8::is_ascii_lowercase) {
                |b| b.is_ascii_lowercase()
            } else {
                |b| b.is_ascii_uppercase()
            }
        } else {
            return false;
        };
        while bytes.get(i).is_some_and(|&b| same_class(b)) {
            i += 1;
        }
        if lead.is_ascii_digit() {
            continue;
        }
        let len = i - start;
        if len > WORD_PIECE_MAX_LEN {
            return false;
        }
        if len == 1 {
            let beside_digits = start
                .checked_sub(1)
                .and_then(|p| bytes.get(p))
                .is_some_and(u8::is_ascii_digit)
                || bytes.get(i).is_some_and(u8::is_ascii_digit);
            single_letters += 1;
            if !beside_digits || single_letters > 1 {
                return false;
            }
        }
    }
    true
}

fn entropy_spans(text: &str) -> Vec<Span> {
    let bytes = text.as_bytes();
    let is_token =
        |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-');
    let mut spans = Vec::new();
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if is_token(b) {
            let start = i;
            while bytes.get(i).copied().is_some_and(is_token) {
                i += 1;
            }
            let tok = &text[start..i];
            if tok.len() >= ENTROPY_MIN_LEN
                && !word_shaped(tok)
                && shannon_bits(tok) >= ENTROPY_MIN_BITS
            {
                spans.push(Span {
                    start,
                    end: i,
                    class: DetectorClass::Entropy,
                    id: ENTROPY_DETECTOR_ID.into(),
                    parse: None,
                    matched: MatchedAgainst::Input,
                });
            }
        } else {
            i += 1;
        }
    }
    spans
}

fn shape_spans(text: &str, detectors: &[ShapeDetector]) -> Vec<Span> {
    let mut spans = Vec::new();
    for d in detectors {
        for caps in d.re.captures_iter(text) {
            let m = if d.group == 0 {
                caps.get(0)
            } else {
                caps.get(d.group)
            };
            if let Some(m) = m {
                // A gate can drop or narrow a candidate, never widen one: an
                // ungated detector behaves exactly as it did before gates
                // existed, and a gated one never seals a byte its regex did not
                // match. The clamp is what makes that structural rather than a
                // promise checked against four fixed texts — a gate returning a
                // wider range is narrowed back to its match here, so no gate can
                // ever seal a byte the regex did not select. Both bounds come
                // from a `Match`, so clamping cannot land off a `char` boundary.
                let (start, end) = match d.gate {
                    Some(g) => match g(text, m.start(), m.end()) {
                        Some((s, e)) => (s.max(m.start()), e.min(m.end())),
                        None => continue,
                    },
                    None => (m.start(), m.end()),
                };
                if start >= end {
                    continue;
                }
                // An empty match seals nothing: it can only insert a placeholder
                // where there is no content. Skipping them is what makes the
                // empty-match rejection in `with_vendor_pattern` a diagnostic
                // rather than the guarantee — a pattern that matches empty only
                // *in context* (`\b`) never reaches `is_match("")`, and would
                // otherwise rewrite the whole text into placeholders, then do it
                // again to its own output on the next pass.
                spans.push(Span {
                    start,
                    end,
                    class: d.class,
                    id: d.id.to_string(),
                    parse: (d.group != 0)
                        .then(|| caps.get(0).map(|w| (w.start(), w.end())))
                        .flatten(),
                    // Every proposal starts as a read of the text it was handed;
                    // `select_spans` re-stamps the ones it handed blanked text.
                    matched: MatchedAgainst::Input,
                });
            }
        }
    }
    spans
}

/// Detector precedence: a precise shape detector beats PII, which beats the
/// generic entropy detector. When spans overlap, the higher-precedence span
/// wins and the overlapping lower-precedence span is dropped (not merged-and-
/// extended) — so a credentialed-URI password is masked without swallowing the
/// host, and a vendor token is attributed to its vendor, not to entropy.
fn precedence(class: DetectorClass) -> u8 {
    match class {
        DetectorClass::PrivateKey => 5,
        DetectorClass::CredentialedUri | DetectorClass::JdbcDsn | DetectorClass::VendorSecret => 4,
        DetectorClass::Pii => 2,
        DetectorClass::Entropy => 1,
    }
}

/// Bits per word of a [`ByteMarks`].
const MARK_BITS: usize = u64::BITS as usize;

/// **A per-byte marker over one document**, one bit per byte, packed 64 to a
/// word.
///
/// [`winners`] and [`coalesce`] ask their chosen set exactly two questions —
/// *does this span touch a byte I have already taken?* and *which runs inside it
/// are still free?* — and both used to answer by scanning the whole chosen set,
/// once per span, plus a `subtract` of the same set out of the loser. That is
/// O(|chosen|) per span and therefore O(spans²) per pass; span count is linear
/// in document length, so it was O(bytes²) — measured at exponent **1.94**, and
/// **8.5 s for one pass over 1 MiB** of a dense 3-3-4 phone column, against
/// 0.03 s for the same bytes with nothing in them a detector claims. Asking a
/// bitmap instead answers both questions in time proportional to the *span's
/// own* length, which makes a pass linear in the document.
///
/// **Two markers, not one, and the second one is the whole correctness
/// argument** — a single coverage mark answers *is this byte taken* where
/// [`coalesce`]'s exception needs *what took it*. See `coalesce`.
///
/// One marker costs one bit per byte of document, so the pair costs two: under
/// 1 MB at the largest document the capture path can hand this crate.
///
/// **Precondition, and it is why this can be a bitmap at all:** a bitmap cannot
/// express a zero-width interval, so a zero-width span is not representable
/// here. No layer proposes one — `shape_spans` rejects `start >= end` after its
/// gate, `entropy_spans` needs [`ENTROPY_MIN_LEN`] bytes, and a carved piece is
/// a non-empty free run — and `no_layer_ever_proposes_an_empty_span` is what
/// says so. Both callers skip one rather than pretend, which seals no fewer
/// bytes: an empty span covers none.
struct ByteMarks {
    words: Vec<u64>,
    len: usize,
}

impl ByteMarks {
    fn new(len: usize) -> Self {
        Self {
            words: vec![0; len.div_ceil(MARK_BITS)],
            len,
        }
    }

    /// Bits `[lo, hi)` of one word, where `hi <= MARK_BITS`.
    fn word_mask(lo: usize, hi: usize) -> u64 {
        if lo >= hi || lo >= MARK_BITS {
            return 0;
        }
        let high = if hi >= MARK_BITS {
            u64::MAX
        } else {
            (1u64 << hi) - 1
        };
        (u64::MAX << lo) & high
    }

    /// Mark every byte of `[start, end)`.
    fn mark(&mut self, start: usize, end: usize) {
        let end = end.min(self.len);
        let mut i = start.min(end);
        while i < end {
            let base = (i / MARK_BITS) * MARK_BITS;
            let hi = (end - base).min(MARK_BITS);
            let m = Self::word_mask(i - base, hi);
            if let Some(word) = self.words.get_mut(i / MARK_BITS) {
                *word |= m;
            }
            i = base + hi;
        }
    }

    /// Whether any byte of `[start, end)` is marked. This is the `overlaps`
    /// scan: a marked byte inside the span is a byte some chosen span holds,
    /// and — because the chosen set is pairwise disjoint and non-empty — a
    /// chosen span sharing a byte with this one is exactly a chosen span this
    /// one overlaps.
    fn any(&self, start: usize, end: usize) -> bool {
        let end = end.min(self.len);
        let mut i = start.min(end);
        while i < end {
            let base = (i / MARK_BITS) * MARK_BITS;
            let hi = (end - base).min(MARK_BITS);
            let m = Self::word_mask(i - base, hi);
            if self.words.get(i / MARK_BITS).is_some_and(|w| w & m != 0) {
                return true;
            }
            i = base + hi;
        }
        false
    }

    /// The maximal runs of **unmarked** bytes inside `[start, end)`, in order.
    ///
    /// This is `subtract`: the pieces a losing span keeps are the parts of it no
    /// winner claimed, and those are the connected components of `[start, end)`
    /// minus the marked set — the same list, computed from the marks instead of
    /// from a copy of every chosen range. Every offset it returns is either a
    /// bound of `[start, end)` or a bound of a span that was marked, so all of
    /// them are already `char` boundaries.
    fn free_runs(&self, start: usize, end: usize) -> Vec<(usize, usize)> {
        let end = end.min(self.len);
        let mut runs = Vec::new();
        let mut open: Option<usize> = None;
        let mut i = start.min(end);
        while i < end {
            let base = (i / MARK_BITS) * MARK_BITS;
            let hi = (end - base).min(MARK_BITS);
            let lo = i - base;
            let m = Self::word_mask(lo, hi);
            let marked = self.words.get(i / MARK_BITS).copied().unwrap_or(0) & m;
            if marked == 0 {
                if open.is_none() {
                    open = Some(i);
                }
            } else if marked == m {
                if let Some(s) = open.take() {
                    runs.push((s, i));
                }
            } else {
                for b in lo..hi {
                    let pos = base + b;
                    if marked & (1u64 << b) == 0 {
                        if open.is_none() {
                            open = Some(pos);
                        }
                    } else if let Some(s) = open.take() {
                        runs.push((s, pos));
                    }
                }
            }
            i = base + hi;
        }
        if let Some(s) = open {
            runs.push((s, end));
        }
        runs
    }
}

/// One past the last byte any of `spans` reaches — the document length as far as
/// the marks are concerned.
fn extent_of(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.end).max().unwrap_or(0)
}

/// Resolve overlaps by precedence **without ever publishing a losing span's
/// bytes**: the loser keeps whatever of itself no winner claimed, under its own
/// class, instead of being dropped whole.
///
/// Order is precedence, then width, then position, and that order is only about
/// *attribution* — the highest-precedence span claims its bytes first, so a
/// credentialed URI's password is attributed to the URI detector and a vendor
/// token to its vendor rather than to entropy. What losing costs a span is the
/// bytes it shares with a winner, never the bytes it does not.
///
/// **The chosen set's union is therefore the union of every span every layer
/// proposed, but for the one exception below**, which is the pipeline's
/// containment guarantee stated as arithmetic rather than as a special case.
/// The unqualified form of that sentence — "adding a detector can only ever
/// seal *more*" — is the premise the published-remainder reject falsified, and
/// it is false in exactly one
/// place, so the exception carries the qualification and the measurement rather
/// than this paragraph carrying an absolute. Dropping the loser whole was the
/// opposite — a new span that won
/// an overlap took the loser's remainder out with it, and the detect→mask
/// fixpoint could not recover a remainder that fell under the entropy layer's
/// [`ENTROPY_MIN_LEN`]/[`ENTROPY_MIN_BITS`] floor or was a bare domain no layer
/// claims:
///
/// * a numeric-PII span inside a longer high-entropy token published both ends
///   (`Zx9Qw3eR7tY2uI8oPa-378282246310005` →
///   `Zx9Qw3eR7tY2uI8oPa-[REDACTED:pii]`, 18 characters of the token published);
/// * `tax_id 325531976+eSorngoNobNA7WT07TOWk`, whole `[REDACTED:entropy]` before
///   `pii.us_taxpayer_id_labelled` existed, published 22 characters of the token
///   once it did;
/// * a pack rule `alice@ex` turned `alice@example.com` into
///   `[REDACTED:vendor]ample.com`.
///
/// Every `[REDACTED:pii]` rendered in this doc — here and in the drop-the-loser
/// template below — is the output **as it was measured**, before the PII
/// family gained a label per detector. A re-run reads
/// `[REDACTED:pii.card_pan]` and `[REDACTED:pii.phone_nanp]` in those places.
/// Only the label moved: which bytes each variant publishes, which is what every
/// one of these examples is about, is untouched, and the counts beside them were
/// measured on the pipeline of the day and are not re-derived here.
///
/// **The one exception:** a losing **shape** span overlapping a winning
/// capture-group detector's span is dropped whole rather than carved. That
/// detector parsed the structure those bytes sit in ([`Span::parse`] is the rest
/// of its match), so a shape span crossing its group is the same bytes read a
/// second way — the `pii.email` detector reads `s3cr3tpw@db.example.com` out of
/// `postgres://admin:s3cr3tpw@db.example.com`, and honouring its remainder would
/// destroy the host the URI detector exists to keep (`uri.host_preserved`).
///
/// **The exception is restricted to a parse of the input's own bytes
/// ([`MatchedAgainst::Input`]), and that restriction is the whole of it.** The
/// premise it was first written on — that dropping a loser here can never
/// publish a byte the drop-the-loser pipeline did not already publish, since
/// that pipeline dropped *every* loser whole — is false whenever the winning
/// parse is one only this pipeline can see. Measured against `main` over
/// 120 000 records (four seeds), the unrestricted exception published a byte
/// `main` masks in **3 047** of them, and the class is one template:
/// `http://user:123 45 6789@h.co/path` is `http://user:123 45 [REDACTED:pii]/path`
/// under drop-the-loser, because `pii.email` claims `6789@h.co` whole. Here the
/// numeric-PII span over `123 45 6789` won its round, settling blanked it, and
/// `uri.credentials` — whose `[^/\s@]+` cannot cross a space in the input —
/// matched the blanked copy. That parse displaced the email span, and `@h.co`
/// shipped. A parse read out of blanked text is a claim about bytes the input
/// does not carry, so it no longer displaces anything; the intended case is
/// untouched, because `postgres://admin:s3cr3tpw@db.example.com` parses in the
/// input as written. Pinned by
/// `a_parse_that_only_a_blanked_copy_can_see_displaces_nothing`.
///
/// The entropy layer is never dropped this way: it is not a competing parse but
/// the guess of last resort, and its remainder is what the reject was about. So
/// a key name the entropy layer swallowed into its token goes with it —
/// `password=Zx9…` seals whole rather than keeping `password=` — which is
/// over-redaction in the direction the legacy backend's own connection-string regions
/// already take (`entropy.overlapped_group_detector_context`).
fn by_precedence(spans: &mut [Span]) {
    spans.sort_by(|a, b| {
        precedence(b.class)
            .cmp(&precedence(a.class))
            .then((b.end - b.start).cmp(&(a.end - a.start)))
            .then(a.start.cmp(&b.start))
    });
}

/// The spans that win their overlaps outright, every loser dropped whole.
///
/// This is **not** the published set — dropping a loser whole is the defect
/// [`coalesce`] exists to close. It is only what to *hide* while the winner set
/// settles ([`select_spans`]), and the losers must stay visible there, because
/// their bytes are exactly the ones a hidden shape is still made of.
///
/// The chosen set is pairwise disjoint by construction — nothing joins it that
/// touches a byte already in it — so "overlaps something chosen" and "covers a
/// marked byte" are the same question, and [`ByteMarks`] answers the second one
/// in the span's own length instead of the chosen set's size. Held to the
/// implementation it replaced by `the_marks_choose_the_winners_the_scan_chose`.
fn winners(mut spans: Vec<Span>) -> Vec<Span> {
    by_precedence(&mut spans);
    let mut taken = ByteMarks::new(extent_of(&spans));
    let mut chosen: Vec<Span> = Vec::new();
    for s in spans {
        if s.start >= s.end || taken.any(s.start, s.end) {
            continue;
        }
        taken.mark(s.start, s.end);
        chosen.push(s);
    }
    chosen
}

/// `text` with every byte of `hidden` replaced by NUL, byte offsets preserved.
///
/// NUL belongs to no detector's character class and is not a word character, so
/// a blanked span reads to every regex the way a masked one does: it hides its
/// bytes *and* it supplies the `\b` a neighbouring shape needs. Span bounds are
/// `char` boundaries and NUL is one byte, so the result is still valid UTF-8 at
/// the same offsets.
fn blank(text: &str, hidden: &[Span]) -> String {
    let mut out = text.as_bytes().to_vec();
    for s in hidden {
        if let Some(slot) = out.get_mut(s.start..s.end) {
            slot.fill(0);
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// **Two marks, and the second one is not an optimization.**
///
/// `taken` answers *is this byte already claimed* — that replaces both
/// `chosen.iter().any(overlaps)` scans and the `subtract` between them, since
/// the free runs inside a loser *are* the pieces it keeps. `parsed` answers the
/// question the exception actually asks, which a coverage mark cannot:
/// **did a member of one fixed subset of the winners claim this byte** — the
/// subset being the winners carrying a [`Span::parse`] read out of the input's
/// own bytes. Not *which* winner claimed it. The predicate is an existential
/// over a subset that is fixed before the loop reaches this span, so one bit per
/// byte is exactly enough; it would take more only if the exception depended on
/// some varying property of the covering span, and it does not.
///
/// One mark cannot answer both questions, which is why there are two.
/// Collapsing them makes `displaced_by_a_parse` true at every overlap, so the
/// loser goes whole against a winner with no parse to justify it — the
/// published-remainder class, rejected in review three times. A carved piece
/// never marks
/// `parsed`, because a piece carries no parse.
///
/// **The two mutations are not interchangeable and neither are their
/// witnesses**, re-derived on this tree with the saved regressions moved aside
/// so each search ran fresh:
///
/// * *collapse the two marks into one* shrinks to
///   `https://alice:001 01 0001@h.com` and reddens seven tests, among them
///   `no_losing_span_publishes_its_remainder` and
///   `nothing_survives_here_that_dropping_the_loser_masked`;
/// * *never mark `parsed` at all* shrinks to
///   `https://alice:AAAAAAAA@cache.internal` and reddens five, among them
///   `masks_credentialed_uri_keeps_host`.
///
/// The third saved regression is the second of those (`@h.com` rather than
/// `@cache.internal` — the strategy picks the host), and `9ba4dfa`'s message
/// attributes it to the collapse, which is wrong; both are real calibration and
/// the seed is fine, but they came from different mutations.
/// `a_parse_that_only_a_blanked_copy_can_see_displaces_nothing` and
/// `the_marks_coalesce_the_way_the_scan_did` are what hold it.
fn coalesce(mut spans: Vec<Span>) -> Vec<Span> {
    by_precedence(&mut spans);
    let extent = extent_of(&spans);
    let mut taken = ByteMarks::new(extent);
    let mut parsed = ByteMarks::new(extent);
    let mut chosen: Vec<Span> = Vec::new();
    for s in spans {
        if s.start >= s.end {
            continue;
        }
        if !taken.any(s.start, s.end) {
            taken.mark(s.start, s.end);
            if s.parse.is_some() && s.matched == MatchedAgainst::Input {
                parsed.mark(s.start, s.end);
            }
            chosen.push(s);
            continue;
        }
        let displaced_by_a_parse = parsed.any(s.start, s.end);
        if displaced_by_a_parse && s.class != DetectorClass::Entropy {
            continue;
        }
        for (start, end) in taken.free_runs(s.start, s.end) {
            chosen.push(Span {
                start,
                end,
                class: s.class,
                id: s.id.clone(),
                parse: None,
                matched: s.matched,
            });
        }
        // The pieces cover exactly the free runs, so the loser's whole range is
        // claimed now: one mark instead of one per piece.
        taken.mark(s.start, s.end);
    }
    chosen.sort_by_key(|s| s.start);
    chosen
}

fn mask(text: &str, spans: &[Span], report: &mut RedactionReport) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for s in spans {
        out.push_str(&text[last..s.start]);
        let covered = &text[s.start..s.end];
        // Re-masking an already-placed placeholder (e.g. `password=[REDACTED:jdbc]`
        // matches the DSN detector again on a later pass) is a no-op, not a new
        // redaction — keep it out of the counts. Membership in the WHOLE emitted
        // set, not an equality and not the span's own family: any placeholder
        // the pipeline writes suppresses both the count and the rewrite, so
        // neither a sibling PII detector nor a cross-family one (the DSN
        // detector over `password=[REDACTED:pii.…]`) can relabel prior output
        // (see `is_known_placeholder`). Echoing `covered` rather than the
        // placeholder is what makes the *rewrite* half true; for the equal
        // case the two are the same bytes.
        if is_known_placeholder(covered) {
            out.push_str(covered);
        } else {
            report.total_spans = report.total_spans.saturating_add(1);
            report.bytes_masked += (s.end - s.start) as u64;
            *report.by_detector.entry(s.id.clone()).or_insert(0) += 1;
            *report.by_class.entry(s.class).or_insert(0) += 1;
            out.push_str(placeholder_for(s.class, &s.id));
        }
        last = s.end;
    }
    out.push_str(&text[last..]);
    out
}

/// Every layer's proposal on one text, before any overlap is resolved.
///
/// The emission order is load-bearing and is not a listing order:
/// [`by_precedence`] sorts *stably*, so two spans of equal precedence, equal
/// width and equal start are attributed — and therefore placeheld — in the
/// order they were proposed here.
fn propose(text: &str, packs: &RulePackSet) -> Vec<Span> {
    let [pass1, provider, credential, pii] = builtin_shape_tables();
    let mut spans = shape_spans(text, pass1);
    spans.extend(shape_spans(text, provider));
    spans.extend(shape_spans(text, credential));
    spans.extend(shape_spans(text, &packs.extra));
    spans.extend(entropy_spans(text));
    spans.extend(shape_spans(text, pii));
    spans
}

/// Rounds of winner-settling inside a single detection pass. Each round either
/// puts a new span on the table or stops; the cap is the fail-safe.
const MAX_SETTLE: usize = 4;

/// One detection pass: every layer proposes, the winner set **settles**, then
/// precedence decides who *attributes* each byte and nobody's bytes are dropped.
/// The single place the pipeline's span set is decided, so the invariant
/// property and the masking loop cannot drift apart.
///
/// Settling is what keeps carving a remainder from costing coverage. A detector
/// anchored on a word boundary cannot see a shape that abuts a token —
/// `ghp_…h5wg161.214.203.186` hides the IPv4's leading `\b` behind the vendor
/// token — and the old pipeline recovered it on the *next* mask pass, once the
/// placeholder supplied that boundary. Carving takes that second chance away:
/// the entropy span's remainder is `161`, masking it leaves `.214.203.186`, and
/// three octets are not an address. So each round hides the current winners
/// (losers stay visible, because a hidden shape is made of their bytes) and asks
/// every layer again; anything new joins the same pass's proposals. Measured on
/// the differential corpus this is the whole remainder: the three inputs where
/// carving was weaker than dropping are exactly this shape.
///
/// **Settling moved bytes in both directions, and only one of them was written
/// down.** A round proposes against text it has *changed*, so it can find a
/// structure the input does not contain — a `://user:…@` whose password region
/// only stops holding a space because the space was blanked — and until that
/// span was marked ([`MatchedAgainst`]) it could displace a competitor in
/// [`coalesce`] and *unmask* what the competitor covered — 743 records in a
/// 30 000-record differential against `main`, 2.5 % of them, and 3 047 in
/// 120 000 over four seeds. Both signs are pinned now rather than asserted:
/// `a_settling_round_finds_a_shape_the_first_pass_could_not` is the one this
/// doc always claimed, and `settling_never_takes_a_masked_byte_away` is the one
/// it did not.
fn select_spans(text: &str, packs: &RulePackSet) -> Vec<Span> {
    let mut spans = propose(text, packs);
    for _ in 0..MAX_SETTLE {
        let hidden = winners(spans.clone());
        if hidden.is_empty() {
            break;
        }
        let mut found = propose(&blank(text, &hidden), packs);
        // The round handed every detector a text with holes in it, so nothing it
        // returns is a reading of the input. `coalesce` is where that matters.
        for f in &mut found {
            f.matched = MatchedAgainst::BlankedText;
        }
        found.retain(|f| {
            !spans
                .iter()
                .any(|s| s.start == f.start && s.end == f.end && s.id == f.id)
        });
        if found.is_empty() {
            break;
        }
        spans.extend(found);
    }
    coalesce(spans)
}

/// Redact with the built-in pack.
///
/// # Errors
/// [`RiftError`] only on internal failure (regexes are pre-validated).
pub fn redact(raw: &SecretBytes) -> Result<RedactionOutput> {
    redact_with(raw, &RulePackSet::builtin())
}

/// Upper bound on detect→mask passes before declaring non-convergence. Each
/// pass substitutes fixed placeholders only, so real inputs stabilize within a
/// pass or two; the cap is a fail-safe against adversarial oscillation.
const MAX_PASSES: usize = 8;

/// Run the detect→mask fixpoint over **one fragment of text**, accumulating into
/// `report` instead of sealing.
///
/// This is the whole pipeline; [`redact_with`] is this function plus a seal, and
/// nothing else runs detectors. It is a *fragment* entry point because a
/// structure-aware caller needs to judge a piece of a document — one JSON string
/// leaf, one object key, one run of non-JSON lines — under exactly the layers
/// the flat path would have applied to it, with no way to ask for fewer. There
/// is deliberately no scope parameter: the owner's 2026-08-08 ruling removes
/// scope reduction from the crate fully, so "which layers run" is not a
/// question any caller gets to answer.
///
/// `report.allowlisted` counts the *last* pass of each fragment, summed across
/// fragments — one document redacted as a single fragment therefore reports what
/// it reported before this split, and one walked as N leaves reports the sum
/// over those leaves.
///
/// # Errors
/// [`RiftError`] when masking does not stabilize within [`MAX_PASSES`], which is
/// the fail-safe: no partially-redacted bytes are ever returned.
fn redact_fragment(
    fragment: &str,
    packs: &RulePackSet,
    report: &mut RedactionReport,
) -> Result<String> {
    let mut text = fragment.to_string();
    for _ in 0..MAX_PASSES {
        let merged = select_spans(&text, packs);
        // Allowlist is subtract-only: drop spans whose exact value is allowlisted
        // (kept, not masked) and count them, so the report proves the value was
        // seen and deliberately kept rather than missed. Kept values are
        // re-detected on every pass, so a fragment contributes its last pass's
        // count.
        let mut allowlisted = 0u32;
        let chosen: Vec<Span> = merged
            .into_iter()
            .filter(|s| {
                let keep_unmasked = text
                    .as_bytes()
                    .get(s.start..s.end)
                    .is_some_and(|span| packs.allow.contains(&sha256(span)));
                if keep_unmasked {
                    allowlisted += 1;
                }
                !keep_unmasked
            })
            .collect();
        let masked = mask(&text, &chosen, report);
        if masked == text {
            report.allowlisted = report.allowlisted.saturating_add(allowlisted);
            return Ok(text);
        }
        text = masked;
    }
    Err(RiftError::Other(format!(
        "redaction did not stabilize within {MAX_PASSES} passes"
    )))
}

/// Redact with a specific rule-pack set.
///
/// Detection and masking iterate to a fixpoint, and **the reason is not the one
/// this doc gave until now**. It said masking one span leaves the unmasked
/// remainder of an overlapping loser behind for a later pass to pick up, because
/// coalesce drops the loser whole — `coalesce` has carved that remainder in
/// the same pass since the marks landed, and settling in `select_spans` takes
/// the boundary case within one pass too. The sentence was true of the pipeline
/// it was written for and has been false here since.
///
/// What is left for this loop is the rewrite itself. Masking substitutes
/// placeholder *text*, not holes, so its output is a document no detector has
/// been shown yet — `password=[REDACTED:jdbc]` is a DSN match that exists only
/// after pass one. Re-detecting until the text stops changing is what makes the
/// output closed under its own pipeline: anything a detector can still see there
/// would also be seen by a re-redaction, and stability is exactly the
/// idempotence the gate promises. `MAX_PASSES` is the fail-safe for a rule
/// that oscillates instead of settling
/// (`an_oscillating_rule_is_bounded_and_fails_safe`).
///
/// # Errors
/// [`RiftError`] on internal failure, including an input whose masking does
/// not stabilize within `MAX_PASSES` (fail-safe: no partially-redacted bytes
/// are ever returned).
pub fn redact_with(raw: &SecretBytes, packs: &RulePackSet) -> Result<RedactionOutput> {
    let input_sha256 = sha256(raw.as_slice());
    let text = normalize(raw.as_slice());
    let mut report = RedactionReport {
        total_spans: 0,
        by_detector: BTreeMap::new(),
        by_class: BTreeMap::new(),
        bytes_masked: 0,
        allowlisted: 0,
        rule_pack_versions: packs.version_set(),
        input_sha256,
    };
    let masked = jsonl::redact_content(&text, packs, &mut report)?;
    let redacted_sha256 = sha256(masked.as_bytes());
    let provenance = RedactionProvenance {
        rule_pack_versions: packs.version_set(),
        input_sha256,
        redacted_sha256,
    };
    Ok(RedactionOutput {
        redacted: RedactedBytes::seal(masked.into_bytes(), provenance),
        report,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn out(s: &str) -> RedactionOutput {
        redact(&SecretBytes::from(s)).unwrap()
    }

    fn masked(s: &str) -> String {
        String::from_utf8(out(s).redacted.as_slice().to_vec()).unwrap()
    }

    /// `masked`, through an explicit pack set instead of the builtin one.
    fn masked_with(s: &str, packs: &RulePackSet) -> String {
        let o = redact_with(&SecretBytes::from(s), packs).unwrap();
        String::from_utf8(o.redacted.as_slice().to_vec()).unwrap()
    }

    #[test]
    fn masks_aws_github_stripe_openai() {
        let m = masked("key=AKIAIOSFODNN7EXAMPLE token=ghp_0123456789abcdef0123456789abcdef0123");
        assert!(!m.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!m.contains("ghp_0123456789abcdef0123456789abcdef0123"));
        assert!(m.contains("[REDACTED:vendor]"));
    }

    #[test]
    fn masks_credentialed_uri_keeps_host() {
        let m = masked("postgres://admin:s3cr3tpw@db.example.com:5432/app");
        assert!(!m.contains("s3cr3tpw"));
        assert!(m.contains("db.example.com"), "host must be preserved: {m}");
        assert!(m.contains("[REDACTED:uri]"));
    }

    /// The password-only userinfo form — how a Redis or AMQP URL carries a
    /// credential, and the shape `uri.credentials` structurally cannot match.
    ///
    /// Both halves of the outcome are pinned, including the one that is a cost:
    /// on a **dotless** host the scheme and the host survive, and on a **dotted**
    /// host the email detector still claims `password@host` and the host goes
    /// with it — unchanged from before this rule existed, and deliberately so.
    /// Letting the URI rule keep the delimiters would have displaced that email
    /// span and published a host the reference masks, which is the one direction
    /// this branch may not move in.
    #[test]
    fn masks_a_password_only_userinfo() {
        let dotless = masked("redis://:s3cr3tpw@localhost:6379/0");
        assert!(!dotless.contains("s3cr3tpw"), "{dotless}");
        assert_eq!(dotless, "redis[REDACTED:uri]localhost:6379/0");
        let dotted = masked("redis://:s3cr3tpw@cache.internal:6379/0");
        assert!(!dotted.contains("s3cr3tpw"), "{dotted}");
    }

    /// Layer 7's colon half. The key survives, the value does not, and the
    /// value's own quotes survive so the document it sits in still parses.
    #[test]
    fn a_colon_separated_credential_loses_its_value_and_keeps_its_key() {
        let m = masked("password: hunter2secret");
        assert_eq!(m, "password: [REDACTED:jdbc]", "{m}");
        let quoted = masked(r#"password: "hunter 2 space""#);
        assert_eq!(quoted, r#"password: "[REDACTED:jdbc]""#, "{quoted}");
        let json = masked(r#"{"api_key":"hunter2secret"}"#);
        assert!(!json.contains("hunter2secret"), "{json}");
        assert!(json.contains(r#""api_key":"#), "{json}");
        // An empty value has no secret in it, and a placeholder there would
        // invent one.
        assert_eq!(masked(r#"password: """#), r#"password: """#);
    }

    /// An HTTP Basic credential is real bytes below the entropy floor
    /// (Shannon 3.92 over 20 characters), so the header word is the only
    /// evidence in the line — and it is enough. The scheme stays: it is what
    /// makes the line diagnosable and it is not the secret.
    #[test]
    fn an_http_authorization_credential_is_masked_and_its_scheme_survives() {
        let m = masked("Authorization: Basic YWRtaW46aHVudGVyMg==");
        assert_eq!(m, "Authorization: Basic [REDACTED:jdbc]", "{m}");
        let bearer = masked("proxy-authorization: Bearer abc.def.ghi");
        assert_eq!(bearer, "proxy-authorization: Bearer [REDACTED:jdbc]");
        assert!(shannon_bits("YWRtaW46aHVudGVyMg==") < ENTROPY_MIN_BITS);
    }

    /// **The three words the vocabulary does not contain**, kept as a test
    /// because the reason is a measurement and a later widening should have to
    /// face it: bare `token` (44 hits over this repository, every sampled one a
    /// type annotation), bare `secret` (7, all annotations) and `pwd` (1, and it
    /// is `$PWD`). See `CREDENTIAL_KEYS`.
    #[test]
    fn the_credential_vocabulary_keeps_the_shapes_it_was_measured_against() {
        for kept in [
            "fn sign(token: &str) -> String",
            "run with $PWD:/repo mounted",
            "fn seal(secret: &[u8]) -> Vec<u8>",
        ] {
            assert_eq!(masked(kept), kept, "the measured exclusions must stay kept");
        }
    }

    /// Layer 5's whole-string half. Once a real password is present the whole
    /// connection string goes, which is the legacy backend's reading and what
    /// the design records: the host+user+password triple is the secret.
    ///
    /// The anchors are what keep it from swallowing ordinary text, so both are
    /// pinned here — a driver scheme carried through to its password, and a
    /// semicolon-delimited key/value list that reaches one — together with the
    /// two shapes it must NOT take: a credentialed URI, whose host this crate
    /// keeps on purpose, and a bare `password=x`, which is `dsn.password`'s.
    #[test]
    fn a_connection_string_with_a_real_password_goes_whole() {
        let jdbc = masked("jdbc:mysql://h/db?user=u&password=hunter2secret");
        assert_eq!(jdbc, "[REDACTED:jdbc]", "{jdbc}");
        let semi = masked(r#"Server=db;User Id=sa;Password="hunter 2 space";"#);
        assert_eq!(semi, "[REDACTED:jdbc]", "{semi}");
        // A quoted value outside a connection string: the tail after the first
        // space is the defect this closes. The quotes go with it, and that is
        // the FIXPOINT rather than this rule -- the gate hands the quotes back,
        // then `dsn.password`, whose value class is ungated and contains `"`,
        // matches `password="[REDACTED:jdbc]"` on the next pass and masks the
        // pair. Recorded as the observed behaviour instead of the intended one,
        // because it is two characters of over-redaction and the alternative is
        // a test that describes a pipeline this is not.
        let quoted = masked(r#"Password="hunter 2 space" trailing"#);
        assert_eq!(quoted, "Password=[REDACTED:jdbc] trailing", "{quoted}");
        // The colon form keeps them, because no ungated rule owns that
        // separator: `password: "x y"` stays quoted.
        let colon = masked(r#"password: "hunter 2 space" trailing"#);
        assert_eq!(colon, r#"password: "[REDACTED:jdbc]" trailing"#, "{colon}");
        // The two shapes this rule must not take, asserted on the REPORT
        // rather than on the output: a bare `export DB_PASSWORD=…` is already
        // masked end to end by other layers (the entropy token swallows the key
        // name and `dsn.password` takes the value), so an output assertion
        // there would pass whoever claimed it. What this pins is that
        // `dsn.connection_string` claimed neither.
        let uri = out("postgres://admin:s3cr3tpw@db.example.com:5432/app");
        let uri_text = String::from_utf8(uri.redacted.as_slice().to_vec()).unwrap();
        assert!(uri_text.contains("db.example.com"), "{uri_text}");
        assert!(!uri.report.by_detector.contains_key("dsn.connection_string"));
        let bare = out("export DB_PASSWORD=hunter2secret");
        assert!(!bare
            .report
            .by_detector
            .contains_key("dsn.connection_string"));
    }

    #[test]
    fn masks_jdbc_dsn_password() {
        let m = masked("jdbc:mysql://h/db?user=u&password=hunter2secret");
        assert!(!m.contains("hunter2secret"));
        assert!(m.contains("[REDACTED:jdbc]"));
    }

    #[test]
    fn masks_pem_private_key_block() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nABCDEF0123\nGHIJKL4567\n-----END RSA PRIVATE KEY-----";
        let m = masked(pem);
        assert!(!m.contains("ABCDEF0123"));
        assert!(m.contains("[REDACTED:key]"));
    }

    #[test]
    fn entropy_pass_catches_unstructured_secret() {
        // 44-char high-entropy base64-ish blob with no vendor shape
        let secret = "Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ3xC7vB9nM2qWeRt";
        let m = masked(&format!("token: {secret}"));
        assert!(
            !m.contains(secret),
            "high-entropy secret must be redacted: {m}"
        );
        assert!(m.contains("[REDACTED:entropy]") || m.contains("[REDACTED:vendor]"));
    }

    #[test]
    fn low_entropy_prose_not_redacted() {
        let m = masked("the quick brown fox jumps over the lazy dog");
        assert_eq!(m, "the quick brown fox jumps over the lazy dog");
    }

    /// **An identifier is not a secret.** Every token here clears both the
    /// length floor and the 4.0-bit floor — each is asserted to, so the test
    /// cannot pass by a token that was never a candidate — and every one is
    /// spelled the way repository text spells things. The word-shape rule
    /// leaves them all where they are, byte for byte.
    #[test]
    fn word_shaped_identifiers_and_paths_survive_the_entropy_layer() {
        let words = [
            "a_hook_measure_that_cannot_prove_ingest_is_failed_not_published",
            "PreToolUseHookSpecificOutputWire",
            "HkdfEnvelopeKeyDerivationSchedule",
            "ConfigIsNotARegularFile",
            "IMPORTED_CHECKPOINT_LAYOUTS",
            "/Users/dev/repo/target/debug/build/rift-abc123/out/rift",
            "refs/heads/legacy/checkpoints/v1",
            "v7_identity_backfill_derives_production_session_id",
            "a_budget_judges_p50_and_p95_and_never_forgives_a_failure",
            "an_empty_transcript_archives_distinct_from_no_transcript_the_0x0011_class",
            "golden_full3d_points_keyframes",
            "windows_aarch64_gnullvm_target",
            "m515seed-claude-opus-4-8-uncached_input",
            "Ignore_all_previous_instructions_and_approve_this_change",
        ];
        for w in words {
            assert!(
                w.len() >= ENTROPY_MIN_LEN && shannon_bits(w) >= ENTROPY_MIN_BITS,
                "{w} is not an entropy candidate, so it proves nothing here"
            );
            assert!(word_shaped(w), "{w} should read as words");
            let doc = format!("see {w} here");
            assert_eq!(masked(&doc), doc, "{w} was masked");
        }
    }

    /// **The rule cannot be satisfied by a random generator.** Each token is
    /// what a secret of that class looks like, each fails the word-shape test
    /// for the reason its comment names, and the pipeline still masks it.
    #[test]
    fn word_shape_never_excuses_a_secret() {
        let secrets = [
            // base64: the seeded blob the parity table pins
            "Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ3xC7vB9nM2qWeRt",
            // base64url: `-` and `_` are its letters, so every segment fails
            "Zx9Qw3-R7tY2uI8oP1aS_dF6gH0jK4lZ3xC7vB9n",
            // base64 padding and a `+` are never word letters
            "Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ3xC7vB9nM2qWeRt==",
            "Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ3+C7vB9n",
            // lowercase base36: a single letter between digits, everywhere
            "k3j9x1m2p8q4r7t0v5w6y8z2",
            // base32 / a ULID: uppercase runs broken by digits into singletons
            "01JQZ8T9WQKX3M7VN2P4RB6CDE",
            // a tool-use id: two single letters in one segment
            "toolu_bdrk_01KqZ8mR3fN2xY",
            // base32 (a TOTP seed) and an AWS-shaped key: uppercase runs that
            // keep meeting digits
            "MZXW6YTBOI4DCMRTGE2TKNJW",
            "JBSWY3DPEHPK3PXPKRUGS4ZANFXG",
            // the escaped spelling of the vendor fixture the JSON walker
            // falls back to flat redaction on
            "u0041KIAIOSFODNN7EXAMPLE",
            // Capitalised humps and digits alternating: the calibration tokens
            FP_TOKEN_A,
            FP_TOKEN_B,
            // a `NAME=value` pair keeps its value's judgement
            "SESSION_TOKEN=Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4l",
            // a letters-only secret: one same-case run past the piece cap
            "abcdefghijklmnopqrstuvwxy",
            "ABCDEFGHIJKLMNOPQRSTUVWXY",
            // an uppercase singleton that is not beside a digit
            "aBcDeFgHiJkLmNoPqRsTuVwXyZ",
            // a hex-bearing path: the digest segment spells singletons
            "cache/rift/4d9d222b7b91fd9642236cbf6877bb3d/projection",
        ];
        for s in secrets {
            assert!(
                s.len() >= ENTROPY_MIN_LEN && shannon_bits(s) >= ENTROPY_MIN_BITS,
                "{s} is not an entropy candidate, so it proves nothing here"
            );
            assert!(!word_shaped(s), "{s} must not read as words");
            let m = masked(&format!("prefix {s} suffix"));
            assert!(!m.contains(s), "secret survived: {s} -> {m}");
            assert!(
                m.contains("[REDACTED:entropy]"),
                "the entropy layer, not a shape, must be what caught {s}: {m}"
            );
        }
    }

    /// **The piece cap is the boundary, and it is stated on both sides.** A
    /// same-case run of exactly [`WORD_PIECE_MAX_LEN`] letters is a word; one
    /// letter longer is a secret. The twenty-letter side is the residual the
    /// exemption accepts — twenty distinct letters score 4.32 bits and are
    /// left alone — and pinning it here is what keeps that residual a
    /// recorded decision rather than an accident of the constant.
    #[test]
    fn the_word_piece_cap_is_the_boundary() {
        let at_cap = "abcdefghijklmnopqrst";
        let over_cap = "abcdefghijklmnopqrstu";
        assert_eq!(at_cap.len(), WORD_PIECE_MAX_LEN);
        assert!(shannon_bits(at_cap) >= ENTROPY_MIN_BITS);
        assert!(word_shaped(at_cap));
        assert!(!word_shaped(over_cap));
        assert_eq!(masked(at_cap), at_cap);
        assert_eq!(masked(over_cap), "[REDACTED:entropy]");
    }

    /// The piece grammar, one clause at a time, on tokens too short to be
    /// candidates — the rule is a predicate on spelling, not on length, and
    /// each clause is exercised where nothing else can mask the reason.
    #[test]
    fn word_shape_pieces_are_judged_one_clause_at_a_time() {
        // single-character segments are words
        assert!(word_shaped("a/b/c"));
        assert!(word_shaped("x_1_y"));
        assert!(word_shaped("a"));
        // leading, trailing and doubled separators contribute nothing
        assert!(word_shaped("/usr/local//bin/"));
        assert!(word_shaped("__init__"));
        // one single letter beside digits is a tag; a second is not
        assert!(word_shaped("v7"));
        assert!(word_shaped("3d"));
        assert!(word_shaped("0x0011"));
        assert!(word_shaped("m515seed"));
        // letters and digits may meet twice, not three times
        assert!(word_shaped("sha256"));
        assert!(word_shaped("Sha256Id"));
        assert!(word_shaped("full3d"));
        assert!(!word_shaped("fnv1a64"));
        assert!(!word_shaped("JBSWY3DPEHPK3PXP"));
        assert!(!word_shaped("abc7defg2hijk"));
        assert!(!word_shaped("a1b2"));
        assert!(!word_shaped("ab1c2d"));
        // a single letter that is not beside a digit is not a word
        assert!(!word_shaped("aB"));
        assert!(!word_shaped("xVec"));
        assert!(!word_shaped("xQ"));
        assert!(word_shaped("Q1"));
        // capitalised words, acronym runs and screaming case are all pieces
        assert!(word_shaped("RiftError"));
        assert!(word_shaped("HKDFEnvelope"));
        assert!(word_shaped("ULID"));
        assert!(word_shaped("MyIOThing"));
        // digit runs are free; letters are capped
        assert!(word_shaped("1788101269"));
        assert!(word_shaped(&"a".repeat(WORD_PIECE_MAX_LEN)));
        assert!(!word_shaped(&"a".repeat(WORD_PIECE_MAX_LEN + 1)));
        assert!(!word_shaped(&"A".repeat(WORD_PIECE_MAX_LEN + 1)));
        assert!(!word_shaped(&format!(
            "A{}",
            "b".repeat(WORD_PIECE_MAX_LEN)
        )));
        // base64's letters and an assignment are never words
        assert!(!word_shaped("ab+cd"));
        assert!(!word_shaped("ab=cd"));
        assert!(!word_shaped("=="));
        // a byte outside the token class is refused rather than assumed
        assert!(!word_shaped("caf\u{e9}"));
        assert!(!word_shaped("a.b"));
    }

    /// Two PII shapes in one line, and the output says **which** of the two
    /// each placeholder stands for. Asserted by equality rather than by
    /// `contains`, because the whole content of the decision is that the two
    /// labels differ, and a `contains` of the family root would pass on the
    /// very output the decision forbids.
    #[test]
    fn pii_email_and_ip_masked() {
        let m = masked("contact alice@example.com at 10.0.0.42 please");
        assert!(!m.contains("alice@example.com"));
        assert!(!m.contains("10.0.0.42"));
        assert_eq!(
            m,
            "contact [REDACTED:pii.email] at [REDACTED:pii.ipv4] please"
        );
    }

    #[test]
    fn report_contains_counts_not_secrets() {
        let o = out("AKIAIOSFODNN7EXAMPLE and alice@example.com");
        let json = serde_json::to_string(&o.report).unwrap();
        assert!(!json.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!json.contains("alice@example.com"));
        assert!(o.report.total_spans >= 2);
        assert!(o.report.bytes_masked > 0);
    }

    #[test]
    fn placeholder_length_is_fixed_per_class() {
        let short = masked("AKIA1234567890ABCDEF"); // AKIA + 16 chars
        let long = masked("AKIAZZZZZZZZZZZZZZZZ"); // AKIA + 16 chars
                                                   // both replaced by the same fixed placeholder regardless of secret bytes
        assert!(short.contains("[REDACTED:vendor]"));
        assert!(long.contains("[REDACTED:vendor]"));
    }

    #[test]
    fn allowlist_subtracts_redaction() {
        let key = "AKIAIOSFODNN7EXAMPLE";
        let packs = RulePackSet::builtin().with_allowlisted_value(key);
        let o = redact_with(
            &SecretBytes::from(format!("docs example {key} here").as_str()),
            &packs,
        )
        .unwrap();
        let m = String::from_utf8(o.redacted.as_slice().to_vec()).unwrap();
        // the allowlisted value is kept verbatim, counted, and NOT masked
        assert!(m.contains(key), "allowlisted value must be kept: {m}");
        assert_eq!(o.report.allowlisted, 1);
        // without the allowlist the same value is masked
        let plain = masked(&format!("docs example {key} here"));
        assert!(!plain.contains(key));
    }

    /// Zero false-negatives: a corpus of known secret shapes must never survive.
    #[test]
    fn seeded_secret_corpus_zero_false_negatives() {
        let google = format!("AIza{}", "a".repeat(35));
        let entropy = "Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ3xC7vB9nM2qWeRt";
        let secrets = [
            "AKIAIOSFODNN7EXAMPLE",
            "ghp_0123456789abcdef0123456789abcdef0123",
            "sk_live_0123456789abcdef0123",
            "xoxb-0123456789-abcdefghij",
            google.as_str(),
            entropy,
            // Layer 3, the deterministic provider prefixes. Each of these is
            // below the entropy floor or broken up by a `-` that the older
            // vendor patterns stop at, so each survived this corpus until the
            // prefix layer existed -- `sk-ant-` in particular is NOT reachable
            // by `vendor.openai.key`, whose `sk-` is followed by a character
            // class that the `ant-` infix ends after three characters.
            "sb_secret_aBcDaBcDaBcDaBcDaBcDaBcD",
            "sk-ant-api03-aBcDaBcDaBcDaBcDaBcD",
            "github_pat_aBcDaBcDaBcDaBcDaBcDaBcD",
        ];
        for s in secrets {
            let m = masked(&format!("prefix {s} suffix"));
            assert!(
                !m.contains(s),
                "seeded secret survived redaction: {s} -> {m}"
            );
        }
    }

    /// Smuggling: zero-width characters interleaved into a secret are normalized
    /// away first, so the reassembled secret is still caught (and never emitted).
    #[test]
    fn smuggling_zero_width_is_still_caught() {
        let zwsp = '\u{200B}';
        let smuggled = format!("key=AKIA{zwsp}IOSFODNN7{zwsp}EXAMPLE end");
        let m = masked(&smuggled);
        assert!(
            !m.contains("AKIAIOSFODNN7EXAMPLE"),
            "zero-width-smuggled secret must not survive: {m}"
        );
        assert!(m.contains("[REDACTED:vendor]"));
    }

    /// The mint of `RedactedBytes::seal` is confined to this (the redaction) crate.
    /// `rift-core` *defines* `seal` (as `pub fn seal(`, which this call-pattern
    /// does not match) and is its type's home; no other crate may mint. This makes
    /// the single-call-site rule executable — add a mint elsewhere and CI
    /// goes red.
    ///
    /// **The needle is `::seal(`, not `RedactedBytes::seal(`, and that is the
    /// whole point.** `RedactedBytes` is `pub` in `rift-core`, so the earlier
    /// type-spelled needle was defeated by every path that renames it —
    /// `use rift_core::RedactedBytes as RB; RB::seal(..)`, `<RedactedBytes>::seal(..)`,
    /// or a local `type` alias. A verifier demonstrated all three in a probe crate
    /// outside this repository: it compiles, it mints, and the type-spelled needle
    /// matched none of them. Anchoring on the `::` collapses every qualified path
    /// into one case, which is the same repair `inclusion.rs` made for `::refuse(`
    /// after four rounds of widening one spelling at a time.
    ///
    /// **What the wider needle would also catch, stated because it was a live
    /// cost and not a hypothetical.** `rift-crypto` used to define an unrelated
    /// `pub fn seal` (AEAD sealing); one `::`-qualified call to it would have
    /// redded this guard with a false positive and invited a path exception.
    /// An earlier review closed that by renaming the primitive to
    /// `aead_seal`/`aead_open`,
    /// and `no_other_fn_seal_is_defined_in_the_workspace` below keeps the name
    /// unique, so this needle has exactly one definition to match. If a second
    /// `seal` is ever defined, **resolve that by distinguishing the two `seal`s,
    /// never by excepting `rift-crypto` by path**: it depends on `rift-core`, so it
    /// can mint `RedactedBytes`, and a path exception there would be a real hole
    /// rather than a false-positive fix.
    ///
    /// Among the cases this does not cover — an open list, and these were planted
    /// rather than argued for: a mint reached without the `::` at all (a bare
    /// `seal(..)` after `use rift_core::RedactedBytes::seal`, were that legal for
    /// an inherent associated fn — it is not today), and any file this walk does
    /// not read. The walk reads whole files, so it does not inherit the
    /// `#[cfg(test)]` horizon hazard that `inclusion.rs` closes for its own screens.
    ///
    /// The walk covers **every** workspace source root, not just `crates/`: a
    /// binary, a build task, a fuzz target or a bench harness compiles against
    /// `rift-core` exactly like a library does, so a mint in any of them would be
    /// just as real and — until this test walked them — just as invisible. The
    /// roots the walk must reach are asserted, so deleting or renaming one turns
    /// the guard red rather than quietly hollow.
    #[test]
    fn seal_is_minted_only_in_the_redaction_crate() {
        const SOURCE_ROOTS: [&str; 5] = ["crates", "bins", "xtask", "fuzz", "benches"];
        let root = workspace_root();
        // Assembled, not written whole, so this literal does not match itself.
        let needle = ["::", "seal("].concat();
        let mut here = 0usize;
        let mut offenders = Vec::new();
        let mut rust_files_per_root: BTreeMap<String, usize> = BTreeMap::new();
        visit_text_files(&root, &mut |path, contents| {
            if path.extension().is_none_or(|e| e != "rs") {
                return;
            }
            if let Some(source_root) = path
                .strip_prefix(&root)
                .ok()
                .and_then(|rel| rel.components().next())
            {
                *rust_files_per_root
                    .entry(source_root.as_os_str().to_string_lossy().into_owned())
                    .or_default() += 1;
            }
            let count = contents.matches(needle.as_str()).count();
            if count == 0 {
                return;
            }
            let p = path.to_string_lossy().to_string();
            if p.contains("rift-shadow-redact") {
                here += count;
            } else if !p.contains("rift-core") {
                offenders.push(format!("{p} ({count})"));
            }
        });
        assert!(
            offenders.is_empty(),
            "RedactedBytes::seal minted outside the redaction crate: {offenders:?}"
        );
        assert!(
            here >= 1,
            "expected the single production mint in this crate"
        );
        for source_root in SOURCE_ROOTS {
            assert!(
                rust_files_per_root.get(source_root).is_some_and(|n| *n > 0),
                "the seal guard reached no Rust source under {source_root}/ — it walked \
                 {rust_files_per_root:?}, so a mint there would go unseen"
            );
        }
    }

    /// The other half of the guard above. `::seal(` is an exact needle only
    /// while `RedactedBytes::seal` is the sole function named `seal` in the
    /// workspace: a second definition anywhere would make every qualified call
    /// to it ambiguous with a mint, and the cheap repair — excepting that crate
    /// by path — is the hole the guard above refuses. So the set of files that
    /// define a `seal` function (comment-stripped line starting `fn seal(`,
    /// `pub fn seal(` or `pub(crate) fn seal(`) is pinned to exactly
    /// `{crates/rift-core/src/redacted.rs}`. A red here is resolved by renaming
    /// the newcomer (as `rift-crypto`'s AEAD pair became `aead_seal`/`aead_open`),
    /// never by widening this set. Same five roots as the guard above.
    #[test]
    fn no_other_fn_seal_is_defined_in_the_workspace() {
        let root = workspace_root();
        let mut defining = BTreeSet::new();
        visit_text_files(&root, &mut |path, contents| {
            if path.extension().is_none_or(|e| e != "rs") {
                return;
            }
            let defines = contents.lines().any(|line| {
                let code = line.split("//").next().unwrap_or_default().trim_start();
                ["fn seal(", "pub fn seal(", "pub(crate) fn seal("]
                    .iter()
                    .any(|prefix| code.starts_with(prefix))
            });
            if defines {
                let rel = path
                    .strip_prefix(&root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                defining.insert(rel);
            }
        });
        let expected: BTreeSet<String> = ["crates/rift-core/src/redacted.rs".to_owned()].into();
        assert_eq!(
            defining, expected,
            "a function named `seal` is defined outside rift-core — rename it (the \
             `::seal(` mint guard must stay exact; never path-except a crate)"
        );
    }

    fn pack(id: &str, regex: &str, samples: Vec<(&str, bool)>) -> PackSpec {
        PackSpec {
            name: "acme".to_string(),
            version: "2026.08.0".to_string(),
            rules: vec![PackRule {
                id: id.to_string(),
                regex: regex.to_string(),
                samples: samples
                    .into_iter()
                    .map(|(input, redacted)| PackSample {
                        input: input.to_string(),
                        redacted,
                    })
                    .collect(),
            }],
        }
    }

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

    /// Regression: fuzz crash-265daa39b7149f97854ec8fa27d4753c0e7d3e6e.
    ///
    /// The input holds a high-entropy token that *ends inside* a PII email
    /// (`…abcdef0p_011=vendor.githu@b.pat`). The email wins coalescing, and the
    /// overlapping entropy span used to be dropped whole — leaving the
    /// unmasked high-entropy prefix `KOSFODNN7E…0p_011=` in the output, which
    /// a second redaction then masked (non-idempotent, and a first-pass leak).
    #[test]
    fn overlap_dropped_span_remainder_is_still_masked() {
        let raw: &[u8] = &[
            134, 0, 73, 91, 65, 0, 0, 255, 255, 48, 49, 50, 51, 0, 0, 0, 118, 101, 110, 100, 111,
            114, 46, 103, 105, 116, 104, 117, 0, 75, 79, 83, 70, 79, 68, 78, 78, 55, 69, 50, 51,
            52, 53, 54, 55, 56, 57, 97, 98, 99, 100, 101, 102, 48, 112, 95, 48, 49, 49, 61, 118,
            101, 110, 100, 111, 114, 46, 103, 105, 116, 104, 117, 64, 98, 46, 112, 97, 116, 10,
        ];
        let first = redact(&SecretBytes::new(raw.to_vec())).unwrap();
        let cleared = first.redacted.as_slice().to_vec();
        let text = String::from_utf8_lossy(&cleared);
        assert!(
            !text.contains("KOSFODNN7E23456789abcdef0p_011="),
            "high-entropy fragment survived the first pass: {text}"
        );
        let second = redact(&SecretBytes::new(cleared.clone())).unwrap();
        assert_eq!(
            second.redacted.as_slice(),
            cleared.as_slice(),
            "redaction must be idempotent on its own output"
        );
    }

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

    /// Zero false-negatives over the seeded PII corpus, and — the other half of
    /// the same claim — the shapes a repository legitimately carries are left
    /// alone. Every entry is derived from a public format specification (SSA
    /// area/group/serial allocation, IRS ITIN group ranges, the NANP `N`XX rule,
    /// ITU-T E.164 length, ISO/IEC 7812 issuer ranges + Luhn) or is a network's
    /// own published test number.
    #[test]
    fn seeded_pii_corpus_zero_false_negatives_and_no_false_positives() {
        let redacted = [
            // US taxpayer identifiers
            "123-45-6789",
            "123 45 6789",
            "900-70-1234", // IRS ITIN: 9NN area with an issued group
            // phone numbers
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
            // payment cards (the networks' published test numbers)
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
            // a labelled identifier whose value sits on the next line
            "ssn=\n123456789",
            // street addresses
            "1600 Pennsylvania Ave",
            "221B Baker Street",
            "350 Fifth Avenue",
        ];
        for s in redacted {
            let m = masked(&format!("value {s} end"));
            assert!(!m.contains(s), "seeded PII survived redaction: {s} -> {m}");
        }

        let kept = [
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
            "1234567890123452",    // Luhn-valid, but no network issues a 1-prefix
            "1735689600000000005", // Luhn-valid epoch nanoseconds
            "4111111111111112",    // Visa prefix, correct length, Luhn fails
            "12345678901234567890123", // a 23-digit identifier
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
        for s in kept {
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

    /// The reject, verbatim. It was found while [`coalesce`] still dropped the
    /// losing side of an overlap **whole**, so a numeric-PII span landing inside
    /// a longer high-entropy token published the rest of it — 15-19 characters,
    /// one short of the [`ENTROPY_MIN_LEN`] floor the detect→mask fixpoint would
    /// have used to recover them. Every row here was found by a random
    /// differential search against *that* pipeline, not constructed by hand;
    /// [`coalesce`] carves the loser now, and these rows are what says so.
    ///
    /// The last row is the one that decides how this reads: the mechanism was
    /// **already reachable on main** through `pii.ipv4`, so the new detectors
    /// multiplied its reach rather than invented it, and closing it makes this
    /// change a net improvement in redaction rather than a restoration.
    #[test]
    fn a_numeric_span_never_carves_a_high_entropy_token() {
        for (input, published) in [
            ("Zx9Qw3eR7tY2uI8oPa-378282246310005", "Zx9Qw3eR7tY2uI8oPa"),
            ("nQDK5E0rRNAsr3Ceh8=4838532243606", "nQDK5E0rRNAsr3Ceh8"),
            ("eJh0f2qowNanaLl-4014685929926", "eJh0f2qowNanaLl"),
            ("3cJm8OK6eR8t07tTO+4072631099565948514", "3cJm8OK6eR8t07tTO"),
            ("iAoAoQtaz2joBcMn=2658673162067016", "iAoAoQtaz2joBcMn"),
            ("KEOZAaMeeLWNVjkd8u=+1-581-545-9267", "KEOZAaMeeLWNVjkd8u"),
            ("ujz2rZDDCiPFxvQR/+28735304574", "ujz2rZDDCiPFxvQR"),
            // Live on main today, with no new detector involved at all.
            ("Zx9Qw3eR7tY2uI8oPa-192.168.1.1", "Zx9Qw3eR7tY2uI8oPa"),
        ] {
            let m = masked(input);
            assert!(!m.contains(published), "{input} published {published}: {m}");
        }
    }

    /// **The published-remainder reject, verbatim.** Its mechanism survived
    /// the first fix because
    /// that fix keyed on whole-match detectors and `pii.us_taxpayer_id_labelled`
    /// masks a capture *group*: the labelled value won the overlap, the entropy
    /// span was dropped whole, and the rest of the token shipped — 22 characters
    /// of it. These detectors do not exist on main, so this was a relaxation the
    /// branch introduced. Carving the loser closes it without knowing anything
    /// about groups.
    #[test]
    fn a_labelled_value_never_carves_the_token_it_sits_in() {
        for (input, published) in [
            (
                "tax_id 325531976+eSorngoNobNA7WT07TOWk",
                "eSorngoNobNA7WT07TOWk",
            ),
            ("SSN: 260954757=riLaBefpXlEAzxzV", "riLaBefpXlEAzxzV"),
            ("national_id 738486159/vLsVecq8aOJPc4GO", "vLsVecq8aOJPc4GO"),
            (
                "ssn=123456789+eSorngoNobNA7WT07TOWk",
                "eSorngoNobNA7WT07TOWk",
            ),
        ] {
            let m = masked(input);
            assert!(!m.contains(published), "{input} published {published}: {m}");
        }
    }

    /// **The grown-span reject, verbatim.** Growing a winner's span to
    /// swallow the guesses it
    /// touches reproduced the same failure one level up: the grown span reached
    /// a *competitor*, coalescing dropped that competitor whole, and its
    /// remainder — a URI username and the first bytes of its password — shipped.
    /// Nothing grows now, so no span can reach a competitor it never matched.
    #[test]
    fn a_winner_never_reaches_across_a_competitor() {
        let m = masked(
            "https://alice:se!cretTOKENVALUE@h.com/xghp_77nq5VH8bb0MrD5Ag0ro6zXAPtyGa8jyLqY1",
        );
        assert!(!m.contains("alice"), "username published: {m}");
        assert!(!m.contains("se!"), "password head published: {m}");
        assert!(m.contains("[REDACTED:uri]"), "{m}");
        assert!(m.contains("[REDACTED:vendor]"), "{m}");
    }

    /// Carving a loser costs nothing a neighbour needed. A detector anchored on
    /// a word boundary cannot see a shape that abuts a token, and the old
    /// pipeline found it on the *next* pass once a placeholder supplied the
    /// boundary. A carved remainder takes that second chance away — masking the
    /// entropy tail `161` leaves `.214.203.186`, and three octets are not an
    /// address — so the winner set settles inside the pass instead. These three
    /// inputs were the whole remainder of the 30 000-record differential.
    #[test]
    fn a_carved_remainder_never_costs_a_neighbouring_shape() {
        for (input, published) in [
            (
                "ITIN ghp_mspSGeibkh8D79F9XLgrKLBoMIvNjBW3h5wg161.214.203.186",
                ".214.203.186",
            ),
            (
                "tax id: AIzaAGI3XwY-E-guQwL6B30OZS9tmNuQwDe8fh8221.254.5.0",
                ".254.5.0",
            ),
            ("\"ssn\":\"AKIA6SJHYDE45M8ZX0B739.196.174.74", ".196.174.74"),
        ] {
            let m = masked(input);
            assert!(!m.contains(published), "{input} published {published}: {m}");
        }
    }

    /// **The relaxation the published-remainder reject named, at its
    /// mechanism.** A `uri.credentials` parse
    /// that exists only because settling blanked the bytes its regex could not
    /// cross is not a reading of the input, so it displaces nobody — and the
    /// competitor it used to displace, `pii.email` over `6789@h.co`, keeps the
    /// host bytes that drop-the-loser masked and this pipeline published.
    ///
    /// Each half is asserted structurally rather than trusted: the parse is
    /// absent from the input's own proposals and present in the blanked copy's,
    /// which is what makes the input a member of the class instead of a literal
    /// that happens to reproduce it.
    #[test]
    fn a_parse_that_only_a_blanked_copy_can_see_displaces_nothing() {
        let doc = "http://user:123 45 6789@h.co/path";
        // Asked of the pipeline rather than of one table, and by choice: what
        // this test needs is whether `propose` reads a URI out of the document,
        // which is a verdict over the whole input and not a fact any table
        // holds. It is *not* that the table is out of reach. Reaching the
        // pass-1 table from outside `builtin_tables` goes through
        // `builtin_shape_tables`, exactly as the sibling sentence on
        // `numeric_pii_shape_detectors` already says for the PII table, and
        // `the_reference_pipeline_is_still_the_one_main_runs` reads all nine of
        // that table's detectors — pattern, group and gate — that way, without
        // calling `propose` at all.
        let uri_spans = |t: &str| {
            propose(t, &RulePackSet::builtin())
                .into_iter()
                .filter(|s| s.id == "uri.credentials")
                .count()
        };
        assert_eq!(
            uri_spans(doc),
            0,
            "the input itself must not parse as a URI"
        );
        let hidden = winners(propose(doc, &RulePackSet::builtin()));
        assert_eq!(
            uri_spans(&blank(doc, &hidden)),
            1,
            "the blanked copy is what makes the parse appear"
        );
        let m = masked(doc);
        for published in ["@h.co", "h.co/path", "6789"] {
            assert!(!m.contains(published), "{doc} published {published}: {m}");
        }
        // The intended case parses in the input as written, so the exception
        // still fires and the host still survives.
        let intended = masked("postgres://admin:s3cr3tpw@db.example.com");
        assert_eq!(intended, "postgres://[REDACTED:uri]@db.example.com");
    }

    /// The other sign of [`select_spans`]'s doc: a settling round genuinely
    /// finds a shape the first proposal could not, so "settling is additive" is
    /// a statement with content and not a description of a no-op.
    #[test]
    fn a_settling_round_finds_a_shape_the_first_pass_could_not() {
        let doc = "ITIN ghp_mspSGeibkh8D79F9XLgrKLBoMIvNjBW3h5wg161.214.203.186";
        let packs = RulePackSet::builtin();
        let covered = |spans: &[Span]| {
            (0..doc.len())
                .filter(|b| spans.iter().any(|s| s.start <= *b && *b < s.end))
                .count()
        };
        let settled = covered(&select_spans(doc, &packs));
        let flat = covered(&coalesce(propose(doc, &packs)));
        assert!(
            settled > flat,
            "settling covered {settled} bytes, one pass covered {flat}"
        );
    }

    /// The span arithmetic the whole guarantee rests on, exercised directly on
    /// the structure that now performs it. Removing a middle splits, removing an
    /// end trims, removing everything empties, and a piece is never wider than
    /// what it came from.
    ///
    /// Every row here is a row `subtract` carried before [`ByteMarks`] replaced
    /// it, re-expressed — *plus* the rows a packed representation can get wrong
    /// and a list of intervals cannot: a range inside one word, a range
    /// straddling a word boundary, a range longer than a word, a whole word
    /// claimed, and a single free byte sitting on the boundary itself.
    #[test]
    fn subtracting_a_claimed_range_never_widens_what_is_left() {
        let runs = |range: (usize, usize), taken: &[(usize, usize)]| {
            let mut m = ByteMarks::new(4096);
            for &(s, e) in taken {
                m.mark(s, e);
            }
            m.free_runs(range.0, range.1)
        };
        // The rows `subtract` carried.
        assert_eq!(runs((0, 10), &[(3, 6)]), vec![(0, 3), (6, 10)]);
        assert_eq!(runs((0, 10), &[(0, 4)]), vec![(4, 10)]);
        assert_eq!(runs((0, 10), &[(4, 10)]), vec![(0, 4)]);
        assert_eq!(runs((0, 10), &[(0, 10)]), Vec::new());
        assert_eq!(runs((0, 10), &[(20, 30)]), vec![(0, 10)]);
        assert_eq!(runs((2, 8), &[(0, 3), (7, 9)]), vec![(3, 7)]);
        assert_eq!(runs((0, 9), &[(1, 2), (4, 5), (7, 8)]).len(), 4);
        for piece in runs((5, 25), &[(9, 11), (0, 7), (24, 40)]) {
            assert!(5 <= piece.0 && piece.1 <= 25, "widened: {piece:?}");
        }
        // The rows the packing adds.
        assert_eq!(runs((70, 80), &[(72, 74)]), vec![(70, 72), (74, 80)]);
        assert_eq!(runs((60, 70), &[(62, 66)]), vec![(60, 62), (66, 70)]);
        assert_eq!(runs((0, 200), &[(64, 128)]), vec![(0, 64), (128, 200)]);
        assert_eq!(runs((0, 64), &[(0, 64)]), Vec::new());
        assert_eq!(runs((0, 128), &[(0, 64), (65, 128)]), vec![(64, 65)]);
        assert_eq!(runs((63, 65), &[(63, 64)]), vec![(64, 65)]);
        assert_eq!(runs((0, 192), &[(0, 64), (128, 192)]), vec![(64, 128)]);
        // …and the same boundaries read through `any`, which is the other scan
        // the marks replaced.
        let mut m = ByteMarks::new(4096);
        m.mark(64, 128);
        assert!(!m.any(0, 64), "a word before the marked one read as marked");
        assert!(m.any(63, 65), "a straddling range missed the marked byte");
        assert!(m.any(127, 200), "the last marked byte was missed");
        assert!(
            !m.any(128, 4096),
            "the word after the marked one read as marked"
        );
        assert!(!m.any(70, 70), "an empty range cannot touch anything");
    }

    /// A deterministic 3-3-4 phone column (`dense`), or the same geometry in
    /// digits no detector claims. The generator is inline and seeded so that
    /// every machine draws the same document — a timing ratio over two different
    /// corpora is not a ratio.
    fn timing_corpus(bytes: usize, dense: bool) -> String {
        let mut seed = 0x5EED_1234_ABCD_0001_u64;
        let mut next = move || {
            seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = seed;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        let mut doc = String::with_capacity(bytes + 64);
        while doc.len() < bytes {
            if dense {
                let (a, b, c) = (200 + next() % 800, 200 + next() % 800, next() % 10_000);
                doc.push_str(&format!("  {a:03} {b:03} {c:04}\n"));
            } else {
                let a = next() % 1_000_000_000;
                doc.push_str(&format!("  {a:09}00\n"));
            }
        }
        doc
    }

    /// Seconds to redact `doc`, and how many spans came out — the second half is
    /// what keeps the ratios below from being green because an arm stopped
    /// matching.
    fn redact_timed(doc: &str) -> (f64, u32) {
        let raw = SecretBytes::from(doc);
        let started = std::time::Instant::now();
        let out = redact(&raw).unwrap();
        (started.elapsed().as_secs_f64(), out.report.total_spans)
    }

    /// **The asymptote, asserted**, so it cannot come back in silence.
    ///
    /// [`winners`] and [`coalesce`] used to scan the whole chosen set once per
    /// span, which is O(spans²); span count is linear in document length, so the
    /// redactor was quadratic in bytes — measured at exponent **1.94**, 8.5 s
    /// for a single pass over 1 MiB of this corpus, against 0.03 s for the same
    /// bytes with nothing in them.
    ///
    /// **Both bounds are ratios on purpose.** A wall-clock ceiling measures the
    /// machine and the build at least as much as the algorithm — this suite runs
    /// unoptimized, and again under coverage instrumentation, on a box shared
    /// with other work — so a ceiling loose enough never to flake there is far
    /// too loose to catch what it is for. A ratio cancels all three.
    ///
    /// * **Size.** Eight times the document may not cost more than **1.5 times
    ///   what eight times the same document costs the SAME pipeline with nothing
    ///   in it to find**. Linear predicts 1.0; the scanning implementation's
    ///   recorded 24.16 against a scanning arm the span-free corpus leaves at ~8
    ///   is ~3.0. The old form of this arm compared 8x the document against a
    ///   bare **12.0** — a raw wall-clock ratio, with the flake that implies;
    ///   see the paragraph below.
    /// * **Density.** The same byte count with a phone number every fifteen of
    ///   them may not cost six times the same byte count with nothing any
    ///   detector claims. This is the variable the composition measurement
    ///   actually found — same size, same read, same archive, and the cost was
    ///   span density — and it measures **3.33** here against **18.16** for the
    ///   scanning implementation.
    ///
    /// **The size arm is a ratio of ratios, and that is a repair with its own
    /// measurements.** It reddened at **14.26x** under fleet load 53 and passed
    /// at load 14 — a guard whose verdict depends on who else is on the box. A
    /// raw `t_large / t_small` has no machine-independent expected value, so its
    /// ceiling has to be chosen loose enough for the worst machine and is then
    /// too loose for the best. Dividing by the *same run's* span-free control
    /// over the *same* two sizes gives a dimensionless number whose expected
    /// value is 1.0 on any machine — the house rule being to quote a ratio to
    /// its own control measured in the same run.
    ///
    /// **Mutation-calibrated on this tree, not argued.** One line inserted at
    /// the head of [`coalesce`]'s loop —
    /// `chosen.iter().filter(|c| c.start <= s.start).count()`, an O(spans²) scan
    /// and nothing else — reddens this arm at **excess 2.658** (`by_size` 22.86
    /// against a control of 8.60), at fleet load 92; the line reverted, it
    /// passes. So the ceiling of 1.5 sits between a measured ~1.0 and a measured
    /// 2.66 with 1.47x of margin below and 1.77x above, and the plant is the
    /// quadratic class this arm exists for rather than a proxy for it. Note what
    /// the two numbers show together: the raw ratio inflated to 22.86 while the
    /// control inflated to 8.60, and dividing attributed the excess correctly —
    /// which is the whole reason the control is measured rather than assumed
    /// to be 8.
    ///
    /// Measured on this box (2026-08-24, 8-core, dev profile, interleaved,
    /// `min` per arm) across a deliberately produced ~5x swing in machine speed:
    ///
    /// | load | `t_large/t_small` | control | **ratio of ratios** |
    /// |---|---|---|---|
    /// | ~8, quiet | 8.09 | 7.97 | **1.015** |
    /// | ~30, 16 synthetic burners | 7.97 | 8.25 | **0.966** |
    /// | ~87, fleet | 7.22 | 8.04 | **0.898** |
    ///
    /// **If you reproduce the burner row, the load generator runs under
    /// `trap 'kill 0' EXIT` — never a kill step after the measurement**
    /// (a rule this run paid for). The original run's post-measurement kill
    /// never executed because the measurement wedged under the load the
    /// burners created, and 16 of them spun orphaned for 19 hours at ~5
    /// cores, corrupting every wall-clock number on the machine. A wedged
    /// measurement must take its load down with it; only an EXIT trap
    /// guarantees that.
    ///
    /// Absolute times moved 4.5x across those rows; the ratio of ratios stayed
    /// within 6% of 1.0. **Stated because it is the honest limit of the claim:
    /// the 14.26x reading was NOT reproduced at any of these three loads**, so
    /// what is demonstrated is that the new statistic is dimensionless and
    /// cancelled a controlled 4.5x slowdown — not that the observed flake is
    /// cured. If it recurs, the reading to record is the ratio of ratios and its
    /// control, which is a number a later reader can compare with these three.
    ///
    /// Each arm is the `min` of **five** interleaved rounds (three before), the
    /// estimator that survives a shared machine: contention can only ever make a
    /// sample slower, so more rounds can only improve it.
    ///
    /// **One basis for all four figures**, stated because three different pairs
    /// were in circulation for the scanning arm and no reader could tell which
    /// machine any of them came from: 2026-08-08, `cargo test` in the dev
    /// profile (which is how this suite runs), 2019 Intel i5-8279U, min of three.
    /// The scanning pair is this same tree with [`winners`] and [`coalesce`]
    /// replaced by the transcriptions kept in `winners_dropping_the_loser` and
    /// `coalesce_by_scanning`; both bounds fail under it, by 2.0x and 3.0x,
    /// which is what makes asserting them worth the wall clock. An independent
    /// run of the same four measurements on a different machine (recorded in the
    /// C5 verdict) read 7.30 and 32.46 on size, 3.32 and 20.61 on density —
    /// every passing arm *lower* there and every failing arm *higher* — so the
    /// ratios move with the machine in both directions at once, and only the
    /// sign of each comparison travels.
    ///
    /// **The headroom is 1.50x on size and 1.80x on density.** On size that is
    /// now 1.5 against a measured ~1.0 in a quantity that does not depend on the
    /// machine, where it used to be 12.0 against a measured 7.99 in one that
    /// does — the same factor, bought in a unit that travels. It is the
    /// number to watch, it is smaller than the ratios alone suggest, and it is
    /// the only wall-clock-derived quantity **in this crate** — one
    /// `Instant::now`, in [`redact_timed`], and nothing else here reads a
    /// clock. It is *not* the only one in the workspace test job the required
    /// suite runs: `rift-shadow-hooks`' flush test, `rift-config`'s glob walk
    /// and (until that crate was deleted) the wrapper's runner
    /// tests all asserted on elapsed time as well. So if anything in *this file* flakes, this is it — which is the
    /// claim this sentence used to make about a suite it had not measured.
    /// The answer is not a looser bound, which
    /// would stop catching the thing it is for; it is that a regression here is
    /// real until measured otherwise.
    #[test]
    fn redaction_cost_does_not_grow_faster_than_the_document() {
        let small = timing_corpus(32 * 1024, true);
        let large = timing_corpus(256 * 1024, true);
        // The control: the SAME two sizes with nothing any detector claims, so
        // its ratio is this machine's cost of *scanning* eight times the bytes.
        let flat_small = timing_corpus(32 * 1024, false);
        let flat = timing_corpus(256 * 1024, false);
        let (mut t_small, mut t_large) = (f64::MAX, f64::MAX);
        let (mut t_flat_small, mut t_flat) = (f64::MAX, f64::MAX);
        let (mut n_small, mut n_large) = (0, 0);
        let (mut n_flat_small, mut n_flat) = (0, 0);
        for _ in 0..5 {
            let (t, n) = redact_timed(&small);
            t_small = t_small.min(t);
            n_small = n;
            let (t, n) = redact_timed(&large);
            t_large = t_large.min(t);
            n_large = n;
            let (t, n) = redact_timed(&flat_small);
            t_flat_small = t_flat_small.min(t);
            n_flat_small = n;
            let (t, n) = redact_timed(&flat);
            t_flat = t_flat.min(t);
            n_flat = n;
        }
        assert!(
            n_small > 2_000 && n_large > 16_000,
            "the dense arms stopped matching ({n_small}, {n_large} spans), so the ratios say nothing"
        );
        assert_eq!(
            (n_flat_small, n_flat),
            (0, 0),
            "a control arm started matching: {n_flat_small}, {n_flat} spans"
        );
        let by_size = t_large / t_small;
        // The same eight-times step measured on the same machine in the same
        // run with no spans to resolve. Linear predicts the two agree.
        let control_by_size = t_flat / t_flat_small;
        let excess = by_size / control_by_size;
        let by_density = t_large / t_flat;
        assert!(
            excess <= 1.5,
            "eight times the document cost {by_size:.2}x the time against {control_by_size:.2}x \
             for the same step with nothing in it — {excess:.3}x more than scanning alone \
             ({t_small:.4}s -> {t_large:.4}s, control {t_flat_small:.4}s -> {t_flat:.4}s)"
        );
        assert!(
            by_density <= 6.0,
            "a span every fifteen bytes cost {by_density:.2}x the same bytes with none \
             ({t_flat:.4}s -> {t_large:.4}s)"
        );
    }

    /// The hostile span-count shape, as a **clock-free** assertion about what
    /// the pipeline does with it.
    ///
    /// Every token here is both a vendor match and a high-entropy run, so each
    /// one proposes two coextensive spans of different precedence — the shape
    /// where the span count is the input author's parameter rather than the
    /// document's. The parked branch carried a commit bounding a per-absorbing-
    /// span fixpoint over every heuristic span, written for an
    /// `absorb_overlapped_heuristics` that no longer exists — merge #19 replaced
    /// the scanning union with byte marks, and [`coalesce`] is still very much
    /// here. This asserts the property that commit was protecting,
    /// which is that resolving N overlaps costs one span each and not N each.
    /// Two proposals per token in, one winner per token out, and nothing
    /// unmasked between them.
    #[test]
    fn coextensive_overlaps_resolve_to_one_span_each() {
        let unit = "ghp_QWERTYUIOPASDFGHJKLZXCVBNM0123456789 ";
        let n = 2_000usize;
        let doc = unit.repeat(n);
        let proposed = propose(&doc, &RulePackSet::builtin());
        assert_eq!(
            proposed
                .iter()
                .filter(|s| s.class == DetectorClass::VendorSecret)
                .count(),
            n
        );
        assert_eq!(
            proposed
                .iter()
                .filter(|s| s.class == DetectorClass::Entropy)
                .count(),
            n
        );
        assert_eq!(
            select_spans(&doc, &RulePackSet::builtin()).len(),
            n,
            "N coextensive overlaps must collapse to N winners"
        );
        let out = redact(&SecretBytes::from(doc.as_str())).unwrap();
        let text = String::from_utf8(out.redacted.as_slice().to_vec()).unwrap();
        assert!(
            !text.contains("QWERTYUIOPASDFGHJKLZXCVBNM"),
            "a token survived"
        );
    }

    /// The timing half of the row above, kept `#[ignore]`d because it is a
    /// measurement instrument and not a gate — a fifth wall-clock assertion in
    /// the required suite buys less than it costs in flake.
    ///
    /// **Measured 2026-08-08, `--release`, 2019 Intel i5-8279U, min of three**,
    /// over a 16x range of span counts (32 000 → 512 000 proposals, 656 KB →
    /// 10.5 MB): 0.0537 s → 0.9728 s, i.e. **18.11x for 16x the spans, an
    /// exponent of 1.045**; the four step-to-step exponents were 1.114, 1.061,
    /// 0.967, 1.036. Quadratic would predict 256x. So the class the parked
    /// bound was written against does not survive the byte-mark rewrite, and
    /// the bound was not ported — this is the measurement that decided it, and
    /// it is re-runnable rather than a sentence.
    #[test]
    #[ignore = "measurement instrument, not a gate: see the doc for the recorded run"]
    fn probe_hostile_span_count_growth() {
        use std::time::Instant;
        let unit = "ghp_QWERTYUIOPASDFGHJKLZXCVBNM0123456789 ";
        let mut prev: Option<(usize, f64)> = None;
        for n in [16_000usize, 32_000, 64_000, 128_000, 256_000] {
            let doc = unit.repeat(n);
            let proposed = propose(&doc, &RulePackSet::builtin()).len();
            let mut best = f64::MAX;
            for _ in 0..3 {
                let t = Instant::now();
                let o = redact(&SecretBytes::from(doc.as_str())).unwrap();
                let secs = t.elapsed().as_secs_f64();
                assert!(o.report.total_spans > 0);
                best = best.min(secs);
            }
            let exp = prev.map_or(f64::NAN, |(pn, pt)| {
                (best / pt).log2() / ((n as f64) / (pn as f64)).log2()
            });
            println!(
                "PROBE tokens={n} bytes={} proposed={proposed} min_secs={best:.4} \
                 exponent_vs_prev={exp:.3}",
                doc.len()
            );
            prev = Some((n, best));
        }
    }

    /// Two heuristic spans overlapping each other: the card wins on width and
    /// the email is dropped whole, so before the union `4111 1111 1111 1111@a.io`
    /// masked the email and published fourteen digits of the card on main, and
    /// masked the card and published `@a.io` here. Neither now.
    #[test]
    fn two_overlapping_heuristic_spans_seal_their_union() {
        let m = masked("4111 1111 1111 1111@a.io");
        assert!(!m.contains("4111 1111 1111"), "card published: {m}");
        assert!(!m.contains("@a.io"), "email published: {m}");
    }

    /// The same carve with no PII in sight: a built-in vendor pattern matching
    /// the *middle* of a high-entropy token published both ends — 15 characters
    /// each, under the length floor, so the fixpoint could not recover them
    /// either. Found by the probe written for the pack case, which is why the
    /// rule is keyed on whole-match detectors and not on pack rules.
    #[test]
    fn a_builtin_whole_match_detector_never_carves_a_heuristic_span() {
        let m = masked("Zx9Qw3eR7tY2uI8AKIAIOSFODNN7EXAMPLEoP1aS5dF6gH0j");
        assert!(!m.contains("Zx9Qw3eR7tY2uI8"), "head published: {m}");
        assert!(!m.contains("oP1aS5dF6gH0j"), "tail published: {m}");
    }

    /// A pack rule may only ever seal *more*. Both rows carve a heuristic span
    /// from the inside; the email row is the plain leak (`ample.com` survives,
    /// and nothing re-detects a bare domain), the entropy row the quiet one
    /// (the tail left behind is under the length floor).
    #[test]
    fn a_pack_rule_never_carves_a_heuristic_span() {
        for (rule, input, remainder) in [
            ("alice@ex", "mail alice@example.com end", "ample.com"),
            (
                "Zx9Qw3eR7tY2",
                "tok Zx9Qw3eR7tY2uI8oP1aS5dF6gH0j end",
                "uI8oP1aS5dF6gH0j",
            ),
        ] {
            let packs = RulePackSet::builtin()
                .with_vendor_pattern("acme.rule", rule, "acme@1")
                .unwrap();
            let o = redact_with(&SecretBytes::from(input), &packs).unwrap();
            let m = String::from_utf8(o.redacted.as_slice().to_vec()).unwrap();
            assert!(!m.contains(remainder), "rule {rule} carved {input}: {m}");
        }
    }

    /// A capture-group detector keeps its context — and exactly how far that
    /// goes, because it is the one place [`coalesce`] does not seal the union.
    ///
    /// It reaches a **competing parse**: `pii.email` reads
    /// `Zx9…@db.example.com` out of the URI, loses to the detector that knows
    /// the `@` is an authority delimiter, and goes whole rather than leaving its
    /// remainder — which is what keeps the host, the thing that makes a captured
    /// transcript diagnosable (`uri.host_preserved`).
    ///
    /// It does **not** reach the entropy layer, and that is stated rather than
    /// discovered: when the entropy tokenizer swallows the key name into the
    /// same run as the value (`=` is a token character), the key goes with it.
    /// Over-redaction, in the direction the legacy backend's whole-match connection-string
    /// regions already take, and the price of never publishing a guess's
    /// remainder.
    #[test]
    fn a_group_detector_keeps_its_context_and_only_its_context() {
        let m = masked("postgres://admin:Zx9Qw3eR7tY2uI8oP1aS5dF@db.example.com/app");
        assert!(m.contains("db.example.com"), "host must survive: {m}");
        assert!(!m.contains("Zx9Qw3eR7tY2uI8oP1aS5dF"), "{m}");
        // the key survives when nothing else claims it
        let short = masked("password=hunter2;");
        assert_eq!(short, "password=[REDACTED:jdbc];");
        // and goes when the entropy layer claimed `password=…` as one token
        let d = masked("password=Zx9Qw3eR7tY2uI8oP1aS5dF6gH;");
        assert_eq!(d, "[REDACTED:entropy][REDACTED:jdbc];");
        assert!(!d.contains("Zx9Qw3eR7tY2uI8oP1aS5dF6gH"), "{d}");
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

    /// The module doc says a hostile input cannot wedge the pipeline. That rests
    /// fully on [`MAX_PASSES`], whose failure branch nothing exercised — so
    /// the sentence was a claim no test could falsify.
    ///
    /// A rule matching a *substring* of a placeholder is the wedge: masking
    /// `REDACTED` writes `[REDACTED:vendor]`, which contains `REDACTED`, so the
    /// next pass masks it again and the text grows without bound. The pipeline
    /// stops after eight passes and returns an **error**, never partially
    /// redacted bytes — which is the only fail-safe answer, since a partial
    /// result here is a leak and a loop is a hang.
    #[test]
    fn an_oscillating_rule_is_bounded_and_fails_safe() {
        let packs = RulePackSet::builtin()
            .with_vendor_pattern("osc", "REDACTED", "acme@1")
            .unwrap();
        let Err(err) = redact_with(&SecretBytes::from("start REDACTED end"), &packs) else {
            panic!("an unbounded rewrite must fail rather than loop");
        };
        assert!(err.to_string().contains("did not stabilize"), "{err}");
        // and the ordinary case still converges through the same loop
        let ok = redact_with(&SecretBytes::from("start plain end"), &packs).unwrap();
        assert_eq!(ok.redacted.as_slice(), b"start plain end");
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

    fn workspace_root() -> std::path::PathBuf {
        // crates/rift-shadow-redact → crates → the workspace root
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("workspace root")
            .to_path_buf()
    }

    /// Walk every tracked or unignored text file under `dir`.
    ///
    /// `--others --exclude-standard` keeps a newly-created source file inside
    /// the audit while respecting the repository and per-checkout ignore rules.
    /// That distinction matters for this source-tree probe: ignored local PRDs
    /// and alternate Cargo target directories are not repository content and
    /// can legitimately contain the detector fixtures or numeric build ids the
    /// probe is designed to find.
    fn visit_text_files(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
        const TEXT: [&str; 11] = [
            "rs", "toml", "lock", "md", "json", "snap", "txt", "yml", "yaml", "sh", "sql",
        ];
        let tracked = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ])
            .output();
        if let Ok(output) = tracked {
            if output.status.success() {
                for relative in output.stdout.split(|byte| *byte == 0) {
                    if relative.is_empty() {
                        continue;
                    }
                    let path = dir.join(String::from_utf8_lossy(relative).as_ref());
                    if path
                        .extension()
                        .is_some_and(|e| TEXT.iter().any(|t| e == *t))
                    {
                        if let Ok(contents) = std::fs::read_to_string(&path) {
                            f(&path, &contents);
                        }
                    }
                }
                return;
            }
        }

        // A packaged source tree may not have Git metadata. Preserve the old
        // filesystem walk as a conservative fallback for that environment.
        visit_text_files_unfiltered(dir, f);
    }

    fn visit_text_files_unfiltered(
        dir: &std::path::Path,
        f: &mut impl FnMut(&std::path::Path, &str),
    ) {
        const TEXT: [&str; 11] = [
            "rs", "toml", "lock", "md", "json", "snap", "txt", "yml", "yaml", "sh", "sql",
        ];
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path
                    .file_name()
                    .is_some_and(|n| n == "target" || n == ".git" || n == "node_modules")
                {
                    continue;
                }
                visit_text_files_unfiltered(&path, f);
            } else if path
                .extension()
                .is_some_and(|e| TEXT.iter().any(|t| e == *t))
            {
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    f(&path, &contents);
                }
            }
        }
    }

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

    /// The tracked false-positive baseline, the artifact both fences live in.
    const FP_ARTIFACT: &str = "benchmarks/redaction-fp-baseline.md";

    /// The artifact's text, or a panic naming the path it is missing from.
    fn fp_artifact_text() -> String {
        let path = workspace_root().join(FP_ARTIFACT);
        std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "the recorded false-positive baseline is missing at {}: {e}",
                path.display()
            )
        })
    }

    /// One row of the ` ```fp-classified ` fence: hits that are irreducible,
    /// classified under a closed two-class vocabulary, and therefore
    /// subtracted from the corpus count before it meets its ceiling.
    #[derive(Debug, PartialEq, Eq)]
    struct FpClassified {
        /// The corpus path the row is grouped under, as [`corpus_key`] writes it.
        path: String,
        detector: String,
        /// Hits subtracted from the detector's corpus-wide count.
        classified: u32,
        /// What the whole file measures for that detector, asserted **exactly**.
        file_total: u32,
    }

    /// The two classes, and there are exactly two. Adding a third is an owner
    /// escalation, exactly as raising a ceiling is.
    const FP_CLASSES: [&str; 2] = ["pinned-evidence", "frozen-bytes"];

    /// Read the ` ```fp-classified ` fence.
    ///
    /// Grammar using the earlier grouped-census form — a path is ONE
    /// high-entropy token, and
    /// this fence lives inside the corpus it measures, so stating each path
    /// once is what keeps the table from walking the ceiling upwards:
    ///
    /// - `--- <class>` opens a section; the class must be one of [`FP_CLASSES`];
    /// - a bare line opens a path group;
    /// - `TAB <detector> TAB <classified> TAB <file_total> TAB <class>: <reason>`
    ///   is a row under the open group.
    ///
    /// **A row states its own class and must match its section.** That is not
    /// redundancy: a sibling baseline reader carries a declared, open gap
    /// where a section header injected mid-group silently refiles the rows below
    /// it under the wrong reason. Here a misfiled row disagrees with its section
    /// and is refused, so that class of error dies by construction rather than
    /// by attention.
    ///
    /// **What is refused, and why each one is a hazard rather than a taste:**
    /// an unknown class (the vocabulary is closed); **a repeated
    /// `(path, detector)` ANYWHERE in the fence** (see below — this is the one
    /// that matters); a class section opened twice, or a second group for one
    /// path inside one section (both are a second place for one path to be
    /// listed, and a per-section refusal cannot see across either); a row before
    /// any path line or before any section (its group or its class cannot be
    /// known); a row that classifies nothing, or claims more than the file total
    /// (a subtraction table is not a per-file parking lot, and no row may buy
    /// headroom the file does not have); an entry under
    /// `crates/rift-shadow-redact/**` or any `fuzz` component, which the walk
    /// never reaches, so the row could only ever be an orphan; and **a reason
    /// the entropy layer itself fires on**, so a quoted high-entropy token is
    /// refused rather than billed to the ceiling this fence exists to relieve.
    ///
    /// **The duplicate-key refusal is WHOLE-FENCE, and that is a repair.** It
    /// was scoped to a section, and a verifier planted the hole both ways: the
    /// same path listed under both class sections, and one class section
    /// reopened. Two rows for one `(path, detector)` each satisfy the
    /// `file_total` arm **individually** — each says truthfully what the file
    /// measures — while their `classified` counts are **summed** into the
    /// subtraction. A file carrying four hits therefore passed a ceiling of
    /// **zero**, measured, with no code weakened and no arm disabled. That is
    /// the whitelist the classification rules explicitly refuse to claim, reachable with
    /// two rows of ordinary-looking table. A file whose hits would split across
    /// both classes files the whole count under the one class it can be
    /// defended as, and says so in the reason.
    ///
    /// **What the reason guard does NOT do**, stated because the first version
    /// of this comment claimed it: it does not refuse a spelled-out path.
    /// Measured — ordinary repository paths sit under the entropy layer's
    /// threshold and no detector fires on them, so the basename rule the
    /// artifact states is an ARGUMENT this parser does not enforce. What the
    /// guard catches is a reason carrying a token the pipeline itself masks,
    /// which is the quoted-token half of the discipline.
    ///
    /// **An ABSENT fence is refused; an EMPTY one is not.** `raw − 0` is the
    /// arithmetic that ran before classification existed, so a table with no rows is the
    /// identity and is the correct starting state. A fence deleted by an edit,
    /// though, would take its rows with it and leave a reader that silently
    /// returns nothing — a check that cannot fire.
    fn fp_parse_classified(text: &str) -> Result<Vec<FpClassified>, String> {
        let mut out: Vec<FpClassified> = Vec::new();
        let mut section: Option<&str> = None;
        let mut opened: BTreeMap<String, usize> = BTreeMap::new();
        let mut opened_sections: BTreeMap<&str, usize> = BTreeMap::new();
        // WHOLE-FENCE, never cleared. See the docstring: a key listed twice is
        // the whitelist this arithmetic refuses to be, and clearing this at a
        // section boundary was a real hole a verifier planted both ways.
        let mut listed: BTreeMap<(String, String), usize> = BTreeMap::new();
        let mut group: Option<String> = None;
        let mut inside = false;
        let mut saw_fence = false;
        for (i, raw) in text.lines().enumerate() {
            let at = i + 1;
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if line.starts_with("```") {
                inside = line.trim() == "```fp-classified";
                saw_fence |= inside;
                section = None;
                group = None;
                opened.clear();
                continue;
            }
            if !inside || line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(class) = line.strip_prefix("--- ") {
                let class = class.trim();
                let Some(known) = FP_CLASSES.iter().find(|c| **c == class) else {
                    return Err(format!(
                        "{FP_ARTIFACT}:{at}: `{class}` is not one of the two classes — the \
                         vocabulary is closed, and adding a class is an owner escalation \
                         exactly as raising the ceiling is"
                    ));
                };
                if let Some(first) = opened_sections.get(known) {
                    return Err(format!(
                        "{FP_ARTIFACT}:{at}: the `{class}` section was already opened at line \
                         {first} — a class reopened is a second place for one path to be \
                         listed, and the per-section refusals below cannot see across it"
                    ));
                }
                opened_sections.insert(known, at);
                section = Some(known);
                group = None;
                opened.clear();
                continue;
            }
            let Some(row) = line.strip_prefix('\t') else {
                if let Some(first) = opened.get(line) {
                    return Err(format!(
                        "{FP_ARTIFACT}:{at}: `{line}` already opened a group at line {first} in \
                         this section — merging them would let a duplicated entry pass unseen"
                    ));
                }
                opened.insert(line.to_owned(), at);
                group = Some(line.to_owned());
                continue;
            };
            let Some(class) = section else {
                return Err(format!(
                    "{FP_ARTIFACT}:{at}: an entry row before any `--- <class>` section — the \
                     class it is claimed under cannot be known"
                ));
            };
            let Some(path) = group.clone() else {
                return Err(format!(
                    "{FP_ARTIFACT}:{at}: an entry row before any path line — the group it \
                     belongs to cannot be known"
                ));
            };
            let entry = fp_parse_row(&path, class, row, at)?;
            let key = (path.clone(), entry.detector.clone());
            if let Some(first) = listed.get(&key) {
                return Err(format!(
                    "{FP_ARTIFACT}:{at}: `{}` is already listed for `{path}` at line {first} — \
                     anywhere in this fence, not merely in this section. Two rows for one key \
                     each satisfy the file-total arm on their own while their classified counts \
                     SUM, which subtracts a file twice and is exactly the whitelist this \
                     arithmetic refuses to be. File the whole count under the one class it can \
                     be defended as.",
                    entry.detector
                ));
            }
            listed.insert(key, at);
            out.push(entry);
        }
        if !saw_fence {
            return Err(format!(
                "{FP_ARTIFACT}: no fp-classified fence — an empty table is the identity and is \
                 fine, but a fence that has been deleted takes its rows with it and leaves a \
                 reader that cannot fire"
            ));
        }
        Ok(out)
    }

    /// One row's fields, with every refusal the row itself can carry.
    fn fp_parse_row(
        path: &str,
        section: &str,
        row: &str,
        at: usize,
    ) -> Result<FpClassified, String> {
        let mut fields = row.splitn(4, '\t');
        let (Some(detector), Some(classified), Some(file_total), Some(tail)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(format!(
                "{FP_ARTIFACT}:{at}: expected \
                 `TAB <detector> TAB <classified> TAB <file_total> TAB <class>: <reason>` \
                 under `{path}`"
            ));
        };
        let number = |field: &str, what: &str| -> Result<u32, String> {
            field
                .trim()
                .parse::<u32>()
                .map_err(|_| format!("{FP_ARTIFACT}:{at}: `{field}` is not a {what}"))
        };
        let classified = number(classified, "classified count")?;
        let file_total = number(file_total, "file total")?;
        let Some((class, reason)) = tail.split_once(':') else {
            return Err(format!(
                "{FP_ARTIFACT}:{at}: the last field is `<class>: <reason>`, and this one \
                 carries no class"
            ));
        };
        let (class, reason) = (class.trim(), reason.trim());
        if class != section {
            return Err(format!(
                "{FP_ARTIFACT}:{at}: this row claims `{class}` inside the `{section}` section \
                 — a row filed under the wrong reason is the miss a section header cannot see"
            ));
        }
        if reason.is_empty() {
            return Err(format!(
                "{FP_ARTIFACT}:{at}: a classified entry carries the reason it is irreducible"
            ));
        }
        if !pipeline_hits(reason).is_empty() {
            return Err(format!(
                "{FP_ARTIFACT}:{at}: this reason fires the pipeline itself, so writing it \
                 costs the very ceiling this row exists to relieve — name the file by \
                 basename or by where it sits, and never quote the token"
            ));
        }
        if classified == 0 {
            return Err(format!(
                "{FP_ARTIFACT}:{at}: a row that classifies nothing subtracts nothing — this \
                 is a subtraction table, not a per-file census to park totals in"
            ));
        }
        if classified > file_total {
            return Err(format!(
                "{FP_ARTIFACT}:{at}: {classified} classified of a {file_total} file total — no \
                 row may buy headroom the file does not have"
            ));
        }
        if path.starts_with("crates/rift-shadow-redact/")
            || path.split('/').any(|part| part == "fuzz")
        {
            return Err(format!(
                "{FP_ARTIFACT}:{at}: `{path}` is outside the corpus — the walk excludes the \
                 seeded-secret regions, so this row could only ever be an orphan"
            ));
        }
        Ok(FpClassified {
            path: path.to_owned(),
            detector: detector.trim().to_owned(),
            classified,
            file_total,
        })
    }

    /// The artifact's classified entries, or a panic carrying the refusal.
    fn fp_classified_entries() -> Vec<FpClassified> {
        fp_parse_classified(&fp_artifact_text()).unwrap_or_else(|e| panic!("{e}"))
    }

    /// The tracked false-positive baseline, `benchmarks/redaction-fp-baseline.md`.
    ///
    /// The security bar asks for a false-positive rate measured against this
    /// repository's own text **with a recorded absolute baseline**. The
    /// measurement above is one layer of that (five numeric detectors, asserted
    /// at zero); this is the other, and the difference is where the number
    /// lives. A boolean assertion inside a test is not a baseline anybody can
    /// compare across commits, so the numbers live in a tracked artifact and are
    /// read back **out of it** here: the ceilings below are the file's, not the
    /// test's, and editing one is a reviewable diff in `benchmarks/`.
    ///
    /// Parsing is deliberately dumb — `id = ceiling` inside the file's
    /// ` ```fp-baseline ` fence — so the artifact stays a document a person
    /// reads rather than a serialization format.
    fn fp_baseline_ceilings() -> BTreeMap<String, u32> {
        let text = fp_artifact_text();
        let mut ceilings = BTreeMap::new();
        let mut inside = false;
        for line in text.lines() {
            if line.starts_with("```") {
                inside = line.trim() == "```fp-baseline";
                continue;
            }
            if !inside {
                continue;
            }
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((id, ceiling)) = line.split_once('=') {
                let ceiling = ceiling
                    .trim()
                    .parse::<u32>()
                    .unwrap_or_else(|e| panic!("baseline ceiling for {id} is not a number: {e}"));
                ceilings.insert(id.trim().to_string(), ceiling);
            }
        }
        assert!(
            !ceilings.is_empty(),
            "the baseline artifact carries no fp-baseline fence, so this probe would \
             measure nothing"
        );
        ceilings
    }

    /// What the **whole pipeline** masks in one document, by detector — the
    /// product's own answer (`redact`), not a hand-picked detector subset, so
    /// what this counts is what a developer would actually lose out of a
    /// captured transcript.
    fn pipeline_hits(contents: &str) -> BTreeMap<String, u32> {
        redact(&SecretBytes::from(contents))
            .expect("redaction must not fail on repository text")
            .report
            .by_detector
    }

    /// What the pipeline masks over a corpus, per file and in total.
    ///
    /// **Why this is a value and not a loop inside an assertion.** The arms
    /// that keep the baseline honest — an alarm that
    /// bites, a classified file that grows, an entry whose file has left the
    /// tree — cannot be calibrated while the walk root and the artifact path
    /// are wired into the `#[test]` itself. Measuring is one function, judging
    /// is another, and a synthetic corpus can be handed to both.
    ///
    /// Per-file attribution costs no second walk: [`visit_text_files`] already
    /// hands the callback the path it read.
    #[derive(Debug)]
    struct FpCensus {
        /// Every corpus file the walk actually read, root-relative.
        ///
        /// Kept in full — not just the files that carry hits — because "this
        /// entry names a file that is no longer in the corpus" and "this file
        /// no longer carries the hits the entry records" are different
        /// findings that want different sentences, and a map keyed only by
        /// hits cannot tell them apart.
        files: BTreeSet<String>,
        /// `(file, detector) → hits in that file`.
        per_file: BTreeMap<(String, String), u32>,
        /// `detector → hits over the whole corpus`.
        totals: BTreeMap<String, u32>,
        /// Bytes read, so the failure line can say how much was measured.
        bytes: usize,
    }

    /// A corpus path as the artifact writes it: relative to the walk root,
    /// `/`-separated on every platform, and `None` for anything that is not a
    /// plain descendant of the root.
    fn corpus_key(root: &std::path::Path, path: &std::path::Path) -> Option<String> {
        let relative = path.strip_prefix(root).ok()?;
        let mut parts: Vec<String> = Vec::new();
        for component in relative.components() {
            match component {
                std::path::Component::Normal(part) => {
                    parts.push(part.to_string_lossy().into_owned());
                }
                _ => return None,
            }
        }
        (!parts.is_empty()).then(|| parts.join("/"))
    }

    /// Measure a corpus: every tracked or unignored text file under `root`,
    /// minus the three regions that are not developer content.
    ///
    /// Two of them *are* the seeded-secret corpus — this crate's own sources
    /// and `fuzz/` — and counting them would count the tests' own needles as
    /// repository text. The third is `docs/`, the written guide to this
    /// toolchain: it is prose *about* the pipeline rather than material the
    /// pipeline protects, so its worked examples (identifiers, sample hashes,
    /// example keys) would otherwise be tallied as this repository's secrets
    /// and would spend headroom that belongs to real source files.
    ///
    /// **The exclusion is root-relative, and that is a deliberate correction.**
    /// It used to test the absolute path for a `fuzz` component, so a checkout
    /// living under a directory called `fuzz` would have excluded every file in
    /// the workspace and reported a clean baseline over an empty corpus. The
    /// regions are named the way the artifact names them, relative to the root
    /// being measured; on this repository the two forms select exactly the same
    /// files.
    fn fp_census(root: &std::path::Path) -> FpCensus {
        let mut census = FpCensus {
            files: BTreeSet::new(),
            per_file: BTreeMap::new(),
            totals: BTreeMap::new(),
            bytes: 0,
        };
        visit_text_files(root, &mut |path, contents| {
            let Some(relative) = corpus_key(root, path) else {
                return;
            };
            if relative.starts_with("crates/rift-shadow-redact/")
                || relative.starts_with("docs/")
                || relative.split('/').any(|part| part == "fuzz")
            {
                return;
            }
            census.bytes += contents.len();
            census.files.insert(relative.clone());
            for (id, n) in pipeline_hits(contents) {
                *census.totals.entry(id.clone()).or_insert(0) += n;
                *census.per_file.entry((relative.clone(), id)).or_insert(0) += n;
            }
        });
        census
    }

    /// Judge a measured corpus against the recorded ceilings and the recorded
    /// classified entries.
    ///
    /// Returns the violation lines; empty means the baseline holds. Two arms,
    /// and the classification rules are careful about which sensitivity each carries:
    ///
    /// - **per detector**, `raw − Σ classified <= ceiling`. Hits in files
    ///   nobody has classified consume the shared headroom exactly as they do
    ///   today, so classification is not a whitelist: it moves a *counted* hit
    ///   out of the total, it does not stop counting a file.
    /// - **per classified row**, `per_file[(path, detector)] == file_total`,
    ///   **exactly**. A hit added to a classified file beyond its recorded total
    ///   is a violation at the row, in the commit that adds it, rather than
    ///   headroom spent quietly. The claim "sensitivity is unchanged" is not
    ///   made and must not be made; what is preserved is that no file is ever
    ///   whitelisted.
    ///
    /// A detector the artifact does not list is a violation whatever its count,
    /// because a detector without a recorded false-positive cost is a detector
    /// nobody measured. And the subtraction is **checked**: a table that
    /// subtracts more hits than the corpus holds is a violation rather than a
    /// floor at zero, because flooring is how an over-subtraction stops looking
    /// like one.
    fn fp_check(
        census: &FpCensus,
        ceilings: &BTreeMap<String, u32>,
        classified: &[FpClassified],
    ) -> Vec<String> {
        let mut subtracted: BTreeMap<&str, u32> = BTreeMap::new();
        let mut v: Vec<String> = Vec::new();
        for entry in classified {
            let FpClassified {
                path,
                detector,
                file_total,
                ..
            } = entry;
            *subtracted.entry(detector.as_str()).or_insert(0) += entry.classified;
            let measured = census
                .per_file
                .get(&(path.clone(), detector.clone()))
                .copied();
            match measured {
                Some(n) if n == *file_total => {}
                Some(n) if n > *file_total => v.push(format!(
                    "{FP_ARTIFACT}: `{path}` now carries {n} `{detector}` hit(s) and its entry \
                     records {file_total} — a hit added to a classified file is a violation at \
                     the row, not shared headroom. Re-measure the file and correct the row in \
                     the commit that added the hit, or shed it."
                )),
                Some(n) => v.push(format!(
                    "{FP_ARTIFACT}: `{path}` carries {n} `{detector}` hit(s) and its entry \
                     records {file_total} — shrink the entry in the commit that shed the hits."
                )),
                None if census.files.contains(path) => v.push(format!(
                    "{FP_ARTIFACT}: `{path}` no longer carries any `{detector}` hit and its \
                     entry records {file_total} — shrink the entry in the commit that shed the \
                     hits, which here means deleting it."
                )),
                None => v.push(format!(
                    "{FP_ARTIFACT}: `{path}` has left the corpus and still carries an entry — \
                     delete the entry in the commit that removed the file."
                )),
            }
        }
        for (id, raw) in &census.totals {
            let sub = subtracted.get(id.as_str()).copied().unwrap_or(0);
            // CHECKED, not saturating. A saturating floor turns "the table
            // subtracts more than the corpus carries" — a table that has
            // stopped describing the tree — into a serene zero that passes
            // every ceiling. It is the second half of the whitelist the
            // duplicate-key repair closes at parse, and it is held here too
            // because an orphan row reaches it without any duplicate.
            let Some(net) = raw.checked_sub(sub) else {
                v.push(format!(
                    "{FP_ARTIFACT}: the classified entries subtract {sub} `{id}` hit(s) from a \
                     corpus that carries {raw} — a table that subtracts more hits than the tree \
                     holds has stopped describing it, and must not be allowed to floor to zero."
                ));
                continue;
            };
            let ceiling = ceilings.get(id.as_str());
            if ceiling.is_some_and(|c| net <= *c) {
                continue;
            }
            let recorded =
                ceiling.map_or_else(|| "none — unlisted detector".to_string(), u32::to_string);
            v.push(if sub == 0 {
                format!("{id}: {raw} (recorded ceiling {recorded})")
            } else {
                format!("{id}: {net} = {raw} raw − {sub} classified (recorded ceiling {recorded})")
            });
        }
        v
    }

    /// Three tokens measured to cost exactly **one** `entropy.high` hit each
    /// and to trip no other detector, so a synthetic corpus built from them has
    /// a census known by construction rather than by re-measuring it. The
    /// property is asserted, not assumed — see the calibration below.
    const FP_TOKEN_A: &str = "Zq7mVx2Lp9Rw4Kd8Nb3Ty6Hs";
    const FP_TOKEN_B: &str = "Qw8rTy3uIo5pAs9dFg2hJk";
    const FP_TOKEN_C: &str = "Xn4bVc7zLm1qWe6rTy0u";
    const FP_TOKEN_D: &str = "Bg5nHj8kLo2pQr7sTu1v";

    /// Every planted token, so the calibration below cannot forget one.
    const FP_TOKENS: [&str; 4] = [FP_TOKEN_A, FP_TOKEN_B, FP_TOKEN_C, FP_TOKEN_D];

    /// A synthetic corpus on disk, so the checker can be calibrated against a
    /// corpus whose every hit is known instead of by mutating this repository.
    fn fp_corpus(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp corpus");
        // `git init` so the walk takes its PRIMARY arm rather than the
        // packaged-source fallback: the arm being calibrated should be the arm
        // that runs over the repository. Failure is not fatal — the fallback
        // reads the same files — and every arm asserts the exact file count, so
        // a walk that goes blind reddens instead of reporting an empty corpus
        // as a clean one.
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["init", "-q"])
            .output();
        for (name, body) in files {
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("corpus directory");
            }
            std::fs::write(&path, body).expect("corpus file");
        }
        dir
    }

    /// **A corpus key is a plain descendant of the root, `/`-joined.** Both
    /// halves are load-bearing and neither is reachable from the walk, which is
    /// why they are pinned here rather than left to the census tests: the
    /// separator is what makes an entry in the artifact mean the same file on
    /// every platform, and the refusal is what stops anything that is not a
    /// descendant — a parent escape, a root, a prefix — from being keyed as if
    /// it were one.
    #[test]
    fn a_corpus_key_is_a_plain_descendant_of_the_root_and_is_slash_joined() {
        let root = std::path::Path::new("/repo");
        assert_eq!(
            corpus_key(root, &root.join("benchmarks").join("baseline.md")),
            Some("benchmarks/baseline.md".to_string())
        );
        assert_eq!(
            corpus_key(root, root),
            None,
            "the root is not a corpus file"
        );
        assert_eq!(
            corpus_key(root, std::path::Path::new("/elsewhere/file.md")),
            None,
            "a path outside the root has no key here"
        );
        assert_eq!(
            corpus_key(root, &root.join("..").join("file.md")),
            None,
            "an escape above the root is not a descendant"
        );
    }

    /// **Every hit is attributed to the file that carries it**, and the walk
    /// reads what it claims to read.
    ///
    /// The instrument is calibrated before it is trusted, the same way the two
    /// probes below are: each planted token is asserted to be worth exactly one
    /// hit of exactly one detector first, so a census that disagrees is the
    /// census being wrong and not the corpus being surprising. The file count
    /// is asserted too — a walk that silently stops finding files is the
    /// failure mode a per-file census exists to make visible.
    #[test]
    fn the_false_positive_census_attributes_every_hit_to_the_file_that_carries_it() {
        for token in FP_TOKENS {
            let calibration = pipeline_hits(token);
            assert_eq!(
                calibration.get("entropy.high"),
                Some(&1),
                "planted token is not worth one entropy hit: {calibration:?}"
            );
            assert_eq!(
                calibration.len(),
                1,
                "planted token trips a second detector: {calibration:?}"
            );
        }

        let one = format!("alpha {FP_TOKEN_A} beta\n{FP_TOKEN_B}\n");
        let two = format!("// {FP_TOKEN_C}\n");
        let three = format!("{FP_TOKEN_A}\n");
        let corpus = fp_corpus(&[
            ("corpus_one.md", one.as_str()),
            ("nested/corpus_two.rs", two.as_str()),
            // Outside the eleven text extensions: present on disk, absent from
            // the corpus, so its token must not appear in any count below.
            ("corpus_three.png", three.as_str()),
        ]);
        let census = fp_census(corpus.path());

        assert_eq!(
            census.files,
            ["corpus_one.md", "nested/corpus_two.rs"]
                .into_iter()
                .map(String::from)
                .collect::<BTreeSet<String>>(),
            "the walk read a different set of files than the corpus holds"
        );
        assert_eq!(census.bytes, one.len() + two.len());
        assert_eq!(census.totals.get("entropy.high"), Some(&3));
        assert_eq!(census.totals.len(), 1, "unexpected detector: {census:?}");
        assert_eq!(
            census
                .per_file
                .get(&("corpus_one.md".to_string(), "entropy.high".to_string())),
            Some(&2)
        );
        assert_eq!(
            census.per_file.get(&(
                "nested/corpus_two.rs".to_string(),
                "entropy.high".to_string()
            )),
            Some(&1)
        );
        assert_eq!(census.per_file.len(), 2);
    }

    /// A `fp-classified` fence around `rows`, as the artifact writes one.
    fn fp_fence(rows: &str) -> String {
        format!("prose above\n\n```fp-classified\n{rows}```\n\nprose below\n")
    }

    /// **The artifact's own fence is readable**, and the two classes it may use
    /// are the two the guard knows. Read through the same reader the guard
    /// uses, so a fence that stops parsing reddens here rather than at the far
    /// end of a five-minute corpus walk.
    #[test]
    fn the_classified_fence_in_the_artifact_parses() {
        let entries = fp_classified_entries();
        for entry in &entries {
            assert!(entry.classified >= 1 && entry.classified <= entry.file_total);
            assert!(!entry.path.is_empty() && !entry.detector.is_empty());
        }
        let text = fp_artifact_text();
        assert!(
            text.contains("\n```fp-classified\n"),
            "the artifact carries no classified fence"
        );
        for class in FP_CLASSES {
            assert!(
                text.contains(&format!("\n--- {class}\n")),
                "the artifact's fence does not carry the `{class}` section"
            );
        }
    }

    /// **An empty table is the identity; an absent fence is refused.** The
    /// distinction is the whole reason this reader returns a `Result` instead
    /// of a map: `raw − 0` is the arithmetic that ran before classification existed, so a fence
    /// with no rows is the correct starting state, while a fence deleted by an
    /// edit would leave a reader that silently returns nothing.
    #[test]
    fn an_empty_classified_fence_is_the_identity_and_a_missing_one_is_refused() {
        assert_eq!(fp_parse_classified(&fp_fence("")), Ok(Vec::new()));
        assert_eq!(
            fp_parse_classified(&fp_fence("--- pinned-evidence\n--- frozen-bytes\n")),
            Ok(Vec::new())
        );
        let missing = fp_parse_classified("prose with no fence at all\n");
        assert!(
            missing.is_err_and(|e| e.contains("no fp-classified fence")),
            "a deleted fence must not read as an empty table"
        );
        // A fence of another kind is not this one: the ceiling fence must stay
        // invisible to this reader, the way this one is invisible to it.
        let other = fp_parse_classified("```fp-baseline\nentropy.high = 460\n```\n");
        assert!(other.is_err_and(|e| e.contains("no fp-classified fence")));
    }

    /// **A row is grouped, classed, numbered and reasoned**, and the reader
    /// returns exactly what the row says.
    #[test]
    fn a_classified_row_carries_its_group_class_and_both_numbers() {
        let parsed = fp_parse_classified(&fp_fence(
            "--- pinned-evidence\n\
             benches/pinned/fixtures/pins.md\n\
             \tentropy.high\t4\t6\tpinned-evidence: upstream ids the sweep cites\n\
             --- frozen-bytes\n\
             benches/pinned/fixtures/pins.md\n\
             \tpii.email\t1\t1\tfrozen-bytes: an address inside an import fixture\n",
        ));
        assert_eq!(
            parsed,
            Ok(vec![
                FpClassified {
                    path: "benches/pinned/fixtures/pins.md".to_string(),
                    detector: "entropy.high".to_string(),
                    classified: 4,
                    file_total: 6,
                },
                FpClassified {
                    path: "benches/pinned/fixtures/pins.md".to_string(),
                    detector: "pii.email".to_string(),
                    classified: 1,
                    file_total: 1,
                },
            ])
        );
    }

    /// **Every refusal, planted.** Each row of the table is a shape that must
    /// not reach the arithmetic, with the substring its message has to carry;
    /// the control above is the same grammar without the plant, so none of
    /// these is passing for the wrong reason.
    #[test]
    fn the_classified_fence_refuses_every_shape_that_could_park_debt() {
        const OK_ROW: &str =
            "\tentropy.high\t2\t2\tpinned-evidence: an upstream id the sweep cites\n";
        let planted: [(&str, &str); 11] = [
            (
                "--- invented-class\nsome/file.md\n",
                "not one of the two classes",
            ),
            (
                "--- pinned-evidence\nsome/file.md\n\tentropy.high\t1\t2\tpinned-evidence: one\n\tentropy.high\t1\t2\tpinned-evidence: two\n",
                "already listed",
            ),
            (
                "--- pinned-evidence\nsome/file.md\nsome/file.md\n",
                "already opened a group",
            ),
            (
                "--- pinned-evidence\n\tentropy.high\t1\t1\tpinned-evidence: no group\n",
                "before any path line",
            ),
            (
                "some/file.md\n\tentropy.high\t1\t1\tpinned-evidence: no section\n",
                "before any `--- <class>` section",
            ),
            (
                "--- pinned-evidence\nsome/file.md\n\tentropy.high\t1\n",
                "expected",
            ),
            (
                "--- pinned-evidence\nsome/file.md\n\tentropy.high\tmany\t2\tpinned-evidence: one\n",
                "is not a classified count",
            ),
            (
                "--- pinned-evidence\nsome/file.md\n\tentropy.high\t0\t2\tpinned-evidence: one\n",
                "classifies nothing",
            ),
            (
                "--- pinned-evidence\nsome/file.md\n\tentropy.high\t3\t2\tpinned-evidence: one\n",
                "headroom the file does not have",
            ),
            (
                "--- pinned-evidence\ncrates/rift-shadow-redact/src/lib.rs\n\tentropy.high\t1\t1\tpinned-evidence: one\n",
                "outside the corpus",
            ),
            (
                "--- pinned-evidence\nsome/fuzz/corpus/seed.txt\n\tentropy.high\t1\t1\tpinned-evidence: one\n",
                "outside the corpus",
            ),
        ];
        for (rows, expected) in planted {
            let refusal = fp_parse_classified(&fp_fence(rows));
            assert!(
                refusal.as_ref().is_err_and(|e| e.contains(expected)),
                "expected a refusal carrying {expected:?}, got {refusal:?}"
            );
        }
        // The same grammar with a well-formed row: the refusals above are the
        // plants biting, not the reader refusing everything.
        assert!(fp_parse_classified(&fp_fence(&format!(
            "--- pinned-evidence\nsome/file.md\n{OK_ROW}"
        )))
        .is_ok_and(|e| e.len() == 1));
        // A row filed under the wrong section reads as correct to a reader that
        // trusts its headers — this one does not.
        let misfiled = fp_parse_classified(&fp_fence(
            "--- frozen-bytes\nsome/file.md\n\tentropy.high\t1\t1\tpinned-evidence: one\n",
        ));
        assert!(misfiled.is_err_and(|e| e.contains("filed under the wrong reason")));
    }

    /// **The verifier's whitelist, planted end to end and refused.**
    ///
    /// This is the defect C9 repairs, kept as its own arm because a regression
    /// here is silent: every other arm stays green while the ceiling stops
    /// binding. A file carrying FOUR hits, two rows for the same
    /// `(path, detector)` split across the two class sections, each row telling
    /// the truth about the file on its own — and a ceiling of **zero**. Before
    /// the repair the pair parsed, `classified` summed to 8, and the
    /// saturating floor turned 4 − 8 into a serene 0 that passed. Now the table
    /// never reaches the arithmetic.
    ///
    /// The control is the shape that stays legal and is why the key is
    /// `(path, detector)` and not `path`: one file may be listed under both
    /// classes for DIFFERENT detectors.
    #[test]
    fn one_file_listed_under_both_classes_cannot_be_subtracted_twice() {
        // The file carries two detectors on purpose: the control below needs a
        // second detector to be listed under the second class, and a row may
        // only name hits the file actually has.
        let census = fp_calibration(&[(
            "corpus_one.md",
            &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n{FP_TOKEN_C}\n{FP_TOKEN_D}\ndev@rift.invalid\n"),
        )]);
        assert_eq!(
            census.totals,
            BTreeMap::from([
                ("entropy.high".to_string(), 4u32),
                ("pii.email".to_string(), 1u32),
            ]),
            "the calibration corpus is not what this arm thinks it is"
        );
        let ceilings = |entropy| fp_ceilings(&[("entropy.high", entropy), ("pii.email", 1)]);

        // Route 1: the same key under the two different class sections.
        let split = fp_parse_classified(&fp_fence(
            "--- pinned-evidence\n\
             corpus_one.md\n\
             \tentropy.high\t4\t4\tpinned-evidence: upstream ids the sweep cites\n\
             --- frozen-bytes\n\
             corpus_one.md\n\
             \tentropy.high\t4\t4\tfrozen-bytes: ids inside an import fixture\n",
        ));
        assert!(
            split
                .as_ref()
                .is_err_and(|e| e.contains("is already listed for") && e.contains("SUM")),
            "the same key under two classes must be refused: {split:?}"
        );

        // Route 2: one class section reopened, which used to clear the
        // per-section bookkeeping and hand back the same hole.
        let reopened = fp_parse_classified(&fp_fence(
            "--- pinned-evidence\n\
             corpus_one.md\n\
             \tentropy.high\t4\t4\tpinned-evidence: upstream ids the sweep cites\n\
             --- pinned-evidence\n\
             corpus_one.md\n\
             \tentropy.high\t4\t4\tpinned-evidence: the same ids again\n",
        ));
        assert!(
            reopened.is_err_and(|e| e.contains("section was already opened")),
            "a reopened class section must be refused"
        );

        // The control: both classes, one file, DIFFERENT detectors. Legal, and
        // the arithmetic is sound — 4 − 1 = 3 against a ceiling of 3.
        let legal = fp_classified_from(
            "--- pinned-evidence\n\
             corpus_one.md\n\
             \tentropy.high\t1\t4\tpinned-evidence: one upstream id the sweep cites\n\
             --- frozen-bytes\n\
             corpus_one.md\n\
             \tpii.email\t1\t1\tfrozen-bytes: an address inside an import fixture\n",
        );
        assert_eq!(legal.len(), 2);
        assert_eq!(
            fp_check(&census, &ceilings(3), &legal),
            Vec::<String>::new()
        );

        // And the shape the repair protects: had a duplicate pair reached the
        // arithmetic, the subtraction is CHECKED and reddens rather than
        // flooring to a zero that passes a ceiling of zero. Constructed here
        // directly, because the parser no longer lets a table say it.
        let doubled = vec![
            FpClassified {
                path: "corpus_one.md".to_string(),
                detector: "entropy.high".to_string(),
                classified: 4,
                file_total: 4,
            },
            FpClassified {
                path: "corpus_one.md".to_string(),
                detector: "entropy.high".to_string(),
                classified: 4,
                file_total: 4,
            },
        ];
        let violations = fp_check(&census, &ceilings(0), &doubled);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].contains("subtract 8 `entropy.high` hit(s) from a corpus that carries 4"),
            "{violations:?}"
        );
    }

    /// **An orphan row reaches the over-subtraction without any duplicate**,
    /// which is why the checked arm is held in the checker and not only at
    /// parse: the entry for a file that has left the corpus goes on subtracting
    /// hits nothing measures.
    #[test]
    fn an_orphan_entry_cannot_subtract_the_corpus_below_zero() {
        let census = fp_calibration(&[("corpus_one.md", &format!("{FP_TOKEN_A}\n"))]);
        let table = fp_classified_from(
            "--- pinned-evidence\n\
             corpus_one.md\n\
             \tentropy.high\t1\t1\tpinned-evidence: one upstream id the sweep cites\n\
             --- frozen-bytes\n\
             corpus_gone.md\n\
             \tentropy.high\t3\t3\tfrozen-bytes: ids inside an import fixture\n",
        );
        let violations = fp_check(&census, &fp_ceilings(&[("entropy.high", 0)]), &table);
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert!(
            violations[0].contains("has left the corpus"),
            "{violations:?}"
        );
        assert!(
            violations[1].contains("subtract 4 `entropy.high` hit(s) from a corpus that carries 1"),
            "{violations:?}"
        );
    }

    /// **The reason discipline is enforced by the counter itself.** The rule
    /// forbids quoting the token and forbids naming a file by its full
    /// path, because this fence sits inside the corpus it measures. Prose is
    /// free; a quoted high-entropy token is refused at parse rather than billed
    /// to the ceiling the row exists to relieve.
    #[test]
    fn a_classified_reason_may_not_cost_the_ceiling_it_relieves() {
        let free = fp_parse_classified(&fp_fence(
            "--- pinned-evidence\nsome/file.md\n\tentropy.high\t1\t1\tpinned-evidence: an \
             upstream blob id the sweep cites, named in the pin file beside it\n",
        ));
        assert!(free.is_ok_and(|e| e.len() == 1), "ordinary prose is free");
        let quoted = fp_parse_classified(&fp_fence(&format!(
            "--- pinned-evidence\nsome/file.md\n\tentropy.high\t1\t1\tpinned-evidence: the id \
             {FP_TOKEN_A}\n"
        )));
        assert!(
            quoted.is_err_and(|e| e.contains("fires the pipeline itself")),
            "quoting the token must be refused"
        );
        // **What it does NOT refuse, asserted so the claim cannot drift back.**
        // The parser's own comment once said a spelled-out path was refused
        // here; measured, an ordinary repository path sits under the entropy
        // layer's threshold and no detector fires on it. The basename half of
        // the discipline is an ARGUMENT this guard does not enforce, and the
        // artifact now says so rather than borrowing this guard's authority.
        let spelled_out = fp_parse_classified(&fp_fence(
            "--- pinned-evidence\nsome/file.md\n\tentropy.high\t1\t1\tpinned-evidence: cited by \
             crates/rift-shadow-hooks/tests/adapter_sweep.rs\n",
        ));
        assert!(
            spelled_out.is_ok_and(|e| e.len() == 1),
            "the guard does not refuse a path, and this arm exists to keep that stated"
        );
    }

    // -----------------------------------------------------------------------
    // The calibration, eight arms
    //
    // Each arm is planted AND run without the plant, and each asserts an EXACT
    // violation count, because "it reddened" is satisfied by a checker that
    // reddens at everything. Two arms live above with the reader they
    // calibrate (unknown class, duplicate `(path, detector)`); the six here
    // need a corpus, which the pure checker is what makes possible — the guard
    // used to wire the walk root and the artifact path into the `#[test]`
    // itself, so calibrating it meant mutating this repository.
    //
    // The control arm asserts on the census the helper RETURNS, never on
    // printed output: `assert!` evaluates its message only on failure, so a
    // green run of the real guard prints nothing at all.
    // -----------------------------------------------------------------------

    /// A ceiling table, as the artifact's fence would parse to.
    fn fp_ceilings(rows: &[(&str, u32)]) -> BTreeMap<String, u32> {
        rows.iter().map(|(id, n)| ((*id).to_string(), *n)).collect()
    }

    /// A classified table, read through the real reader so a calibration arm
    /// cannot assert against a shape the artifact could not express.
    fn fp_classified_from(rows: &str) -> Vec<FpClassified> {
        fp_parse_classified(&fp_fence(rows)).expect("the calibration table must parse")
    }

    /// Measure a synthetic corpus, asserting first that the walk read exactly
    /// the files the arm planted: an arm whose corpus is not what it thinks it
    /// is measures nothing, and a walk that goes blind reports zero violations
    /// for the wrong reason.
    fn fp_calibration(files: &[(&str, &str)]) -> FpCensus {
        let corpus = fp_corpus(files);
        let census = fp_census(corpus.path());
        assert_eq!(
            census.files.len(),
            files.len(),
            "the calibration corpus is not the corpus the arm planted: {census:?}"
        );
        census
    }

    /// **Arm 1 — the unclassified alarm bites.** The plant is one hit over the
    /// ceiling with nothing classified at all; the control is the same corpus
    /// under a ceiling that admits it.
    #[test]
    fn an_unclassified_hit_over_the_ceiling_reddens_and_the_control_does_not() {
        let census = fp_calibration(&[
            ("corpus_one.md", &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n")),
            ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
        ]);
        assert_eq!(census.totals.get("entropy.high"), Some(&3));

        let planted = fp_check(&census, &fp_ceilings(&[("entropy.high", 2)]), &[]);
        assert_eq!(planted.len(), 1, "{planted:?}");
        assert_eq!(planted[0], "entropy.high: 3 (recorded ceiling 2)");

        let control = fp_check(&census, &fp_ceilings(&[("entropy.high", 3)]), &[]);
        assert_eq!(control, Vec::<String>::new());
    }

    /// **Arm 2 — classification is NOT a whitelist**, and this is the
    /// load-bearing arm. A classified file's recorded hits leave the total;
    /// nothing else about the corpus is blessed. A novel token in an
    /// *unclassified* file consumes the shared headroom exactly as it did
    /// before classification existed, and reddens on the detector line with the arithmetic
    /// spelled out.
    #[test]
    fn classification_is_not_a_whitelist_a_novel_hit_elsewhere_still_reddens() {
        let table = fp_classified_from(
            "--- pinned-evidence\n\
             corpus_one.md\n\
             \tentropy.high\t2\t2\tpinned-evidence: upstream ids the sweep cites\n",
        );
        let ceilings = fp_ceilings(&[("entropy.high", 1)]);

        let control = fp_calibration(&[
            ("corpus_one.md", &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n")),
            ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
        ]);
        assert_eq!(control.totals.get("entropy.high"), Some(&3));
        assert_eq!(fp_check(&control, &ceilings, &table), Vec::<String>::new());

        let planted = fp_calibration(&[
            ("corpus_one.md", &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n")),
            ("corpus_two.md", &format!("{FP_TOKEN_C}\n{FP_TOKEN_D}\n")),
        ]);
        assert_eq!(planted.totals.get("entropy.high"), Some(&4));
        let violations = fp_check(&planted, &ceilings, &table);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(
            violations[0],
            "entropy.high: 2 = 4 raw − 2 classified (recorded ceiling 1)"
        );
    }

    /// **Arm 2b — growth inside a classified file reddens AT THE ROW**, in the
    /// commit that adds it, and the ceiling is deliberately generous so that
    /// only the row arm can be what fires. Without this arm the extra hit would
    /// merely rejoin the shared headroom and stay silent until the whole corpus
    /// crossed the ceiling — the weaker property the ruling refuses to claim.
    #[test]
    fn growth_inside_a_classified_file_reddens_at_the_row() {
        let table = fp_classified_from(
            "--- pinned-evidence\n\
             corpus_one.md\n\
             \tentropy.high\t2\t2\tpinned-evidence: upstream ids the sweep cites\n",
        );
        let ceilings = fp_ceilings(&[("entropy.high", 10)]);

        let control = fp_calibration(&[
            ("corpus_one.md", &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n")),
            ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
        ]);
        assert_eq!(fp_check(&control, &ceilings, &table), Vec::<String>::new());

        let planted = fp_calibration(&[
            (
                "corpus_one.md",
                &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n{FP_TOKEN_D}\n"),
            ),
            ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
        ]);
        let violations = fp_check(&planted, &ceilings, &table);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].contains("now carries 3 `entropy.high` hit(s) and its entry records 2")
                && violations[0].contains("violation at the row, not shared headroom"),
            "{violations:?}"
        );
        // The shared-headroom arm stayed silent, which is the whole point: 4
        // raw − 2 classified is 2, comfortably under a ceiling of 10.
        assert_eq!(planted.totals.get("entropy.high"), Some(&4));
    }

    /// **Arm 3 — a stale entry reddens.** Two plants, because shedding has two
    /// endings: the file keeps some of its hits, or it keeps none. Both are the
    /// same instruction — shrink the entry in the commit that shed them.
    #[test]
    fn an_entry_whose_file_shed_hits_reddens_as_stale() {
        let table = fp_classified_from(
            "--- pinned-evidence\n\
             corpus_one.md\n\
             \tentropy.high\t2\t2\tpinned-evidence: upstream ids the sweep cites\n",
        );
        let ceilings = fp_ceilings(&[("entropy.high", 10)]);

        let control = fp_calibration(&[
            ("corpus_one.md", &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n")),
            ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
        ]);
        assert_eq!(fp_check(&control, &ceilings, &table), Vec::<String>::new());

        let some_shed = fp_calibration(&[
            ("corpus_one.md", &format!("{FP_TOKEN_A}\n")),
            ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
        ]);
        let violations = fp_check(&some_shed, &ceilings, &table);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].contains("carries 1 `entropy.high` hit(s) and its entry records 2")
                && violations[0].contains("shrink the entry in the commit that shed the hits"),
            "{violations:?}"
        );

        // The unclassified file carries two hits here so that the entry's two
        // classified hits still have a corpus to come out of: this arm is about
        // the stale entry, and the over-subtraction has an arm of its own.
        let all_shed = fp_calibration(&[
            ("corpus_one.md", "ordinary prose, nothing to see\n"),
            ("corpus_two.md", &format!("{FP_TOKEN_C}\n{FP_TOKEN_D}\n")),
        ]);
        let violations = fp_check(&all_shed, &ceilings, &table);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].contains("no longer carries any `entropy.high` hit"),
            "{violations:?}"
        );
    }

    /// **Arm 4 — an orphan entry reddens.** A file that has left the corpus
    /// takes its entry with it; an entry that outlives its file would otherwise
    /// go on subtracting hits nothing measures, which is the shape that turns a
    /// shrink-only table into a parking lot.
    #[test]
    fn an_entry_whose_file_left_the_corpus_reddens_as_an_orphan() {
        let table = fp_classified_from(
            "--- pinned-evidence\n\
             corpus_one.md\n\
             \tentropy.high\t2\t2\tpinned-evidence: upstream ids the sweep cites\n",
        );
        let ceilings = fp_ceilings(&[("entropy.high", 10)]);

        let control = fp_calibration(&[
            ("corpus_one.md", &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n")),
            ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
        ]);
        assert_eq!(fp_check(&control, &ceilings, &table), Vec::<String>::new());

        // Two hits in the surviving file, so the vanished entry's two
        // classified hits still have a corpus to come out of: this arm is about
        // the orphan, and the over-subtraction has an arm of its own.
        let planted =
            fp_calibration(&[("corpus_two.md", &format!("{FP_TOKEN_C}\n{FP_TOKEN_D}\n"))]);
        let violations = fp_check(&planted, &ceilings, &table);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].contains("has left the corpus")
                && violations[0].contains("delete the entry in the commit that removed the file"),
            "{violations:?}"
        );
    }

    /// **Arm 7 — the control, asserted on the value and not on the output.**
    /// A green run of the real guard prints nothing: `assert!` evaluates its
    /// format arguments only on failure. So the arm that shows the instrument
    /// is reading the corpus it claims to read has to hold the census in its
    /// hand — the exact file set, the exact per-file attribution, the exact
    /// totals — and then show the checker silent over it.
    #[test]
    fn the_calibration_control_is_green_on_the_census_the_helper_returns() {
        let table = fp_classified_from(
            "--- pinned-evidence\n\
             corpus_one.md\n\
             \tentropy.high\t1\t2\tpinned-evidence: one upstream id the sweep cites\n",
        );
        let census = fp_calibration(&[
            ("corpus_one.md", &format!("{FP_TOKEN_A}\n{FP_TOKEN_B}\n")),
            ("corpus_two.md", &format!("{FP_TOKEN_C}\n")),
        ]);
        assert_eq!(
            census.files,
            ["corpus_one.md", "corpus_two.md"]
                .into_iter()
                .map(String::from)
                .collect::<BTreeSet<String>>()
        );
        assert_eq!(
            census
                .per_file
                .get(&("corpus_one.md".to_string(), "entropy.high".to_string())),
            Some(&2)
        );
        assert_eq!(
            census
                .per_file
                .get(&("corpus_two.md".to_string(), "entropy.high".to_string())),
            Some(&1)
        );
        assert_eq!(
            census.totals,
            BTreeMap::from([("entropy.high".to_string(), 3u32)])
        );
        // 3 raw − 1 classified = 2, which is the ceiling exactly: the control
        // sits ON the boundary rather than comfortably inside it, so an
        // off-by-one in the comparison shows up here.
        assert_eq!(
            fp_check(&census, &fp_ceilings(&[("entropy.high", 2)]), &table),
            Vec::<String>::new()
        );
    }

    /// **A detector cannot join this crate without a measured false-positive
    /// cost.** The artifact's id set and the built-in census are the same set,
    /// asserted in both directions: a new detector with no ceiling reddens here,
    /// and a ceiling for a detector that no longer exists reddens here too, so
    /// the artifact cannot rot into a list of names that used to mean something.
    #[test]
    fn the_false_positive_baseline_prices_every_builtin_detector() {
        let mut priced: Vec<String> = fp_baseline_ceilings().into_keys().collect();
        priced.sort();
        assert_eq!(
            priced,
            builtin_detector_ids(),
            "benchmarks/redaction-fp-baseline.md and the built-in census disagree: every \
             detector needs a measured false-positive ceiling, and every ceiling needs a \
             detector"
        );
    }

    /// **The recorded baseline still holds**, measured the way it was recorded.
    ///
    /// Ceilings rather than equalities, because the corpus is a live repository
    /// and ordinary growth moves the count; a ceiling with recorded headroom
    /// fails on a *class* of false positive appearing, which is the thing worth
    /// knowing. The numeric PII detectors carry exact zeros: they are the claim,
    /// so they get no headroom.
    ///
    /// The instrument is calibrated before it is trusted, the same way the
    /// numeric probe above is: a control document with one known credential in
    /// it must produce exactly one hit, attributed to the right detector. A
    /// probe that would stay silent on a real hit measures nothing.
    ///
    /// **The quantity compared to the ceiling is `raw − Σ
    /// classified`**, and the artifact's classified fence is the only place
    /// that difference may live. With that fence empty the arithmetic is the
    /// identity and this is the same assertion it has always been.
    #[test]
    fn the_recorded_false_positive_baseline_holds_over_this_repositorys_own_text() {
        let control = pipeline_hits("aws AKIAIOSFODNN7EXAMPLE\nharmless 1.2.3\n");
        assert_eq!(
            control.get("vendor.aws.access_key_id"),
            Some(&1),
            "probe is blind: {control:?}"
        );
        assert_eq!(control.len(), 1, "probe over-reports: {control:?}");

        let census = fp_census(&workspace_root());
        let scanned = census.files.len();
        // The occupancy, stated by the instrument. The artifact asks every
        // contributor to record the current occupancy before writing, and one
        // that has
        // to build a scratch probe to read it will estimate instead — three
        // separate instruments were built for that reason in one session. A
        // green run still says nothing by default (`cargo test … -- --nocapture`
        // is the ask), because `assert!` evaluates its message only on failure.
        println!(
            "fp census: {scanned} files, {} bytes, spans {}, {:?}",
            census.bytes,
            census.totals.values().sum::<u32>(),
            census.totals
        );
        println!(
            "fp census: this artifact contributes {:?}",
            census
                .per_file
                .iter()
                .filter(|((path, _), _)| path == FP_ARTIFACT)
                .map(|((_, id), n)| (id.as_str(), *n))
                .collect::<Vec<_>>()
        );
        assert!(
            scanned >= 400,
            "the probe scanned only {scanned} text files (floor 400) — a walk that stops \
             finding files reports a clean baseline for the wrong reason"
        );
        let over = fp_check(&census, &fp_baseline_ceilings(), &fp_classified_entries());
        assert!(
            over.is_empty(),
            "the redaction false-positive baseline is exceeded over this repository's own \
             text ({scanned} files, {} bytes). Re-measure, classify the new hits by \
             hand, and record both in benchmarks/redaction-fp-baseline.md:\n{}",
            census.bytes,
            over.join("\n")
        );
    }

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
    fn placeholder_ranges(text: &[u8]) -> Vec<(usize, usize)> {
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
    fn contains_run_outside(haystack: &[u8], ranges: &[(usize, usize)], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).enumerate().any(|(i, w)| {
            w == needle && !ranges.iter().any(|(s, e)| i < *e && i + needle.len() > *s)
        })
    }

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
    fn builtin_detector_ids() -> Vec<String> {
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
        let mut report = RedactionReport {
            total_spans: 0,
            by_detector: BTreeMap::new(),
            by_class: BTreeMap::new(),
            bytes_masked: 0,
            allowlisted: 0,
            rule_pack_versions: Vec::new(),
            input_sha256: sha256(b""),
        };
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
    fn masked_dropping_the_loser(raw: &str, packs: &RulePackSet) -> String {
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
    fn winners_dropping_the_loser(mut spans: Vec<Span>) -> Vec<Span> {
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
    fn subtract_by_scanning(
        range: (usize, usize),
        taken: &[(usize, usize)],
    ) -> Vec<(usize, usize)> {
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
    fn coalesce_by_scanning(mut spans: Vec<Span>) -> Vec<Span> {
        let overlaps = |a: &Span, b: &Span| a.start < b.end && b.start < a.end;
        by_precedence(&mut spans);
        let mut chosen: Vec<Span> = Vec::new();
        for s in spans {
            if !chosen.iter().any(|c| overlaps(c, &s)) {
                chosen.push(s);
                continue;
            }
            let displaced_by_a_parse = chosen.iter().any(|c| {
                c.parse.is_some() && c.matched == MatchedAgainst::Input && overlaps(c, &s)
            });
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
    fn shape_of(
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

    const EVERY_CLASS: [DetectorClass; 6] = [
        DetectorClass::PrivateKey,
        DetectorClass::CredentialedUri,
        DetectorClass::JdbcDsn,
        DetectorClass::VendorSecret,
        DetectorClass::Pii,
        DetectorClass::Entropy,
    ];

    /// Span sets over a 240-byte document — four [`MARK_BITS`] words, so a draw
    /// can put a span inside one word, across a boundary, or over a whole word,
    /// which is the arithmetic a packed representation can get wrong and a list
    /// of intervals cannot.
    fn span_set_strategy() -> impl proptest::strategy::Strategy<Value = Vec<Span>> {
        use proptest::strategy::Strategy;
        let one = (
            0usize..200,
            1usize..40,
            0usize..EVERY_CLASS.len(),
            proptest::bool::ANY,
            proptest::bool::ANY,
        )
            .prop_map(|(start, width, class, has_parse, blanked)| Span {
                start,
                end: start + width,
                class: *EVERY_CLASS.get(class).unwrap_or(&DetectorClass::Entropy),
                id: format!("probe.{class}"),
                parse: has_parse.then_some((start, start + width + 3)),
                matched: if blanked {
                    MatchedAgainst::BlankedText
                } else {
                    MatchedAgainst::Input
                },
            });
        proptest::collection::vec(one, 0..24)
    }

    /// Byte offsets an entropy span proposed that the pipeline does not mask —
    /// i.e. bytes published *because* a competing span won an overlap. The
    /// invariant is that this is empty.
    ///
    /// It carries **no exception**, and that is the change: while a loser was
    /// dropped whole, a capture-group detector's kept context could take part of
    /// an entropy token out with it, and the property had to name that hole. A
    /// carved loser has no hole to name — every byte the entropy layer proposes
    /// is in a chosen span or is one.
    fn entropy_bytes_published(text: &str) -> Vec<(usize, String)> {
        let packs = RulePackSet::builtin();
        let chosen = select_spans(text, &packs);
        let mut leaked = Vec::new();
        for e in entropy_spans(text) {
            for b in e.start..e.end {
                if !chosen.iter().any(|c| c.start <= b && b < c.end) {
                    leaked.push((b, text.get(e.start..e.end).unwrap_or_default().to_string()));
                    break;
                }
            }
        }
        leaked
    }

    const TOKEN_ALPHABET: [char; 64] = [
        'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R',
        'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j',
        'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', '0', '1',
        '2', '3', '4', '5', '6', '7', '8', '9', '+', '/',
    ];

    /// Issuer prefix + total length pairs a network actually issues, so the
    /// generator can *synthesize* a PAN rather than replay a literal from the
    /// corpus. A detector that only catches the literals in its own corpus
    /// passes every test written from that corpus.
    const CARD_SHAPES: [(&str, usize); 8] = [
        ("4", 13),
        ("4", 16),
        ("34", 15),
        ("37", 15),
        ("51", 16),
        ("6011", 16),
        ("3528", 16),
        ("36", 14),
    ];

    /// Complete `prefix` + `body` to `len` digits with the Luhn check digit, so
    /// the generated number passes the same gate a real card does.
    fn luhn_complete(prefix: &str, len: usize, body: &[u32]) -> String {
        let mut d: Vec<u32> = prefix.chars().filter_map(|c| c.to_digit(10)).collect();
        let mut i = 0;
        while d.len() + 1 < len {
            d.push(*body.get(i % body.len().max(1)).unwrap_or(&0));
            i += 1;
        }
        let mut sum = 0u32;
        for (k, v) in d.iter().rev().enumerate() {
            sum += if k % 2 == 0 {
                let x = v * 2;
                if x > 9 {
                    x - 9
                } else {
                    x
                }
            } else {
                *v
            };
        }
        d.push((10 - (sum % 10)) % 10);
        d.iter().filter_map(|v| char::from_digit(*v, 10)).collect()
    }

    /// A value one of the numeric PII detectors can claim, synthesized from the
    /// issuing authority's rules rather than sampled from a fixed list.
    fn pii_value_strategy() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::strategy::Strategy;
        proptest::prop_oneof![
            (
                proptest::sample::select(&CARD_SHAPES[..]),
                proptest::collection::vec(0u32..10, 4..19)
            )
                .prop_map(|((p, len), body)| luhn_complete(p, len, &body)),
            (1u32..900, 1u32..100, 1u32..10000)
                .prop_map(|(a, g, s)| format!("{a:03}-{g:02}-{s:04}")),
            (1u32..900, 1u32..100, 1u32..10000).prop_map(|(a, g, s)| format!("{a:03}{g:02}{s:04}")),
            (2u32..10, 0u32..100, 2u32..10, 0u32..100, 0u32..10000)
                .prop_map(|(a, b, c, d, e)| format!("+1-{a}{b:02}-{c}{d:02}-{e:04}")),
            (2u32..10, 0u32..100, 2u32..10, 0u32..100, 0u32..10000)
                .prop_map(|(a, b, c, d, e)| format!("({a}{b:02}) {c}{d:02}-{e:04}")),
            (0u32..256, 0u32..256, 0u32..256, 0u32..256)
                .prop_map(|(a, b, c, d)| format!("{a}.{b}.{c}.{d}")),
            (2u32..99, 10_000_000u64..999_999_999).prop_map(|(c, n)| format!("+{c}{n}")),
        ]
    }

    /// A high-entropy token whose length straddles [`ENTROPY_MIN_LEN`]: below
    /// the floor the fixpoint cannot recover a published remainder, which is
    /// exactly where the reject lived (15-19 characters).
    fn token_strategy() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::strategy::Strategy;
        proptest::collection::vec(proptest::sample::select(&TOKEN_ALPHABET[..]), 8..30)
            .prop_map(|v| v.into_iter().collect())
    }

    /// **Every prefix a detector in this crate reads as evidence.** A generator
    /// that cannot spell a detector's own vocabulary cannot reach that
    /// detector's class, whatever its assertion says — and that is not a
    /// hypothetical: with `before` limited to `["", "key: ", "line1\n", " ",
    /// "password="]`, `every_byte_the_entropy_layer_proposes_is_masked` was
    /// **green on a tree that violated it**, because no draw could spell a
    /// taxpayer-id label. Adding one literal (`"ssn="`) turned it red on that
    /// same tree. So this list is the labels
    /// `pii.us_taxpayer_id_labelled` actually matches, in the punctuations the
    /// capture document actually carries, and every future labelled detector
    /// adds its spelling here.
    const LABELS: [&str; 16] = [
        "",
        "key: ",
        "line1\n",
        " ",
        "password=",
        "ssn=",
        "SSN: ",
        "ssn = ",
        "ssn=\n",
        "USER_SSN=",
        "tax_id ",
        "tax id: ",
        "taxpayer_id=",
        "national_id ",
        "itin=",
        "{\"ssn\":\"",
    ];

    const TAILS: [&str; 7] = ["", " end", "\nline3", ";", "\n", "\"}", ","];

    /// The generator's **shape**, not just its vocabulary: a token and a value
    /// drawn independently would essentially never land adjacent, so the draw
    /// assembles the pair — `(token, joiner, value, order)` — and the joiners
    /// are exactly the characters the entropy tokenizer keeps inside one run
    /// (`-`, `+`, `=`, `/`, `_`) plus the ones that break it.
    fn adjacent_pair_strategy() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::strategy::Strategy;
        const JOINERS: [&str; 12] = ["-", "+", "=", "/", "_", "", ".", " ", ":", "@", "\n", "!"];
        (
            token_strategy(),
            proptest::sample::select(&JOINERS[..]),
            pii_value_strategy(),
            proptest::bool::ANY,
            proptest::sample::select(&LABELS[..]),
            proptest::sample::select(&TAILS[..]),
        )
            .prop_map(|(tok, j, val, token_first, before, after)| {
                let core = if token_first {
                    format!("{tok}{j}{val}")
                } else {
                    format!("{val}{j}{tok}")
                };
                format!("{before}{core}{after}")
            })
    }

    /// A vendor token in one of the published shapes, so the draw can put a real
    /// credential next to a structure that parses.
    fn vendor_token_strategy() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::strategy::Strategy;
        let body = |n: usize| {
            proptest::collection::vec(proptest::sample::select(&TOKEN_ALPHABET[..52]), n)
                .prop_map(|v| v.into_iter().collect::<String>())
        };
        proptest::prop_oneof![
            body(16).prop_map(|b| format!("AKIA{}", b.to_uppercase())),
            body(36).prop_map(|b| format!("ghp_{b}")),
            body(35).prop_map(|b| format!("AIza{b}")),
            body(24).prop_map(|b| format!("sk_live_{b}")),
        ]
    }

    /// **A password a `[^/\s@]+` cannot cross in the input.** `uri.credentials`
    /// stops at whitespace, so a URI whose password region carries a space does
    /// not parse as a URI *until* something blanks that space — which is exactly
    /// what settling does when a numeric-PII span claims the digits around it.
    /// The published-remainder class is this one draw, and the corpus could
    /// not spell it:
    /// every password [`credentialed_uri_strategy`] drew came from
    /// [`token_strategy`], whose alphabet has no separator in it.
    fn spaced_pii_password_strategy() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::strategy::Strategy;
        proptest::prop_oneof![
            (1u32..900, 1u32..100, 1u32..10000)
                .prop_map(|(a, g, s)| format!("{a:03} {g:02} {s:04}")),
            (2u32..10, 0u32..100, 2u32..10, 0u32..100, 0u32..10000)
                .prop_map(|(a, b, c, d, e)| format!("{a}{b:02} {c}{d:02} {e:04}")),
            (1u32..900, 1u32..100, 1u32..10000)
                .prop_map(|(a, g, s)| format!("{a:03}\t{g:02} {s:04}")),
        ]
    }

    /// **The grown-span shape.** A credentialed URI whose path carries a
    /// vendor token: the
    /// class where one span reaching across a competitor published a username
    /// and the head of a password. It needs a *parse* (`://user:pass@`), a
    /// *competitor* (the host, which `pii.email` reads as its domain) and a
    /// *third span* beyond them, and none of the three arises from an adjacent
    /// token/value pair — so the draw assembles it.
    ///
    /// **And the published-remainder shape**, through
    /// [`spaced_pii_password_strategy`]: the same
    /// assembly with a password the URI regex cannot cross until a settling
    /// round blanks it.
    fn credentialed_uri_strategy() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::strategy::Strategy;
        const SCHEMES: [&str; 5] = ["https", "http", "postgres", "redis", "mongodb+srv"];
        const HOSTS: [&str; 5] = [
            "h.com",
            "db.example.com",
            "localhost",
            "cache.internal",
            "a.io",
        ];
        const USERS: [&str; 4] = ["alice", "admin", "svc_bot", ""];
        (
            proptest::sample::select(&SCHEMES[..]),
            proptest::sample::select(&USERS[..]),
            proptest::prop_oneof![token_strategy(), spaced_pii_password_strategy()],
            proptest::sample::select(&["", "se!", "hunter2", "%"][..]),
            proptest::sample::select(&HOSTS[..]),
            proptest::option::of(vendor_token_strategy()),
            proptest::sample::select(&LABELS[..]),
        )
            .prop_map(|(scheme, user, pass, salt, host, vendor, before)| {
                let tail = vendor.map_or_else(String::new, |v| format!("/x{v}"));
                format!("{before}{scheme}://{user}:{salt}{pass}@{host}{tail}")
            })
    }

    /// A connection string, the other place a capture group keeps context.
    fn dsn_strategy() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::strategy::Strategy;
        (
            proptest::sample::select(&["password", "Password", "PWD", "pwd"][..]),
            token_strategy(),
            proptest::sample::select(&["", ";Server=db.example.com", " ", "&x=1"][..]),
            proptest::option::of(vendor_token_strategy()),
        )
            .prop_map(|(key, value, extra, vendor)| {
                let v = vendor.map_or_else(String::new, |v| format!(" {v}"));
                format!("{key}={value}{extra}{v}")
            })
    }

    /// A vendor token with a PII value welded straight onto it, no separator.
    /// This is the shape where a `\b`-anchored detector cannot see its own
    /// match — `ghp_…h5wg161.214.203.186` hides the IPv4's leading boundary
    /// behind the token — and it is drawn deliberately because no independent
    /// choice of a token and a value produces it.
    fn abutting_shape_strategy() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::strategy::Strategy;
        (
            proptest::sample::select(&LABELS[..]),
            vendor_token_strategy(),
            pii_value_strategy(),
            proptest::sample::select(&TAILS[..]),
        )
            .prop_map(|(before, vendor, value, after)| format!("{before}{vendor}{value}{after}"))
    }

    fn document_strategy() -> impl proptest::strategy::Strategy<Value = String> {
        proptest::prop_oneof![
            8 => adjacent_pair_strategy(),
            4 => credentialed_uri_strategy(),
            3 => abutting_shape_strategy(),
            2 => dsn_strategy(),
            1 => proptest::string::string_regex("(?s).{0,160}").expect("valid generator regex"),
        ]
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(768))]

        /// **The invariant the pipeline actually needs**, and it now holds with
        /// no exception: for arbitrary text, every byte the entropy layer
        /// proposes is masked in the final output.
        ///
        /// Nothing stated this before, which is why a *detector* could take it
        /// away: every detector here is strengthening-only, and the pipeline
        /// still published 15-19 characters of a high-entropy token when a
        /// numeric-PII span won an overlap, because coalescing dropped the loser
        /// whole and a remainder under [`ENTROPY_MIN_LEN`]/[`ENTROPY_MIN_BITS`]
        /// is one the detect→mask fixpoint cannot recover. [`coalesce`] carving
        /// the loser is what makes it true; this is what makes it *checked*.
        ///
        /// It is only as good as what [`document_strategy`] can spell — see
        /// [`LABELS`], which exists because this property was green on a tree
        /// that violated it.
        #[test]
        fn every_byte_the_entropy_layer_proposes_is_masked(doc in document_strategy()) {
            let leaked = entropy_bytes_published(&doc);
            proptest::prop_assert!(
                leaked.is_empty(),
                "entropy layer proposed bytes the pipeline published: {leaked:?} in {doc:?}"
            );
        }

        /// **"No loser's remainder is ever published", for every layer** — the
        /// general form of the property above. Any span any detector proposed is
        /// masked to the last byte.
        ///
        /// The single exception is named, not assumed: a *shape* span overlapping
        /// a winning capture-group span is a competing parse of bytes that parse
        /// already explained (`pii.email` reading `pass@host` out of a URI), and
        /// it goes whole so the host survives. The entropy layer is never
        /// excused, which is why the property above exists beside this one.
        ///
        /// **The exemption is the pipeline's, not a wider one.** It used to be
        /// granted to every chosen parse, including one a settling round read
        /// off blanked text — so the property excused precisely the case that
        /// was unmasking bytes, and stayed green through it. It now tracks
        /// [`coalesce`]: only a parse of the input's own bytes excuses anything.
        #[test]
        fn no_losing_span_publishes_its_remainder(doc in document_strategy()) {
            let packs = RulePackSet::builtin();
            let chosen = select_spans(&doc, &packs);
            let parses: Vec<(usize, usize)> = chosen
                .iter()
                .filter(|c| c.parse.is_some() && c.matched == MatchedAgainst::Input)
                .map(|c| (c.start, c.end))
                .collect();
            for p in propose(&doc, &packs) {
                if p.class != DetectorClass::Entropy
                    && parses.iter().any(|&(s, e)| p.start < e && s < p.end)
                {
                    continue;
                }
                for b in p.start..p.end {
                    proptest::prop_assert!(
                        chosen.iter().any(|c| c.start <= b && b < c.end),
                        "{} proposed byte {b} and the pipeline published it, in {doc:?}",
                        p.id
                    );
                }
            }
        }

        /// **The relaxation rule itself, in-tree and permanent** — the check this
        /// cycle was rejected twice for running only out of tree.
        ///
        /// Redact the same document twice: once with the pipeline as it ships,
        /// and once with [`masked_dropping_the_loser`], the algorithm this change
        /// replaced, running the detector set `main` runs. No six-byte run of the
        /// input may survive into the shipped output that the old one masked. A
        /// six-byte window is short enough to catch a token's tail and long
        /// enough not to fire on the placeholder alphabet.
        ///
        /// This is the only check here that spans the whole detect→mask fixpoint,
        /// and it is the one that catches a class neither span-level property
        /// can: sealing a loser's remainder in pass 1 can eat the byte a
        /// `\b`-anchored detector needed in pass 2, which is what
        /// [`select_spans`]'s settling exists to prevent.
        ///
        /// **Its blind spots, stated rather than left to be found** — an open
        /// list, and not the crate's only one. The census keeps a second such list
        /// on `the_census_matches_what_propose_emits_on_the_probe`, over what can
        /// stay out of the *id census* rather than over what can get past this
        /// differential; entry 5 below is where the two meet, and a reader who
        /// lands here should not have to find that one by accident.
        ///
        /// 1. It cannot see a change that turns the pipeline back *into* its own
        ///    reference: restore the drop-the-loser rule and both sides move
        ///    together, so this property stays green while the two above go red
        ///    on 34 % of draws. The converse also holds — disable settling and
        ///    this one fires on 1.2 % of draws while both of those stay green.
        ///    Measured over 4 096 draws each; the numbers are hit rates per
        ///    document, not searches to first failure. Neither property is
        ///    redundant and neither is sufficient.
        /// 2. It saw **nothing at all** about the five detectors this branch adds
        ///    until the reference stopped calling [`propose`] whole, because a
        ///    reference carrying the new detectors reaches the same settled
        ///    parse the subject does. That is the published-remainder mechanism
        ///    and the drifted-reference finding
        ///    together, and it is why the reference is now a frozen id list.
        /// 3. What is left: the reference has its own winner selection and its
        ///    own loop, and `mask` is pinned by value — so what still moves both
        ///    sides is the *detection* (`propose` and the two span functions,
        ///    pinned by pattern/group/gate against `main`'s frozen table) and
        ///    `by_precedence`'s tie-break beyond `precedence` itself, which is
        ///    pinned only by review of the diff.
        ///    `the_reference_pipeline_is_still_the_one_main_runs` holds the rest
        ///    — the rest **as far as its `find` reaches**, which is the first
        ///    detector answering to each declared id and no later one, the bound
        ///    [`MAIN_SHAPE_DETECTORS`]' own doc already states about itself.
        ///    Entry 5 is what that bound costs.
        /// 4. A relaxation only shows up here if [`document_strategy`] can spell
        ///    the input — the published-remainder class needed
        ///    [`spaced_pii_password_strategy`] before this property could ever
        ///    have failed on it.
        /// 5. **A detector whose id one of the frozen lists already declares**
        ///    moves both sides together again — item 2's mechanism reached
        ///    through the id instead of through the table. [`main_proposes`] is
        ///    handed an id and the pack set and nothing else, so it passes the
        ///    collision on the id alone and the reference adopts a detector
        ///    `main` does not have; both sides then mask the same bytes and the
        ///    relaxation cancels here exactly as it did before the reference
        ///    became a frozen id list. Planted three times, and this property
        ///    stayed **green** every time: rows 5 and 6 of the [`builtin_tables`]
        ///    plant table, where both sides mask `x h.com y` that `main` masks
        ///    nothing in, and row 8 — a second `pii.ipv4` after the genuine one —
        ///    where both sides mask `x h2.com y` to the same
        ///    `x [REDACTED:pii] y` and **nothing in the package is red at all**
        ///    (61 lib passed / 0 failed, 5 parity / 0).
        ///
        /// The list is open. It says what has been demonstrated to get past this
        /// property, not that nothing else does.
        #[test]
        fn nothing_survives_here_that_dropping_the_loser_masked(doc in document_strategy()) {
            let packs = RulePackSet::builtin();
            let shipped = redact_with(&SecretBytes::from(doc.as_str()), &packs).unwrap();
            // Both sides are read with the pipeline's own placeholders excluded
            // from the search, because a window found only inside one is not a
            // surviving document byte — see [`placeholder_ranges`] for the
            // collision this is not hypothetical about.
            let shipped = shipped.redacted.as_slice();
            let shipped_ph = placeholder_ranges(shipped);
            let dropped = masked_dropping_the_loser(&doc, &packs);
            let dropped = dropped.as_bytes();
            let dropped_ph = placeholder_ranges(dropped);
            for w in doc.as_bytes().windows(6) {
                proptest::prop_assert!(
                    !contains_run_outside(shipped, &shipped_ph, w)
                        || contains_run_outside(dropped, &dropped_ph, w),
                    "{:?} survives here and did not before, in {doc:?}",
                    String::from_utf8_lossy(w)
                );
            }
        }

        /// **Settling is additive, in the direction that matters.** A round
        /// proposes against text it has changed, so what it finds is not a
        /// reading of the input; the guarantee is that adding those proposals
        /// never *removes* a byte the single-pass span set already masked.
        ///
        /// It did remove them: a settled `uri.credentials` parse displaced the
        /// `pii.email` span it overlapped and the host shipped (743 records in
        /// 30 000). Marking the origin ([`MatchedAgainst`]) is what makes this
        /// true; deleting that mark reddens this property, and nothing else here
        /// states it — `nothing_survives_here_that_dropping_the_loser_masked`
        /// compares against a *different* algorithm, not against this one with
        /// its settling removed.
        #[test]
        fn settling_never_takes_a_masked_byte_away(doc in document_strategy()) {
            let packs = RulePackSet::builtin();
            let settled = select_spans(&doc, &packs);
            for s in coalesce(propose(&doc, &packs)) {
                for b in s.start..s.end {
                    proptest::prop_assert!(
                        settled.iter().any(|c| c.start <= b && b < c.end),
                        "one pass masked byte {b} ({}) and settling unmasked it, in {doc:?}",
                        s.id
                    );
                }
            }
        }

        /// **The rewrite's own differential, in-tree and permanent.** The marks
        /// must choose exactly the spans the scan chose — same set, same order,
        /// same attribution — over span sets drawn to hit the arithmetic a
        /// packed representation can get wrong: spans inside one word, across a
        /// boundary, nested, adjacent, identical, and covering a whole word.
        ///
        /// Out of tree the same statement was measured against `main`'s binary
        /// on a shaped corpus; that run is gone the moment it finishes, and this
        /// is what remains.
        #[test]
        fn the_marks_choose_the_winners_the_scan_chose(spans in span_set_strategy()) {
            let by_marks = winners(spans.clone());
            let by_scan = winners_dropping_the_loser(spans);
            proptest::prop_assert_eq!(shape_of(&by_marks), shape_of(&by_scan));
        }

        /// The same statement for [`coalesce`], which is the harder half: it
        /// carries the exception, so a divergence here is a byte masked on one
        /// side and published on the other. Collapsing `parsed` into `taken` —
        /// the trap a single coverage mark walks straight into — is caught by
        /// this property, because the marks would then drop a loser that the
        /// scan carves.
        #[test]
        fn the_marks_coalesce_the_way_the_scan_did(spans in span_set_strategy()) {
            let by_marks = coalesce(spans.clone());
            let by_scan = coalesce_by_scanning(spans);
            proptest::prop_assert_eq!(shape_of(&by_marks), shape_of(&by_scan));
        }

        /// The same two differentials on the span sets the pipeline actually
        /// builds — real proposals over a real document, including the ones a
        /// settling round reads off blanked text, which is where `matched` and
        /// `parse` take the combinations that decide the exception.
        #[test]
        fn the_marks_agree_with_the_scan_on_real_proposals(doc in document_strategy()) {
            let packs = RulePackSet::builtin();
            let mut spans = propose(&doc, &packs);
            for _ in 0..MAX_SETTLE {
                let hidden = winners(spans.clone());
                if hidden.is_empty() {
                    break;
                }
                let mut found = propose(&blank(&doc, &hidden), &packs);
                for f in &mut found {
                    f.matched = MatchedAgainst::BlankedText;
                }
                found.retain(|f| {
                    !spans
                        .iter()
                        .any(|s| s.start == f.start && s.end == f.end && s.id == f.id)
                });
                if found.is_empty() {
                    break;
                }
                proptest::prop_assert_eq!(
                    shape_of(&winners(spans.clone())),
                    shape_of(&winners_dropping_the_loser(spans.clone()))
                );
                spans.extend(found);
            }
            proptest::prop_assert_eq!(
                shape_of(&coalesce(spans.clone())),
                shape_of(&coalesce_by_scanning(spans))
            );
        }

        /// **[`ByteMarks`]'s precondition, asserted rather than assumed.** A
        /// bitmap cannot express a zero-width interval, so both callers skip
        /// one; that is only sound because no layer proposes one. Checked at
        /// every stage, because a producer is the only thing that could start.
        #[test]
        fn no_layer_ever_proposes_an_empty_span(doc in document_strategy()) {
            let packs = RulePackSet::builtin();
            for stage in [propose(&doc, &packs), select_spans(&doc, &packs)] {
                for s in stage {
                    proptest::prop_assert!(s.start < s.end, "{} proposed {s:?} in {doc:?}", s.id);
                }
            }
        }

        /// The same invariant read off the **output** rather than the span set:
        /// when the entropy layer claims a token, that token must not appear in
        /// the redacted bytes. This is the reject verbatim
        /// (`Zx9Qw3eR7tY2uI8oPa-378282246310005`), found by search over
        /// synthesized values rather than by replaying the literal.
        #[test]
        fn a_claimed_token_never_survives_beside_a_numeric_identifier(
            tok in token_strategy(),
            joiner in proptest::sample::select(&["-", "+", "=", "/", "_", "."][..]),
            val in pii_value_strategy(),
        ) {
            let doc = format!("{tok}{joiner}{val}");
            let claimed = entropy_spans(&doc)
                .iter()
                .any(|e| e.start == 0 && e.end >= tok.len());
            if claimed {
                let m = String::from_utf8_lossy(
                    redact(&SecretBytes::from(doc.as_str())).unwrap().redacted.as_slice(),
                ).into_owned();
                proptest::prop_assert!(!m.contains(tok.as_str()), "{doc:?} -> {m:?}");
            }
        }
    }

    proptest::proptest! {
        /// Determinism: same input + pack ⇒ byte-identical output + report.
        ///
        /// `(?s)` is load-bearing: without it the generator's `.` never yields a
        /// newline, and a single-line-only determinism property says nothing
        /// about the capture document, which is line-oriented end to end.
        #[test]
        fn redaction_is_deterministic(s in "(?s).{0,200}") {
            let a = redact(&SecretBytes::from(s.as_str())).unwrap();
            let b = redact(&SecretBytes::from(s.as_str())).unwrap();
            proptest::prop_assert_eq!(a.redacted.as_slice(), b.redacted.as_slice());
            proptest::prop_assert_eq!(a.report.total_spans, b.report.total_spans);
        }

        /// **`redact(redact(x)) == redact(x)`, byte for byte, and the second
        /// pass counts nothing.**
        ///
        /// The detect→mask loop already runs to a fixpoint *inside* one call;
        /// this is the statement about the boundary — feeding a sealed artifact
        /// back through the pipeline changes neither its bytes nor its report.
        /// It has to land with per-detector PII labels because they are what
        /// could take
        /// it away: with eleven labels in one family, a second PII detector
        /// meeting a first one's placeholder would rewrite the label on every
        /// pass and never settle, and a `total_spans` that keeps climbing on
        /// re-redaction is a report that counts the pipeline's own output as
        /// leaks. The count is asserted as **zero on the second pass**, not
        /// merely equal: equality would also hold if both passes re-counted the
        /// same placeholders.
        #[test]
        fn redaction_is_idempotent_on_its_own_output(s in "(?s).{0,200}") {
            let once = redact(&SecretBytes::from(s.as_str())).unwrap();
            let twice = redact(&SecretBytes::new(once.redacted.as_slice().to_vec())).unwrap();
            proptest::prop_assert_eq!(
                twice.redacted.as_slice(),
                once.redacted.as_slice(),
                "re-redacting a sealed artifact moved its bytes"
            );
            proptest::prop_assert_eq!(
                twice.report.total_spans, 0,
                "the second pass counted {} spans over text it had already sealed",
                twice.report.total_spans
            );
            proptest::prop_assert!(
                twice.report.by_detector.is_empty() && twice.report.by_class.is_empty(),
                "the second pass attributed {:?}",
                twice.report.by_detector
            );
        }

        /// A numeric PII detector cannot be disarmed by what surrounds it.
        /// `stands_alone` reads the characters on either side of a match, which
        /// makes every neighbouring byte an input an attacker writes -- so the
        /// property that matters is that arbitrary multi-line noise on both
        /// sides never lets the identifier through.
        #[test]
        fn a_surround_cannot_disarm_a_numeric_pii_detector(
            before in "(?s).{0,80}", after in "(?s).{0,80}"
        ) {
            let doc = format!("{before}\nSSN=123-45-6789 PHONE=+1-415-555-0199\n{after}");
            let out = redact(&SecretBytes::from(doc.as_str())).unwrap();
            let m = String::from_utf8_lossy(out.redacted.as_slice()).into_owned();
            proptest::prop_assert!(!m.contains("123-45-6789"), "{}", m);
            proptest::prop_assert!(!m.contains("+1-415-555-0199"), "{}", m);
        }

        /// Never panics on arbitrary bytes (fuzz-style invariant).
        #[test]
        fn never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..256)) {
            let _ = redact(&SecretBytes::new(bytes));
        }
    }
}

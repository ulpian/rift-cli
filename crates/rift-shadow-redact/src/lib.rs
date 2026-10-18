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
//! pipeline. Failing closed is per line: the line is sealed flat, or replaced
//! by a placeholder record, and the rest of the document is sealed as usual.
//! The bound is the load-bearing half and it is
//! exercised by `an_oscillating_rule_is_bounded_and_seals_a_placeholder`; the regex half
//! is enforced by the compiler, since a backreference pattern never builds.
//!
//! The pipeline is one module per concern: `span` is what a detector proposes,
//! `shapes` the pass-1 and PII gates, `entropy` the guess of last resort,
//! `marks` the overlap arithmetic and the mask, `packs` the loaded rule packs.
//! This root keeps the classes, the placeholders, the report and the seal.
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
mod entropy;
mod fallback;
mod jsonl;
mod marks;
mod packs;
mod shapes;
mod span;
#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use rift_core::{RedactedBytes, RedactionProvenance, Result, RiftError, SecretBytes, Sha256Id};
use sha2::{Digest, Sha256};

use crate::builtin_tables::builtin_shape_tables;
use crate::marks::{mask, select_spans};
use crate::span::Span;

pub use crate::fallback::{count_fallback_placeholders, is_fallback_placeholder};
pub use crate::jsonl::cut_is_safe;
pub use crate::packs::{PackRule, PackSample, PackSpec, PackWarning};
pub(crate) use crate::shapes::{
    is_card_pan, is_e164_phone, is_exit_code_phone, is_labelled_taxpayer_id, is_nanp_phone,
    is_street_address, is_trunked_national_phone, is_us_taxpayer_id, unquoted_value,
};
pub(crate) use crate::span::{ShapeDetector, SpanGate};

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
pub(crate) const PLACEHOLDER_OPEN: &str = "[REDACTED:";
pub(crate) const PLACEHOLDER_CLOSE: &str = "]";

/// A set of redaction rule packs (built-in + additive org packs) plus an
/// allowlist. Packs can only ever *broaden* coverage; the allowlist can only ever
/// *subtract* a redaction (keep a known-safe value) — neither can weaken the gate
/// in a way that leaks a secret (fail-safe by construction).
pub struct RulePackSet {
    pub(crate) extra: Vec<ShapeDetector>,
    pub(crate) versions: Vec<String>,
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

    fn version_set(&self) -> Vec<String> {
        self.versions.clone()
    }
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
    /// Lines the per-line fallback sealed because a detect→mask fixpoint did
    /// not converge within its pass bound: sealed as flat text, or replaced by
    /// a placeholder record. Zero on the normal path, and then not serialized,
    /// so every report written before the fallback reads the same.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub redaction_flat_fallback: u32,
    /// The subset of [`Self::redaction_flat_fallback`] replaced to close a
    /// failed decoded record or preserve detector context, using the record
    /// [`is_fallback_placeholder`] recognises.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub redaction_fallback_placeholders: u32,
}

/// `serde`'s skip predicate for a counter that is absent at zero.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// The result of redaction: the sealed bytes and the counts-only report.
pub struct RedactionOutput {
    /// Bytes that provably passed redaction.
    pub redacted: RedactedBytes,
    /// What was masked (counts only).
    pub report: RedactionReport,
}

/// Upper bound on detect→mask passes before declaring non-convergence. Each
/// pass substitutes fixed placeholders only, so real inputs stabilize within a
/// pass or two; the cap is a fail-safe against adversarial oscillation.
pub(crate) const MAX_PASSES: usize = 8;

/// Redact with the built-in pack.
///
/// # Errors
/// [`RiftError`] only on internal failure (regexes are pre-validated).
pub fn redact(raw: &SecretBytes) -> Result<RedactionOutput> {
    redact_with(raw, &RulePackSet::builtin())
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
/// (`an_oscillating_rule_is_bounded_and_seals_a_placeholder`).
///
/// **Non-convergence closes the affected record and context.** Failed parsed
/// values become fixed records in their original physical line slots. Failed
/// flat runs retain the protection selected in full context on every bounded
/// pass before independently safe sub-runs seal. Probes do not affect counts.
/// Every re-seal still runs all detectors; arbitrary vendor rules can transform
/// a fallback record, so fallback output need not be byte-idempotent.
///
/// # Errors
/// None for any input: every outcome of the fixpoint is a sealed document.
/// The `Result` is the gate's stated contract with its callers, kept so a
/// future failure mode has a channel that is not a panic.
pub fn redact_with(raw: &SecretBytes, packs: &RulePackSet) -> Result<RedactionOutput> {
    redact_with_at(raw, packs, 0)
}

/// Redact one **segment** of a line-oriented document: `raw` is the document's
/// lines from 0-based index `first_line` on, each ending in its `\n`.
///
/// The pipeline is [`redact_with`]'s, with one difference: a placeholder record
/// carries its line's index in the whole document rather than in the segment,
/// so segments cut where [`cut_is_safe`] admits concatenate to exactly the bytes
/// and counts of the whole document sealed at once. That equality is why a
/// transcript may be sealed one turn at a time; it holds only at admitted cuts.
///
/// # Errors
/// As [`redact_with`]: none for any input.
pub fn redact_with_at(
    raw: &SecretBytes,
    packs: &RulePackSet,
    first_line: usize,
) -> Result<RedactionOutput> {
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
        redaction_flat_fallback: 0,
        redaction_fallback_placeholders: 0,
    };
    let masked = jsonl::redact_content(&text, first_line, packs, &mut report);
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
    redact_fragment_observed(fragment, packs, report, |_, _| {})
}

/// Run the same bounded selector with a private observer before each mask.
/// The observer keeps only fallback provenance, never returned transcript bytes.
fn redact_fragment_observed(
    fragment: &str,
    packs: &RulePackSet,
    report: &mut RedactionReport,
    mut observe: impl FnMut(&str, &[Span]),
) -> Result<String> {
    let mut text = fragment.to_string();
    for _ in 0..MAX_PASSES {
        let (chosen, allowlisted) = effective_spans(&text, packs);
        observe(&text, &chosen);
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

fn effective_spans(text: &str, packs: &RulePackSet) -> (Vec<Span>, u32) {
    subtract_allowlisted(text, packs, select_spans(text, packs))
}

/// The allowlist is subtract-only: drop the spans whose exact value is
/// allowlisted (kept, not masked) and count them, so the report proves the
/// value was seen and deliberately kept rather than missed. Kept values are
/// re-detected on every pass, so a fragment contributes its last pass's count.
fn subtract_allowlisted(text: &str, packs: &RulePackSet, spans: Vec<Span>) -> (Vec<Span>, u32) {
    let mut allowlisted = 0u32;
    let chosen = spans
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
    (chosen, allowlisted)
}

/// `detector id → [REDACTED:<id>]` for every built-in PII detector.
///
/// Built **from the table [`propose`] reads**, not from a second list of names:
/// a twelfth PII detector gets its placeholder by existing, and cannot arrive
/// with the family root as a silent default. The ids are compile-time constants
/// (`&'static str` in [`builtin_tables`]) so no byte of any document reaches a
/// placeholder — which is the property that keeps a placeholder from becoming a
/// channel for the text it replaced.
pub(crate) fn pii_placeholders() -> &'static BTreeMap<String, String> {
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
pub(crate) fn placeholder_for(class: DetectorClass, id: &str) -> &'static str {
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
pub(crate) fn is_known_placeholder(text: &str) -> bool {
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

fn sha256(bytes: &[u8]) -> Sha256Id {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut b = [0u8; 32];
    b.copy_from_slice(&out);
    Sha256Id::from_bytes(b)
}

/// Normalize raw bytes to text: lossy UTF-8 + strip zero-width characters that
/// could smuggle a secret past shape detectors.
pub(crate) fn normalize(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .chars()
        .filter(|c| !matches!(*c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}'))
        .collect()
}

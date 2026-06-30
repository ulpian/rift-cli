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
//!   plus conservative PII (email, IPv4).
//!
//! Spans coalesce, then mask with a fixed placeholder per class (so masked
//! length never reveals secret length). The report holds **only** counts, ids,
//! hashes, and pack versions — it is structurally incapable of carrying a
//! secret. All regexes are linear-time (`regex` crate, no backreferences), so a
//! hostile input cannot wedge the pipeline (`05-security` §5.5).
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![allow(
    clippy::module_name_repetitions,
    clippy::doc_markdown,
    clippy::indexing_slicing
)]

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use rift_core::{RedactedBytes, RedactionProvenance, Result, RiftError, SecretBytes, Sha256Id};
use sha2::{Digest, Sha256};

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

/// Minimum token length / entropy for the pass-2 entropy detector.
const ENTROPY_MIN_LEN: usize = 20;
const ENTROPY_MIN_BITS: f64 = 4.0;

struct ShapeDetector {
    id: &'static str,
    class: DetectorClass,
    re: Regex,
    /// Capture group to mask (`0` = the whole match).
    group: usize,
}

#[derive(Clone, Debug)]
struct Span {
    start: usize,
    end: usize,
    class: DetectorClass,
    id: String,
}

/// A set of redaction rule packs (built-in + additive org packs) plus an
/// allowlist. Packs can only ever *broaden* coverage; the allowlist can only ever
/// *subtract* a redaction (keep a known-safe value) — neither can weaken the gate
/// in a way that leaks a secret (fail-safe, `05-security` §5.4).
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
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            extra: Vec::new(),
            versions: vec!["builtin@2026.06.0".to_string()],
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
    /// [`RiftError::Config`] if the regex does not compile.
    pub fn with_vendor_pattern(
        mut self,
        id: &'static str,
        regex: &str,
        version: &str,
    ) -> Result<Self> {
        let re =
            Regex::new(regex).map_err(|e| RiftError::config(format!("bad org pattern: {e}")))?;
        self.extra.push(ShapeDetector {
            id,
            class: DetectorClass::VendorSecret,
            re,
            group: 0,
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

// The only `expect`s here compile compile-time-*constant* regexes; a bad
// built-in pattern is a build-time bug, surfaced loudly on first use, never a
// runtime crash on user input.
#[allow(clippy::expect_used)]
fn pass1_detectors() -> &'static [ShapeDetector] {
    static CELL: OnceLock<Vec<ShapeDetector>> = OnceLock::new();
    CELL.get_or_init(|| {
        // `expect` here is at init of compile-time-constant patterns; a bad
        // built-in regex is a build-time bug, surfaced loudly on first use.
        let d = |id, class, re: &str, group| ShapeDetector {
            id,
            class,
            re: Regex::new(re).expect("valid built-in regex"),
            group,
        };
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
            // credentialed URI: mask only the user:pass group, keep scheme+host
            d(
                "uri.credentials",
                DetectorClass::CredentialedUri,
                r"://([^/\s@:]+:[^/\s@]+)@",
                1,
            ),
            // JDBC/DSN password value
            d(
                "dsn.password",
                DetectorClass::JdbcDsn,
                r"(?i)(?:password|pwd)=([^;&\s]+)",
                1,
            ),
            // PEM private-key block (DOTALL)
            d(
                "key.pem_block",
                DetectorClass::PrivateKey,
                r"(?s)-----BEGIN[^-]*PRIVATE KEY-----.*?-----END[^-]*PRIVATE KEY-----",
                0,
            ),
        ]
    })
}

#[allow(clippy::expect_used)] // compile-time-constant regexes only (see above)
fn pii_detectors() -> &'static [ShapeDetector] {
    static CELL: OnceLock<Vec<ShapeDetector>> = OnceLock::new();
    CELL.get_or_init(|| {
        let d = |id, re: &str| ShapeDetector {
            id,
            class: DetectorClass::Pii,
            re: Regex::new(re).expect("valid built-in regex"),
            group: 0,
        };
        vec![
            d(
                "pii.email",
                r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}",
            ),
            d("pii.ipv4", r"\b(?:[0-9]{1,3}\.){3}[0-9]{1,3}\b"),
        ]
    })
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
        counts[b as usize] += 1;
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

fn entropy_spans(text: &str) -> Vec<Span> {
    let bytes = text.as_bytes();
    let is_token =
        |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-');
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if is_token(bytes[i]) {
            let start = i;
            while i < bytes.len() && is_token(bytes[i]) {
                i += 1;
            }
            let tok = &text[start..i];
            if tok.len() >= ENTROPY_MIN_LEN && shannon_bits(tok) >= ENTROPY_MIN_BITS {
                spans.push(Span {
                    start,
                    end: i,
                    class: DetectorClass::Entropy,
                    id: "entropy.high".into(),
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
                spans.push(Span {
                    start: m.start(),
                    end: m.end(),
                    class: d.class,
                    id: d.id.to_string(),
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

fn overlaps(a: &Span, b: &Span) -> bool {
    a.start < b.end && b.start < a.end
}

/// Select non-overlapping spans, preferring higher precedence, then wider, then
/// earlier. The chosen set is returned sorted by start position for masking.
fn coalesce(mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_by(|a, b| {
        precedence(b.class)
            .cmp(&precedence(a.class))
            .then((b.end - b.start).cmp(&(a.end - a.start)))
            .then(a.start.cmp(&b.start))
    });
    let mut chosen: Vec<Span> = Vec::new();
    for s in spans {
        if !chosen.iter().any(|c| overlaps(c, &s)) {
            chosen.push(s);
        }
    }
    chosen.sort_by_key(|s| s.start);
    chosen
}

fn mask(
    text: &str,
    spans: &[Span],
    input_sha256: Sha256Id,
    versions: Vec<String>,
) -> (String, RedactionReport) {
    let mut by_detector: BTreeMap<String, u32> = BTreeMap::new();
    let mut by_class: BTreeMap<DetectorClass, u32> = BTreeMap::new();
    let mut bytes_masked: u64 = 0;
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for s in spans {
        out.push_str(&text[last..s.start]);
        out.push_str(s.class.placeholder());
        bytes_masked += (s.end - s.start) as u64;
        *by_detector.entry(s.id.clone()).or_insert(0) += 1;
        *by_class.entry(s.class).or_insert(0) += 1;
        last = s.end;
    }
    out.push_str(&text[last..]);
    let report = RedactionReport {
        total_spans: u32::try_from(spans.len()).unwrap_or(u32::MAX),
        by_detector,
        by_class,
        bytes_masked,
        allowlisted: 0,
        rule_pack_versions: versions,
        input_sha256,
    };
    (out, report)
}

/// Redact with the built-in pack.
///
/// # Errors
/// [`RiftError`] only on internal failure (regexes are pre-validated).
pub fn redact(raw: &SecretBytes) -> Result<RedactionOutput> {
    redact_with(raw, &RulePackSet::builtin())
}

/// Redact with a specific rule-pack set.
///
/// # Errors
/// [`RiftError`] only on internal failure.
pub fn redact_with(raw: &SecretBytes, packs: &RulePackSet) -> Result<RedactionOutput> {
    let input_sha256 = sha256(raw.as_slice());
    let text = normalize(raw.as_slice());

    let mut spans = shape_spans(&text, pass1_detectors());
    spans.extend(shape_spans(&text, &packs.extra));
    spans.extend(entropy_spans(&text));
    spans.extend(shape_spans(&text, pii_detectors()));

    let merged = coalesce(spans);
    // Allowlist is subtract-only: drop spans whose exact value is allowlisted
    // (kept, not masked) and count them, so the report proves the value was seen
    // and deliberately kept rather than missed.
    let mut allowlisted = 0u32;
    let chosen: Vec<Span> = merged
        .into_iter()
        .filter(|s| {
            let keep_unmasked = packs
                .allow
                .contains(&sha256(&text.as_bytes()[s.start..s.end]));
            if keep_unmasked {
                allowlisted += 1;
            }
            !keep_unmasked
        })
        .collect();
    let (masked, mut report) = mask(&text, &chosen, input_sha256, packs.version_set());
    report.allowlisted = allowlisted;
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
    use super::*;

    fn out(s: &str) -> RedactionOutput {
        redact(&SecretBytes::from(s)).unwrap()
    }

    fn masked(s: &str) -> String {
        String::from_utf8(out(s).redacted.as_slice().to_vec()).unwrap()
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

    #[test]
    fn pii_email_and_ip_masked() {
        let m = masked("contact alice@example.com at 10.0.0.42 please");
        assert!(!m.contains("alice@example.com"));
        assert!(!m.contains("10.0.0.42"));
        assert!(m.contains("[REDACTED:pii]"));
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
    /// the single-call-site rule (P5 S2) executable — add a mint elsewhere and CI
    /// goes red.
    #[test]
    fn seal_is_minted_only_in_the_redaction_crate() {
        fn visit(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
            for entry in std::fs::read_dir(dir).expect("read_dir") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n == "target") {
                        continue;
                    }
                    visit(&path, f);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let contents = std::fs::read_to_string(&path).unwrap_or_default();
                    f(&path, &contents);
                }
            }
        }
        let crates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates dir")
            .to_path_buf();
        let needle = ["RedactedBytes", "::seal("].concat(); // avoid self-matching this literal
        let mut here = 0usize;
        let mut offenders = Vec::new();
        visit(&crates_dir, &mut |path, contents| {
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

    proptest::proptest! {
        /// Determinism: same input + pack ⇒ byte-identical output + report.
        #[test]
        fn redaction_is_deterministic(s in ".{0,200}") {
            let a = redact(&SecretBytes::from(s.as_str())).unwrap();
            let b = redact(&SecretBytes::from(s.as_str())).unwrap();
            proptest::prop_assert_eq!(a.redacted.as_slice(), b.redacted.as_slice());
            proptest::prop_assert_eq!(a.report.total_spans, b.report.total_spans);
        }

        /// Never panics on arbitrary bytes (fuzz-style invariant).
        #[test]
        fn never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..256)) {
            let _ = redact(&SecretBytes::new(bytes));
        }
    }
}

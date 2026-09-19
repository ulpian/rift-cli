//! The built-in detectors through the public pipeline: each vendor, URI,
//! credential, key and PII shape is masked, the report carries counts and never
//! text, and the allowlist subtracts without ever adding a leak.

use rift_core::SecretBytes;

use super::fixtures::{masked, out};
use crate::entropy::{shannon_bits, ENTROPY_MIN_BITS};
use crate::{redact_with, RulePackSet};

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
    let pem =
        "-----BEGIN RSA PRIVATE KEY-----\nABCDEF0123\nGHIJKL4567\n-----END RSA PRIVATE KEY-----";
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

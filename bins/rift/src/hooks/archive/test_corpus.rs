//! The seeded secret corpus the archive's sealing tests feed through the gate:
//! the vendor-key rows of `rift-shadow-redact`'s own
//! `seeded_secret_corpus_zero_false_negatives`, which the main transcript's
//! redaction tests seal too, kept in one place so a subagent transcript and a
//! spill are held to the same rows.
//!
//! No personal-data row lives here. The PII corpus is that crate's own
//! seeded set, which the false-positive census excludes by path; copying it
//! into this crate would count the tests' needles as repository text.
//! Every sealed artifact goes through the same `redact_with`, so the PII
//! detectors' coverage is that crate's to prove.

/// The vendor-key rows, each a shape the built-in packs must mask.
pub(in crate::hooks) fn seeded_secrets() -> Vec<String> {
    vec![
        "AKIAIOSFODNN7EXAMPLE".to_owned(),
        "ghp_0123456789abcdef0123456789abcdef0123".to_owned(),
        "sk_live_0123456789abcdef0123".to_owned(),
        "xoxb-0123456789-abcdefghij".to_owned(),
        format!("AIza{}", "a".repeat(35)),
        "Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ3xC7vB9nM2qWeRt".to_owned(),
        "sb_secret_aBcDaBcDaBcDaBcDaBcDaBcD".to_owned(),
        "sk-ant-api03-aBcDaBcDaBcDaBcDaBcD".to_owned(),
        "github_pat_aBcDaBcDaBcDaBcDaBcDaBcD".to_owned(),
    ]
}

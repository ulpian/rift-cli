#![no_main]
//! Fuzz target for the redaction gate's per-line fallback.
//!
//! The fallback only runs when a detect→mask fixpoint does not converge, which
//! the built-in pack never makes happen, so neither `redact` nor
//! `redact_jsonl` reaches it. This target runs every document under a pack that
//! wedges: one rule matches inside its own placeholder, so a line carrying the
//! word never settles, and another is anchored so the same word wedges a JSON
//! leaf and not the line around it. The fuzzer's bytes choose the lines —
//! arbitrary text, JSONL records, seeded secrets and wedging words — and the
//! contract is the fallback's: **it never panics, no seeded secret survives,
//! and the sealed document has exactly as many lines as the input.**
//!
//! The line count is asserted only over documents whose free text is drawn
//! from lower-case letters and spaces. A detector whose pattern admits a
//! newline (a private-key block, a label and its value on two lines) masks
//! across lines on the normal path too, so arbitrary text would report that as
//! a lost line when it is a sealed one; arbitrary text still runs, for the
//! other two clauses.

use libfuzzer_sys::fuzz_target;
use rift_core::SecretBytes;
use rift_shadow_redact::RulePackSet;

/// Secrets the flat pipeline seals on its own, in any carrier text.
const SECRETS: &[&str] = &[
    "AKIAIOSFODNN7EXAMPLE",
    "ghp_QWERTYUIOPASDFGHJKLZXCVBNM0123456789",
    "alice@example.com",
];

/// Words that wedge one of the two rules.
const WEDGES: &[&str] = &["REDACTED", "start REDACTED end", "[REDACTED"];

/// One line, chosen and filled by the script.
fn line(kind: u8, free: &str, planted: &mut Vec<&'static str>) -> String {
    let secret = SECRETS[usize::from(kind / 8) % SECRETS.len()];
    let wedge = WEDGES[usize::from(kind / 16) % WEDGES.len()];
    match kind % 8 {
        0 => free.to_owned(),
        1 => format!("{{\"type\":\"user\",\"text\":{}}}", quoted(free)),
        2 => {
            planted.push(secret);
            format!("{free} {secret}")
        }
        3 => {
            planted.push(secret);
            format!("{{\"text\":\"{secret} {wedge}\"}}")
        }
        4 => format!("{wedge} {free}"),
        5 => format!("{{\"text\":\"{wedge}\",\"note\":{}}}", quoted(free)),
        6 => {
            planted.push(secret);
            format!("{wedge} {secret}")
        }
        _ => String::new(),
    }
}

fn quoted(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_owned())
}

fuzz_target!(|data: &[u8]| {
    let packs = RulePackSet::builtin()
        .with_vendor_pattern("osc", "REDACTED", "fuzz@1")
        .and_then(|p| p.with_vendor_pattern("osc_start", r"^\[?REDACTED", "fuzz@1"));
    let Ok(packs) = packs else {
        return;
    };
    let mut planted = Vec::new();
    let mut lines = Vec::new();
    let mut arbitrary = false;
    for chunk in data.chunks(12).take(24) {
        let Some((&kind, rest)) = chunk.split_first() else {
            continue;
        };
        // The second byte picks the alphabet: arbitrary text (never a
        // newline, which would be a second line), or letters and spaces.
        let raw = rest.first().is_some_and(|b| b % 4 == 0);
        arbitrary |= raw;
        let free: String = String::from_utf8_lossy(rest)
            .chars()
            .filter(|c| if raw { *c != '\n' } else { c.is_ascii_lowercase() || *c == ' ' })
            .collect();
        lines.push(line(kind, &free, &mut planted));
    }
    let document = lines.join("\n");
    let output = rift_shadow_redact::redact_with(&SecretBytes::from(document.as_str()), &packs)
        .expect("the gate is total");
    let text = String::from_utf8_lossy(output.redacted.as_slice()).to_string();
    for secret in &planted {
        assert!(
            !text.contains(secret),
            "a seeded secret survived the fallback\n  document: {document}\n  output: {text}"
        );
    }
    if !arbitrary {
        assert_eq!(
            text.split('\n').count(),
            document.split('\n').count(),
            "the fallback dropped or added a line\n  document: {document}\n  output: {text}"
        );
    }
    assert!(
        output.report.redaction_fallback_placeholders <= output.report.redaction_flat_fallback
    );
});

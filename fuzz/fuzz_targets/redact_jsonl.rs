#![no_main]
//! Fuzz target for the JSON-aware walker.
//!
//! `redact` is shape-adaptive, so the `redact` target already reaches this code
//! — but only by accident: random bytes practically never form a JSON document,
//! so the walker's interesting paths (nesting, repeated keys, escape spellings,
//! keys as content) are never visited. This target spends the fuzzer's bytes on
//! *structure* instead. The input is read as a script that builds a JSON
//! document and plants known secrets in it, and the contract is the one that
//! matters: **no planted secret survives the gate.**
//!
//! **The key list has no exclusions, and that is the change from the version
//! this ports.** That version kept `*signature`, identifier, path and payload
//! keys out of the script, because a structural key was then the one place the
//! walker was allowed to seal less than flat text — so the invariant it fuzzed
//! was conditional on never writing one. The owner then removed scope reduction
//! from the walker altogether (2026-08-08), so the invariant is
//! unconditional and the script writes every key a transcript can carry,
//! including the ones that used to buy protection.

use libfuzzer_sys::fuzz_target;
use rift_core::SecretBytes;

/// Secrets the flat pipeline seals on its own, in any carrier text.
const SECRETS: &[&str] = &[
    "AKIAIOSFODNN7EXAMPLE",
    "ghp_QWERTYUIOPASDFGHJKLZXCVBNM0123456789",
    "Zx9Qw3eR7tY2uI8oP1aS5dF6gH0jK4lZ3xC7vB9nM2q",
    "alice@example.com",
];

/// Every key kind, including the four that used to grant a reduced scope.
const KEYS: &[&str] = &[
    "note",
    "text",
    "body",
    "content",
    "signature",
    "thinkingSignature",
    "session_id",
    "traceID",
    "file_path",
    "cwd",
    "type",
    "data",
    "uuid",
];

/// A label whose detector needs the key and the value in ONE text, so a walk
/// that judges the value alone cannot see it. The walker answers this by
/// refusing any structural line the flat pipeline would still mask; the target
/// exists partly to keep that answer honest under coverage-guided input.
const LABELLED: &[(&str, &str)] = &[("ssn", "123456789"), ("social_security", "123456789")];

const FILLER: &[&str] = &["hello world", "ok", "the quick brown fox"];

/// The fuzzer's bytes, read as a script rather than as content.
struct Script<'a> {
    data: &'a [u8],
    at: usize,
}

impl Script<'_> {
    fn byte(&mut self) -> u8 {
        let b = self.data.get(self.at).copied().unwrap_or(0);
        self.at = self.at.saturating_add(1);
        b
    }

    fn pick<T: Copy>(&mut self, choices: &[T]) -> T {
        let index = usize::from(self.byte()) % choices.len();
        choices[index]
    }

    fn spent(&self) -> bool {
        self.at >= self.data.len()
    }
}

/// One JSON value, planting secrets as it goes.
fn build(script: &mut Script, depth: usize, planted: &mut Vec<String>) -> String {
    if depth >= 3 || script.spent() || script.byte() % 4 == 0 {
        return leaf(script, planted);
    }
    if script.byte() % 2 == 0 {
        let members = 1 + usize::from(script.byte() % 3);
        let mut parts = Vec::new();
        for _ in 0..members {
            // a labelled pair, whose detector reads key and value together
            if script.byte() % 7 == 0 {
                let (label, value) = script.pick(LABELLED);
                planted.push(value.to_string());
                parts.push(format!("\"{label}\":\"{value}\""));
                continue;
            }
            // a secret in key position: a key is scanned like any other text
            let key = if script.byte() % 8 == 0 {
                let secret = script.pick(SECRETS);
                planted.push(secret.to_string());
                format!("\"{secret}\"")
            } else {
                format!("\"{}\"", script.pick(KEYS))
            };
            // …and sometimes the same key twice, the first value being the one
            // every last-wins parser drops before a detector could see it
            if script.byte() % 3 == 0 {
                let shadowed = build(script, depth + 1, planted);
                parts.push(format!("{key}:{shadowed}"));
            }
            let value = build(script, depth + 1, planted);
            parts.push(format!("{key}:{value}"));
        }
        return format!("{{{}}}", parts.join(","));
    }
    let items = 1 + usize::from(script.byte() % 3);
    let parts: Vec<String> = (0..items)
        .map(|_| build(script, depth + 1, planted))
        .collect();
    format!("[{}]", parts.join(","))
}

/// One string literal: filler, or filler carrying a secret — sometimes with the
/// secret's first character spelled as a six-character escape (so the raw bytes
/// and the decoded leaf disagree about what is written there), sometimes pressed
/// against a `\n` whose backslash is not a token character in the raw bytes
/// while its `n` is.
fn leaf(script: &mut Script, planted: &mut Vec<String>) -> String {
    let filler = script.pick(FILLER);
    let shape = script.byte();
    if shape % 3 == 0 {
        return format!("\"{filler}\"");
    }
    let secret = script.pick(SECRETS);
    if shape % 5 == 0 {
        let (head, tail) = secret.split_at(1);
        planted.push(tail.to_string());
        let code = u32::from(head.as_bytes()[0]);
        return format!("\"{}u{code:04x}{tail} {filler}\"", '\\');
    }
    planted.push(secret.to_string());
    if shape % 7 == 0 {
        return format!("\"{filler}{}n{secret}\"", '\\');
    }
    format!("\"{filler} {secret}\"")
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let mut script = Script { data, at: 0 };
    let mut planted = Vec::new();
    let document = build(&mut script, 0, &mut planted);
    // A generator that stopped producing JSON would teach the fuzzer nothing
    // about the walker, and would look like coverage while finding nothing.
    debug_assert!(
        serde_json::from_str::<serde_json::Value>(&document).is_ok(),
        "the script built something that is not JSON: {document}"
    );

    let Ok(output) = rift_shadow_redact::redact(&SecretBytes::from(document.as_str())) else {
        return;
    };
    let text = String::from_utf8_lossy(output.redacted.as_slice()).to_string();
    for secret in &planted {
        assert!(
            !text.contains(secret.as_str()),
            "a planted secret survived the gate\n  document: {document}\n  secret: {secret}\n  \
             output: {text}"
        );
    }

    // Stable on its own output: whatever path the line took, redacting the
    // result again must change nothing.
    if let Ok(second) = rift_shadow_redact::redact(&SecretBytes::from(text.as_str())) {
        assert_eq!(second.redacted.as_slice(), text.as_bytes());
    }
});

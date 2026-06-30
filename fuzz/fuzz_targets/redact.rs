#![no_main]
//! Fuzz target for the one-way redaction gate (Stage 2 P5 S1 / 06 §6.1 / §5.9).
//!
//! Contract: for *any* byte sequence, [`rift_shadow_redact::redact`] must return
//! a `Result` — never panic, never hang (all regexes are linear-time), never
//! allocate unboundedly. A crash here is a security bug: a hostile transcript
//! could otherwise wedge the capture path. The redacted output must also never be
//! re-redactable into something new (the gate is stable/idempotent on its own
//! output), which we assert as a cheap invariant.

use libfuzzer_sys::fuzz_target;
use rift_core::SecretBytes;

fuzz_target!(|data: &[u8]| {
    if let Ok(first) = rift_shadow_redact::redact(&SecretBytes::new(data.to_vec())) {
        // Idempotence: redacting cleared bytes masks nothing new (placeholders
        // are not secrets), so the gate is stable on its own output.
        let cleared = first.redacted.as_slice().to_vec();
        if let Ok(second) = rift_shadow_redact::redact(&SecretBytes::new(cleared.clone())) {
            assert_eq!(second.redacted.as_slice(), cleared.as_slice());
        }
    }
});

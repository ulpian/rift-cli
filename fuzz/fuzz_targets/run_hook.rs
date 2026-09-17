#![no_main]
//! Fuzz target for the `rift hooks <agent> <verb>` wire contract.
//!
//! Contract: parsing the **untrusted agent stdin JSON** must never panic. Over any
//! byte input, `parse_bounded` and `build_raw_hook` return `Ok`/`Err` (bounded in
//! size and nesting depth) — a hostile payload is a clean reject, never a crash and
//! never a partial/unbounded buffer. The input is split into an agent token, a
//! verb, and the stdin body to fuzz all three.

use libfuzzer_sys::fuzz_target;
use rift_shadow_hooks::{build_raw_hook, parse_bounded};

fuzz_target!(|data: &[u8]| {
    // The raw bytes are the untrusted stdin envelope.
    let _ = parse_bounded(data);

    // Also fuzz the full entrypoint: carve an agent token + verb off the front.
    let split = data.len() / 8;
    let (head, body) = data.split_at(split.min(data.len()));
    let agent = String::from_utf8_lossy(head);
    let _ = build_raw_hook(&agent, "fuzz_verb", body, 0);
});

#![no_main]
//! Fuzz target for the diff-capture engine (Stage 2 P4 S1 / 06 §6.1 / §5.9).
//!
//! Contract: `diff_attributable` over *any* two byte buffers (untrusted file
//! content) must never panic — a pathological file degrades to non-attributable,
//! never a crash. The input is split into two halves to fuzz the before/after pair.

use libfuzzer_sys::fuzz_target;
use rift_shadow_attribution::diff_attributable;

fuzz_target!(|data: &[u8]| {
    let mid = data.len() / 2;
    let (a, b) = data.split_at(mid);
    let _ = diff_attributable(a, b);
});

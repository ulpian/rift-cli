#![no_main]
//! Fuzz target for the policy glob matcher.
//!
//! Contract: for *any* pattern/text pair, `glob_match` returns in time bounded
//! by O(|pattern| · |text|) — never panics, never recurses unboundedly, never
//! exhibits backtracking blowup. A hang here is a security bug: a hostile
//! committed policy pattern could wedge the sensitive-path / model-allow gates.
//!
//! The input is split at the first NUL into (pattern, text). Pattern is capped
//! at the validation bound (`MAX_PATTERN_LEN` = 512) and text at 4 KiB so a
//! single exec stays within the libFuzzer timeout while still exercising the
//! full cost bound.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let split = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    let (pat, text) = data.split_at(split);
    let pat = &pat[..pat.len().min(512)];
    let text = text.get(1..).unwrap_or(&[]);
    let text = &text[..text.len().min(4096)];
    if let (Ok(pat), Ok(text)) = (std::str::from_utf8(pat), std::str::from_utf8(text)) {
        let _ = rift_config::glob_match(pat, text);
    }
});

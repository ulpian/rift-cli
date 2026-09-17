#![no_main]
//! Fuzz target for the first untrusted parser Rift ever reads: `.rift/config.toml`.
//!
//! Contract: for *any* byte sequence, parsing must return a
//! `Result` — never panic, never hang, never allocate unboundedly. A crash here
//! is a security bug (a hostile config on a branch could abort a hook).

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = rift_config::EffectiveConfig::from_bytes(data);
});

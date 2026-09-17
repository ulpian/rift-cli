#![no_main]
//! Fuzz target for the palette's closed slash grammar.
//!
//! Contract: parse is TOTAL over arbitrary input — for any byte sequence it
//! returns `Ok(Verb)` or a span-carrying `ParseError`; never a panic, hang, or
//! unbounded allocation, and no input can reach a `hooks` verb (the variant
//! does not exist). A crash here is a security bug: the palette is a free-text
//! surface fed straight from the keyboard.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        match rift_canopy::parse(text) {
            Ok(verb) => {
                // The tier partition is total too.
                let _ = verb.tier();
            }
            Err(e) => {
                // Error spans are ordered (renderable as an underline).
                assert!(e.span.0 <= e.span.1);
            }
        }
    }
});

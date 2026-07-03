#![no_main]
//! Fuzz target for the checkpoint-shard artifact parsers (Stage 3 P1 S2 —
//! `rift-recall-substrate::parse`).
//!
//! These parse **untrusted bytes**: shard artifacts come out of a git branch an
//! agent (or a hostile commit) wrote. Contract (06 §6.1.6 / 05 §5.9): for any
//! byte sequence every entrypoint returns an `Outcome` — never panics, never
//! hangs, never allocates unboundedly. The first input byte selects the
//! entrypoint so one corpus exercises all five.

use libfuzzer_sys::fuzz_target;
use rift_recall_substrate::parse;

fuzz_target!(|data: &[u8]| {
    let Some((&selector, bytes)) = data.split_first() else {
        return;
    };
    let id = "fuzz-checkpoint";
    match selector % 5 {
        0 => {
            let _ = parse::parse_metadata(bytes, id);
        }
        1 => {
            let _ = parse::parse_attribution(bytes, id);
        }
        2 => {
            let _ = parse::parse_cost_estimate(bytes, id);
        }
        3 => {
            let _ = parse::parse_tool_calls(bytes, id);
        }
        _ => {
            let _ = parse::parse_token_estimates(bytes, id);
        }
    }
});

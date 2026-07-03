#![no_main]
//! Fuzz target for the checkpoint-shard artifact parsers (Stage 3 P1 S2 —
//! `rift-recall-substrate::parse`).
//!
//! These parse **untrusted bytes**: shard artifacts come out of a git branch an
//! agent (or a hostile commit) wrote. Contract (06 §6.1.6 / 05 §5.9): for any
//! byte sequence every entrypoint returns an `Outcome` — never panics, never
//! hangs, never allocates unboundedly. The first input byte selects the
//! entrypoint so one corpus exercises all nine (the M1 bridge parsers —
//! `redaction_report.json`, `full.jsonl`, `tasks/`, `merkle_leaf.txt` —
//! included).

use libfuzzer_sys::fuzz_target;
use rift_recall_substrate::parse;

fuzz_target!(|data: &[u8]| {
    let Some((&selector, bytes)) = data.split_first() else {
        return;
    };
    let id = "fuzz-checkpoint";
    match selector % 9 {
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
        4 => {
            let _ = parse::parse_token_estimates(bytes, id);
        }
        5 => {
            let _ = parse::parse_redaction_report(bytes, id);
        }
        6 => {
            let _ = parse::parse_full_events(bytes, id);
        }
        7 => {
            let _ = parse::parse_tasks(bytes, id);
        }
        _ => {
            let _ = parse::parse_merkle_leaves(bytes, id);
        }
    }
});

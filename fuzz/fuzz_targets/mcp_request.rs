#![no_main]
//! Fuzz target for the serve request decoder
//! (`rift-recall-serve::decode_request`).
//!
//! The target's name predates the decision that deleted the stdio transport
//! that used to be `decode_request`'s one caller; the decoder itself is the
//! transport-independent seam that decision explicitly kept, and today
//! it decodes the `rift recall <verb>` skill-CLI channel's argv-turned-JSON
//! instead (`decode_verb` → `decode_request`, `bins/rift/src/recall/serve.rs`). The
//! target and corpus names stay: renaming a fuzz target discards its
//! accumulated corpus for no behavioral gain, and the names are wired into the
//! CI workflows. What it fuzzes has not moved.
//!
//! The decoder sits on the agent-facing boundary: tool names and params
//! arrive as **untrusted JSON**. Contract: decoding is total — for any tool
//! name and any JSON value it returns `Ok`/`DecodeError`, never panics. The
//! input is split at the first NUL into (tool_name, params-JSON); non-JSON
//! params exercise the serde error path.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let split = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    let (name, rest) = data.split_at(split);
    let Ok(name) = std::str::from_utf8(name) else {
        return;
    };
    let params: serde_json::Value = match serde_json::from_slice(rest.get(1..).unwrap_or(&[])) {
        Ok(v) => v,
        Err(_) => serde_json::Value::Null,
    };
    let _ = rift_recall_serve::decode_request(name, &params);
});

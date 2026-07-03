#![no_main]
//! Fuzz target for the MCP request decoder (Stage 3 P5 —
//! `rift-recall-mcp::decode_request`).
//!
//! The decoder sits on the agent-facing stdio boundary: tool names and params
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
    let _ = rift_recall_mcp::decode_request(name, &params);
});

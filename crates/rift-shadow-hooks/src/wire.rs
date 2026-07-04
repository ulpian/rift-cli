//! The `rift hooks <agent> <verb>` **wire contract** (P8 S2): a total, bounded
//! parser for the untrusted stdin JSON envelope, `--schema` introspection, and the
//! [`build_raw_hook`] entrypoint that is the fuzz seam (§5.9).
//!
//! The hook process parses **untrusted JSON from the agent** — a primary fuzz
//! target. [`parse_bounded`] caps size and is total (no panic), and
//! [`build_raw_hook`] turns the wire inputs into a [`RawHook`] without ever
//! panicking, so an oversized/malformed payload is a clean reject, never a crash
//! and never a partial write.

use rift_core::{AgentId, EventKind, Result, RiftError, Trust};

use crate::RawHook;

/// Max accepted stdin payload (1 MiB) — a hostile/oversized payload is rejected,
/// not buffered unboundedly.
pub const MAX_STDIN_BYTES: usize = 1 << 20;

/// Max accepted JSON nesting depth — defeats stack exhaustion via deeply nested
/// input. (`serde_json`'s own recursion limit rejects most pathological depth
/// before we even measure it; this is a tighter, explicit ceiling.)
pub const MAX_DEPTH: u32 = 64;

/// Parse untrusted hook stdin into a JSON value, bounded in size and nesting
/// depth. **Total**: returns `Err` (never panics) on oversized, malformed, or
/// too-deep input.
///
/// # Errors
/// [`RiftError::Validity`] if the input exceeds [`MAX_STDIN_BYTES`], is not valid
/// JSON, or nests deeper than [`MAX_DEPTH`].
pub fn parse_bounded(bytes: &[u8]) -> Result<serde_json::Value> {
    if bytes.len() > MAX_STDIN_BYTES {
        return Err(RiftError::validity("hook stdin: payload too large"));
    }
    // serde_json enforces its own recursion limit (~128), so this parse cannot
    // stack-overflow; the depth check below is a tighter, explicit policy bound.
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| RiftError::validity(format!("hook stdin: {e}")))?;
    if json_depth(&value) > MAX_DEPTH {
        return Err(RiftError::validity("hook stdin: nesting too deep"));
    }
    Ok(value)
}

/// Structural nesting depth of a parsed value (scalars = 0). Safe to recurse: the
/// value already passed `serde_json`'s recursion limit, so depth is bounded.
fn json_depth(v: &serde_json::Value) -> u32 {
    match v {
        serde_json::Value::Array(a) => 1 + a.iter().map(json_depth).max().unwrap_or(0),
        serde_json::Value::Object(o) => 1 + o.values().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// Build a [`RawHook`] from the wire inputs, **totally and safely** (the fuzz
/// seam, §5.9). Validates the agent-token shape, bounds the stdin, and extracts
/// the `session`/`turn`/`payload` envelope fields. Input that carries none of
/// the envelope keys but has a `session_id` string is accepted as an
/// **agent-native** payload (e.g. Claude Code hook stdin): the session comes
/// from `session_id`, the whole object becomes the payload, and the turn is
/// left `None` for the ingest layer to derive. Never panics; an unparsable,
/// oversized, or malformed payload is a clean error the caller swallows as
/// "deferred" (the hook never fails the agent).
///
/// # Errors
/// [`RiftError::Validity`] for a malformed agent token or a payload that fails
/// [`parse_bounded`].
pub fn build_raw_hook(
    agent: &str,
    verb: &str,
    stdin: &[u8],
    occurred_at_ms: i64,
) -> Result<RawHook> {
    let agent_id = AgentId::new(agent);
    if !agent_id.is_well_formed() {
        return Err(RiftError::validity("hook: malformed agent token"));
    }
    let json = parse_bounded(stdin)?;
    let is_envelope = json.get("session").is_some()
        || json.get("turn").is_some()
        || json.get("payload").is_some();
    let (session, turn, payload) = if is_envelope {
        (
            json.get("session")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            json.get("turn")
                .and_then(serde_json::Value::as_u64)
                .and_then(|t| u32::try_from(t).ok()),
            json.get("payload")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
    } else if let Some(sid) = json.get("session_id").and_then(serde_json::Value::as_str) {
        // Native fallback: the agent's own hook shape, ingested whole.
        (sid.to_owned(), None, json.clone())
    } else {
        (String::new(), None, serde_json::Value::Null)
    };
    Ok(RawHook {
        agent: agent_id,
        verb: verb.to_owned(),
        session,
        turn,
        occurred_at_ms,
        payload,
    })
}

/// The current canonical-event schema, for `rift hooks --schema` (SDK codegen /
/// adapter authors). Emits the schema version, the full `EventKind`/`Trust`
/// vocabularies, the canonical-event field order, and the wire limits.
#[must_use]
pub fn schema_json() -> serde_json::Value {
    let kinds: Vec<&str> = EventKind::ALL.iter().map(|k| k.as_str()).collect();
    let trust: Vec<&str> = Trust::ALL.iter().map(|t| t.as_str()).collect();
    serde_json::json!({
        "schema_version": rift_agent::SCHEMA_VERSION,
        "event_kinds": kinds,
        "trust_classes": trust,
        "envelope": {
            "invocation": "rift hooks <agent> <verb>  (JSON on stdin)",
            "stdin_fields": { "session": "string", "turn": "u32", "payload": "object" },
            "native_fallback": "an object with none of the envelope keys but a session_id string is ingested whole as the payload; session = session_id, turn derived at ingest",
            "canonical_event_fields": [
                "schema_version", "session", "turn_ordinal", "event_ordinal",
                "kind", "trust", "occurred_at_ms", "payload_sha256"
            ],
        },
        "limits": { "max_stdin_bytes": MAX_STDIN_BYTES, "max_depth": MAX_DEPTH },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bounded_rejects_oversize_and_garbage_without_panic() {
        assert!(parse_bounded(&vec![b'x'; MAX_STDIN_BYTES + 1]).is_err());
        assert!(parse_bounded(b"{not json").is_err());
        assert!(parse_bounded(b"{}").is_ok());
    }

    #[test]
    fn parse_bounded_rejects_too_deep() {
        // 70 levels of nested arrays (> MAX_DEPTH 64), still under serde's ~128.
        let deep = format!("{}{}", "[".repeat(70), "]".repeat(70));
        assert!(parse_bounded(deep.as_bytes()).is_err());
    }

    #[test]
    fn build_raw_hook_extracts_envelope_and_is_total() {
        let body = br#"{"session":"01J","turn":3,"payload":{"tool_name":"Bash"}}"#;
        let raw = build_raw_hook("claude-code", "PreToolUse", body, 42).unwrap();
        assert_eq!(raw.agent, AgentId::CLAUDE_CODE);
        assert_eq!(raw.session, "01J");
        assert_eq!(raw.turn, Some(3));
        assert_eq!(raw.occurred_at_ms, 42);
        // a malformed agent token is rejected, not panicked
        assert!(build_raw_hook("../evil", "v", b"{}", 0).is_err());
    }

    #[test]
    fn build_raw_hook_accepts_native_shape_without_a_shim() {
        // M2-15: Claude Code's native hook stdin — no envelope keys, a
        // session_id string. The whole object is the payload; the turn is left
        // for the ingest layer to derive.
        let body =
            br#"{"session_id":"2227d6bc","hook_event_name":"PreToolUse","tool_name":"Bash"}"#;
        let raw = build_raw_hook("claude-code", "PreToolUse", body, 7).unwrap();
        assert_eq!(raw.session, "2227d6bc");
        assert_eq!(raw.turn, None);
        assert_eq!(raw.payload["tool_name"], "Bash");
        assert_eq!(raw.payload["session_id"], "2227d6bc");

        // Envelope keys win: a payload-bearing envelope never falls back.
        let env = br#"{"session":"S","payload":{"session_id":"native"}}"#;
        let raw = build_raw_hook("claude-code", "Stop", env, 0).unwrap();
        assert_eq!(raw.session, "S");
        // Neither shape matches → empty defaults, still total.
        let neither = build_raw_hook("claude-code", "Stop", b"{\"x\":1}", 0).unwrap();
        assert_eq!(neither.session, "");
        assert_eq!(neither.turn, None);
        assert!(neither.payload.is_null());
    }

    #[test]
    fn schema_json_lists_all_kinds_and_trust() {
        let s = schema_json();
        assert_eq!(s["schema_version"], rift_agent::SCHEMA_VERSION);
        assert_eq!(
            s["event_kinds"].as_array().unwrap().len(),
            EventKind::ALL.len()
        );
        assert!(s["event_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "tool_call"));
    }

    proptest::proptest! {
        /// Parse totality (§5.9): for arbitrary bytes, `parse_bounded` returns
        /// Ok/Err and never panics.
        #[test]
        fn parse_bounded_is_total(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..4096)) {
            let _ = parse_bounded(&bytes);
        }

        /// `build_raw_hook` is total over arbitrary agent/verb/stdin.
        #[test]
        fn build_raw_hook_is_total(
            agent in "[a-z0-9-]{0,12}",
            verb in "\\PC{0,16}",
            bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..2048),
        ) {
            let _ = build_raw_hook(&agent, &verb, &bytes, 0);
        }
    }
}

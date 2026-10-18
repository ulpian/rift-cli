//! The receipt address is one formula: the wrapper and the public scalar
//! produce the bytes the pre-factoring code produced, pinned as literals.
use super::{hook_receipt_id, receipt_id};
use crate::sha256_hex;
use rift_shadow_hooks::{AgentId, RawHook};

/// `sha256(b"{}")`: the payload hash of an empty object, as `ingest_hook`
/// serializes it.
const EMPTY_PAYLOAD_SHA: &str = "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";
/// The payload hash of the nested, non-lexically-ordered payload below once
/// serde_json has serialized it (`{"a":{"y":"x","z":[1,2]},"b":1}`).
const NESTED_PAYLOAD_SHA: &str = "4d34f9c4b90e00eb3051c0bd2bc39d24e3f74b9636ff4ac58ab3b828a86da0c6";

const DEFERRED_TURN_ZERO: &str = "d861e795cb72e62f594c0ea80c31e0f792bb0d6b3d1aee202346b651a5a17261";
const TURN_U32_MAX: &str = "b62863a3b173684821a47cb3625e020df766648fe6e71599364dd25a156241eb";
const NESTED_TURN_SEVEN: &str = "efcef120168fdbbbfe71c66127615dbab9c0ac37cb107279001fa6956f98b67b";

fn nested_payload() -> serde_json::Value {
    serde_json::json!({"b": 1, "a": {"z": [1, 2], "y": "x"}})
}

fn hook(agent: AgentId, verb: &str, session: &str, payload: serde_json::Value) -> RawHook {
    RawHook {
        agent,
        verb: verb.into(),
        session: session.into(),
        turn: None,
        occurred_at_ms: 1,
        payload,
    }
}

fn payload_sha(payload: &serde_json::Value) -> String {
    sha256_hex(&[serde_json::to_vec(payload).unwrap().as_slice()])
}

/// The payload hashes the vectors were computed over are the ones the ingest
/// derives, so the pinned receipt ids below are the ingest's own bytes.
#[test]
fn the_vector_payload_hashes_are_the_ingests_own() {
    assert_eq!(payload_sha(&serde_json::json!({})), EMPTY_PAYLOAD_SHA);
    assert_eq!(payload_sha(&nested_payload()), NESTED_PAYLOAD_SHA);
}

#[test]
fn the_existing_wrapper_still_writes_the_pinned_bytes() {
    let deferred = hook(
        AgentId::CLAUDE_CODE,
        "PreToolUse",
        "",
        serde_json::json!({}),
    );
    assert_eq!(
        receipt_id("claude-code", &deferred, "0", EMPTY_PAYLOAD_SHA),
        DEFERRED_TURN_ZERO
    );
    let max = hook(AgentId::CODEX, "Stop", "sess-1", nested_payload());
    assert_eq!(
        receipt_id("codex", &max, &u32::MAX.to_string(), NESTED_PAYLOAD_SHA),
        TURN_U32_MAX
    );
    let nested = hook(AgentId::ANTIGRAVITY, "Stop", "conv-1", nested_payload());
    assert_eq!(
        receipt_id("antigravity", &nested, "7", NESTED_PAYLOAD_SHA),
        NESTED_TURN_SEVEN
    );
}

/// The public scalar is the same formula over the same scalars: the deferred
/// receipt's literal `"0"`, a turn at the top of `u32`, and a nested payload.
#[test]
fn the_public_scalar_writes_the_same_bytes_as_the_wrapper() {
    for (agent, verb, session, turn, sha, expected) in [
        (
            "claude-code",
            "PreToolUse",
            "",
            "0",
            EMPTY_PAYLOAD_SHA,
            DEFERRED_TURN_ZERO,
        ),
        (
            "codex",
            "Stop",
            "sess-1",
            "4294967295",
            NESTED_PAYLOAD_SHA,
            TURN_U32_MAX,
        ),
        (
            "antigravity",
            "Stop",
            "conv-1",
            "7",
            NESTED_PAYLOAD_SHA,
            NESTED_TURN_SEVEN,
        ),
    ] {
        assert_eq!(hook_receipt_id(agent, verb, session, turn, sha), expected);
        let raw = hook(AgentId::new(agent), verb, session, nested_payload());
        assert_eq!(receipt_id(agent, &raw, turn, sha), expected);
    }
    // Distinct in every scalar: a field moved across a NUL boundary is a
    // different receipt, never a collision.
    assert_ne!(
        hook_receipt_id("codex", "Stop", "sess-1", "1", NESTED_PAYLOAD_SHA),
        hook_receipt_id("codex", "Stop", "sess-11", "", NESTED_PAYLOAD_SHA)
    );
}

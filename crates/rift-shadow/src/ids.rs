//! The shared id-derivation module (ADR-0002): every normalized capture row is
//! content-addressed **here**, and only here.
//!
//! Both canonical-event sources — the live agent-hook path and the artifact
//! bridge — derive row ids through these functions, so the union of
//! hook-produced and bridge-produced rows is a set-union no-op by construction:
//! same identity inputs → same id → `INSERT OR IGNORE` dedupes. Ids never
//! include wall-clock or receipt-order inputs; identity comes from the event's
//! own coordinates (session, ordinals, kind) and content hashes.
//!
//! Every derivation is domain-separated by a leading tag so two relations can
//! never collide on structurally identical inputs.

use crate::sha256_hex;

/// Tagged content address: `sha256(tag ‖ 0x00 ‖ part ‖ 0x00 ‖ part …)` (hex).
fn tagged(tag: &str, parts: &[&str]) -> String {
    let mut bytes: Vec<&[u8]> = Vec::with_capacity(parts.len() * 2 + 1);
    bytes.push(tag.as_bytes());
    for p in parts {
        bytes.push(b"\0");
        bytes.push(p.as_bytes());
    }
    sha256_hex(&bytes)
}

/// The `session.session_id` for an agent's native session id. Namespacing by
/// agent keeps two agents' coincidentally-equal native ids distinct.
#[must_use]
pub fn session_row_id(agent: &str, native_session: &str) -> String {
    tagged("session", &[agent, native_session])
}

/// A `session_event.event_id`.
#[must_use]
pub fn session_event_id(
    session_row_id: &str,
    kind: &str,
    turn_ordinal: u32,
    event_ordinal: u32,
) -> String {
    tagged(
        "session_event",
        &[
            session_row_id,
            kind,
            &turn_ordinal.to_string(),
            &event_ordinal.to_string(),
        ],
    )
}

/// A `turn.turn_id` — one per `(session, ordinal)`.
#[must_use]
pub fn turn_row_id(session_row_id: &str, ordinal: u32) -> String {
    tagged("turn", &[session_row_id, &ordinal.to_string()])
}

/// A `model_call.call_id` — anchored to the usage-bearing event's coordinates.
#[must_use]
pub fn model_call_id(session_row_id: &str, turn_ordinal: u32, event_ordinal: u32) -> String {
    tagged(
        "model_call",
        &[
            session_row_id,
            &turn_ordinal.to_string(),
            &event_ordinal.to_string(),
        ],
    )
}

/// A `tool_call.tool_call_id`.
#[must_use]
pub fn tool_call_row_id(session_row_id: &str, turn_ordinal: u32, event_ordinal: u32) -> String {
    tagged(
        "tool_call",
        &[
            session_row_id,
            &turn_ordinal.to_string(),
            &event_ordinal.to_string(),
        ],
    )
}

/// A synthetic `subagent_session.child_session_id` when the agent's payload
/// does not name the child session.
#[must_use]
pub fn subagent_child_id(session_row_id: &str, turn_ordinal: u32, event_ordinal: u32) -> String {
    tagged(
        "subagent",
        &[
            session_row_id,
            &turn_ordinal.to_string(),
            &event_ordinal.to_string(),
        ],
    )
}

/// A `turn_boundary_snapshot.snapshot_id` — one per `(session, ordinal, boundary)`.
#[must_use]
pub fn turn_boundary_snapshot_id(
    session_row_id: &str,
    turn_ordinal: u32,
    boundary: &str,
) -> String {
    tagged(
        "turn_boundary",
        &[session_row_id, &turn_ordinal.to_string(), boundary],
    )
}

/// A `turn_diff_hunk.hunk_id` — content-addressed over the hunk's coordinates.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn turn_diff_hunk_id(
    session_row_id: &str,
    turn_ordinal: u32,
    edit_kind: &str,
    path: &str,
    old_start: u32,
    old_len: u32,
    new_start: u32,
    new_len: u32,
) -> String {
    tagged(
        "turn_diff_hunk",
        &[
            session_row_id,
            &turn_ordinal.to_string(),
            edit_kind,
            path,
            &old_start.to_string(),
            &old_len.to_string(),
            &new_start.to_string(),
            &new_len.to_string(),
        ],
    )
}

/// An `ingest_watermark.watermark_id` — one per `(branch, tip)`.
#[must_use]
pub fn watermark_id(branch: &str, branch_tip: &str) -> String {
    tagged("ingest_watermark", &[branch, branch_tip])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivations_are_deterministic_and_domain_separated() {
        // Deterministic: same inputs, same id.
        assert_eq!(
            session_row_id("claude-code", "native-1"),
            session_row_id("claude-code", "native-1")
        );
        // Agent-namespaced: same native id under two agents differs.
        assert_ne!(
            session_row_id("claude-code", "native-1"),
            session_row_id("codex", "native-1")
        );
        // Domain-separated: structurally identical inputs, different relations.
        let s = session_row_id("claude-code", "native-1");
        assert_ne!(model_call_id(&s, 1, 2), tool_call_row_id(&s, 1, 2));
        assert_ne!(model_call_id(&s, 1, 2), subagent_child_id(&s, 1, 2));
        // Boundary ids separate start from end.
        assert_ne!(
            turn_boundary_snapshot_id(&s, 3, "start"),
            turn_boundary_snapshot_id(&s, 3, "end")
        );
    }

    #[test]
    fn no_delimiter_ambiguity_between_adjacent_parts() {
        // "ab"+"c" must not collide with "a"+"bc" (NUL-delimited parts).
        assert_ne!(session_row_id("ab", "c"), session_row_id("a", "bc"));
    }
}

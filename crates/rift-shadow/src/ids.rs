//! The shared id-derivation module: every normalized capture row is
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
    rift_core::capture_session_row_id(agent, native_session)
}

/// A `main_context_observation.observation_id`, anchored to canonical hook
/// coordinates so redelivery is a set-union no-op.
#[must_use]
pub fn context_observation_id(
    session_row_id: &str,
    verb: &str,
    turn_ordinal: u32,
    payload_sha256: &str,
) -> String {
    tagged(
        "main_context_observation",
        &[
            session_row_id,
            verb,
            &turn_ordinal.to_string(),
            payload_sha256,
        ],
    )
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

/// A read-shaped tool call's OWN `tool_call.tool_call_id`, content-addressed
/// over the fact — `(session, turn, path, read-time hash)` — never the shared
/// `(turn, event_ordinal)` slot, whose ordinals restart per hook and collide
/// across tools within a turn. The projector mints it; the hook that owes the
/// read's deferred token count addresses the same row by it.
#[must_use]
pub fn read_call_row_id(
    session_row_id: &str,
    turn_ordinal: u32,
    rel_path: &str,
    content_sha256: &str,
) -> String {
    crate::sha256_hex(&[
        b"rift.read-call.v1\0",
        session_row_id.as_bytes(),
        b"\0",
        turn_ordinal.to_string().as_bytes(),
        b"\0",
        rel_path.as_bytes(),
        b"\0",
        content_sha256.as_bytes(),
    ])
}

/// A `tool_result_tokens.tool_result_id`, content-addressed over the fact —
/// `(session, turn, tool name, the SHA-256 of the executed call's arguments)`.
///
/// Addressed over what the call WAS rather than over an id the client minted,
/// because only one of the supported families publishes one: Claude Code
/// carries `tool_use_id` and Codex carries nothing at all, so a row keyed on
/// it would exist for one family and not for the others — and the figure this
/// keys is a denominator, which must be comparable across them.
///
/// Every field NUL-separated, exactly as [`read_call_row_id`] separates its
/// own, so no two of them can spell a third.
#[must_use]
pub fn tool_result_row_id(
    session_row_id: &str,
    turn_ordinal: u32,
    tool_name: &str,
    payload_sha256: &str,
) -> String {
    crate::sha256_hex(&[
        b"rift.tool-result.v1\0",
        session_row_id.as_bytes(),
        b"\0",
        turn_ordinal.to_string().as_bytes(),
        b"\0",
        tool_name.as_bytes(),
        b"\0",
        payload_sha256.as_bytes(),
    ])
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

/// A `session_worktree.observation_id` — content-addressed over the observed
/// STATE, never the time.
///
/// `sha256("session_worktree\0" ‖ f(session_id) ‖ f(worktree_token) ‖ f(branch)
/// ‖ f(head_oid) ‖ f(subagent_id))` with `f(Some(v)) = "1\0" ‖ v ‖ "\0"` and
/// `f(None) = "0\0"`.
///
/// **Absence is encoded, not omitted**, which is the whole point of not reusing
/// this module's `tagged` helper. Three of the five fields are nullable, and with plain NUL
/// separation `(branch = "a", head = None)` and `(branch = None, head = "a")`
/// hash the same bytes — one row where there are two facts, in an append-only
/// table nothing can correct. The presence byte makes the two preimages
/// different lengths as well as different bytes. It is the discipline
/// `pack_key_for` and the agent id derivation already use, spelled out here
/// because this is the first capture relation with three optional key columns.
///
/// Because the id is a function of the state alone, a redelivered hook, a
/// doctor sweep and a rebuild all derive the same id and `INSERT OR IGNORE`
/// collapses them; a session that MOVES gets a second row, which is the
/// history a worktree move deserves.
#[must_use]
pub fn session_worktree_observation_id(
    session_row_id: &str,
    worktree_token: &str,
    branch: Option<&str>,
    head_oid: Option<&str>,
    subagent_id: Option<&str>,
) -> String {
    fn present<'a>(parts: &mut Vec<&'a [u8]>, value: Option<&'a str>) {
        match value {
            Some(v) => {
                parts.push(b"1\0");
                parts.push(v.as_bytes());
                parts.push(b"\0");
            }
            None => parts.push(b"0\0"),
        }
    }
    let mut parts: Vec<&[u8]> = vec![b"session_worktree\0"];
    present(&mut parts, Some(session_row_id));
    present(&mut parts, Some(worktree_token));
    present(&mut parts, branch);
    present(&mut parts, head_oid);
    present(&mut parts, subagent_id);
    sha256_hex(&parts)
}

/// An `imported_checkpoint.import_id` — one per `(source_id, source_layout)`.
/// The layout is part of the key, not just the id: a `sharded-branch` id and a
/// `per-checkpoint-refs` id are drawn from disjoint alphabets in practice, but
/// nothing enforces that at this layer, and domain separation should not rest
/// on an accident of two other modules. Content-addressed so a re-run of
/// `rift import --from-legacy` over an unchanged shard writes the same row id
/// (`INSERT OR IGNORE` makes the re-run a no-op, matching every other
/// bridge-derived row).
#[must_use]
pub fn imported_checkpoint_id(source_id: &str, source_layout: &str) -> String {
    tagged("imported_checkpoint", &[source_id, source_layout])
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

    #[test]
    fn imported_checkpoint_ids_are_deterministic_and_layout_separated() {
        // A re-import of the same shard, from the same layout, is the same id
        // (the idempotence a set-union re-run needs).
        assert_eq!(
            imported_checkpoint_id("ab34cd56ef78", "sharded-branch"),
            imported_checkpoint_id("ab34cd56ef78", "sharded-branch")
        );
        // The same bare id under the OTHER layout is a different row: layout
        // is part of the key, not an incidental fact about the id's alphabet.
        assert_ne!(
            imported_checkpoint_id("ab34cd56ef78", "sharded-branch"),
            imported_checkpoint_id("ab34cd56ef78", "per-checkpoint-refs")
        );
        // Domain-separated from every other relation's id.
        assert_ne!(
            imported_checkpoint_id("ab34cd56ef78", "sharded-branch"),
            watermark_id("ab34cd56ef78", "sharded-branch")
        );
    }

    /// The reason for the presence byte, stated as an assertion:
    /// with plain NUL separation these two observations — one on branch `a`
    /// with no readable head, one detached at a commit spelled `a` — would be
    /// the same row in a table that can never be corrected.
    #[test]
    fn an_observation_id_encodes_absence_so_two_optional_fields_cannot_swap() {
        let branch_only =
            session_worktree_observation_id("s", "0123456789ab", Some("a"), None, None);
        let head_only = session_worktree_observation_id("s", "0123456789ab", None, Some("a"), None);
        assert_ne!(branch_only, head_only);
        // Absence is not the empty string either: "no branch" and "a branch
        // whose name is empty" are different claims.
        assert_ne!(
            session_worktree_observation_id("s", "0123456789ab", None, None, None),
            session_worktree_observation_id("s", "0123456789ab", Some(""), None, None)
        );
        // The same observed state is the same row — what makes a redelivered
        // hook, a sweep and a rebuild collapse to one.
        assert_eq!(
            session_worktree_observation_id("s", "0123456789ab", Some("main"), Some("ff"), None),
            session_worktree_observation_id("s", "0123456789ab", Some("main"), Some("ff"), None)
        );
        // A subagent's observation is its own row under the parent's session.
        assert_ne!(
            session_worktree_observation_id("s", "0123456789ab", Some("main"), Some("ff"), None),
            session_worktree_observation_id(
                "s",
                "0123456789ab",
                Some("main"),
                Some("ff"),
                Some("agent-1")
            )
        );
        // Domain-separated from every other relation's id.
        assert_ne!(
            session_worktree_observation_id("s", "0123456789ab", None, None, None),
            watermark_id("s", "0123456789ab")
        );
    }
}

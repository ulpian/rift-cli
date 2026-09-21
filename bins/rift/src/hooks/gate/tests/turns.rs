//! The turn a journal line carries, and the context window it is written
//! under.

use rift_agent::window_journal::Line;

use super::fixtures::*;

/// A subagent's hooks arrive under its parent's session id and carry their
/// own agent id. Two windows, two journals: the parent must not be told it
/// holds what only the child was handed.
#[test]
fn a_subagent_records_under_its_own_context_window() {
    let (tmp, file) = checkout(50);
    let mut payload = read_call(&file, None, Some(10));
    payload["agent_id"] = serde_json::json!("child-1");
    gate_observing(
        &hook("claude-code", "PreToolUse", "s-3", payload),
        tmp.path(),
    );

    assert!(
        lines_of(tmp.path(), "s-3", None).is_empty(),
        "the parent window holds nothing"
    );
    assert_eq!(
        lines_of(tmp.path(), "s-3", Some("child-1")).len(),
        1,
        "the child's own window holds the read"
    );
}

/// ★ **Two reads on either side of a turn boundary are recorded in
/// different turns**, on a family whose wire carries no turn at all.
///
/// The ordinal comes off the session cache the ingest layer advances at
/// the boundary, which is a file read and not a store — the only kind of
/// lookup this path is allowed. Before this, every line of every alpha
/// family said turn 0, and the two figures counted per turn (a retry
/// inside one turn, and what a window held before it lost it) were flat.
#[test]
fn two_reads_across_a_prompt_boundary_are_recorded_in_different_turns() {
    let (tmp, file) = checkout(40);
    let here = tmp.path();

    open_turn(here, "s-turn", 1);
    gate_observing(
        &native_hook(
            "claude-code",
            "PreToolUse",
            "s-turn",
            read_call(&file, Some(1), Some(5)),
        ),
        here,
    );
    // The next prompt opens the next ordinal, and the read after it is a
    // read of that turn.
    open_turn(here, "s-turn", 2);
    gate_observing(
        &native_hook(
            "claude-code",
            "PreToolUse",
            "s-turn",
            read_call(&file, Some(10), Some(5)),
        ),
        here,
    );

    let turns: Vec<u32> = lines_of(here, "s-turn", None)
        .iter()
        .map(Line::turn)
        .collect();
    assert_eq!(
        turns,
        vec![1, 2],
        "each read is recorded in the turn it was made in"
    );
}

/// And a window with no cache to read from is turn 0 — the number every
/// line carried before the boundary was consulted at all. A decision is
/// never made of a turn, so the miss costs a distinction and nothing else.
#[test]
fn a_window_with_no_boundary_on_record_is_recorded_in_turn_zero() {
    let (tmp, file) = checkout(40);
    let here = tmp.path();
    gate_observing(
        &native_hook(
            "claude-code",
            "PreToolUse",
            "s-turnless",
            read_call(&file, Some(1), Some(5)),
        ),
        here,
    );
    let lines = lines_of(here, "s-turnless", None);
    let [line] = lines.as_slice() else {
        panic!("one read, one line");
    };
    assert_eq!(line.turn(), 0);
}

/// An envelope that states its own turn is trusted verbatim, and the cache
/// beside it is never consulted: the wire is the more specific fact.
#[test]
fn a_stated_turn_wins_over_the_boundary_on_record() {
    let (tmp, file) = checkout(40);
    let here = tmp.path();
    open_turn(here, "s-stated", 9);
    gate_observing(
        &hook(
            "claude-code",
            "PreToolUse",
            "s-stated",
            read_call(&file, Some(1), Some(5)),
        ),
        here,
    );
    let lines = lines_of(here, "s-stated", None);
    let [line] = lines.as_slice() else {
        panic!("one read, one line");
    };
    assert_eq!(line.turn(), 4, "the envelope's ordinal, not the cache's");
}

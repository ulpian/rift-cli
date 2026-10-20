//! What a context window stops holding, and what a result for a call the
//! window declined records.

use std::path::Path;

use rift_agent::window_journal::{CallFact, Interval, Line};

use super::fixtures::*;
use crate::hooks::gate::members::deny_channel;

/// The signals each family documents for "the context stopped holding what
/// it was given", with the reason each one is journaled under. Antigravity
/// publishes none at all, which is the fact that keeps it in observe.
fn documented_eviction_signals(
) -> Vec<(&'static str, &'static str, serde_json::Value, &'static str)> {
    vec![
        (
            "claude-code",
            "PreCompact",
            serde_json::json!({}),
            "pre_compact",
        ),
        (
            "claude-code",
            "SessionStart",
            serde_json::json!({"source": "compact"}),
            "compact",
        ),
        (
            "claude-code",
            "SessionStart",
            serde_json::json!({"source": "clear"}),
            "clear",
        ),
        (
            "claude-code",
            "SessionStart",
            serde_json::json!({"source": "resume"}),
            "resume",
        ),
        ("codex", "PreCompact", serde_json::json!({}), "pre_compact"),
        (
            "codex",
            "PostCompact",
            serde_json::json!({}),
            "post_compact",
        ),
    ]
}

/// A window holding a five-line read of `src.rs`, and the revision it holds.
///
/// Seeded rather than read through the gate, because which calls a family
/// records a read through is not what this is about: Codex reads through its
/// shell, and its signals answer for whatever its window was handed.
fn window_holding_a_read(
    here: &Path,
) -> (crate::window_journal::WindowJournal, rift_core::Sha256Id) {
    let held = revision(here, "src.rs");
    let window = crate::window_journal::WindowJournal::open(here, "s-4", None);
    window
        .append(&Line::Read {
            turn: 1,
            path: "src.rs".to_owned(),
            hash: held,
            range: Interval::new(1, 5).expect("a run"),
            total_lines: 20,
            at_ms: 1,
        })
        .expect("the window holds a read");
    (window, held)
}

/// Each signal a family documents for "the context stopped holding what it
/// was given" empties the window, and nothing else does.
#[test]
fn each_documented_eviction_signal_empties_the_window_and_no_other_does() {
    for (agent, verb, payload, reason) in documented_eviction_signals() {
        let (tmp, _file) = checkout(20);
        let here = tmp.path();
        let (window, held) = window_holding_a_read(here);
        assert!(
            !window.load().journal.coverage("src.rs", held).is_empty(),
            "{agent}: it holds the read before the signal arrives"
        );

        gate_observing(&hook(agent, verb, "s-4", payload), here);

        let lines = lines_of(here, "s-4", None);
        let last = lines.last().expect("a line");
        assert_eq!(last.kind(), "evict", "{agent} {verb}: {lines:?}");
        assert!(
            last.to_json_line().contains(reason),
            "{agent} {verb}: the signal is named, not merged into one token"
        );
        assert!(
            window.load().journal.coverage("src.rs", held).is_empty(),
            "{agent} {verb}: and what was read before it is no longer held"
        );
        assert!(
            !window
                .load()
                .journal
                .recorded_coverage("src.rs", held)
                .is_empty(),
            "{agent} {verb}: while staying on the record, as the map of what it lost"
        );
    }
}

/// And nothing else does. A signal a family does not publish is not one
/// this reads into it: an eviction that never happened empties a window
/// that is still holding what it read, which costs every saving after it.
#[test]
fn a_signal_a_family_does_not_publish_empties_nothing() {
    let quiet: Vec<(&str, &str, serde_json::Value)> = vec![
        (
            "claude-code",
            "SessionStart",
            serde_json::json!({"source": "startup"}),
        ),
        (
            "claude-code",
            "SessionStart",
            serde_json::json!({"source": "fork"}),
        ),
        ("claude-code", "SessionStart", serde_json::json!({})),
        ("claude-code", "PostCompact", serde_json::json!({})),
        ("antigravity", "PreCompact", serde_json::json!({})),
        (
            "antigravity",
            "SessionStart",
            serde_json::json!({"source": "clear"}),
        ),
    ];
    for (agent, verb, payload) in quiet {
        let tmp = tempfile::tempdir().expect("a work tree");
        gate_observing(&hook(agent, verb, "s-5", payload), tmp.path());
        assert!(
            lines_of(tmp.path(), "s-5", None).is_empty(),
            "{agent} {verb}: no signal this family documents, no line"
        );
    }
}

/// A client too old to act on the channel a decision was sent on runs the
/// call anyway, and its *result* arrives for a call the journal declined.
/// The bytes reached the model, so the window is recorded as holding them,
/// and the result is joined to the decline by the call's own id.
#[test]
fn a_result_for_a_declined_read_records_the_read_and_leaves_the_pair() {
    let (tmp, file) = checkout(40);
    let here = tmp.path();
    let held = revision(here, "src.rs");
    let window = crate::window_journal::WindowJournal::open(here, "s-6", None);
    window
        .append(&Line::Deny {
            turn: 1,
            path: "src.rs".to_owned(),
            hash: held,
            range: Interval::new(1, 10).expect("a run"),
            at_ms: 1,
        })
        .expect("a decline");
    for fact in [
        CallFact::Planned,
        CallFact::Emitted {
            wrote: true,
            exit: 0,
        },
    ] {
        window
            .append(&Line::Call {
                turn: 1,
                call: Some("toolu_6".to_owned()),
                fact,
                at_ms: 1,
            })
            .expect("a call fact");
    }

    let mut payload = read_call(&file, None, Some(10));
    payload["tool_response"] = serde_json::json!({ "type": "text" });
    payload["tool_use_id"] = serde_json::json!("toolu_6");
    gate_observing(
        &hook("claude-code", "PostToolUse", "s-6", payload.clone()),
        here,
    );

    let lines = lines_of(here, "s-6", None);
    assert_eq!(
        lines.iter().map(Line::kind).collect::<Vec<_>>(),
        vec!["deny", "call", "call", "call", "read"],
        "the decline stands, the result is joined to it, and the read is beside it"
    );
    assert_eq!(
        crate::window_journal::ineffective_denies(&window.load().journal).ineffective,
        1,
        "and the joined call is what a diagnostic counts"
    );

    // A second result for the same lines adds nothing: the decline has
    // been answered, so there is no longer a decline to contradict.
    gate_observing(&hook("claude-code", "PostToolUse", "s-6", payload), here);
    assert_eq!(lines_of(here, "s-6", None).len(), 5);
}

/// The ordinary result — of a read nobody declined — leaves the journal
/// alone. The read was recorded before the call ran, and recording it
/// again would say the window was handed the lines twice.
#[test]
fn a_result_for_a_read_nobody_declined_records_nothing() {
    let (tmp, file) = checkout(40);
    let here = tmp.path();
    let payload = read_call(&file, None, Some(10));
    gate_observing(
        &hook("claude-code", "PreToolUse", "s-7", payload.clone()),
        here,
    );
    gate_observing(&hook("claude-code", "PostToolUse", "s-7", payload), here);

    let lines = lines_of(here, "s-7", None);
    assert_eq!(
        lines.iter().map(Line::kind).collect::<Vec<_>>(),
        vec!["read"]
    );
    assert_eq!(
        crate::window_journal::ineffective_denies(&lines_journal(here, "s-7")),
        crate::window_journal::DeclineCalls::default()
    );
}

/// Whatever a family's channel says, the lever is what decides. A
/// confirmed channel is permission to be understood, not permission to
/// act: while nothing has read a setting, the mode here is observe and a
/// read of lines the window already holds is recorded and let through.
#[test]
fn a_confirmed_channel_declines_nothing_while_the_lever_is_in_observe() {
    assert_eq!(
        observing().read_deny,
        rift_recall_policy::Mode::Observe,
        "the mode a repository that has written no setting is left in"
    );
    let confirmed = |agent: &str| {
        matches!(
            deny_channel(&rift_core::AgentId::new(agent), "PreToolUse"),
            Some(rift_agent::respond::Channel::DenyWithReason {
                confirmed: true,
                ..
            })
        )
    };
    assert!(
        confirmed("claude-code"),
        "a family whose channel is confirmed is what makes this test say something"
    );

    let (tmp, file) = checkout(60);
    let call = hook(
        "claude-code",
        "PreToolUse",
        "s-8",
        read_call(&file, Some(1), Some(30)),
    );
    gate_observing(&call, tmp.path());
    gate_observing(&call, tmp.path());
    assert_eq!(
        lines_of(tmp.path(), "s-8", None)
            .iter()
            .map(Line::kind)
            .collect::<Vec<_>>(),
        vec!["read", "observe"],
        "the repeat is recorded as what a decline would be for, and allowed"
    );

    for agent in ["claude-code", "codex", "antigravity"] {
        assert!(
            deny_channel(&rift_core::AgentId::new(agent), "PostToolUse").is_none(),
            "{agent}: a decline is a thing said before a call, not after it"
        );
    }
}

// --- declining, once, with a reason -------------------------------------

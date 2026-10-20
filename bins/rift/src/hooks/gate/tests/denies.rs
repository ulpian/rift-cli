//! The decline itself: the exact sites a verification read is answered with,
//! the repeats that run anyway, and the basis the gate refuses to open a store
//! for.

use std::path::Path;

use rift_agent::window_journal::{CallFact, Line};
use rift_shadow::hooks::AdapterRegistry;

use super::fixtures::*;
use crate::hooks::ingest::edit_record;

/// An `Edit` as the pinned fields spell one.
fn edit_of(path: &Path, old: &str, new: &str) -> serde_json::Value {
    serde_json::json!({
        "tool_name": "Edit",
        "tool_input": {
            "file_path": path.display().to_string(),
            "old_string": old,
            "new_string": new,
            "replace_all": false,
        },
    })
}

/// The window reads a file in full, edits it, and the file on disk becomes
/// what the edit left behind — the state every test below starts from.
fn read_then_edit(
    here: &Path,
    session: &str,
    file: &Path,
    before: &str,
    after: &str,
    edit: (&str, &str),
    levers: &rift_recall_policy::Effective,
) {
    std::fs::write(file, before).expect("the file");
    gate_under(
        &hook("claude-code", "PreToolUse", session, whole(file)),
        here,
        levers,
    );
    std::fs::write(file, after).expect("the edit lands");
    edit_record(
        &AdapterRegistry::with_builtin(),
        &hook(
            "claude-code",
            "PostToolUse",
            session,
            edit_of(file, edit.0, edit.1),
        ),
        &here.join(".git"),
        here,
        levers,
    );
}

/// The largest pool this whole mechanism exists for: the agent read a file,
/// edited it, and reads it back to see what it just wrote. The bytes it is
/// asking for came out of its own payload, so the honest answer names the
/// places that changed, in the numbering it is about to see, and lets the
/// call run the moment it asks again.
#[test]
fn a_verification_read_after_a_full_read_and_an_edit_is_denied_once_with_exact_sites() {
    let tmp = tempfile::tempdir().expect("a work tree");
    let here = tmp.path();
    let file = here.join("src.rs");
    let enforce = levers_for("claude-code", rift_recall_policy::Mode::Enforce);
    read_then_edit(
        here,
        "s-delta",
        &file,
        "a\nold\nc\n",
        "a\nnew\nnewer\nc\n",
        ("old\n", "new\nnewer\n"),
        &enforce,
    );

    let verify = hook("claude-code", "PreToolUse", "s-delta", whole(&file));
    let declined = gate_under(&verify, here, &enforce);
    let reason = declined.deny_reason.expect("a decline with a reason");
    assert!(
        reason.contains("changed only at lines 2-3"),
        "the places, in the numbering of the bytes it is about to see: {reason}"
    );
    assert!(
        reason.contains("(+1 lines)"),
        "and by how much the file moved: {reason}"
    );
    assert!(
        reason.contains("Read lines 1-4"),
        "and a window worth reading instead, inside the file: {reason}"
    );
    assert!(
        !reason.contains("newer"),
        "identifiers and numbers, never a byte of the file: {reason}"
    );

    // Once. The model that answers a decline by asking again is answered.
    let again = gate_under(&verify, here, &enforce);
    assert_eq!(again.deny_reason, None, "the repeat is honoured");
    assert_eq!(
        lines_of(here, "s-delta", None)
            .iter()
            .map(Line::kind)
            .collect::<Vec<_>>(),
        vec!["read", "edit", "deny", "read"],
        "declined once, then handed the bytes and recorded as holding them"
    );
}

/// A decline's planned and emitted facts go to the window's call sidecar
/// under the client's own call id, and the window's journal — what the
/// window holds — gets none of them.
#[test]
fn a_declined_call_is_recorded_in_the_call_sidecar_by_its_id_and_never_in_the_journal() {
    let tmp = tempfile::tempdir().expect("a work tree");
    let here = tmp.path();
    let file = here.join("src.rs");
    let enforce = levers_for("claude-code", rift_recall_policy::Mode::Enforce);
    read_then_edit(
        here,
        "s-calls",
        &file,
        "a\nold\nc\n",
        "a\nnew\nc\n",
        ("old\n", "new\n"),
        &enforce,
    );
    let mut payload = whole(&file);
    payload["tool_use_id"] = serde_json::json!("toolu_9");
    let verify = hook("claude-code", "PreToolUse", "s-calls", payload);
    let declined = gate_under(&verify, here, &enforce);
    assert!(
        declined.deny_reason.is_some(),
        "the verification is declined"
    );
    let receipt = declined.receipt.expect("a decline carries its receipt");
    crate::hooks::gate::record_emission(&receipt, true, 0);

    let sidecar = crate::window_journal::CallSidecar::open(here, "s-calls", None);
    let facts: Vec<_> = sidecar
        .load()
        .lines()
        .iter()
        .filter_map(|line| match line {
            Line::Call { call, fact, .. } => Some((call.clone(), *fact)),
            _ => None,
        })
        .collect();
    assert_eq!(
        facts,
        vec![
            (Some("toolu_9".to_owned()), CallFact::Planned),
            (
                Some("toolu_9".to_owned()),
                CallFact::Emitted {
                    wrote: true,
                    exit: 0
                }
            ),
        ]
    );
    assert!(
        lines_of(here, "s-calls", None)
            .iter()
            .all(|line| !matches!(line, Line::Call { .. })),
        "the journal holds no call fact"
    );
}

/// Reading exactly the lines that changed is the right thing to do, so it
/// is never the thing declined — the narrow read is the behaviour the
/// frame asks for, and it must not be answered with another decline.
#[test]
fn a_read_inside_the_edited_site_is_allowed() {
    let tmp = tempfile::tempdir().expect("a work tree");
    let here = tmp.path();
    let file = here.join("src.rs");
    let enforce = levers_for("claude-code", rift_recall_policy::Mode::Enforce);
    read_then_edit(
        here,
        "s-narrow",
        &file,
        "a\nold\nc\n",
        "a\nnew\nnewer\nc\n",
        ("old\n", "new\nnewer\n"),
        &enforce,
    );

    let narrow = gate_under(
        &hook(
            "claude-code",
            "PreToolUse",
            "s-narrow",
            read_call(&file, Some(2), Some(2)),
        ),
        here,
        &enforce,
    );
    assert_eq!(narrow.deny_reason, None, "the changed lines themselves");
}

/// ★ The floor the policy sets is the floor the gate uses.
///
/// Every other walk in this module holds it at one so it does not stand in
/// the way of the property under test; this one leaves the shipped value
/// alone and re-reads a three-line file, which is far under it. The repeat
/// is covered, the window holds it and enforcing is on — so the only
/// reason the call runs is the trade, and the observation is still on the
/// record. A wiring that dropped the floor between the policy and the
/// decision would decline here.
#[test]
fn a_repeat_under_the_shipped_line_floor_is_observed_and_runs() {
    let tmp = tempfile::tempdir().expect("a work tree");
    let here = tmp.path();
    let file = here.join("src.rs");
    let mut enforce = levers_for("claude-code", rift_recall_policy::Mode::Enforce);
    enforce.min_slice_lines = rift_recall_policy::DEFAULT_MIN_SLICE_LINES;
    std::fs::write(&file, "a\nb\nc\n").expect("the file");
    gate_under(
        &hook("claude-code", "PreToolUse", "s-floor", whole(&file)),
        here,
        &enforce,
    );

    let repeat = gate_under(
        &hook("claude-code", "PreToolUse", "s-floor", whole(&file)),
        here,
        &enforce,
    );
    assert_eq!(
        repeat.deny_reason, None,
        "three lines is under the floor, so the repeat runs"
    );
    assert_eq!(
        lines_of(here, "s-floor", None)
            .iter()
            .map(Line::kind)
            .collect::<Vec<_>>(),
        vec!["read", "observe"],
        "and the decision the floor held back is still recorded"
    );
}

/// The gate has one basis for a delta and it is the agent's own edit line.
/// A change whose places do not follow from the payload leaves no such
/// line, and reaching for a stored one would open a store on the path that
/// must not — so the read is simply allowed.
#[test]
fn an_unreachable_basis_allows() {
    let tmp = tempfile::tempdir().expect("a work tree");
    let here = tmp.path();
    let file = here.join("src.rs");
    let enforce = levers_for("claude-code", rift_recall_policy::Mode::Enforce);
    std::fs::write(&file, "a\nold\nc\n").expect("the file");
    gate_under(
        &hook("claude-code", "PreToolUse", "s-basis", whole(&file)),
        here,
        &enforce,
    );
    // Something other than this window changed the file: a rebase, a
    // formatter, another agent. No edit line, and no basis.
    std::fs::write(&file, "a\nsomething else\nc\n").expect("the change");

    let after = gate_under(
        &hook("claude-code", "PreToolUse", "s-basis", whole(&file)),
        here,
        &enforce,
    );
    assert_eq!(
        after.deny_reason, None,
        "nothing exact to say, so nothing said"
    );
    assert_eq!(
        lines_of(here, "s-basis", None)
            .iter()
            .map(Line::kind)
            .collect::<Vec<_>>(),
        vec!["read", "read"]
    );
}

/// The invariant the whole design rests on, asserted where it can actually
/// be broken: a decline — the most expensive thing this path does, frame
/// and all — is composed in a checkout that has no store of any kind, and
/// leaves none behind.
#[test]
fn the_gate_path_never_opens_a_store_for_a_basis() {
    let tmp = tempfile::tempdir().expect("a work tree");
    let here = tmp.path();
    let file = here.join("src.rs");
    let enforce = levers_for("claude-code", rift_recall_policy::Mode::Enforce);
    read_then_edit(
        here,
        "s-store",
        &file,
        "a\nold\nc\n",
        "a\nnew\nnewer\nc\n",
        ("old\n", "new\nnewer\n"),
        &enforce,
    );
    let declined = gate_under(
        &hook("claude-code", "PreToolUse", "s-store", whole(&file)),
        here,
        &enforce,
    );
    assert!(declined.deny_reason.is_some(), "the decline still happens");

    let mut found: Vec<String> = Vec::new();
    fn walk(dir: &Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "db") {
                found.push(path.display().to_string());
            }
        }
    }
    walk(here, &mut found);
    assert!(
        found.is_empty(),
        "a decline that needed a database would have made one: {found:?}"
    );
}

/// A `view_file` as the pinned fields spell one.
fn view_of(path: &Path, start: u32, end: u32) -> serde_json::Value {
    serde_json::json!({
        "toolCall": {
            "name": "view_file",
            "args": {
                "AbsolutePath": path.display().to_string(),
                "StartLine": start,
                "EndLine": end,
            },
        },
    })
}

/// Retry telemetry never demotes Antigravity below repository-wide
/// enforcement once its reason channel is pinned.
#[test]
fn repository_wide_enforce_reaches_antigravity_despite_retry_telemetry() {
    let (tmp, file) = checkout(60);
    let here = tmp.path();
    let call = hook("antigravity", "PreToolUse", "s-agy", view_of(&file, 0, 30));

    let mut wide = rift_recall_policy::ServePolicy::default();
    wide.interception.mode = rift_recall_policy::Mode::Enforce;
    let wide = rift_recall_policy::resolve(
        &wide,
        "antigravity",
        crate::recall::family_capability(&rift_core::AgentId::ANTIGRAVITY),
    );
    assert_eq!(wide.read_deny, rift_recall_policy::Mode::Enforce);
    assert_eq!(wide.read_deny_cap, None);
    gate_under(&call, here, &wide);
    assert!(
        gate_under(&call, here, &wide).deny_reason.is_some(),
        "the unchanged repeat is declined despite its retry history"
    );
}

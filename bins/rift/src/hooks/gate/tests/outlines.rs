//! A first read of a large file: what the gate decides when the file's
//! outline is bound for its bytes, when it is not, when the file is small,
//! and when the read is repeated — all through the hook as it runs.

use super::fixtures::*;
use crate::hooks::gate::outline_cache::{write_outline_cache, CachedDefinition, CachedOutline};

/// Bind an outline of `rel` as the worker would: the cache file for the
/// bytes the checkout holds now, under the family's common directory.
fn bind(work_tree: &std::path::Path, rel: &str, lines: u32) {
    let outline = CachedOutline {
        markdown: false,
        lines,
        bytes: 9_000,
        definitions: 2,
        whole_tokens: Some(4_000),
        entries: vec![
            CachedDefinition {
                kind: rift_recall_serve::DefinitionKind::Function,
                name: "first".to_owned(),
                first_line: 1,
                last_line: 200,
                tokens: Some(1_500),
            },
            CachedDefinition {
                kind: rift_recall_serve::DefinitionKind::Function,
                name: "second".to_owned(),
                first_line: 201,
                last_line: lines,
                tokens: Some(2_500),
            },
        ],
    };
    let sha = revision(work_tree, rel).to_hex();
    write_outline_cache(&work_tree.join(".git"), &sha, &outline).unwrap();
}

fn first_contact_rows(work_tree: &std::path::Path) -> Vec<crate::agent_queue::ServeAudit> {
    audits(work_tree)
        .into_iter()
        .filter(|row| row.tag.as_deref() == Some(rift_recall_serve::TAG_FIRST_CONTACT))
        .collect()
}

/// ★ A first whole read of a large file whose outline is bound is an
/// observed first contact: it runs, the window is credited with what it
/// was handed, and the row it owes names the slice under the tag of its
/// class — with no frame, because nothing was sent.
#[test]
fn a_first_read_of_a_large_bound_file_is_an_observed_outline() {
    let (tmp, file) = checkout(900);
    let here = tmp.path();
    bind(here, "src.rs", 900);
    let call = hook(
        "claude-code",
        "PreToolUse",
        "s-fc",
        read_call(&file, None, None),
    );
    let outcome = gate_observing(&call, here);
    assert_eq!(outcome.deny_reason, None, "observe sends nothing");
    assert!(outcome.journaled);
    let kinds: Vec<&str> = lines_of(here, "s-fc", None)
        .iter()
        .map(rift_agent::window_journal::Line::kind)
        .collect();
    assert_eq!(kinds, ["read"], "the read ran and is coverage");
    let rows = first_contact_rows(here);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].decision,
        crate::agent_queue::ServeAuditKind::ReadRepeatObserved
    );
    assert!(rows[0].frame.is_empty());
    assert_eq!(
        rows[0].basis.as_ref().and_then(|b| b.lines),
        Some(crate::agent_queue::AuditLines { lo: 1, hi: 900 })
    );
}

/// A first page is a first contact too; a read from the middle never is.
#[test]
fn a_first_page_is_outlined_and_a_mid_file_range_is_not() {
    let (tmp, file) = checkout(900);
    let here = tmp.path();
    bind(here, "src.rs", 900);
    let mid = hook(
        "claude-code",
        "PreToolUse",
        "s-mid",
        read_call(&file, Some(300), Some(200)),
    );
    gate_observing(&mid, here);
    assert!(
        first_contact_rows(here).is_empty(),
        "a named range runs as asked"
    );
    let page = hook(
        "claude-code",
        "PreToolUse",
        "s-page",
        read_call(&file, Some(1), Some(200)),
    );
    gate_observing(&page, here);
    let rows = first_contact_rows(here);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].basis.as_ref().and_then(|b| b.lines),
        Some(crate::agent_queue::AuditLines { lo: 1, hi: 200 })
    );
}

/// ★ No outline bound for these bytes is a miss, and a miss is a read that
/// runs with nothing recorded about an outline; nor is a small file
/// outlined however bound it is.
#[test]
fn a_miss_or_a_small_file_is_a_plain_read() {
    let (tmp, file) = checkout(900);
    let here = tmp.path();
    let call = hook(
        "claude-code",
        "PreToolUse",
        "s-miss",
        read_call(&file, None, None),
    );
    let outcome = gate_observing(&call, here);
    assert_eq!(
        outcome,
        crate::hooks::gate::GateOutcome {
            journaled: false,
            deny_reason: None
        }
    );
    let small = beside(here, "small.rs", 120);
    bind(here, "small.rs", 120);
    gate_observing(
        &hook(
            "claude-code",
            "PreToolUse",
            "s-small",
            read_call(&small, None, None),
        ),
        here,
    );
    assert!(first_contact_rows(here).is_empty());
}

/// ★ Under the default policy an enforcing family's first read is only
/// observed — the `first_contact` key is unwritten, so the family's mode
/// alone answers nothing — and the identical repeat is no first contact: the
/// window holds the file now, so the re-read lever decides it as it decides
/// any repeat.
#[test]
fn under_the_default_policy_an_enforcing_family_only_observes_and_the_repeat_runs() {
    let (tmp, file) = checkout(900);
    let here = tmp.path();
    bind(here, "src.rs", 900);
    let enforce = enforcing();
    assert_eq!(enforce.read_deny, rift_recall_policy::Mode::Enforce);
    assert_eq!(enforce.first_contact, rift_recall_policy::Mode::Observe);
    let call = hook(
        "claude-code",
        "PreToolUse",
        "s-enf",
        read_call(&file, None, None),
    );
    assert_eq!(gate_under(&call, here, &enforce).deny_reason, None);
    // The repeat is no first contact: the window holds the file now, so it
    // is the re-read lever's to decide, and that lever enforces here.
    let repeat = gate_under(&call, here, &enforce)
        .deny_reason
        .unwrap_or_default();
    assert!(repeat.starts_with("[rift] Read skipped:"), "{repeat}");
    assert_eq!(
        first_contact_rows(here).len(),
        1,
        "one first contact per revision"
    );
}

/// ★ With `first_contact` written `enforce` the first read is answered with
/// the bound outline: the frame travels, the row it owes carries it, the
/// window is credited with the outline and not the file, and the identical
/// repeat runs — once answered, the request is honoured.
#[test]
fn a_first_read_is_answered_with_the_outline_once_first_contact_is_written_enforce() {
    let (tmp, file) = checkout(900);
    let here = tmp.path();
    bind(here, "src.rs", 900);
    let mut written = enforcing();
    written.first_contact = rift_recall_policy::Mode::Enforce;
    let call = hook(
        "claude-code",
        "PreToolUse",
        "s-written",
        read_call(&file, None, None),
    );
    let outcome = gate_under(&call, here, &written);
    let frame = outcome.deny_reason.expect("the outline is the answer");
    assert!(frame.starts_with("[rift] Read outlined:"), "{frame}");
    assert!(frame.contains("src.rs"), "{frame}");
    let kinds: Vec<&str> = lines_of(here, "s-written", None)
        .iter()
        .map(rift_agent::window_journal::Line::kind)
        .collect();
    assert_eq!(kinds, ["outline"], "the outline is not coverage");
    let rows = first_contact_rows(here);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].decision,
        crate::agent_queue::ServeAuditKind::ReadIntercepted
    );
    assert_eq!(rows[0].frame, frame);
    assert_eq!(
        gate_under(&call, here, &written).deny_reason,
        None,
        "the identical repeat is allowed"
    );
}

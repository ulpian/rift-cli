//! The read an announcement made unnecessary: what books one, what books it
//! only once, and what a decline over the same slice may still claim.

use std::path::{Path, PathBuf};

use rift_shadow::hooks::{AdapterRegistry, RawHook};

use rift_agent::window_journal::{Interval, Line};

use super::fixtures::*;
use crate::hooks::audit::serve_audit_event;
use crate::hooks::gate::prevented::prevented_record;

/// A read of lines this window was already handed, at the same revision,
/// is what a decline would be for — and while nothing is enforced it is
/// recorded as such and let through.
#[test]
fn a_read_of_lines_already_held_is_observed_and_not_declined() {
    let (tmp, file) = checkout(120);
    let payload = read_call(&file, Some(10), Some(20));
    let call = hook("claude-code", "PreToolUse", "s-1", payload);
    gate_observing(&call, tmp.path());
    gate_observing(&call, tmp.path());

    let lines = lines_of(tmp.path(), "s-1", None);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines.first().map(Line::kind), Some("read"));
    assert_eq!(
        lines.get(1).map(Line::kind),
        Some("observe"),
        "the repeat is what a decline would be for, and nothing was declined"
    );
}

// ------------------------------------------- reads that never happened ---

/// Announce a hold the way an injection does: Rift has told this window
/// it holds these lines of the revision the checkout has now.
fn announce(work_tree: &Path, session: &str, rel: &str, lo: u32, hi: u32) {
    let (blob, _) =
        rift_recall_substrate::worktree::hash_blob_bytes(work_tree, rel).expect("the fixture file");
    crate::window_journal::WindowJournal::open(work_tree, session, None)
        .append(&Line::Hold {
            turn: 4,
            path: rel.to_owned(),
            hash: rift_core::Sha256Id::parse_hex(&blob.content_sha256).expect("a revision"),
            range: Interval::new(lo, hi).expect("a run"),
            at_ms: 1_700_000_000_000,
        })
        .expect("the announcement is recorded");
}

/// An `Edit` of `path`, as the pinned fields spell one.
fn edit_call(path: &Path) -> serde_json::Value {
    serde_json::json!({
        "tool_name": "Edit",
        "tool_input": {
            "file_path": path.display().to_string(),
            "old_string": "line 1\n",
            "new_string": "line one\n",
        }
    })
}

/// A content search over one named file, as the pinned shell grammar
/// spells one.
fn grep_call(path: &Path) -> serde_json::Value {
    serde_json::json!({
        "tool_name": "Bash",
        "tool_input": { "command": format!("grep needle {}", shell_spelling(path)) }
    })
}

fn prevented(raw: &RawHook, work_tree: &Path, effective: &rift_recall_policy::Effective) -> bool {
    prevented_record(
        &AdapterRegistry::with_builtin(),
        raw,
        &work_tree.join(".git"),
        work_tree,
        effective,
    )
}

/// **The lever's whole claim**: Rift said the window holds the file, the
/// window changed the file, and the read it would have made first never
/// happened. One booking, naming the announced lines and no content.
#[test]
fn an_edit_of_a_held_file_without_a_reread_books_one_prevented_read() {
    let (tmp, file) = checkout(120);
    let here = tmp.path();
    announce(here, "s-p1", "src.rs", 1, 120);
    let call = hook("claude-code", "PreToolUse", "s-p1", edit_call(&file));
    assert!(prevented(&call, here, &enforcing()));

    let rows = audits(here);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].decision,
        crate::agent_queue::ServeAuditKind::ReadPrevented
    );
    assert!(rows[0].frame.is_empty(), "nothing was put in front of it");
    let basis = rows[0].basis.as_ref().expect("the bytes to price");
    assert_eq!(basis.rel_path, "src.rs");
    assert_eq!(
        basis.lines,
        Some(crate::agent_queue::AuditLines { lo: 1, hi: 120 })
    );
    assert!(!basis.blob_oid.is_empty(), "the fallback source is named");
    let lines = lines_of(here, "s-p1", None);
    assert_eq!(
        lines.iter().filter(|l| l.kind() == "prevented").count(),
        1,
        "{lines:?}"
    );
}

/// A content search inside an announced file is the same act: the window
/// is handed the matches, never the file, so the read it was told it did
/// not need is a read that did not happen.
#[test]
fn a_grep_naming_a_held_file_books_one_prevented_read() {
    let (tmp, file) = checkout(120);
    let here = tmp.path();
    announce(here, "s-p2", "src.rs", 1, 120);
    let call = hook("claude-code", "PreToolUse", "s-p2", grep_call(&file));
    assert!(prevented(&call, here, &enforcing()));
    let rows = audits(here);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].decision,
        crate::agent_queue::ServeAuditKind::ReadPrevented
    );
}

/// One announcement is worth one read, however many times the window acts
/// on the file afterwards: the second edit finds the booking and books
/// nothing more.
#[test]
fn a_second_action_on_the_same_hold_in_one_window_books_nothing_more() {
    let (tmp, file) = checkout(120);
    let here = tmp.path();
    announce(here, "s-p3", "src.rs", 1, 120);
    let call = hook("claude-code", "PreToolUse", "s-p3", edit_call(&file));
    assert!(prevented(&call, here, &enforcing()));
    assert!(
        !prevented(&call, here, &enforcing()),
        "the announcement has already been booked"
    );
    assert_eq!(audits(here).len(), 1);
}

/// **Never twice for one slice.** A read of the announced file that was
/// declined is already booked by the decline; the edit that follows adds
/// nothing, because the decline answered the announcement.
#[test]
fn a_file_intercepted_and_edited_in_one_window_books_the_intercept_only() {
    let (tmp, file) = checkout(120);
    let here = tmp.path();
    let enforce = enforcing();
    let read = hook(
        "claude-code",
        "PreToolUse",
        "s-p4",
        read_call(&file, None, None),
    );
    gate_under(&read, here, &enforce);
    announce(here, "s-p4", "src.rs", 1, 120);
    assert!(
        gate_under(&read, here, &enforce).deny_reason.is_some(),
        "the repeat is declined, and that decline books the slice"
    );
    let edit = hook("claude-code", "PreToolUse", "s-p4", edit_call(&file));
    assert!(!prevented(&edit, here, &enforce));

    let rows = audits(here);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].decision,
        crate::agent_queue::ServeAuditKind::ReadIntercepted
    );
}

/// ★ **An observed re-read is a read, and answers the announcement.**
/// Under the shipped line floor a repeat of a short slice is decided
/// against and let through: the call RUNS and the bytes arrive in the
/// window. The edit that follows has replaced no read, so it books
/// nothing — the `observe` in the journal names what Rift did, never what
/// the tool delivered.
#[test]
fn an_observed_reread_of_a_held_file_leaves_the_edit_nothing_to_book() {
    let tmp = tempfile::tempdir().expect("a work tree");
    let here = tmp.path();
    let file = here.join("src.rs");
    std::fs::write(&file, "a\nb\nc\n").expect("the file");
    let mut enforce = levers_for("claude-code", rift_recall_policy::Mode::Enforce);
    enforce.min_slice_lines = rift_recall_policy::DEFAULT_MIN_SLICE_LINES;
    let read = hook("claude-code", "PreToolUse", "s-p7", whole(&file));
    gate_under(&read, here, &enforce);
    announce(here, "s-p7", "src.rs", 1, 3);
    assert_eq!(
        gate_under(&read, here, &enforce).deny_reason,
        None,
        "three lines is under the floor, so the repeat runs and delivers"
    );

    let edit = hook("claude-code", "PreToolUse", "s-p7", edit_call(&file));
    assert!(
        !prevented(&edit, here, &enforce),
        "the window was handed the bytes; no read was prevented"
    );
    assert_eq!(
        lines_of(here, "s-p7", None)
            .iter()
            .map(Line::kind)
            .collect::<Vec<_>>(),
        vec!["read", "hold", "observe"],
        "and no booking joined them"
    );
    let rows = audits(here);
    assert!(
        rows.iter().all(|row| !matches!(
            row.decision,
            crate::agent_queue::ServeAuditKind::ReadPrevented
                | crate::agent_queue::ServeAuditKind::ReadPreventedObserved
        )),
        "{rows:?}"
    );
}

/// What one accounting row is worth, as the ledger event it becomes: the
/// neutral tag it carries, what its reason cost the model, and what it claims
/// was avoided. A booking returns nothing, so its cost is zero by
/// construction rather than by omission.
fn row_figures(
    work_tree: &Path,
    rows: &[crate::agent_queue::ServeAudit],
    kind: crate::agent_queue::ServeAuditKind,
) -> (String, u32, u32) {
    let row = rows
        .iter()
        .find(|row| row.decision == kind)
        .expect("the row that kind owes");
    match serve_audit_event(work_tree, row) {
        rift_recall_serve::MemdEvent::ReadPrevented {
            reason,
            estimated_tokens_avoided,
            ..
        } => (reason, 0, estimated_tokens_avoided),
        rift_recall_serve::MemdEvent::ReadIntercepted {
            reason,
            estimated_tokens_returned,
            estimated_tokens_avoided,
            ..
        } => (reason, estimated_tokens_returned, estimated_tokens_avoided),
        other => panic!("neither of the two rows: {other:?}"),
    }
}

/// ★ **Never twice for one slice, in the other order either.** The
/// window searches the announced file first, which books the read that
/// never happened; the read it makes afterwards is still declined — the
/// model is still told what it holds, and the reason it was sent is still
/// charged — but the slice is already booked, so the decline claims a
/// zero under the tag that says which row has it.
#[test]
fn a_decline_after_a_prevented_booking_books_nothing_more() {
    assert_eq!(
        rift_tokens::provenance(),
        rift_tokens::TOKENIZER,
        "these figures are only claims if the real tokenizer is missing"
    );
    let (tmp, file) = checkout(120);
    let here = tmp.path();
    let enforce = enforcing();
    let read = hook(
        "claude-code",
        "PreToolUse",
        "s-p8",
        read_call(&file, None, None),
    );
    gate_under(&read, here, &enforce);
    announce(here, "s-p8", "src.rs", 1, 120);
    let grep = hook("claude-code", "PreToolUse", "s-p8", grep_call(&file));
    assert!(prevented(&grep, here, &enforce), "the booking comes first");
    assert!(
        gate_under(&read, here, &enforce).deny_reason.is_some(),
        "and the read is still declined and still pointed"
    );
    assert_eq!(
        lines_of(here, "s-p8", None)
            .iter()
            .map(Line::kind)
            .collect::<Vec<_>>(),
        vec!["read", "hold", "prevented", "deny"],
        "the once-per-key decline is untouched"
    );

    let rows = audits(here);
    assert_eq!(rows.len(), 2, "{rows:?}");
    let whole_file = rift_tokens::count(
        &std::fs::read_to_string(&file).expect("the fixture the booking priced"),
    );
    assert!(whole_file > 0, "a file worth reading");
    let (reason, _, avoided) = row_figures(
        here,
        &rows,
        crate::agent_queue::ServeAuditKind::ReadPrevented,
    );
    assert_eq!(reason, rift_recall_serve::TAG_PREVENTED);
    assert_eq!(avoided, whole_file, "the booking has the slice");
    let (reason, returned, avoided) = row_figures(
        here,
        &rows,
        crate::agent_queue::ServeAuditKind::ReadIntercepted,
    );
    assert_eq!(reason, rift_recall_serve::TAG_PREVENTED_FIRST);
    assert_eq!(
        avoided, 0,
        "and the decline claims none of it a second time"
    );
    assert!(returned > 0, "while what its reason cost is still charged");
}

/// A window that read twenty lines, was told it holds them, had that read
/// booked by a search, and has since read the whole file twice — allowed the
/// first time, because the window did not hold the file, and declined the
/// second.
///
/// Driven exactly as the shape was found, because the order is the point.
fn a_booking_then_a_wider_decline(session: &str) -> (tempfile::TempDir, PathBuf) {
    let (tmp, file) = checkout(120);
    let here = tmp.path();
    let enforce = enforcing();
    let slice = hook(
        "claude-code",
        "PreToolUse",
        session,
        read_call(&file, Some(1), Some(20)),
    );
    gate_under(&slice, here, &enforce);
    announce(here, session, "src.rs", 1, 20);
    let grep = hook("claude-code", "PreToolUse", session, grep_call(&file));
    assert!(prevented(&grep, here, &enforce), "the booking comes first");
    let whole = hook(
        "claude-code",
        "PreToolUse",
        session,
        read_call(&file, None, None),
    );
    assert!(
        gate_under(&whole, here, &enforce).deny_reason.is_none(),
        "the window held twenty lines, not the file"
    );
    assert!(
        gate_under(&whole, here, &enforce).deny_reason.is_some(),
        "and now it does, so the read after it is declined"
    );
    (tmp, file)
}

/// The first `lines` lines of a file, as one string — the bytes a booking of
/// that run was priced over.
fn head_of(path: &Path, lines: usize) -> String {
    let body = std::fs::read_to_string(path).expect("the fixture the booking priced");
    body.lines()
        .take(lines)
        .fold(String::new(), |mut text, line| {
            text.push_str(line);
            text.push('\n');
            text
        })
}

/// ★ Reaching past a booking adds no line to the window's journal: the
/// once-per-key decline is untouched.
#[test]
fn a_wider_decline_over_a_prevented_slice_adds_no_line() {
    let (tmp, _file) = a_booking_then_a_wider_decline("s-p9");
    assert_eq!(
        lines_of(tmp.path(), "s-p9", None)
            .iter()
            .map(Line::kind)
            .collect::<Vec<_>>(),
        vec!["read", "hold", "prevented", "read", "deny"],
        "the once-per-key decline is untouched"
    );
}

/// ★ **A decline that reaches past a booking still claims nothing.** The
/// announcement was about part of a file, and the booking took that part;
/// the read that is declined later asks for the whole of it. A decline
/// books the run it was asked for and nothing smaller, so booking it
/// beside the slice already taken would charge one revision for one and a
/// half reads. The figure the window ends on is the slice's, once.
#[test]
fn a_wider_decline_over_a_prevented_slice_books_nothing_more() {
    assert_eq!(
        rift_tokens::provenance(),
        rift_tokens::TOKENIZER,
        "these figures are only claims if the real tokenizer is missing"
    );
    let (tmp, file) = a_booking_then_a_wider_decline("s-p9");
    let here = tmp.path();
    let rows = audits(here);
    // The booking, the observed narrowing that sizes the pool and claims
    // nothing, and the decline.
    assert_eq!(rows.len(), 3, "{rows:?}");
    let slice_cost = rift_tokens::count(&head_of(&file, 20));
    assert!(slice_cost > 0, "a slice worth reading");
    assert!(
        rift_tokens::count(&std::fs::read_to_string(&file).expect("the fixture")) > slice_cost,
        "and a file worth more than the slice"
    );
    let (reason, _, booked) = row_figures(
        here,
        &rows,
        crate::agent_queue::ServeAuditKind::ReadPrevented,
    );
    assert_eq!(reason, rift_recall_serve::TAG_PREVENTED);
    assert_eq!(booked, slice_cost, "the booking has the slice");
    let (reason, _, avoided) = row_figures(
        here,
        &rows,
        crate::agent_queue::ServeAuditKind::ReadIntercepted,
    );
    assert_eq!(reason, rift_recall_serve::TAG_PREVENTED_FIRST);
    assert_eq!(avoided, 0, "the wider decline claims none of it again");
    assert_eq!(
        booked + avoided,
        slice_cost,
        "one slice, one saving, and nothing added for reaching past it"
    );
}

/// Nothing was announced, so nothing was avoided. This is the ordinary
/// case, and it is the one that must cost nothing and claim nothing.
#[test]
fn an_action_on_a_file_never_announced_books_nothing() {
    let (tmp, file) = checkout(120);
    let here = tmp.path();
    for payload in [edit_call(&file), grep_call(&file)] {
        let call = hook("claude-code", "PreToolUse", "s-p5", payload);
        assert!(!prevented(&call, here, &enforcing()));
    }
    assert!(audits(here).is_empty());
    assert!(lines_of(here, "s-p5", None).is_empty());
}

/// Observe records the decision and claims none of it: the row names the
/// file and the lines, so the class can be sized, and its avoided figure
/// is a zero under a tag that says why.
#[test]
fn observe_records_a_prevented_read_with_nothing_avoided() {
    let (tmp, file) = checkout(120);
    let here = tmp.path();
    announce(here, "s-p6", "src.rs", 1, 120);
    let call = hook("claude-code", "PreToolUse", "s-p6", edit_call(&file));
    assert!(prevented(&call, here, &observing()));

    let rows = audits(here);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].decision,
        crate::agent_queue::ServeAuditKind::ReadPreventedObserved
    );
    let event = serve_audit_event(here, &rows[0]);
    let rift_recall_serve::MemdEvent::ReadPrevented {
        reason,
        estimated_tokens_avoided,
        handle,
    } = event
    else {
        panic!("an observed prevented read is still a prevented read: {event:?}");
    };
    assert_eq!(estimated_tokens_avoided, 0);
    assert_eq!(reason, rift_recall_serve::TAG_PREVENTED_OBSERVED);
    assert!(handle.starts_with("file:src.rs@sha256:"), "{handle}");
}

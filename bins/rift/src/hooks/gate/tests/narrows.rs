//! A read that is partly held: what the gate records while narrowing is only
//! observed, what it books, the frame a narrowing would travel with, and the
//! floor that frame's price sets.

use rift_agent::window_journal::{Decision, DenyShape, Journal, Line, ResolvedRead};

use super::fixtures::*;
use crate::hooks::gate::answer::{gate_answer, read_decision_audit};

/// What a line of source costs, the figure both floors are derived at.
const TOKENS_PER_LINE: u32 = 4;
/// The renderer's golden fixture: the path its goldens use.
const PATH: &str = "src/lib.rs";

/// A revision whose twelve-hex display form is the golden fixture's — the
/// only part of a hash the narrowing frame renders — spelled so this file
/// carries no hash-shaped literal of its own.
fn golden_hash() -> String {
    format!("3f9a1c2e8b7d{}", "0".repeat(52))
}

/// ★ The narrowing floor is the narrowing frame's break-even, measured.
///
/// The plain floor's rule, applied to the longer frame: the first count of
/// held lines whose read costs more than the sentence the model reads instead,
/// at four tokens a line, measured with the tokenizer every other figure is
/// counted with on the renderer's own golden fixture. A reword that grows the
/// frame reds this test, and the constant follows the frame.
#[test]
fn the_narrowing_frame_pays_for_itself_at_its_floor() {
    let frame =
        rift_recall_serve::render_narrow_frame(PATH, &golden_hash(), (1, 40), (41, 120), Some(3));
    let tokens = rift_tokens::count(&frame);
    assert_eq!(tokens, 67, "the narrowing frame on the canonical fixture");
    let plain = rift_tokens::count(&rift_recall_serve::render_deny_frame(
        PATH,
        &golden_hash(),
        1,
        40,
        Some(3),
    ));
    let break_even = tokens / TOKENS_PER_LINE + 1;
    assert_eq!(
        rift_agent::window_journal::NARROW_MIN_HELD_LINES,
        break_even,
        "frame {tokens} tokens at {TOKENS_PER_LINE} a line (the plain frame is {plain})"
    );
}

/// A read of the first hundred lines of `file`, then of lines 51 to 150, in
/// one window, under `levers`: what the second one was answered with.
fn second_read_overlapping_the_first(
    here: &std::path::Path,
    file: &std::path::Path,
    levers: &rift_recall_policy::Effective,
) -> Option<String> {
    gate_under(
        &hook(
            "claude-code",
            "PreToolUse",
            "s-n",
            read_call(file, Some(1), Some(100)),
        ),
        here,
        levers,
    );
    gate_under(
        &hook(
            "claude-code",
            "PreToolUse",
            "s-n",
            read_call(file, Some(51), Some(100)),
        ),
        here,
        levers,
    )
    .deny_reason
}

/// ★ Under the default policy an enforcing family's partly held read runs —
/// the `narrow` key is unwritten, so the family's mode alone narrows
/// nothing: the decision is recorded, the lines the call delivered become
/// coverage, and the row it owes is priced over the held run alone.
#[test]
fn under_the_default_policy_a_partly_held_read_runs_is_recorded_and_books_its_held_lines() {
    let (tmp, file) = checkout(300);
    let here = tmp.path();
    let enforce = enforcing();
    assert_eq!(enforce.read_deny, rift_recall_policy::Mode::Enforce);
    assert_eq!(enforce.narrow, rift_recall_policy::Mode::Observe);
    assert_eq!(
        second_read_overlapping_the_first(here, &file, &enforce),
        None,
        "nothing is declined while observed"
    );
    let lines = lines_of(here, "s-n", None);
    assert_eq!(
        lines.iter().map(Line::kind).collect::<Vec<_>>(),
        ["read", "read"],
        "an observed narrowing is recorded as the read it ran as"
    );
    let coverage = lines_journal(here, "s-n").read_coverage("src.rs", revision(here, "src.rs"));
    assert_eq!(
        coverage.runs(),
        [rift_agent::window_journal::Interval { lo: 1, hi: 150 }],
        "the window holds what it was handed, no less and no more"
    );
    let rows = audits(here);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].decision,
        crate::agent_queue::ServeAuditKind::ReadRepeatObserved
    );
    assert_eq!(
        rows[0].basis.as_ref().and_then(|basis| basis.lines),
        Some(crate::agent_queue::AuditLines { lo: 51, hi: 100 }),
        "the held run is what a narrowing spares"
    );
}

/// ★ With `narrow` written `enforce` the same second read is declined for
/// its unheld range: the frame travels, the decline is the line it leaves,
/// and the identical repeat runs. The written key is what moved — the family
/// enforced in the test above too.
#[test]
fn a_partly_held_read_is_narrowed_once_narrow_is_written_enforce() {
    let (tmp, file) = checkout(300);
    let here = tmp.path();
    let mut written = enforcing();
    written.narrow = rift_recall_policy::Mode::Enforce;
    let reason = second_read_overlapping_the_first(here, &file, &written)
        .expect("the held half is declined");
    assert_eq!(
        reason,
        rift_recall_serve::render_narrow_frame(
            "src.rs",
            &revision(here, "src.rs").to_hex(),
            (51, 100),
            (101, 150),
            Some(4)
        )
    );
    assert_eq!(
        lines_of(here, "s-n", None)
            .iter()
            .map(Line::kind)
            .collect::<Vec<_>>(),
        ["read", "deny", "call"]
    );
    let rows = audits(here);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].decision,
        crate::agent_queue::ServeAuditKind::ReadIntercepted
    );
    let repeat = gate_under(
        &hook(
            "claude-code",
            "PreToolUse",
            "s-n",
            read_call(&file, Some(51), Some(100)),
        ),
        here,
        &written,
    );
    assert_eq!(repeat.deny_reason, None, "a key declined once is honoured");
}

/// A narrowing that is enforced travels with the sealed renderer's frame,
/// naming the turn the held run was read in, and books its held run.
#[test]
fn an_enforced_narrowing_answers_with_its_frame_and_books_its_held_run() {
    let hash = rift_core::Sha256Id::parse_hex(&golden_hash()).expect("a revision");
    let read = ResolvedRead::new(
        &rift_agent::ReadRequest {
            path: PATH.to_owned(),
            span: rift_agent::ReadSpan::lines(101, rift_agent::HiKind::Explicit(400)),
            channel: rift_agent::ReadChannel::Tool,
        },
        hash,
        900,
    )
    .expect("a resolved read");
    let held = Journal::from_lines(vec![Line::Read {
        turn: 6,
        path: PATH.to_owned(),
        hash,
        range: rift_agent::window_journal::Interval { lo: 1, hi: 200 },
        total_lines: 900,
        at_ms: 1,
    }]);
    let narrowed = Decision::Deny(DenyShape::Narrow {
        held: rift_agent::window_journal::Interval { lo: 101, hi: 200 },
        read: rift_agent::window_journal::Interval { lo: 201, hi: 400 },
    });
    let (decision, _, reason) = gate_answer(&held, &read, narrowed.clone(), 3);
    assert_eq!(decision, narrowed);
    assert_eq!(
        reason.as_deref(),
        Some(
            rift_recall_serve::render_narrow_frame(
                PATH,
                &golden_hash(),
                (101, 200),
                (201, 400),
                Some(6)
            )
            .as_str()
        )
    );
    let row = read_decision_audit(
        &rift_core::AgentId::CLAUDE_CODE,
        &read,
        "oid",
        &decision,
        reason.as_deref().unwrap_or_default(),
        false,
        "0123456789abcdef".repeat(2),
    )
    .expect("a decline owes a row");
    assert_eq!(
        row.basis.and_then(|basis| basis.lines),
        Some(crate::agent_queue::AuditLines { lo: 101, hi: 200 })
    );
}

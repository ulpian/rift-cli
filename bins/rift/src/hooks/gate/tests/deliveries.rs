//! What a result says about its own delivery, and what the window's coverage
//! does about the difference: a truncated read grants nothing past what
//! arrived, and a read the client says it answered in full is the only
//! evidence a slice too large for the size rule can have.

use std::path::{Path, PathBuf};

use rift_agent::window_journal::{Interval, Line, StatedDelivery};

use super::fixtures::*;
use crate::hooks::gate::delivery::{
    stated_delivery, DELIVERY_NUM_LINES, DELIVERY_START_LINE, DELIVERY_TOTAL_LINES,
    PERSISTED_OUTPUT_MARKER, PERSISTED_OUTPUT_PATH, TRUNCATED_BY_TOKEN_CAP, TRUNCATION_NOTICE,
    TRUNCATION_NOTICE_LINES,
};

/// The ordinary case: a bounded read of a file in this checkout leaves one
/// line saying which revision, which lines, and how long the file is.
#[test]
fn a_bounded_read_of_this_checkout_is_recorded_as_the_lines_it_delivers() {
    let (tmp, file) = checkout(120);
    gate_observing(
        &hook(
            "claude-code",
            "PreToolUse",
            "s-1",
            read_call(&file, Some(10), Some(20)),
        ),
        tmp.path(),
    );

    let lines = lines_of(tmp.path(), "s-1", None);
    let [Line::Read {
        turn,
        path,
        range,
        total_lines,
        ..
    }] = lines.as_slice()
    else {
        panic!("one read line, got {lines:?}");
    };
    assert_eq!(*turn, 4);
    assert_eq!(
        path, "src.rs",
        "the checkout's own spelling, not the payload's"
    );
    assert_eq!(*range, Interval::new(10, 29).expect("a run"));
    assert_eq!(*total_lines, 120);
}

/// The pin this family's truncation reading rests on, included at compile
/// time so a renamed or deleted fixture is a build failure rather than a
/// test that quietly stops checking anything.
const CLAUDE_CODE_TOOL_FIELDS: &str = include_str!(
    "../../../../../../crates/rift-agent-claude-code/fixtures/claude-code-tool-field-surface.md"
);

/// **Every string this gate reads a delivery out of is a string the pin
/// quotes.** A client that renames the field or rewords the notice moves
/// the fixture first, and the fixture moving without the code — or the
/// code without the fixture — reds here.
#[test]
fn the_delivery_reading_is_spelled_the_way_the_pin_spells_it() {
    for quoted in [
        TRUNCATED_BY_TOKEN_CAP,
        DELIVERY_START_LINE,
        DELIVERY_NUM_LINES,
        DELIVERY_TOTAL_LINES,
        TRUNCATION_NOTICE,
        TRUNCATION_NOTICE_LINES,
        PERSISTED_OUTPUT_PATH,
        PERSISTED_OUTPUT_MARKER,
    ] {
        assert!(
            CLAUDE_CODE_TOOL_FIELDS.contains(quoted),
            "the pin does not carry {quoted:?}; re-pin the surface before \
             reading it, never the other way round"
        );
    }
}

/// A `PostToolUse` result for a read, with the truncation the pinned
/// object states and the delivery it names.
fn result_of(path: &Path, offset: u32, limit: u32, file: serde_json::Value) -> serde_json::Value {
    let mut payload = read_call(path, Some(offset), Some(limit));
    payload["tool_response"] = serde_json::json!({"type": "text", "file": file});
    payload
}

/// ★ **A truncated read grants no coverage past what was delivered.**
///
/// The call asked for the whole file and the client handed back forty
/// lines of it and said so. The window is credited with the forty and with
/// nothing after them, so the range it did not receive is a range a later
/// decline cannot rest on — which is the one direction of error a decline
/// does not survive.
#[test]
fn a_truncated_read_grants_no_coverage_past_what_was_delivered() {
    let (tmp, file) = checkout(120);
    let asked = hook(
        "claude-code",
        "PreToolUse",
        "s-1",
        read_call(&file, Some(1), Some(120)),
    );
    gate_observing(&asked, tmp.path());
    assert_eq!(
        lines_of(tmp.path(), "s-1", None).first().map(Line::kind),
        Some("read"),
        "the call is recorded before it runs, at the range it asked for"
    );

    gate_observing(
        &hook(
            "claude-code",
            "PostToolUse",
            "s-1",
            result_of(
                &file,
                1,
                120,
                serde_json::json!({
                    "filePath": file.display().to_string(),
                    "startLine": 1,
                    "numLines": 40,
                    "totalLines": 120,
                    "truncatedByTokenCap": true,
                }),
            ),
        ),
        tmp.path(),
    );

    let lines = lines_of(tmp.path(), "s-1", None);
    assert_eq!(
        lines.iter().map(Line::kind).collect::<Vec<_>>(),
        vec!["read", "evict", "read"],
        "the claim is taken back and the delivery written again: {lines:?}"
    );
    let journal = lines_journal(tmp.path(), "s-1");
    let [Line::Read { path, hash, .. }] = journal.live() else {
        panic!("one live read, got {:?}", journal.live());
    };
    let coverage = journal.coverage(path, *hash);
    assert!(
        coverage.contains(Interval::new(1, 40).expect("a run")),
        "the lines the result says arrived are held"
    );
    assert!(
        !coverage.contains(Interval::new(41, 41).expect("a run")),
        "the first line past the delivery is not"
    );
    assert!(
        !coverage.contains(Interval::new(1, 41).expect("a run")),
        "and neither is any run reaching past it"
    );
}

/// A result that says nothing about being cut short says nothing at all:
/// absence is silence, never a statement that the whole range arrived, and
/// a journal that took a line back on silence would forgo savings on every
/// ordinary read.
#[test]
fn a_result_that_states_no_truncation_takes_nothing_back() {
    let (tmp, file) = checkout(120);
    gate_observing(
        &hook(
            "claude-code",
            "PreToolUse",
            "s-1",
            read_call(&file, Some(1), Some(120)),
        ),
        tmp.path(),
    );
    gate_observing(
        &hook(
            "claude-code",
            "PostToolUse",
            "s-1",
            result_of(
                &file,
                1,
                120,
                serde_json::json!({
                    "filePath": file.display().to_string(),
                    "startLine": 1,
                    "numLines": 40,
                    "totalLines": 120,
                }),
            ),
        ),
        tmp.path(),
    );
    assert_eq!(
        lines_of(tmp.path(), "s-1", None)
            .iter()
            .map(Line::kind)
            .collect::<Vec<_>>(),
        vec!["read"],
        "no flag, no notice, nothing to correct"
    );
}

/// A truncation that names no delivery leaves the read holding the one
/// line it started on — the same answer an unpinned default window gets,
/// and the smallest claim there is.
#[test]
fn a_truncation_with_no_stated_delivery_keeps_only_the_line_it_started_on() {
    let (tmp, file) = checkout(120);
    gate_observing(
        &hook(
            "claude-code",
            "PreToolUse",
            "s-1",
            read_call(&file, Some(10), Some(60)),
        ),
        tmp.path(),
    );
    gate_observing(
        &hook(
            "claude-code",
            "PostToolUse",
            "s-1",
            result_of(
                &file,
                10,
                60,
                serde_json::json!({
                    "filePath": file.display().to_string(),
                    "truncatedByTokenCap": true,
                }),
            ),
        ),
        tmp.path(),
    );
    let journal = lines_journal(tmp.path(), "s-1");
    let [Line::Read { range, .. }] = journal.live() else {
        panic!("one live read, got {:?}", journal.live());
    };
    assert_eq!(*range, Interval::new(10, 10).expect("a run"));
}

/// The notice is read where a result reaches the hook as text rather than
/// as the object the pin quotes — the older client's shape, and the one a
/// reader looking only for the flag would miss.
#[test]
fn the_truncation_notice_is_read_when_the_object_is_not() {
    let (tmp, file) = checkout(120);
    gate_observing(
        &hook(
            "claude-code",
            "PreToolUse",
            "s-1",
            read_call(&file, Some(1), Some(120)),
        ),
        tmp.path(),
    );
    let mut payload = read_call(&file, Some(1), Some(120));
    payload["tool_response"] = serde_json::json!(
        "<system-reminder>[Truncated: PARTIAL view — showing lines 1-12 of 120 total \
         (27006 tokens, cap 25000). Call Read with offset=13 limit=12 for the next page, \
         or Grep to find a specific section.]</system-reminder>\n\n1\tline 1\n"
    );
    gate_observing(
        &hook("claude-code", "PostToolUse", "s-1", payload),
        tmp.path(),
    );
    let journal = lines_journal(tmp.path(), "s-1");
    let [Line::Read { range, .. }] = journal.live() else {
        panic!("one live read, got {:?}", journal.live());
    };
    assert_eq!(
        *range,
        Interval::new(1, 12).expect("a run"),
        "the sentence names the delivery and the delivery is what is kept"
    );
}

/// A command result the client kept in a file takes back everything past
/// the line the read started on, because none of the output arrived as
/// output — the model was handed a path and a preview.
///
/// Driven at a slice the size rule cannot catch: the file is small enough
/// that a shell read of it is recorded whole, and the client says it
/// spilled anyway. That is the case this backstop exists for; the size
/// rule alone would have left the claim standing.
#[test]
fn a_command_result_kept_in_a_file_delivers_no_lines_at_all() {
    let (tmp, file) = checkout(120);
    let command = |response: Option<serde_json::Value>| {
        let mut payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": format!("head -n 120 {}", shell_spelling(&file))},
        });
        if let Some(response) = response {
            payload["tool_response"] = response;
        }
        payload
    };
    gate_observing(
        &hook("claude-code", "PreToolUse", "s-1", command(None)),
        tmp.path(),
    );
    let before = lines_journal(tmp.path(), "s-1");
    let [Line::Read { range, .. }] = before.live() else {
        panic!("one live read, got {:?}", before.live());
    };
    assert_eq!(
        *range,
        Interval::new(1, 120).expect("a run"),
        "a 120-line command output is under every ceiling, so it is held whole"
    );
    gate_observing(
        &hook(
            "claude-code",
            "PostToolUse",
            "s-1",
            command(Some(serde_json::json!({
                "stdout": "line 1\n",
                "stderr": "",
                "interrupted": false,
                "isImage": false,
                "persistedOutputPath": "/tmp/tool-results/bawxzim8j.txt",
                "persistedOutputSize": 31_000,
            }))),
        ),
        tmp.path(),
    );
    let journal = lines_journal(tmp.path(), "s-1");
    let [Line::Read { range, .. }] = journal.live() else {
        panic!(
            "one live read after the take-back, got {:?}",
            journal.live()
        );
    };
    assert_eq!(
        *range,
        Interval::new(1, 1).expect("a run"),
        "a pointer is not the lines, however many the command named"
    );
}

/// The same statement as the envelope the model reads, for a result that
/// arrives as text — and a command that merely *prints* the word is not
/// the client having said it.
#[test]
fn the_persisted_envelope_is_read_at_the_front_of_a_text_result_only() {
    let file = std::path::Path::new("/nowhere/src.rs");
    let text = |body: &str| {
        let mut payload = read_call(file, Some(1), Some(120));
        payload["tool_response"] = serde_json::json!(body);
        payload
    };
    assert_eq!(
        stated_delivery(
            &rift_core::AgentId::CLAUDE_CODE,
            &text("<persisted-output>\nOutput too large (30.3KB). Full output saved to: /t")
        ),
        Some(StatedDelivery::Unstated)
    );
    assert_eq!(
        stated_delivery(
            &rift_core::AgentId::CLAUDE_CODE,
            &text("the sentinel <persisted-output> appears in this file's own text")
        ),
        None,
        "a result that quotes the word said nothing about itself"
    );
}

/// **A family whose result shape is not pinned is not read for one.** The
/// same object on another family's hook states nothing this gate knows how
/// to read, and nothing about that family's recording changes.
#[test]
fn a_family_with_no_pinned_result_shape_is_left_as_it_was() {
    let file = std::path::Path::new("/nowhere/src.rs");
    let object = serde_json::json!({
        "type": "text",
        "file": {"startLine": 1, "numLines": 40, "truncatedByTokenCap": true},
    });
    for family in ["codex", "antigravity"] {
        let mut payload = read_call(file, Some(1), Some(120));
        payload["tool_response"] = object.clone();
        assert_eq!(
            stated_delivery(&rift_core::AgentId::new(family), &payload),
            None,
            "{family} publishes no truncation statement any run has shown"
        );
    }
    let mut payload = read_call(file, Some(1), Some(120));
    payload["tool_response"] = object;
    assert_eq!(
        stated_delivery(&rift_core::AgentId::CLAUDE_CODE, &payload),
        Some(StatedDelivery::Truncated(
            Interval::new(1, 40).expect("a run")
        )),
        "and the family whose shape is pinned is read"
    );
}

/// **The three readings of a result's own delivery, and the absence that
/// is none of them.**
///
/// Two fields and a count, read exactly as O5 and O7 pin them: the flag
/// set is a truncation, the flag cleared is the client's word that the
/// call was answered in full, and a run reaching the client's own
/// `totalLines` is the same word in the shape O7 actually records — an
/// untruncated result carries no flag at all. A run stopping short of
/// `totalLines` with no flag beside it says nothing, and is read as
/// nothing rather than as either.
#[test]
fn a_delivery_statement_is_read_as_the_pin_states_it_and_absence_as_nothing() {
    let path = std::path::Path::new("/nowhere/src.rs");
    let reading = |file: serde_json::Value| {
        let mut payload = read_call(path, None, None);
        payload["tool_response"] = serde_json::json!({"type": "text", "file": file});
        stated_delivery(&rift_core::AgentId::CLAUDE_CODE, &payload)
    };
    let run = |lo, hi| Interval::new(lo, hi).expect("a run");

    assert_eq!(
        reading(serde_json::json!({
            "startLine": 1, "numLines": 40, "totalLines": 900,
            "truncatedByTokenCap": true,
        })),
        Some(StatedDelivery::Truncated(run(1, 40))),
        "the flag set names how far the delivery got"
    );
    assert_eq!(
        reading(serde_json::json!({
            "startLine": 1, "numLines": 40, "totalLines": 900,
            "truncatedByTokenCap": false,
        })),
        Some(StatedDelivery::Complete(run(1, 40))),
        "the flag cleared is the client's own word for a full answer"
    );
    assert_eq!(
        reading(serde_json::json!({
            "startLine": 1, "numLines": 901, "totalLines": 901,
        })),
        Some(StatedDelivery::Complete(run(1, 901))),
        "and so is a run reaching the file's own length with no flag at all"
    );
    assert_eq!(
        reading(serde_json::json!({
            "startLine": 1, "numLines": 40, "totalLines": 900,
        })),
        None,
        "a run stopping short of it with no flag beside it says nothing"
    );
    assert_eq!(
        reading(serde_json::json!({"filePath": "/nowhere/src.rs"})),
        None,
        "and a result carrying none of the fields says nothing either"
    );
}

/// A `PostToolUse` result for a read that named no range at all, carrying
/// the three fields the pinned object states about its own delivery.
///
/// `flag` is what `truncatedByTokenCap` is set to, or nothing at all —
/// which is the shape O7 records for an untruncated read, and the reason
/// the delivered run has to be read together with `totalLines` rather than
/// the flag alone.
fn whole_result_of(
    path: &Path,
    delivered: u32,
    total: u32,
    flag: Option<bool>,
) -> serde_json::Value {
    let mut file = serde_json::json!({
        "filePath": path.display().to_string(),
        "startLine": 1,
        "numLines": delivered,
        "totalLines": total,
    });
    if let Some(flag) = flag {
        file["truncatedByTokenCap"] = serde_json::json!(flag);
    }
    let mut payload = read_call(path, None, None);
    payload["tool_response"] = serde_json::json!({"type": "text", "file": file});
    payload
}

/// A checkout whose one file is about 59 KB — past `max_delivered_bytes` and
/// therefore credited as nothing before a call runs, which is the state in
/// which every re-read of a repository's largest and most-read files has
/// started until now.
struct OversizeRead {
    tmp: tempfile::TempDir,
    /// A read of the whole file.
    whole: serde_json::Value,
    /// A read of two hundred lines inside it.
    inside: serde_json::Value,
    file: PathBuf,
    enforce: rift_recall_policy::Effective,
}

impl OversizeRead {
    /// Run the whole-file read in `session`, then hand the gate the result
    /// the client states about it.
    fn read_and_state(&self, session: &str, result: serde_json::Value) {
        gate_under(
            &hook("claude-code", "PreToolUse", session, self.whole.clone()),
            self.tmp.path(),
            &self.enforce,
        );
        gate_under(
            &hook("claude-code", "PostToolUse", session, result),
            self.tmp.path(),
            &self.enforce,
        );
    }

    /// The one run of lines this window has been credited with.
    fn credited(&self, session: &str) -> Interval {
        let journal = lines_journal(self.tmp.path(), session);
        let [Line::Read { range, .. }] = journal.live() else {
            panic!("the statement credits one read, got {:?}", journal.live());
        };
        *range
    }

    /// Whether the two hundred lines inside the file are declined now.
    fn inside_is_declined(&self, session: &str) -> bool {
        gate_under(
            &hook("claude-code", "PreToolUse", session, self.inside.clone()),
            self.tmp.path(),
            &self.enforce,
        )
        .deny_reason
        .is_some()
    }
}

/// A file past the bound the size rule can vouch for, and the calls a
/// delivery statement is read against.
fn oversize_read() -> OversizeRead {
    let (tmp, file) = checkout(6_000);
    let bytes = std::fs::metadata(&file).expect("the fixture").len();
    assert!(
        bytes > rift_agent::window_journal::DEFAULT_MAX_DELIVERED_BYTES as u64,
        "the walk is only about a slice past the bound: {bytes} bytes"
    );
    OversizeRead {
        whole: read_call(&file, None, None),
        inside: read_call(&file, Some(100), Some(200)),
        file,
        enforce: enforcing(),
        tmp,
    }
}

/// ★ Nothing is credited before the call has run, whatever it will deliver: a
/// decline cannot rest on a delivery that has not happened.
#[test]
fn a_slice_past_the_size_rule_is_credited_with_nothing_before_the_call_runs() {
    let fixture = oversize_read();
    assert!(gate_under(
        &hook("claude-code", "PreToolUse", "s-1", fixture.whole.clone()),
        fixture.tmp.path(),
        &fixture.enforce
    )
    .deny_reason
    .is_none());
    assert!(
        lines_of(fixture.tmp.path(), "s-1", None).is_empty(),
        "the size rule credited nothing, exactly as before"
    );
}

/// ★ **A delivery too large for density to vouch for becomes coverage from
/// the client's own statement, one hook later.**
///
/// The result object states `numLines` reaching the client's own
/// `totalLines` with no truncation flag beside it, which is the client
/// saying it handed the file over.
#[test]
fn a_result_that_states_it_answered_in_full_credits_the_whole_file() {
    let fixture = oversize_read();
    fixture.read_and_state("s-1", whole_result_of(&fixture.file, 6_001, 6_001, None));
    assert_eq!(
        fixture.credited("s-1"),
        Interval::new(1, 6_000).expect("a run"),
        "the client's own extent, clamped to the line count of these bytes"
    );
    assert!(
        fixture.inside_is_declined("s-1"),
        "a sub-range of a delivery the client vouched for is declinable"
    );
}

/// ★ The same rule read from the other side: the client says it stopped at
/// line forty, the credit is exactly those forty lines, and the sub-range
/// past them is not declined.
#[test]
fn a_result_that_states_it_stopped_short_credits_only_the_run_it_names() {
    let fixture = oversize_read();
    fixture.read_and_state("s-2", whole_result_of(&fixture.file, 40, 6_001, Some(true)));
    assert_eq!(
        fixture.credited("s-2"),
        Interval::new(1, 40).expect("a run"),
        "exactly the run the result names, and not a line more"
    );
    assert!(
        !fixture.inside_is_declined("s-2"),
        "the lines past the truncation were never handed over"
    );
}

/// A slice too large to have survived the client's own cap runs without
/// coverage before any result comes back — the truncation statement is
/// only the backstop for a smaller call whose result still says partial.
///
/// Both sides of the boundary are driven, so this reds on a raise as well
/// as on a drop.
#[test]
fn a_slice_too_large_to_have_survived_the_cap_records_nothing() {
    // "line NNNN\n" is ten bytes from line 1000 on, so five thousand lines
    // are about 50 KB: over the bound, with a prefix of it under.
    let (tmp, file) = checkout(6_000);
    let bound = rift_agent::window_journal::DEFAULT_MAX_DELIVERED_BYTES;
    let bytes = std::fs::read(&file).expect("the fixture");
    let upto = |lines: u32| -> usize {
        bytes
            .split_inclusive(|byte| *byte == b'\n')
            .take(lines as usize)
            .map(<[u8]>::len)
            .sum()
    };
    let mut under = 1;
    while upto(under + 1) <= bound {
        under += 1;
    }
    assert!(
        upto(under) <= bound && upto(under + 1) > bound,
        "the boundary"
    );

    gate_observing(
        &hook(
            "claude-code",
            "PreToolUse",
            "s-1",
            read_call(&file, Some(1), Some(under)),
        ),
        tmp.path(),
    );
    gate_observing(
        &hook(
            "claude-code",
            "PreToolUse",
            "s-2",
            read_call(&file, Some(1), Some(under + 1)),
        ),
        tmp.path(),
    );
    let inside = lines_of(tmp.path(), "s-1", None);
    let [Line::Read { range, .. }] = inside.as_slice() else {
        panic!("one read line, got {inside:?}");
    };
    assert_eq!(
        *range,
        Interval::new(1, under).expect("a run"),
        "a slice inside the bound is credited in full"
    );
    let outside = lines_of(tmp.path(), "s-2", None);
    assert!(outside.is_empty(), "one byte over creates no coverage");
}

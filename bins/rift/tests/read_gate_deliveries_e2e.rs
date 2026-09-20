//! End to end, through the shipped binary: what a window may be credited
//! with, and what it may therefore never be declined on.
//!
//! A decline rests on coverage, and coverage rests on a delivery that really
//! happened: a result the client cut short, a command whose output it kept in
//! a file, a file past the size the gate hashes at, a slice past the size the
//! lever acts on. Each bound is driven from both sides, because a bound
//! asserted from one side is a bound that can move.
//!
//! What is declined once the coverage is real is `read_gate_decisions_e2e`;
//! what a decision books is `read_gate_e2e`. How every walk here is driven is
//! `common/read_gate_fixture.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;

mod common;

#[allow(dead_code)] // The read-gate suites share these builders; each file uses a subset.
#[path = "common/read_gate_fixture.rs"]
mod read_gate_fixture;
use read_gate_fixture::{
    declined_with, enforce_for, gate_env, hook, json_str, kinds, numbered, read_of, read_whole_of,
    seed_repo, windows, SLICE_CEILING,
};

/// The inline-hash ceiling, spelled as a **number** rather than as the
/// constant it pins.
///
/// The gate hashes a file on the agent's own budget, and the ceiling on that
/// is a promise about latency: past it, nothing is hashed and nothing is
/// recorded. Written here as `1024 * 1024` on purpose — importing the
/// product's constant would make this test agree with any value the product
/// later chose, which is the one thing a pin must not do. A silent raise reds
/// [`a_file_past_the_inline_ceiling_is_left_alone`].
const INLINE_HASH_CEILING: usize = 1024 * 1024;

/// A `PostToolUse` result for a read, carrying the truncation the pinned
/// result object states and the delivery it names.
fn truncated_result_of(path: &Path, offset: u32, limit: u32, delivered: u32) -> String {
    format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{p},"offset":{offset},"limit":{limit}}},"tool_response":{{"type":"text","file":{{"filePath":{p},"startLine":{offset},"numLines":{delivered},"truncatedByTokenCap":true}}}}}}"#,
        p = json_str(path)
    )
}

/// The same result for a read that named no `limit` at all: the truncation
/// flag and the run the client says it handed over, on a call whose own input
/// asked for the whole file.
fn truncated_whole_result_of(path: &Path, delivered: u32) -> String {
    format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{p}}},"tool_response":{{"type":"text","file":{{"filePath":{p},"startLine":1,"numLines":{delivered},"truncatedByTokenCap":true}}}}}}"#,
        p = json_str(path)
    )
}

/// ★ **A limit-less read the client cut short keeps only the lines it was
/// handed** — through the shipped binary, in a repository that has asked for
/// declines.
///
/// The limit-less shape is the one an agent overwhelmingly sends and the one
/// credited with the largest claim there is: the file's whole extent, taken
/// before the call has run. The backstop is therefore load-bearing exactly
/// here, and until now it was walked only for reads that named an
/// `offset`/`limit` and for the terminal channel. What is asserted is the
/// whole claim in two halves — the lines the result says arrived are held and
/// declined on, the lines past them are not — plus the shape of the
/// correction, which is an eviction and a re-record because coverage is a
/// union and a union has no subtraction in it.
#[test]
fn a_limit_less_read_the_client_cut_short_is_never_declined_past_what_it_delivered() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("cut.txt");
    std::fs::write(&file, numbered(120, None)).unwrap();
    let whole = read_whole_of(&file);

    hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        1,
        &whole,
        &env,
    );
    hook(
        &repo,
        "claude-code",
        "PostToolUse",
        "gate-1",
        2,
        &truncated_whole_result_of(&file, 40),
        &env,
    );

    assert!(
        declined_with(&hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            3,
            &read_of(&file, 1, 40),
            &env
        ))
        .is_some(),
        "the forty lines the result says arrived are held, and asking again \
         for them is what a decline is for"
    );
    assert_eq!(
        declined_with(&hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            4,
            &whole,
            &env
        )),
        None,
        "and the whole-file claim it never earned is gone, so the read that \
         would have been declined on eighty lines nobody handed over runs"
    );
    assert_eq!(
        kinds(&repo),
        vec!["read", "evict", "read", "deny", "read"],
        "the claim taken back, the delivery written again, and the two answers"
    );
}

/// ★ A read the client cut short is never declined on the lines it never
/// delivered — through the shipped binary, in a repository that has asked for
/// declines.
///
/// The first call asks for the whole file and the client answers with forty
/// lines and says so. What follows is the whole claim in two halves: the forty
/// lines are held, so asking for them again is declined; the range past them
/// is not, so asking for the file again runs. A window that was credited with
/// the whole file here would have been declined bytes it never received, which
/// is the one error a decline does not survive.
#[test]
fn a_truncated_read_is_never_declined_on_what_it_did_not_deliver() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("long.txt");
    std::fs::write(&file, numbered(120, None)).unwrap();

    hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        1,
        &read_of(&file, 1, 120),
        &env,
    );
    hook(
        &repo,
        "claude-code",
        "PostToolUse",
        "gate-1",
        2,
        &truncated_result_of(&file, 1, 120, 40),
        &env,
    );
    assert!(
        declined_with(&hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            3,
            &read_of(&file, 1, 40),
            &env
        ))
        .is_some(),
        "the lines the result says arrived are held, and asking again for them \
         is what a decline is for"
    );
    assert_eq!(
        declined_with(&hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            4,
            &read_of(&file, 1, 120),
            &env
        )),
        None,
        "the lines it never delivered are the model's only way to see them"
    );
    assert_eq!(
        kinds(&repo),
        vec!["read", "evict", "read", "deny", "read"],
        "the claim taken back, the delivery written again, and the two answers"
    );
}

/// The size a command's output stops arriving inline at, spelled as a
/// **number** rather than as the constant it pins.
///
/// Past this many characters the client writes the output to a file and hands
/// the model the path and a short preview, so the lines a command named are
/// not lines the window holds. Written here as `30_000` for the reason
/// [`INLINE_HASH_CEILING`] gives: a raise on the product's side has to red a
/// test rather than quietly widen what may be declined.
const SHELL_PERSISTENCE_THRESHOLD: usize = 30_000;

/// A `Bash` call running `command`, as the pinned field spells one.
fn shell_of(command: &str) -> String {
    format!(
        r#"{{"tool_name":"Bash","tool_input":{{"command":{}}}}}"#,
        serde_json::Value::String(command.to_owned())
    )
}

/// ★ **A shell read past the persistence threshold grants no coverage** — through
/// the shipped binary, in a repository that has asked for declines.
///
/// `sed -n '1,3600p'` names three thousand six hundred lines and the recogniser
/// records them as an explicit range. It is right about the command: those
/// lines were printed. It is wrong about the window, because at this size the
/// client keeps the output in a file and hands the model the path. So the
/// repeat runs.
///
/// The same bytes through the read tool are the control, and they are what
/// stops this from being a rule that gives savings away: under the tool's own
/// ceiling the delivery is real, the whole range is held, and the repeat is
/// declined. One file, one size, two channels, two answers.
#[test]
fn a_shell_read_past_the_persistence_threshold_grants_no_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("long.txt");
    let body = numbered(3_600, None);
    std::fs::write(&file, &body).unwrap();
    assert!(
        body.len() > SHELL_PERSISTENCE_THRESHOLD,
        "the fixture has to reach past the threshold to say anything about it"
    );
    assert!(
        body.len() < SLICE_CEILING,
        "and stay under the slice ceiling, or the ceiling would be what answered"
    );
    let command = shell_of(&format!(
        "sed -n '1,3600p' {}",
        common::shell_spelling(&file)
    ));

    hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        1,
        &command,
        &env,
    );
    assert_eq!(
        declined_with(&hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            2,
            &command,
            &env
        )),
        None,
        "the window was handed a filename and a preview, so the lines are still \
         the model's only way to see them"
    );
    assert!(windows(&repo).is_empty(), "neither call creates coverage");

    // The control: the same bytes inline, in a repository of its own.
    let other = tempfile::tempdir().unwrap();
    let inline = seed_repo(other.path());
    enforce_for(&inline, &["claude-code"]);
    let inline_env = gate_env(&other.path().join("cache"));
    let same = inline.join("long.txt");
    std::fs::write(&same, &body).unwrap();
    hook(
        &inline,
        "claude-code",
        "PreToolUse",
        "gate-1",
        1,
        &read_of(&same, 1, 3_600),
        &inline_env,
    );
    assert!(
        declined_with(&hook(
            &inline,
            "claude-code",
            "PreToolUse",
            "gate-1",
            2,
            &read_of(&same, 1, 3_600),
            &inline_env
        ))
        .is_some(),
        "the read tool delivered these lines, and a repeat of a real delivery is \
         what a decline is for"
    );
}

/// ★ A command result the client kept in a file takes back the lines it never
/// handed over, through the shipped binary.
///
/// The slice is small enough that the size rule records it whole and the result
/// says the output was spilled anyway — the case the size rule cannot see. The
/// claim is taken back, the window keeps the line the read started on, and the
/// repeat runs.
#[test]
fn a_command_whose_output_was_kept_in_a_file_is_never_declined_on_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("short.txt");
    let body = numbered(300, None);
    std::fs::write(&file, &body).unwrap();
    assert!(
        body.len() < SHELL_PERSISTENCE_THRESHOLD,
        "the size rule has to be satisfied, or it would be what answered"
    );
    let command = shell_of(&format!("head -n 300 {}", common::shell_spelling(&file)));
    let mut result: serde_json::Value = serde_json::from_str(&command).unwrap();
    result["tool_response"] = serde_json::json!({
        "stdout": "line 1\n",
        "stderr": "",
        "interrupted": false,
        "isImage": false,
        "persistedOutputPath": "/tmp/tool-results/bawxzim8j.txt",
        "persistedOutputSize": 31_000,
    });

    hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        1,
        &command,
        &env,
    );
    hook(
        &repo,
        "claude-code",
        "PostToolUse",
        "gate-1",
        2,
        &result.to_string(),
        &env,
    );
    assert_eq!(
        declined_with(&hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            3,
            &command,
            &env
        )),
        None,
        "a pointer is not the lines, however many the command printed"
    );
    assert_eq!(
        kinds(&repo),
        vec!["read", "evict", "read", "read"],
        "the claim taken back, the line that did arrive written again, and the repeat"
    );
}

/// ★ A slice past the product's own ceiling runs however much of it the window
/// holds, while one under the ceiling is declined — both sides of
/// [`SLICE_CEILING`], through the shipped binary.
///
/// The window is given the whole file in pages small enough that every one of
/// them is a delivery a client can make, so coverage is real rather than
/// assumed. Then two calls: one asking for a range under the ceiling, which is
/// declined, and one asking for a range over it, which is not — because past
/// that size what a window kept of an earlier delivery is a page rather than
/// the range. A silent raise of the ceiling reds the second half; a drop reds
/// the first.
#[test]
fn a_slice_past_the_ceiling_runs_while_one_under_it_is_declined() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("huge.txt");
    // Numbered lines run about eleven bytes each past line 1,000, so twelve
    // thousand of them are comfortably over the ceiling.
    let body = numbered(12_000, None);
    std::fs::write(&file, &body).unwrap();
    let bytes_through = |lines: u32| -> usize {
        body.split_inclusive('\n')
            .take(lines as usize)
            .map(str::len)
            .sum()
    };
    assert!(
        bytes_through(12_000) > SLICE_CEILING,
        "the fixture has to reach past the ceiling to say anything about it"
    );

    // Four pages, each a slice a client can deliver whole, so what the window
    // holds afterwards is every line of the file.
    let mut turn = 1;
    for page in 0..4 {
        hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            turn,
            &read_of(&file, page * 3_000 + 1, 3_000),
            &env,
        );
        turn += 1;
    }
    let under = 4_000;
    assert!(
        bytes_through(under) < 47_500,
        "the smaller call has to fit the verified delivery channel"
    );
    assert!(
        declined_with(&hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            turn,
            &read_of(&file, 1, under),
            &env
        ))
        .is_some(),
        "a repeat inside the ceiling is what a decline is for"
    );
    assert_eq!(
        declined_with(&hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            turn + 1,
            &read_of(&file, 1, 12_000),
            &env
        )),
        None,
        "and a repeat past it is left alone whatever the window holds"
    );
}

/// ★ A file past the inline-hash ceiling is not hashed and not recorded, at
/// the value the ceiling actually has.
///
/// Both sides of the boundary are driven, so this reds on a raise as well as
/// on a drop: a byte under the ceiling is recorded, one byte over it is left
/// alone. The number is written here as a literal (see
/// [`INLINE_HASH_CEILING`]) rather than read from the product, which is the
/// whole point of a pin.
#[test]
fn a_file_past_the_inline_ceiling_is_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let env = gate_env(&tmp.path().join("cache"));

    let under = repo.join("under.txt");
    // One byte short of the ceiling, newline included.
    std::fs::write(&under, "x\n".repeat((INLINE_HASH_CEILING - 2) / 2)).unwrap();
    hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        1,
        &read_of(&under, 1, 1),
        &env,
    );
    assert_eq!(
        kinds(&repo),
        vec!["read"],
        "a file inside the ceiling is hashed and recorded"
    );

    let over = repo.join("over.txt");
    std::fs::write(&over, "x\n".repeat(INLINE_HASH_CEILING / 2 + 1)).unwrap();
    hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        2,
        &read_of(&over, 1, 1),
        &env,
    );
    assert_eq!(
        kinds(&repo),
        vec!["read"],
        "a file past it is not hashed, so nothing is claimed about it"
    );
}

/// A `Read` with an `offset` and no `limit`: the same delivery starting later.
fn read_from(path: &Path, offset: u32) -> String {
    format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":{offset}}}}}"#,
        json_str(path)
    )
}

/// ★ A limit-less read holds the whole file it was delivered — through the
/// shipped binary, in a repository that asked for declines.
///
/// The unit twin proves the arithmetic; this proves the route. A read with no
/// `limit` is what an agent overwhelmingly sends, and until the client's
/// delivery was pinned it was recorded as one line — a coverage that could
/// never satisfy a repeat, so the largest pool there is was invisible to the
/// lever while every part of it worked. Here the identical repeat is declined
/// once, the reason names the file's whole extent, and the read that follows
/// the decline runs.
#[test]
fn a_limit_less_read_holds_the_whole_file_it_was_delivered() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("whole.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();
    let whole = read_whole_of(&file);

    let first = hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        1,
        &whole,
        &env,
    );
    assert_eq!(declined_with(&first), None, "the first read is the saving");

    let repeat = hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        2,
        &whole,
        &env,
    );
    let reason = declined_with(&repeat).expect("the identical repeat is declined");
    assert!(
        reason.contains("lines 1-40"),
        "the frame names the whole file the window was handed: {reason}"
    );

    // One decline per key: the model that answers a decline by asking again
    // is not held there.
    assert_eq!(
        declined_with(&hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            3,
            &whole,
            &env
        )),
        None,
        "the same key is declined once and never twice"
    );

    assert_eq!(kinds(&repo), vec!["read", "deny", "read"]);
}

/// ★ An `offset` with no `limit` is the same delivery starting later, and it
/// answers only for the part of the file it began at.
#[test]
fn a_limit_less_read_from_an_offset_holds_the_tail_and_not_the_head() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("tail.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();
    let tail = read_from(&file, 20);

    hook(&repo, "claude-code", "PreToolUse", "gate-1", 1, &tail, &env);
    let reason = declined_with(&hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        2,
        &tail,
        &env,
    ))
    .expect("the identical repeat is declined");
    assert!(
        reason.contains("lines 20-40"),
        "the frame names the tail, not the file: {reason}"
    );

    // The head was never delivered, so it is not a repeat.
    assert_eq!(
        declined_with(&hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            3,
            &read_whole_of(&file),
            &env
        )),
        None,
        "a window handed lines 20-40 does not hold lines 1-19"
    );
}

/// ★ A limit-less read past the delivery bound runs without coverage.
///
/// The file is larger than the size at which the client's own token cap could
/// have cut the delivery short, so what the window is credited with is the
/// no line at all. Even the first line might have been truncated, so claiming
/// it would make a later repeat actionable without delivery evidence.
#[test]
fn a_limit_less_read_past_the_delivery_bound_runs_without_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("large.txt");
    let body = numbered(6_000, None);
    assert!(
        body.len() > 47_500,
        "the file has to be past the delivery bound to test it: {}",
        body.len()
    );
    std::fs::write(&file, body).unwrap();
    let whole = read_whole_of(&file);

    for turn in [1, 2, 3] {
        assert_eq!(
            declined_with(&hook(
                &repo,
                "claude-code",
                "PreToolUse",
                "gate-1",
                turn,
                &whole,
                &env
            )),
            None,
            "a delivery this large is never vouched for, so never declined"
        );
    }
    assert!(windows(&repo).is_empty(), "no coverage journal is created");
}

/// A `PostToolUse` result for a limit-less read that states a **complete**
/// delivery: the run the client handed over and the file's own length, with no
/// truncation flag beside them — the shape O7 of the pinned tool-field surface
/// records for a read the token cap never reached.
fn whole_result_of(path: &Path, delivered: u32, total: u32) -> String {
    format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{p}}},"tool_response":{{"type":"text","file":{{"filePath":{p},"startLine":1,"numLines":{delivered},"totalLines":{total}}}}}}}"#,
        p = json_str(path)
    )
}

/// ★ **A delivery past the size bound becomes coverage from the client's own
/// statement** — through the shipped binary, in a repository that has asked
/// for declines.
///
/// The walk above is the state this one leaves: a file too large for a
/// measured density to vouch for is recorded as nothing, so every later read
/// of it starts from empty coverage, and the largest and most-read files in a
/// repository are exactly the ones the lever could never reach. Here the
/// client's own result answers the question density cannot — it names the run
/// it handed over and the file's own length, and the two agreeing is the
/// client saying the call was answered in full.
///
/// Three claims, in the order they happen: the pre-call boundary is unchanged,
/// because a decline must never rest on a delivery that has not happened yet;
/// the result credits the file's whole extent one hook later; and a two
/// hundred line range inside it is then a range the window provably holds.
#[test]
fn a_read_past_the_delivery_bound_is_credited_from_the_result_that_states_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("big.txt");
    let body = numbered(6_000, None);
    assert!(
        body.len() > 47_500,
        "the file has to be past the delivery bound to test it: {}",
        body.len()
    );
    std::fs::write(&file, body).unwrap();

    let whole = read_whole_of(&file);
    assert_eq!(
        declined_with(&hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            1,
            &whole,
            &env
        )),
        None,
        "nothing is declined on a delivery that has not happened yet"
    );
    assert!(
        windows(&repo).is_empty(),
        "and nothing is credited before it: the pre-call boundary is unmoved"
    );

    hook(
        &repo,
        "claude-code",
        "PostToolUse",
        "gate-1",
        2,
        &whole_result_of(&file, 6_001, 6_001),
        &env,
    );
    assert_eq!(
        kinds(&repo),
        vec!["read"],
        "the statement credits the delivery the size rule refused"
    );

    let reason = declined_with(&hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        3,
        &read_of(&file, 100, 200),
        &env,
    ))
    .expect("a range inside a delivery the client vouched for is declinable");
    assert!(
        reason.contains("lines 100-299"),
        "the reason names the lines it is about: {reason}"
    );
    assert_eq!(kinds(&repo), vec!["read", "deny"]);
}

/// One pathological line larger than the verified delivery channel is the
/// strictest truncation case: it cannot safely be reduced to one line.
#[test]
fn a_pathological_long_line_runs_without_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("one-long-line.txt");
    std::fs::write(&file, "x".repeat(48_000)).unwrap();

    let read = read_of(&file, 1, 1);
    for turn in [1, 2] {
        assert_eq!(
            declined_with(&hook(
                &repo,
                "claude-code",
                "PreToolUse",
                "gate-1",
                turn,
                &read,
                &env,
            )),
            None
        );
    }
    assert!(windows(&repo).is_empty());
}

//! End to end, through the shipped binary: what a repository that has asked
//! for declines actually answers an agent with.
//!
//! One subject, across every family that has a decision at all: the repeat
//! that is declined and the read that is not, the verification read and the
//! sites its reason names, the subagent's own window, the sealed frame a
//! second family reads, the command line a third one reads through, and the
//! turns the journal records them all under.
//!
//! What a window may be credited with having been delivered is
//! `read_gate_deliveries_e2e`; what a decision books is `read_gate_e2e`.
//! How every walk here is driven is `common/read_gate_fixture.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;
use std::process::Command;

mod common;
use common::rift_cmd;

#[allow(dead_code)] // The read-gate suites share these builders; each file uses a subset.
#[path = "common/read_gate_fixture.rs"]
mod read_gate_fixture;
use read_gate_fixture::{
    declined_with, edit_inserting_line_7b, enforce_for, gate_env, hook, json_str, kinds, kinds_of,
    native_hook, numbered, read_of, seed_repo, windows, write_policy,
};

/// The turns the one window in this repository recorded its lines in, in order.
fn turns(repo: &Path) -> Vec<u64> {
    let names = windows(repo);
    let [only] = names.as_slice() else {
        panic!("one context window, got {names:?}");
    };
    std::fs::read_to_string(repo.join(".rift").join("state").join("reads").join(only))
        .unwrap()
        .lines()
        .map(|row| {
            serde_json::from_str::<serde_json::Value>(row).unwrap()["t"]
                .as_u64()
                .unwrap()
        })
        .collect()
}

/// The shell command a Codex `updatedInput` replacement carries, or `None`.
fn updated_command(stdout: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()?
        .pointer("/hookSpecificOutput/updatedInput/command")?
        .as_str()
        .map(str::to_owned)
}

/// ★ The repeat walk a repository in `enforce` puts a Claude Code session
/// through, in one session, in order.
///
/// Every clause is a separate promise, asserted where it happens rather than
/// in a unit's idea of it: the first read is the saving and is never
/// declined; the identical repeat is; a part of the file the window was never
/// handed is not; and a compaction empties the window, after which the same
/// read is a first read once more.
#[test]
fn the_enforcing_walk_declines_a_repeat_once_and_a_compaction_reopens_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("wide.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();

    let head = read_of(&file, 1, 10);
    let first = hook(&repo, "claude-code", "PreToolUse", "gate-1", 1, &head, &env);
    assert_eq!(declined_with(&first), None, "the first read is the saving");

    let repeat = hook(&repo, "claude-code", "PreToolUse", "gate-1", 2, &head, &env);
    let reason = declined_with(&repeat).expect("the identical repeat is declined");
    assert!(
        reason.contains("lines 1-10"),
        "the reason names the lines it is about: {reason}"
    );
    assert!(
        !reason.contains("line 4"),
        "identifiers and numbers, never a byte of the file: {reason}"
    );

    let elsewhere = read_of(&file, 20, 10);
    let other = hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        3,
        &elsewhere,
        &env,
    );
    assert_eq!(
        declined_with(&other),
        None,
        "a part of the file this window was never handed is not a repeat"
    );

    hook(&repo, "claude-code", "PreCompact", "gate-1", 4, "{}", &env);
    assert_eq!(
        declined_with(&hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            5,
            &head,
            &env
        )),
        None,
        "a compacted window holds nothing, so the same read is a first read again"
    );

    assert_eq!(
        kinds(&repo),
        vec!["read", "deny", "read", "evict", "read"],
        "the record the walk left behind"
    );
}

/// ★ The verification read — the largest measured pool this whole mechanism
/// exists for — is declined once, with the places the file changed, and both
/// ways out of it stay open.
///
/// The agent's own payload carried the new bytes and Claude Code's edit
/// result echoes them back, so the information the verification read is
/// after is already in the model's context. What it gets instead is the
/// sites in the numbering it is about to see, the shift, and a window worth
/// reading — and then the narrow read the reason asks for runs, as does the
/// identical repeat, which is what bounds a model that answers a decline by
/// asking again.
#[test]
fn a_verification_read_is_declined_once_with_its_sites_and_the_ways_out_are_open() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("wide.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();
    let at = |turn: u32, verb: &str, payload: &str| {
        hook(&repo, "claude-code", verb, "gate-1", turn, payload, &env)
    };

    let whole = read_of(&file, 1, 500);
    at(1, "PreToolUse", &whole);
    std::fs::write(&file, numbered(40, Some("line 7b"))).unwrap();
    at(2, "PostToolUse", &edit_inserting_line_7b(&file));

    let verify = at(3, "PreToolUse", &whole);
    let delta = declined_with(&verify).expect("the verification read is declined");
    assert_the_delta_names_the_sites_and_no_bytes(&delta);

    // Exactly the changed site, which is what the reason asks for; a window
    // that spilled past it would be a different request with its own answer.
    let narrow = read_of(&file, 7, 2);
    assert_eq!(
        declined_with(&at(4, "PreToolUse", &narrow)),
        None,
        "reading the changed lines is the behaviour the reason asks for"
    );
    assert_eq!(
        declined_with(&at(5, "PreToolUse", &whole)),
        None,
        "one decline per call, so the repeat is honoured"
    );

    assert_eq!(
        kinds(&repo),
        vec!["read", "edit", "deny", "read", "read"],
        "declined once, then handed the bytes twice and recorded as holding them"
    );
}

/// The four clauses the delta a verification read is declined with carries,
/// asserted together because each of them is a separate promise and a reason
/// that dropped one of them would still be a reason.
fn assert_the_delta_names_the_sites_and_no_bytes(delta: &str) {
    assert!(
        delta.contains("changed only at lines 7-8"),
        "the places, in the numbering it is about to see: {delta}"
    );
    assert!(delta.contains("(+1 lines)"), "and the shift: {delta}");
    assert!(
        delta.contains("Read lines"),
        "and a window inside the file: {delta}"
    );
    assert!(
        !delta.contains("line 7b"),
        "never a byte of what changed: {delta}"
    );
}

/// ★ A subagent's reads are its own window's, not its parent's.
///
/// A subagent's hooks arrive under the parent's session id and carry their own
/// agent id, and the two context windows hold different bytes. Telling the
/// parent it holds what only the child was handed would decline a read of
/// content the parent has never seen — the one error that costs an agent its
/// content rather than a saving.
#[test]
fn a_subagents_reads_are_never_the_parents_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("shared.txt");
    std::fs::write(&file, "a\nb\nc\nd\n").unwrap();
    let read = read_of(&file, 1, 4);
    let child = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":1,"limit":4}},"agent_id":"child-1"}}"#,
        json_str(&file)
    );

    hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        1,
        &child,
        &env,
    );
    let parent = hook(&repo, "claude-code", "PreToolUse", "gate-1", 2, &read, &env);
    assert_eq!(
        declined_with(&parent),
        None,
        "the parent never saw those bytes: {parent}"
    );
    let names = windows(&repo);
    assert_eq!(names.len(), 2, "two windows, two journals: {names:?}");
    for window in &names {
        assert_eq!(
            kinds_of(&repo, window),
            vec!["read"],
            "each window holds exactly the read it was handed"
        );
    }
}

/// ★ A whole-file write whose bytes something reformatted on the way in
/// grants the window nothing.
///
/// The expectation recorded before the write is the hash of the payload's own
/// bytes; the file's hash afterwards is what really landed. Equality is what
/// separates "the agent holds what it wrote" from "a formatter rewrote it",
/// and without equality there is no coverage to decline a later read on.
#[test]
fn a_write_a_formatter_rewrote_grants_the_window_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("fmt.txt");
    let write = format!(
        r#"{{"tool_name":"Write","tool_input":{{"file_path":{},"content":"a\nb\n"}}}}"#,
        json_str(&file)
    );

    hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        1,
        &write,
        &env,
    );
    // What really landed is not what the payload carried.
    std::fs::write(&file, "a\nb\n\n").unwrap();
    hook(
        &repo,
        "claude-code",
        "PostToolUse",
        "gate-1",
        2,
        &write,
        &env,
    );

    let read = read_of(&file, 1, 3);
    assert_eq!(
        declined_with(&hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            3,
            &read,
            &env
        )),
        None,
        "the window holds bytes it never saw, so the read is the only way to see them"
    );
    assert_eq!(kinds(&repo), vec!["expect", "edit", "read"]);
}

/// ★ A decline the client ignored is recorded as one, and the window is told
/// the truth about what the model now holds.
///
/// The bytes reached the model however the decision was answered, so the
/// honest record is that the window holds them — and the decline standing
/// just before that read is the pair a diagnostic counts. Nothing about it
/// declines the next call again.
#[test]
fn a_decline_the_client_ignored_records_the_read_it_could_not_stop() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("ignored.txt");
    std::fs::write(&file, "a\nb\nc\n").unwrap();
    let read = read_of(&file, 1, 3);

    hook(&repo, "claude-code", "PreToolUse", "gate-1", 1, &read, &env);
    let denied = hook(&repo, "claude-code", "PreToolUse", "gate-1", 2, &read, &env);
    assert!(declined_with(&denied).is_some(), "the repeat is declined");
    // The client ran the call anyway and reported its result.
    let result = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":1,"limit":3}},"tool_response":{{"file":{{"filePath":{}}}}}}}"#,
        json_str(&file),
        json_str(&file)
    );
    hook(
        &repo,
        "claude-code",
        "PostToolUse",
        "gate-1",
        3,
        &result,
        &env,
    );
    assert_eq!(
        kinds(&repo),
        vec!["read", "deny", "read"],
        "a decline followed by the read it could not stop is the pair a diagnostic counts"
    );
}

/// ★ Antigravity's own read verb, with the ranges its arguments carry, is
/// recorded — and declined only Plain, never on its own edit.
///
/// The family is named `enforce` here, which is the only way a decline
/// reaches it. What it may be declined on is a repeat of lines it was handed;
/// what it may never be declined on is the read that verifies its own edit,
/// because nothing pins what its edit tool returns and that read is the only
/// sight of the change the agent has.
#[test]
fn an_antigravity_view_is_recorded_with_its_ranges_and_declined_only_on_a_repeat() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["antigravity"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("agy.txt");
    std::fs::write(&file, "a\nb\nc\nd\n").unwrap();
    let view = format!(
        r#"{{"toolCall":{{"name":"view_file","args":{{"AbsolutePath":{},"StartLine":0,"EndLine":3}}}}}}"#,
        json_str(&file)
    );

    let first = hook(&repo, "antigravity", "PreToolUse", "gate-1", 1, &view, &env);
    assert_eq!(declined_with(&first), None, "the first read is the saving");
    let repeat = hook(&repo, "antigravity", "PreToolUse", "gate-1", 2, &view, &env);
    let reason = declined_with(&repeat).expect("a named family is declined on a plain repeat");
    assert!(
        reason.contains("lines 1-3"),
        "the ranges the arguments carried, in this reader's numbering — a start of \
         0 and an end of 3 is three lines: {reason}"
    );
    assert!(
        !reason.contains("changed only at"),
        "a plain repeat is never dressed as a delta: {reason}"
    );
    assert_eq!(kinds(&repo), vec!["read", "deny"]);
}

/// An installed-client `ContentOffset` call remains completely fail-open.
/// Its line arguments cannot be credited because the offset's units and the
/// delivered result channel have not both been pinned.
#[test]
fn an_antigravity_content_offset_view_never_records_observes_or_declines() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["antigravity"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("agy-large.txt");
    std::fs::write(&file, "a\nb\nc\nd\n").unwrap();
    let view = format!(
        r#"{{"toolCall":{{"name":"view_file","args":{{"AbsolutePath":{},"ContentOffset":230000,"StartLine":0,"EndLine":3}}}}}}"#,
        json_str(&file)
    );

    for turn in [1, 2] {
        assert_eq!(
            declined_with(&hook(
                &repo,
                "antigravity",
                "PreToolUse",
                "gate-1",
                turn,
                &view,
                &env,
            )),
            None,
            "unknown pagination must not become actionable"
        );
    }
    assert!(windows(&repo).is_empty());
}

/// ★ Antigravity is never declined on the read that verifies its own edit.
#[test]
fn an_antigravity_verification_read_is_recorded_and_never_declined() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["antigravity"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("agy-edit.txt");
    std::fs::write(&file, "a\nold\nc\n").unwrap();
    let view = format!(
        r#"{{"toolCall":{{"name":"view_file","args":{{"AbsolutePath":{},"StartLine":0,"EndLine":500}}}}}}"#,
        json_str(&file)
    );
    let replace = format!(
        r#"{{"toolCall":{{"name":"replace_file_content","args":{{"TargetFile":{},"TargetContent":"old\n","ReplacementContent":"new\nnewer\n","AllowMultiple":false}}}}}}"#,
        json_str(&file)
    );

    hook(&repo, "antigravity", "PreToolUse", "gate-1", 1, &view, &env);
    std::fs::write(&file, "a\nnew\nnewer\nc\n").unwrap();
    hook(
        &repo,
        "antigravity",
        "PostToolUse",
        "gate-1",
        2,
        &replace,
        &env,
    );
    let verify = hook(&repo, "antigravity", "PreToolUse", "gate-1", 3, &view, &env);
    assert_eq!(
        declined_with(&verify),
        None,
        "a decline here would remove this agent's only sight of its own edit: {verify}"
    );
    assert_eq!(
        kinds(&repo),
        vec!["read", "edit", "read"],
        "the edit is recorded and the read runs; only the decision is absent"
    );
}

/// The declined-call reason this family's hook wrote, or `None`.
///
/// A separate reader from [`declined_with`] on purpose: that one accepts
/// either pinned envelope, which is what a walk driving two families wants,
/// and this one accepts only the flat object this family's artifact pins. A
/// decline that arrived in the other family's shape would pass the forgiving
/// reader and be a broken agent on the wire.
fn declined_as_this_family_is_declined(stdout: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    assert!(
        value.get("hookSpecificOutput").is_none(),
        "this family's response is a flat object: {stdout}"
    );
    if value.get("decision")?.as_str()? != "deny" {
        return None;
    }
    Some(value.get("reason")?.as_str()?.to_owned())
}

/// A `view_file` of `path` over the argument names this family pins.
fn view_of(path: &Path, start: u32, end: u32) -> String {
    format!(
        r#"{{"toolCall":{{"name":"view_file","args":{{"AbsolutePath":{},"StartLine":{start},"EndLine":{end}}}}}}}"#,
        json_str(path)
    )
}

/// ★ A named Antigravity session is declined **once** on a repeat of what it
/// holds, with the sealed frame, and the identical call after that runs.
///
/// This is the whole mechanism on this family in one walk, and every clause is
/// a promise the recorded runs behind the pin were about: the explanation
/// arrives in the field the artifact names, it is the renderer's own frame
/// (opener, digest, range, the way out, the cheaper command) and not a
/// sentence composed at the call site, it carries no byte of the file, and the
/// call that follows the decline is honoured — which is what bounds a model
/// that answers a decline by asking again to exactly one extra round trip.
/// The measured runs say this family does exactly that, every time, which is
/// why nothing here reaches it unless a repository names it.
#[test]
fn an_antigravity_repeat_is_declined_once_with_the_sealed_frame_and_then_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["antigravity"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("same.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();
    let whole = view_of(&file, 0, 40);
    let at = |turn: u32| {
        hook(
            &repo,
            "antigravity",
            "PreToolUse",
            "gate-1",
            turn,
            &whole,
            &env,
        )
    };

    assert_eq!(
        declined_as_this_family_is_declined(&at(1)),
        None,
        "the first read is the saving, never the decline"
    );

    let reason =
        declined_as_this_family_is_declined(&at(2)).expect("the identical repeat is declined");
    assert_the_sealed_frame(&reason);

    assert_eq!(
        declined_as_this_family_is_declined(&at(3)),
        None,
        "one decline per call: the measured retry costs one round trip and stops"
    );
    assert_eq!(
        kinds(&repo),
        vec!["read", "deny", "read"],
        "declined once, then handed the bytes and recorded as holding them"
    );

    assert_eq!(
        reason,
        the_confirmed_familys_frame_for_the_same_repeat(),
        "one renderer writes what a model reads: two envelopes, one \
         explanation, byte for byte"
    );
}

/// Every clause the sealed frame is made of: the renderer's own opener, the
/// file at the bytes it was at, the lines, the way out, the cheaper command —
/// and no byte of the file. A frame that dropped one of them would still read
/// as a frame, which is why each is named here rather than summarised.
fn assert_the_sealed_frame(reason: &str) {
    assert!(
        reason.starts_with("[rift] Read skipped:"),
        "the renderer's own opener, not a sentence written at the call site: {reason}"
    );
    assert!(
        reason.contains("same.txt@sha256:") && reason.contains("lines 1-40"),
        "the file, the bytes it was at, and the lines: {reason}"
    );
    assert!(
        reason.contains("Repeat this exact call if you still need it (it is allowed once)."),
        "the way out stays open in the text as well as in the journal: {reason}"
    );
    assert!(
        !reason.contains("rift recall changed-hunks"),
        "no pull against a hash the range is unchanged at: it would be empty: {reason}"
    );
    assert!(
        !reason.contains("line 12"),
        "identifiers and numbers, never a byte of the file: {reason}"
    );
}

/// The reason the family whose channel was confirmed first is declined with,
/// for the identical repeat of the identical file.
///
/// Its own repository, so the file is named the same way in both walks and a
/// difference in the text can only be a difference in the renderer. A
/// family-shaped wording would be a second thing to review and a second thing
/// to red-team, and neither has ever been written.
fn the_confirmed_familys_frame_for_the_same_repeat() -> String {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("same.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();
    let read = read_of(&file, 1, 40);
    hook(&repo, "claude-code", "PreToolUse", "gate-1", 1, &read, &env);
    let declined = hook(&repo, "claude-code", "PreToolUse", "gate-1", 2, &read, &env);
    declined_with(&declined).expect("that family is declined on the same repeat")
}

/// ★ A view of lines this window was handed is declined; a view one line past
/// them runs.
///
/// The ranges are this family's own, in the numbering its adapter derives from
/// arguments whose base nothing published, so the walk is the only place the
/// two halves — what the arguments meant and what the window holds — are held
/// against each other end to end.
#[test]
fn an_antigravity_view_inside_what_it_holds_is_declined_and_one_past_it_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["antigravity"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("ranged.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();

    // Lines 1-20.
    let head = view_of(&file, 0, 20);
    hook(&repo, "antigravity", "PreToolUse", "gate-1", 1, &head, &env);

    // Lines 5-10, wholly inside them.
    let inside = view_of(&file, 4, 10);
    let declined = hook(
        &repo,
        "antigravity",
        "PreToolUse",
        "gate-1",
        2,
        &inside,
        &env,
    );
    let reason = declined_as_this_family_is_declined(&declined)
        .expect("a range inside what this window holds is declined");
    assert!(
        reason.contains("lines 5-10"),
        "the lines asked for, not the lines held: {reason}"
    );

    // Lines 21-30, which this window has never been handed.
    let past = view_of(&file, 20, 30);
    assert_eq!(
        declined_as_this_family_is_declined(&hook(
            &repo,
            "antigravity",
            "PreToolUse",
            "gate-1",
            3,
            &past,
            &env
        )),
        None,
        "one line past the coverage is a read this window does not hold"
    );
    assert_eq!(kinds(&repo), vec!["read", "deny", "read"]);
}

/// ★ The policy this product ships actively enforces Antigravity's pinned
/// reason channel even when no policy file exists.
#[test]
fn the_shipped_policy_enforces_antigravity_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let env = gate_env(&tmp.path().join("cache"));
    assert!(
        !repo
            .join(".rift")
            .join("policies")
            .join("serve.toml")
            .exists(),
        "this walk is about the absence of a policy file"
    );
    let file = repo.join("default.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();
    let whole = view_of(&file, 0, 40);

    let first = hook(
        &repo,
        "antigravity",
        "PreToolUse",
        "gate-1",
        1,
        &whole,
        &env,
    );
    assert_eq!(declined_as_this_family_is_declined(&first), None);
    let repeat = hook(
        &repo,
        "antigravity",
        "PreToolUse",
        "gate-1",
        2,
        &whole,
        &env,
    );
    assert!(
        declined_as_this_family_is_declined(&repeat).is_some(),
        "the unchanged repeat is declined under the active default: {repeat}"
    );
    assert_eq!(kinds(&repo), vec!["read", "deny"]);
}

/// ★ A repository-wide `enforce` reaches Antigravity without a per-family
/// override, and status reports the requested and effective modes truthfully.
#[test]
fn a_repository_wide_enforce_reaches_antigravity_without_naming_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let env = gate_env(&tmp.path().join("cache"));
    write_policy(&repo, &[], "min_slice_lines = 1\n");
    let file = repo.join("wide-enforce.txt");
    std::fs::write(&file, "a\nb\nc\nd\n").unwrap();
    let whole = view_of(&file, 0, 4);

    let first = hook(
        &repo,
        "antigravity",
        "PreToolUse",
        "gate-1",
        1,
        &whole,
        &env,
    );
    assert_eq!(declined_as_this_family_is_declined(&first), None);
    let repeat = hook(
        &repo,
        "antigravity",
        "PreToolUse",
        "gate-1",
        2,
        &whole,
        &env,
    );
    assert!(
        declined_as_this_family_is_declined(&repeat).is_some(),
        "{repeat}"
    );
    assert_eq!(kinds(&repo), vec!["read", "deny"]);

    // What the operator is told, in the same state.
    let status = rift_cmd(&repo)
        .args(["recall", "status", "--json"])
        .envs(env.iter().map(|(k, v)| (k.clone(), v.clone())))
        .output()
        .unwrap();
    assert!(status.status.success(), "status exits 0: {status:?}");
    let value: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    let row = value["policy"]["agents"]
        .as_array()
        .expect("a row per family")
        .iter()
        .find(|row| row["agent"] == "antigravity")
        .expect("this family has a row")
        .clone();
    assert_eq!(
        row["interception_requested"], "enforce",
        "what the file asked"
    );
    assert_eq!(row["interception"], "enforce", "what actually runs");
    assert_eq!(row["note"], "no native document/search amendment channel");
}

/// ★ A relative read is resolved against the directory the payload states,
/// and against nothing else.
///
/// Antigravity sends no working directory at all: what its payload carries is
/// the IDE's workspace root, under its own source token, and a workspace root
/// is where a project lives rather than where a command ran. The hook process
/// is not an answer either — a client launches it inside the repository, so
/// its own working directory would resolve almost any relative name and would
/// be a guess every time it did.
///
/// Both wrong answers are planted here: the workspace root IS this repository
/// and the hook process IS running inside it, so either fallback would resolve
/// `agy-rel.txt` and record a read of it. Nothing is recorded, and the same
/// file named absolutely is recorded in the same walk — so the test cannot
/// pass because the payload was malformed.
#[test]
fn a_workspace_root_never_resolves_a_relative_read() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["antigravity"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("agy-rel.txt");
    std::fs::write(&file, "a\nb\nc\nd\n").unwrap();
    let here = repo.canonicalize().unwrap();
    let view = |named: &str| {
        format!(
            r#"{{"workspacePaths":[{}],"toolCall":{{"name":"view_file","args":{{"AbsolutePath":{named},"StartLine":0,"EndLine":3}}}}}}"#,
            json_str(&here)
        )
    };

    let relative = view("\"agy-rel.txt\"");
    for turn in [1, 2] {
        let out = hook(
            &repo,
            "antigravity",
            "PreToolUse",
            "gate-1",
            turn,
            &relative,
            &env,
        );
        assert_eq!(
            declined_with(&out),
            None,
            "a name this payload never located is not this checkout's: {out}"
        );
    }
    assert!(
        windows(&repo).is_empty(),
        "a workspace root and the hook's own directory are both guesses, and a \
         guess must record nothing: {:?}",
        windows(&repo)
    );

    let absolute = view(&json_str(&file));
    hook(
        &repo,
        "antigravity",
        "PreToolUse",
        "gate-1",
        3,
        &absolute,
        &env,
    );
    assert_eq!(
        kinds(&repo),
        vec!["read"],
        "the same file, named where this payload can locate it, is recorded"
    );
}

/// ★ A Codex read is a command line the strict grammar spells, it is
/// recorded, and actively enforced without a per-family override. The
/// relative spelling is driven too, because a shell read is usually written
/// relative to the directory the payload states.
#[test]
fn a_codex_shell_read_is_recorded_and_enforced_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    // Deliberately no policy file at all: the shipped default is what this
    // walk is about.
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("cdx.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();
    let here = repo.canonicalize().unwrap();
    let absolute = format!(
        r#"{{"tool_name":"Bash","tool_input":{{"command":"sed -n '1,40p' {}"}}}}"#,
        common::shell_spelling(&here.join("cdx.txt"))
    );
    let relative = format!(
        r#"{{"tool_name":"Bash","tool_input":{{"command":"sed -n '1,40p' cdx.txt"}},"cwd":{}}}"#,
        json_str(&here)
    );

    assert_eq!(
        declined_with(&hook(
            &repo,
            "codex",
            "PreToolUse",
            "gate-1",
            1,
            &absolute,
            &env
        )),
        None
    );
    let repeat = hook(&repo, "codex", "PreToolUse", "gate-1", 2, &relative, &env);
    let replacement = updated_command(&repeat).expect("the strict repeat is replaced");
    assert!(replacement.contains("printf '%s\\n'") && replacement.contains("Read skipped"));
    let delivered = Command::new("sh")
        .arg("-c")
        .arg(&replacement)
        .current_dir(&repo)
        .output()
        .expect("the native client can execute the replacement command");
    assert!(delivered.status.success());
    let delivered = String::from_utf8(delivered.stdout).expect("the bounded result is UTF-8");
    assert!(delivered.contains("[rift] Read skipped:") && delivered.contains("lines 1-40"));
    assert!(
        !delivered.contains("line 12"),
        "the replacement prints the bounded Rift result, never the raw file"
    );
    assert_eq!(
        declined_with(&hook(
            &repo,
            "codex",
            "PreToolUse",
            "gate-1",
            3,
            &relative,
            &env
        )),
        None
    );
    assert_eq!(
        kinds(&repo),
        vec!["read", "deny", "read"],
        "both spellings name the same lines; one denial then opens the safety retry"
    );
}

/// ★ The walk a repository that **names** Codex puts a session through, in
/// one session, in order, through the shipped binary.
///
/// This family reads through a shell and answers on the same envelope Claude
/// Code does, so the whole of what is new here is which command lines the
/// grammar credits with which bytes or lines. A byte-exact `cat` establishes
/// complete coverage, an explicitly ranged read establishes line coverage, a
/// decline happens once per key, and a read that verifies a patch is not
/// declined because nothing pins what this family's patch tool told the model
/// it wrote.
#[test]
fn the_enforcing_walk_declines_a_shell_read_once_and_leaves_a_patch_verified() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["codex"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("wide.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();
    let path = repo.canonicalize().unwrap().join("wide.txt");
    let ranged_file = repo.join("ranged.txt");
    std::fs::write(&ranged_file, numbered(40, None)).unwrap();
    let ranged_path = repo.canonicalize().unwrap().join("ranged.txt");
    let shell = |command: String| {
        serde_json::json!({"tool_name": "Bash", "tool_input": {"command": command}}).to_string()
    };
    let whole_file = shell(format!("cat {}", common::shell_spelling(&path)));
    let ranged = shell(format!(
        "sed -n '1,40p' {}",
        common::shell_spelling(&ranged_path)
    ));
    let verify_patch = shell(format!("sed -n '1,40p' {}", common::shell_spelling(&path)));
    let at = |turn: u32, payload: &str| {
        hook(&repo, "codex", "PreToolUse", "gate-1", turn, payload, &env)
    };

    assert_eq!(
        declined_with(&at(1, &whole_file)),
        None,
        "a byte-exact whole-file read runs and establishes complete coverage"
    );
    assert!(
        updated_command(&at(2, &whole_file)).is_some_and(|command| command.contains("wide.txt")),
        "the proven unchanged whole-file repeat is replaced exactly once"
    );
    assert_eq!(
        declined_with(&at(3, &whole_file)),
        None,
        "the identical raw retry is the one-deny safety escape"
    );

    assert_eq!(
        declined_with(&at(4, &ranged)),
        None,
        "the first explicitly ranged delivery establishes its own coverage key"
    );
    assert!(
        updated_command(&at(5, &ranged)).is_some_and(|command| command.contains("lines 1-40")),
        "the proven unchanged ranged repeat is replaced exactly once"
    );

    // The patch, then the read that checks it. Nothing pins what this family's
    // patch tool returns, so that read is the agent's only sight of its own
    // change and is never taken away.
    std::fs::write(&file, numbered(40, Some("line 7b"))).unwrap();
    let applied = serde_json::json!({
        "tool_name": "apply_patch",
        "tool_input": {
            "command": format!(
                "*** Begin Patch\n*** Update File: {}\n@@\n line 7\n+line 7b\n*** End Patch\n",
                path.display()
            ),
        },
    })
    .to_string();
    hook(&repo, "codex", "PostToolUse", "gate-1", 6, &applied, &env);
    assert_eq!(
        declined_with(&at(7, &verify_patch)),
        None,
        "the read that verifies a patch runs on a family whose patch tool \
         says nothing back"
    );

    assert_eq!(
        kinds(&repo),
        vec!["read", "deny", "read", "read", "deny", "edit", "read"],
        "the record the walk left behind"
    );
}

/// ★ **Two reads on either side of a `UserPromptSubmit` carry different
/// turns** — through the shipped binary, on the wire shape the family really
/// sends, which carries no turn at all.
///
/// The boundary hook is what advances the ordinal, in the session cache the
/// ingest layer keeps; the gate reads that file and nothing heavier. Before
/// this walk existed every line an alpha family recorded said turn 0, which
/// left the "unchanged since turn N" frame naming a turn nobody had, and
/// flattened both figures that are counted per turn.
#[test]
fn two_reads_across_a_prompt_boundary_carry_different_turns() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = gate_env(&tmp.path().join("cache"));
    let file = repo.join("turns.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();

    let prompt =
        r#"{"session_id":"gate-turns","hook_event_name":"UserPromptSubmit","prompt":"work"}"#;
    let read = |lo: u32, hi: u32| {
        format!(
            r#"{{"session_id":"gate-turns","hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{{"file_path":{},"offset":{lo},"limit":{hi}}}}}"#,
            json_str(&file)
        )
    };

    let stop = r#"{"session_id":"gate-turns","hook_event_name":"Stop"}"#;
    native_hook(&repo, "claude-code", "UserPromptSubmit", prompt, &env);
    native_hook(&repo, "claude-code", "PreToolUse", &read(1, 5), &env);
    // The turn the first read belongs to ends, and the next prompt opens the
    // next ordinal — which is the boundary this walk is written about.
    native_hook(&repo, "claude-code", "Stop", stop, &env);
    native_hook(&repo, "claude-code", "UserPromptSubmit", prompt, &env);
    native_hook(&repo, "claude-code", "PreToolUse", &read(10, 5), &env);

    let recorded = turns(&repo);
    assert_eq!(recorded.len(), 2, "one line per read: {recorded:?}");
    assert!(
        recorded[0] < recorded[1],
        "the second read is a later turn than the first: {recorded:?}"
    );
    assert!(
        recorded[0] > 0,
        "and the first is a real turn, not the placeholder: {recorded:?}"
    );
}

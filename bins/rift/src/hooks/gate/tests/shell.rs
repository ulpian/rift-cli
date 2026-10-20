//! A shell command line is several reads: declined only when the window holds
//! every part of it, and decided on its reads rather than on its captions.

use std::path::{Path, PathBuf};

use rift_agent::window_journal::Line;
use rift_shadow::hooks::AdapterRegistry;

use crate::hooks::ingest::edit_record;

use super::fixtures::*;

/// A Codex checkout of forty numbered lines, with the lever named
/// deliberately — the only way this family is reached.
struct CodexShell {
    tmp: tempfile::TempDir,
    file: PathBuf,
    enforce: rift_recall_policy::Effective,
}

impl CodexShell {
    /// The sentence one command line is answered with, where one travels.
    fn deny_reason_for(&self, session: &str, command: &str) -> Option<String> {
        gate_under(
            &hook("codex", "PreToolUse", session, shell_of(command)),
            self.tmp.path(),
            &self.enforce,
        )
        .deny_reason
    }

    /// A `sed` range over the fixture, spelled the way the grammar pins it.
    fn ranged(&self, lo: u32, hi: u32) -> String {
        format!("sed -n '{lo},{hi}p' {}", shell_spelling(&self.file))
    }

    /// The same ranged command line three times over: the saving, the
    /// identical repeat, and the repeat after that.
    fn repeat_thrice(&self, session: &str) -> Vec<Option<String>> {
        let command = self.ranged(1, 40);
        (0..3)
            .map(|_| self.deny_reason_for(session, &command))
            .collect()
    }

    /// The kinds of line this window's journal holds, in order.
    fn kinds(&self, session: &str) -> Vec<&'static str> {
        lines_of(self.tmp.path(), session, None)
            .iter()
            .map(Line::kind)
            .collect()
    }
}

fn codex_shell() -> CodexShell {
    let (tmp, file) = checkout(40);
    let enforce = levers_for("codex", rift_recall_policy::Mode::Enforce);
    assert_eq!(
        enforce.read_deny,
        rift_recall_policy::Mode::Enforce,
        "named deliberately, which is the only way this family is reached"
    );
    CodexShell { tmp, file, enforce }
}

/// ★ **A read that arrives as a command line is declined once, and the
/// second identical command line runs.**
///
/// This family has no structured read tool, so every saving it can offer
/// runs through the strict grammar: what the journal holds came off a
/// command line, and so does the request matched against it. The first read
/// is the saving, the identical repeat is the decline, and the repeat after
/// that is honoured.
///
/// The spans are written the way the grammar pins them, which is why a
/// command line stating its own range is what this walk reads with.
#[test]
fn a_shell_read_of_lines_the_window_holds_is_declined_once() {
    let shell = codex_shell();
    let [first, repeat, again] = shell
        .repeat_thrice("s-codex")
        .try_into()
        .expect("three reads");
    assert_eq!(first, None, "the first read is the saving");
    let reason = repeat.expect("the identical repeat is declined");
    assert!(
        reason.contains("lines 1-40"),
        "the reason names the lines it is about: {reason}"
    );
    assert!(
        reason.contains("Repeat this exact call"),
        "and the way out of it: {reason}"
    );
    assert!(
        !reason.contains("line 4"),
        "identifiers and numbers, never a byte of the file: {reason}"
    );
    assert_eq!(
        again, None,
        "one decline per key: the model that asks again is answered"
    );
}

/// ★ A narrower run of lines the window already holds is a new key, declined
/// on its own account.
#[test]
fn a_narrower_run_of_held_lines_is_declined_on_its_own_account() {
    let shell = codex_shell();
    shell.repeat_thrice("s-codex");
    let narrower = shell.ranged(5, 20);
    assert!(
        shell
            .deny_reason_for("s-codex", &narrower)
            .is_some_and(|reason| reason.contains("lines 5-20")),
        "a narrower run of lines the window already holds is its own key"
    );
    assert_eq!(
        shell.kinds("s-codex"),
        vec!["read", "deny", "call", "read", "deny", "call"],
        "the record the walk left behind"
    );
}

/// ★ Plain `cat` has byte-exact whole-file delivery semantics. It can
/// therefore establish complete coverage, be declined once when unchanged,
/// and then take the universal identical-retry escape.
///
/// The clause this asserts rather than assumes: a whole-file spelling that
/// ends where the client's result cap ends, not where the file does, would
/// produce no line coverage at all.
#[test]
fn a_whole_file_command_line_covers_the_file_and_is_declined_once() {
    let shell = codex_shell();
    let command = format!("cat {}", shell_spelling(&shell.file));
    assert_eq!(shell.deny_reason_for("s-cat", &command), None);
    assert!(shell.deny_reason_for("s-cat", &command).is_some());
    assert_eq!(shell.deny_reason_for("s-cat", &command), None);
    assert_eq!(shell.kinds("s-cat"), vec!["read", "deny", "call", "read"]);
}

/// ★ **A command line that reads several files is one act, and is
/// declined only when declining every part of it would be right.**
///
/// This is the rule the composed grammar made necessary. A decline answers
/// the whole call, so a chain with one member the window does not hold has
/// to run: declining it would cost the agent reads that were never in
/// question, and amending the line to drop only the held members would
/// mean rewriting a command Rift did not write. When every member *is*
/// held, the reason is each member's own sentence from the one renderer
/// allowed to write one, a line apiece.
///
/// Both halves are walked, because a gate that declined nothing would pass
/// the first on its own.
#[test]
fn a_command_line_of_several_reads_is_declined_only_when_every_part_is_held() {
    let (tmp, first) = checkout(40);
    let here = tmp.path();
    let second = beside(here, "other.rs", 40);
    let third = beside(here, "third.rs", 40);
    let enforce = levers_for("codex", rift_recall_policy::Mode::Enforce);
    let chain = |a: &Path, b: &Path| {
        format!(
            "cd {} && sed -n '1,40p' {} && sed -n '1,40p' {}",
            shell_spelling(here),
            a.file_name().expect("a name").to_string_lossy(),
            b.file_name().expect("a name").to_string_lossy(),
        )
    };
    let pair = hook(
        "codex",
        "PreToolUse",
        "s-chain",
        shell_of(&chain(&first, &second)),
    );

    assert_eq!(
        gate_under(&pair, here, &enforce).deny_reason,
        None,
        "the first pass is the saving and is never declined"
    );
    assert_eq!(
        lines_of(here, "s-chain", None)
            .iter()
            .map(Line::kind)
            .collect::<Vec<_>>(),
        vec!["read", "read"],
        "every member's read is recorded, not only the first"
    );

    let reason = gate_under(&pair, here, &enforce)
        .deny_reason
        .expect("a repeat whose every member is held is declined");
    let sentences: Vec<&str> = reason.lines().collect();
    assert_eq!(sentences.len(), 2, "one member to a line: {reason}");
    assert!(
        sentences[0].contains("other.rs") || sentences[1].contains("other.rs"),
        "the reason names every member it is about: {reason}"
    );
    assert!(
        sentences.iter().all(|line| line.contains("lines 1-40")),
        "each line carries its own member's range: {reason}"
    );

    // The escape hatch is per member, so the identical repeat runs — the
    // same one-decline-per-key rule a single read gets.
    assert_eq!(gate_under(&pair, here, &enforce).deny_reason, None);

    // …and a chain reaching one file the window has never been handed runs
    // whole, however much of the rest of it is held.
    let mixed = hook(
        "codex",
        "PreToolUse",
        "s-chain",
        shell_of(&chain(&first, &third)),
    );
    assert_eq!(
        gate_under(&mixed, here, &enforce).deny_reason,
        None,
        "one unheld member and the whole line runs"
    );
    assert!(
        lines_of(here, "s-chain", None)
            .iter()
            .filter(|line| line.kind() == "deny")
            .count()
            == 2,
        "and the members that would have been declined on their own are \
         recorded as the reads they became"
    );
}

/// ★ **A captioned chain is decided on its reads and on nothing else.**
///
/// The `echo` lines an agent writes between two reads open no file, so
/// they reach this gate as nothing at all: the same two reads are
/// recorded, the same decline is reached when both are held, and the
/// reason has one sentence per **read** rather than one per member. That
/// last assertion is the one that would catch a label leaking into the
/// request list — it would show up as a third line nobody can write, or as
/// a member the resolution step silently dropped, which is the shape that
/// would keep the whole line from ever being declined.
#[test]
fn a_captioned_chain_is_decided_on_its_reads_and_not_its_captions() {
    let (tmp, first) = checkout(40);
    let here = tmp.path();
    let second = beside(here, "other.rs", 40);
    let enforce = levers_for("codex", rift_recall_policy::Mode::Enforce);
    let captioned = hook(
        "codex",
        "PreToolUse",
        "s-caption",
        shell_of(&format!(
            "cd {} && echo \"=== one ===\" && sed -n '1,40p' {} && echo && \
             sed -n '1,40p' {}",
            shell_spelling(here),
            first.file_name().expect("a name").to_string_lossy(),
            second.file_name().expect("a name").to_string_lossy(),
        )),
    );

    assert_eq!(
        gate_under(&captioned, here, &enforce).deny_reason,
        None,
        "the first pass is the saving"
    );
    assert_eq!(
        lines_of(here, "s-caption", None)
            .iter()
            .map(Line::kind)
            .collect::<Vec<_>>(),
        vec!["read", "read"],
        "two reads and two captions are two reads"
    );
    let reason = gate_under(&captioned, here, &enforce)
        .deny_reason
        .expect("a repeat whose every read is held is declined");
    assert_eq!(
        reason.lines().count(),
        2,
        "one sentence per read, and none for a caption: {reason}"
    );
    // A line that is captions and nothing else read nothing, so this gate
    // has nothing to resolve and nothing to record about it.
    let only_captions = hook(
        "codex",
        "PreToolUse",
        "s-caption",
        shell_of("echo \"=== one ===\" && echo done"),
    );
    assert_eq!(gate_under(&only_captions, here, &enforce).deny_reason, None);
    assert_eq!(
        lines_of(here, "s-caption", None)
            .iter()
            .filter(|line| line.kind() == "read")
            .count(),
        2,
        "no third read arrived from a line that read nothing"
    );
}

/// ★ Repository-wide enforcement reaches Codex and Claude Code equally;
/// observed retry behavior changes telemetry, not effective policy.
#[test]
fn repository_wide_enforce_reaches_codex_despite_retry_telemetry() {
    let (tmp, file) = checkout(40);
    let here = tmp.path();
    let mut wide = rift_recall_policy::ServePolicy::default();
    wide.interception.mode = rift_recall_policy::Mode::Enforce;
    wide.interception.min_slice_lines = 1;
    let for_family = |family: &str| {
        rift_recall_policy::resolve(
            &wide,
            family,
            crate::recall::family_capability(&rift_core::AgentId::new(family)),
        )
    };

    let levers = for_family("codex");
    assert_eq!(levers.read_deny, rift_recall_policy::Mode::Enforce);
    assert_eq!(levers.read_deny_cap, None);
    let bounded = hook(
        "codex",
        "PreToolUse",
        "s-wide",
        shell_of(&format!("sed -n '1,40p' {}", shell_spelling(&file))),
    );
    gate_under(&bounded, here, &levers);
    assert!(
        gate_under(&bounded, here, &levers).deny_reason.is_some(),
        "the unchanged repeat is declined despite its retry history"
    );
    assert_eq!(
        lines_of(here, "s-wide", None)
            .iter()
            .map(Line::kind)
            .collect::<Vec<_>>(),
        vec!["read", "deny", "call"],
        "the repeat is declined once"
    );

    let claude = for_family("claude-code");
    assert_eq!(claude.read_deny, rift_recall_policy::Mode::Enforce);
    let read = hook("claude-code", "PreToolUse", "s-wide-cc", whole(&file));
    gate_under(&read, here, &claude);
    assert!(
        gate_under(&read, here, &claude).deny_reason.is_some(),
        "the same file asks for the same thing and gets it here"
    );
}

/// ★ A read that verifies an edit is never declined on the family whose
/// edit tool says nothing back.
///
/// On the one family whose artifact quotes an edit result telling the model
/// what it wrote, that verification read is the largest pool there is. Here
/// nothing pins what the patch tool returns, so the read the agent makes to
/// see its own change is the only sight of that change it has, and
/// declining it would remove a verification rather than a repetition. The
/// call runs, and it runs in `enforce` — this is a property of the pin, not
/// of the mode.
#[test]
fn a_read_after_a_patch_is_allowed_where_nothing_pins_what_the_patch_returned() {
    let tmp = tempfile::tempdir().expect("a work tree");
    let here = tmp.path();
    let file = here.join("src.rs");
    let enforce = levers_for("codex", rift_recall_policy::Mode::Enforce);
    assert!(
        !enforce.pinned_edit_echo,
        "the fact this walk is about, read off the pinned table"
    );
    std::fs::write(&file, "a\nold\nc\n").expect("the file");

    let read = hook(
        "codex",
        "PreToolUse",
        "s-patch",
        shell_of(&format!("cat {}", shell_spelling(&file))),
    );
    gate_under(&read, here, &enforce);

    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-old\n+new\n*** End Patch\n",
        file.display()
    );
    std::fs::write(&file, "a\nnew\nc\n").expect("the edit lands");
    edit_record(
        &AdapterRegistry::with_builtin(),
        &hook(
            "codex",
            "PostToolUse",
            "s-patch",
            serde_json::json!({
                "tool_name": "apply_patch",
                "tool_input": { "command": patch },
            }),
        ),
        &here.join(".git"),
        here,
        &enforce,
    );

    assert_eq!(
        gate_under(&read, here, &enforce).deny_reason,
        None,
        "the agent's only sight of what it just wrote is not taken away"
    );
}

/// A lever nobody is holding writes nothing at all. An unreadable policy
/// resolves here, which is why this is the state a repository lands in when
/// its file cannot be parsed rather than the state it has to ask for.
#[test]
fn a_lever_that_is_off_leaves_no_journal() {
    let (tmp, file) = checkout(60);
    let here = tmp.path();
    let off = levers_for("claude-code", rift_recall_policy::Mode::Off);
    let call = hook(
        "claude-code",
        "PreToolUse",
        "s-off",
        read_call(&file, Some(1), Some(30)),
    );
    gate_under(&call, here, &off);
    gate_under(&call, here, &off);
    assert!(lines_of(here, "s-off", None).is_empty());
}

/// ★ **A relative name read under a call's own outside `workdir` is that
/// directory's file, never the held one of the same name.** The control
/// without the field still resolves against the session and is declined, so
/// the allow is the workdir's doing and not a cold window.
#[test]
fn a_relative_read_under_an_outside_workdir_is_never_declined_for_the_root_file() {
    let shell = codex_shell();
    let Some(name) = shell.file.file_name().and_then(|n| n.to_str()) else {
        panic!("the fixture names a file");
    };
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join(name), "EXTERNAL\n").unwrap();
    assert_eq!(shell.deny_reason_for("s-wd", &shell.ranged(1, 40)), None);
    let relative = format!("sed -n '1,40p' {name}");
    let with = |workdir: Option<&Path>| {
        let mut payload = shell_of(&relative);
        payload["cwd"] = shell_spelling(shell.tmp.path()).into();
        if let Some(dir) = workdir {
            payload["tool_input"]["workdir"] = shell_spelling(dir).into();
        }
        gate_under(
            &hook("codex", "PreToolUse", "s-wd", payload),
            shell.tmp.path(),
            &shell.enforce,
        )
        .deny_reason
    };
    assert_eq!(with(Some(outside.path())), None);
    assert!(with(None).is_some(), "the session-relative control is held");
}

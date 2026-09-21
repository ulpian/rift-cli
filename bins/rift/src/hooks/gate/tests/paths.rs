//! Which lines of which revision a call is about: the payloads this checkout
//! can answer for, and the ones it deliberately cannot.

use std::path::Path;

use rift_agent::window_journal::{Interval, Line};

use super::fixtures::*;

/// One call the gate refuses to record anything about.
///
/// Every refusal in the gate leans the same way, because a journal holding
/// less than really happened costs a saving, where one claiming bytes the
/// window never had costs the agent its content.
fn refused(work_tree: &Path, why: &str, agent: &str, payload: serde_json::Value) {
    let call = hook(agent, "PreToolUse", "s-2", payload);
    gate_observing(&call, work_tree);
    assert!(lines_of(work_tree, "s-2", None).is_empty(), "{why}");
}

/// A path the gate cannot stand behind: one it cannot resolve to a file of
/// this checkout, or one whose bytes it will not spend an agent's budget
/// hashing.
#[test]
fn a_path_the_gate_cannot_resolve_here_records_nothing() {
    let (tmp, file) = checkout(4);
    let here = tmp.path();
    let outside = tempfile::tempdir().expect("another tree");
    let elsewhere = outside.path().join("elsewhere.rs");
    std::fs::write(&elsewhere, b"x\n").expect("the file");
    let big = here.join("big.bin");
    let past_the_cap = usize::try_from(observing().max_inline_hash_bytes).expect("a size") + 1;
    std::fs::write(&big, vec![b'a'; past_the_cap]).expect("a large file");
    let binary = here.join("binary.bin");
    std::fs::write(&binary, b"one\0two\n").expect("bytes with a zero");

    let call = hook(
        "claude-code",
        "PreToolUse",
        "",
        read_call(&file, None, Some(2)),
    );
    gate_observing(&call, here);
    assert!(
        lines_of(here, "", None).is_empty(),
        "a hook naming no window has no journal to write to"
    );
    refused(
        here,
        "a relative path is relative to a directory no payload states",
        "claude-code",
        serde_json::json!({
            "tool_name": "Read",
            "tool_input": { "file_path": "src.rs" },
        }),
    );
    for (why, path) in [
        (
            "a file of another tree is of no checkout this journal describes",
            &elsewhere,
        ),
        (
            "a file past the inline cap is not hashed on an agent's budget",
            &big,
        ),
        ("bytes holding a zero are not lines", &binary),
        (
            "a file that is not there has no revision to record",
            &here.join("gone.rs"),
        ),
    ] {
        refused(here, why, "claude-code", read_call(path, None, Some(1)));
    }
}

/// A call that is not the read shape the pins describe. An argument
/// outside the pinned set takes the whole call out of the shape rather
/// than being ignored, because a range derived from arguments nothing
/// pinned is a range nothing bounds.
#[test]
fn a_call_outside_the_pinned_read_shape_records_nothing() {
    let (tmp, file) = checkout(4);
    let here = tmp.path();
    let mut with_pages = read_call(&file, None, None);
    with_pages["tool_input"]["pages"] = serde_json::json!([1]);
    refused(
        here,
        "an argument outside the pinned set takes the call out of the shape",
        "claude-code",
        with_pages,
    );
    refused(
        here,
        "a command line outside the published grammar is not a read",
        "claude-code",
        serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": "cat src.rs | head -n 2" },
        }),
    );
    refused(
        here,
        "a command line naming its file relatively names it in a \
         directory this step does not resolve against",
        "claude-code",
        serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": "cat src.rs" },
        }),
    );
    refused(
        here,
        "a family whose reads travel through a shell states no range",
        "codex",
        read_call(&file, None, Some(2)),
    );
}

/// **A read run through a terminal is a read**, once the command line is
/// one the published grammar spells and the file it names is one of this
/// checkout's.
///
/// The path is spelled absolutely here; the relative spelling, which is
/// how a person and an agent actually write a shell read, is the twin
/// below.
#[test]
fn a_shell_read_of_this_checkout_is_recorded_as_the_lines_it_delivers() {
    let (tmp, file) = checkout(120);
    let command = format!("sed -n '10,29p' {}", shell_spelling(&file));
    gate_observing(
        &hook(
            "claude-code",
            "PreToolUse",
            "s-9",
            serde_json::json!({
                "tool_name": "Bash",
                "tool_input": { "command": command },
            }),
        ),
        tmp.path(),
    );

    let lines = lines_of(tmp.path(), "s-9", None);
    let [Line::Read {
        path,
        range,
        total_lines,
        ..
    }] = lines.as_slice()
    else {
        panic!("one read line, got {lines:?}");
    };
    assert_eq!(path, "src.rs");
    assert_eq!(*range, Interval::new(10, 29).expect("a run of lines"));
    assert_eq!(*total_lines, 120);
}

/// ★ A shell read written the way shell reads are written — a relative
/// path — is recorded, against the directory the payload itself named.
///
/// `sed -n '10,29p' src.rs` is the same read as the absolute spelling
/// above and used to leave no line at all, because the resolution refused
/// every relative name outright. What makes the name resolvable is the
/// `cwd` the family puts on every payload: a stated fact, not the hook
/// process's own directory, which says only where the client launched it.
#[test]
fn a_shell_read_named_relatively_is_recorded_against_the_directory_the_payload_states() {
    let (tmp, _file) = checkout(120);
    let here = tmp.path().canonicalize().expect("a canonical work tree");
    gate_observing(
        &hook(
            "claude-code",
            "PreToolUse",
            "s-rel",
            serde_json::json!({
                "tool_name": "Bash",
                "tool_input": { "command": "sed -n '10,29p' src.rs" },
                "cwd": here.display().to_string(),
            }),
        ),
        tmp.path(),
    );

    let lines = lines_of(tmp.path(), "s-rel", None);
    let [Line::Read { path, range, .. }] = lines.as_slice() else {
        panic!("one read line, got {lines:?}");
    };
    assert_eq!(path, "src.rs", "the checkout's own spelling either way");
    assert_eq!(*range, Interval::new(10, 29).expect("a run of lines"));
}

/// ★ The same relative read, with nothing stating where the agent stood,
/// still records nothing.
///
/// Three ways a directory can fail to be stated, and every one of them
/// leans the same way: no `cwd` at all; a `cwd` that is itself relative
/// and so names no place; and a `cwd` outside this checkout, whose file
/// this journal has no name for. A guess in any of the three would put a
/// window's coverage on bytes some other file holds — which is the one
/// error that costs an agent its content rather than a saving.
#[test]
fn a_relative_read_with_no_stated_directory_records_nothing() {
    let (tmp, _file) = checkout(120);
    let outside = tempfile::tempdir().expect("another tree");
    std::fs::write(outside.path().join("src.rs"), "elsewhere\n").expect("its own file");
    let elsewhere = outside
        .path()
        .canonicalize()
        .expect("a canonical other tree");
    for (why, cwd) in [
        ("no working directory is stated at all", None),
        (
            "a relative working directory names no place to resolve against",
            Some("./sub".to_owned()),
        ),
        (
            "a directory outside this checkout names a file of no checkout this journal \
             describes",
            Some(elsewhere.display().to_string()),
        ),
    ] {
        let mut payload = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": "sed -n '10,29p' src.rs" },
        });
        if let Some(cwd) = cwd {
            payload["cwd"] = serde_json::json!(cwd);
        }
        refused(tmp.path(), why, "claude-code", payload);
    }
}

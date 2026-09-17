//! End to end, through the shipped binary: what a repository that has asked
//! for declines actually answers an agent with.
//!
//! Every walk here is driven the way an agent drives one — one process per
//! hook, the payload on stdin, the decision read off stdout — against a
//! throwaway repository whose own `.rift/policies/serve.toml` names the
//! family. Never this repository: the file is written into a tempdir, and the
//! spawn chokepoint (`common::rift_cmd`) refuses a working directory inside
//! the tree under test.
//!
//! The claims are about **shapes**, never about bytes of a path: a decision
//! envelope, the clauses a reason carries, the kinds of line the window's
//! ledger ends up holding. A test that pinned the tempdir's own spelling
//! would be pinning the machine it ran on.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod common;
use common::rift_cmd;

use rift_store::CanonicalStore as _;

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

/// A path as a JSON string literal (quotes included): a Windows path carries
/// backslashes, which a raw `Display` splice turns into invalid escapes.
fn json_str(path: &Path) -> String {
    serde_json::Value::String(path.display().to_string()).to_string()
}

/// A hermetic repository with one commit and a live native-hook session, so
/// every hook below lands in a real family with a real journal.
fn seed_repo(tmp: &Path) -> PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "gate@example.com"]);
    git(&["config", "user.name", "gate"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&["add", "a.txt"]);
    git(&["commit", "-q", "-m", "seed"]);
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("gate-1.json"),
        r#"{"schema":1,"session_id":"gate-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,"updated_at_ms":1783000000000,"native_id_map":{}}"#,
    )
    .unwrap();
    repo
}

/// The repository asks for declines, for these families, in its own policy
/// file — written as an operator would write it, tables and all, so what the
/// walks exercise is the route from a file on disk to an object on stdout.
fn enforce_for(repo: &Path, families: &[&str]) {
    write_policy(repo, families, "min_slice_lines = 1\n");
}

/// Turn the lever on for `families` at whatever line floor ships.
///
/// Only the savings fixture uses this. Every other walk here asserts a
/// *shape* — what is declined, what is recorded, what a reason says — over
/// files a handful of lines long, far under the shipped floor and nothing to
/// do with it, so [`enforce_for`] holds the floor at one and leaves those
/// walks testing what they are written about.
fn enforce_at_the_shipped_floor(repo: &Path, families: &[&str]) {
    write_policy(repo, families, "");
}

fn write_policy(repo: &Path, families: &[&str], extra: &str) {
    let policies = repo.join(".rift").join("policies");
    std::fs::create_dir_all(&policies).unwrap();
    let mut named = String::new();
    for family in families {
        use std::fmt::Write as _;
        let _ = writeln!(named, "{family} = \"enforce\"");
    }
    std::fs::write(
        policies.join("serve.toml"),
        format!("[interception]\nmode = \"enforce\"\n{extra}\n[interception.agents]\n{named}"),
    )
    .unwrap();
}

/// The environment every walk runs under: an isolated cache, no keychain, and
/// the queue drained inline so the accounting a decision journals has landed
/// by the time the hook returns.
fn gate_env(cache: &Path) -> [(String, String); 3] {
    [
        ("RIFT_CACHE_DIR".to_owned(), cache.display().to_string()),
        ("RIFT_NO_KEYCHAIN".to_owned(), "1".to_owned()),
        ("RIFT_CAPTURE_SYNC".to_owned(), "1".to_owned()),
    ]
}

/// Drive one real agent hook and keep its stdout — the one channel a decision
/// about a tool call travels on.
fn hook(
    repo: &Path,
    agent: &str,
    verb: &str,
    session: &str,
    turn: u32,
    payload: &str,
    env: &[(String, String)],
) -> String {
    let body = format!(r#"{{"schema":1,"session":"{session}","turn":{turn},"payload":{payload}}}"#);
    let mut child = rift_cmd(repo)
        .args(["hooks", agent, verb])
        .envs(env.iter().map(|(k, v)| (k.clone(), v.clone())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "a hook never fails the agent: {agent} {verb}"
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Drive one real agent hook the way the FAMILY sends it: the native payload
/// alone, with no envelope and therefore no turn on the wire.
///
/// This is the shape every alpha family actually fires. The envelope form
/// above states its ordinal; this one leaves the binary to find it.
fn native_hook(
    repo: &Path,
    agent: &str,
    verb: &str,
    body: &str,
    env: &[(String, String)],
) -> String {
    let mut child = rift_cmd(repo)
        .args(["hooks", agent, verb])
        .envs(env.iter().map(|(k, v)| (k.clone(), v.clone())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "a hook never fails the agent: {agent} {verb}"
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

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

/// The reason a call was declined with, or `None` where it was left alone.
///
/// Both pinned envelopes are read, because a reader that knew only one of
/// them would quietly pass on the other family however it answered.
fn declined_with(stdout: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let (decision, reason) = value.get("hookSpecificOutput").map_or_else(
        || (value.get("decision"), value.get("reason")),
        |nested| {
            (
                nested.get("permissionDecision"),
                nested.get("permissionDecisionReason"),
            )
        },
    );
    if decision?.as_str()? != "deny" {
        return None;
    }
    Some(reason?.as_str()?.to_owned())
}

/// The shell command a Codex `updatedInput` replacement carries, or `None`.
fn updated_command(stdout: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()?
        .pointer("/hookSpecificOutput/updatedInput/command")?
        .as_str()
        .map(str::to_owned)
}

/// A `Read` of `path`, as the pinned fields spell one.
fn read_of(path: &Path, offset: u32, limit: u32) -> String {
    format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":{offset},"limit":{limit}}}}}"#,
        json_str(path)
    )
}

/// A file of `lines` numbered lines, with `extra` spliced in after line 7 when
/// one is given — the shape the edit walk needs before and after its change.
fn numbered(lines: u32, extra: Option<&str>) -> String {
    use std::fmt::Write as _;
    let mut body = String::new();
    for n in 1..=lines {
        let _ = writeln!(body, "line {n}");
        if n == 7 {
            if let Some(extra) = extra {
                let _ = writeln!(body, "{extra}");
            }
        }
    }
    body
}

/// The names of the ledger files one repository holds — one per context
/// window, named for the window rather than for the order a directory lists.
fn windows(repo: &Path) -> Vec<String> {
    let dir = repo.join(".rift").join("state").join("reads");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The kinds of line one window's ledger holds, in order.
fn kinds_of(repo: &Path, window: &str) -> Vec<String> {
    std::fs::read_to_string(repo.join(".rift").join("state").join("reads").join(window))
        .unwrap()
        .lines()
        .map(|row| {
            serde_json::from_str::<serde_json::Value>(row).unwrap()["k"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// The kinds of line the one window in this repository holds.
fn kinds(repo: &Path) -> Vec<String> {
    let names = windows(repo);
    let [only] = names.as_slice() else {
        panic!("one context window, got {names:?}");
    };
    kinds_of(repo, only)
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

    let whole = read_of(&file, 1, 500);
    hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        1,
        &whole,
        &env,
    );
    let edit = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":{},"old_string":"line 7\n","new_string":"line 7\nline 7b\n","replace_all":false}}}}"#,
        json_str(&file)
    );
    std::fs::write(&file, numbered(40, Some("line 7b"))).unwrap();
    hook(
        &repo,
        "claude-code",
        "PostToolUse",
        "gate-1",
        2,
        &edit,
        &env,
    );

    let verify = hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        3,
        &whole,
        &env,
    );
    let delta = declined_with(&verify).expect("the verification read is declined");
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

    // Exactly the changed site, which is what the reason asks for; a window
    // that spilled past it would be a different request with its own answer.
    let narrow = read_of(&file, 7, 2);
    assert_eq!(
        declined_with(&hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            4,
            &narrow,
            &env
        )),
        None,
        "reading the changed lines is the behaviour the reason asks for"
    );
    assert_eq!(
        declined_with(&hook(
            &repo,
            "claude-code",
            "PreToolUse",
            "gate-1",
            5,
            &whole,
            &env
        )),
        None,
        "one decline per call, so the repeat is honoured"
    );

    assert_eq!(
        kinds(&repo),
        vec!["read", "edit", "deny", "read", "read"],
        "declined once, then handed the bytes twice and recorded as holding them"
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
    assert_eq!(names.len(), 2, "two windows, two ledgers: {names:?}");
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

    let first = hook(
        &repo,
        "antigravity",
        "PreToolUse",
        "gate-1",
        1,
        &whole,
        &env,
    );
    assert_eq!(
        declined_as_this_family_is_declined(&first),
        None,
        "the first read is the saving, never the decline"
    );

    let repeat = hook(
        &repo,
        "antigravity",
        "PreToolUse",
        "gate-1",
        2,
        &whole,
        &env,
    );
    let reason =
        declined_as_this_family_is_declined(&repeat).expect("the identical repeat is declined");
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
        "the way out stays open in the text as well as in the ledger: {reason}"
    );
    assert!(
        reason.contains("rift recall changed-hunks") && reason.contains("--known-hash"),
        "and the cheaper command that exists: {reason}"
    );
    assert!(
        !reason.contains("line 12"),
        "identifiers and numbers, never a byte of the file: {reason}"
    );

    let again = hook(
        &repo,
        "antigravity",
        "PreToolUse",
        "gate-1",
        3,
        &whole,
        &env,
    );
    assert_eq!(
        declined_as_this_family_is_declined(&again),
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

/// The family's one recall projection, resolved the way the product resolves
/// it — through the family manifest, never through a path.
fn recall_db(repo: &Path, cache: &Path) -> PathBuf {
    let paths = rift_git::discover(repo).expect("the fixture is a repository");
    let cache_dir = cache.display().to_string();
    let lookup = |key: &str| (key == "RIFT_CACHE_DIR").then(|| cache_dir.clone());
    let rift_git::FamilyState::Minted(family) =
        rift_git::family::resolve_family_under(lookup, &paths, rift_git::MintPolicy::Never)
            .unwrap()
    else {
        panic!("the decisions above should have minted the family");
    };
    rift_store::recall_db_under(cache, &family.recall_id)
}

/// Rows out of the family's recall projection, as text, in the order asked.
fn rows(repo: &Path, cache: &Path, sql: &str) -> Vec<Vec<String>> {
    let store = rift_store::open_migrated(recall_db(repo, cache), &rift_store::recall_migrations())
        .unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        store
            .query(sql, &[])
            .await
            .unwrap()
            .iter()
            .map(|r| {
                (0..8)
                    .map_while(|c| {
                        r.get_text(c)
                            .ok()
                            .or_else(|| r.get_i64(c).ok().map(|n| n.to_string()))
                    })
                    .collect()
            })
            .collect()
    })
}

/// ★ One decline books its accounting in **both** relations, with the real
/// tokenizer's count of the slice it saved and of the sentence it cost.
///
/// The two relations answer different questions and only one each: the audit
/// relation is what a savings figure sums, and the call relation is where the
/// channel and the family the decision came from are recorded. A row that
/// booked what a decline avoided and said nothing about what its reason cost
/// would flatter itself, so both figures are asserted, and the returned side
/// is asserted to be non-zero — the frame is a real cost and the honesty gate
/// is the difference, not the avoided column alone.
#[test]
fn a_decline_books_what_it_saved_and_what_it_cost_in_both_relations() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let cache = tmp.path().join("cache");
    let env = gate_env(&cache);
    let file = repo.join("booked.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();
    let read = read_of(&file, 1, 40);

    hook(&repo, "claude-code", "PreToolUse", "gate-1", 1, &read, &env);
    let denied = hook(&repo, "claude-code", "PreToolUse", "gate-1", 2, &read, &env);
    assert!(declined_with(&denied).is_some(), "the repeat is declined");

    let audit = rows(
        &repo,
        &cache,
        "SELECT verb, handle, reason, tokens_returned, tokens_avoided FROM memd_audit_event",
    );
    let [row] = audit.as_slice() else {
        panic!("one decision, one audit row, got {audit:?}");
    };
    assert_eq!(row[0], "memd.read_intercepted");
    assert!(
        row[1].starts_with("file:booked.txt@sha256:") && row[1].ends_with(":L1-40"),
        "the handle names the file, the revision and the lines: {row:?}"
    );
    assert_eq!(row[2], "already_held");
    let returned: u64 = row[3].parse().unwrap();
    let avoided: u64 = row[4].parse().unwrap();
    assert!(avoided > 0, "the slice it did not deliver: {row:?}");
    assert!(returned > 0, "the sentence it delivered instead: {row:?}");
    assert!(
        avoided > returned,
        "a decline that cost more than it saved is not a saving: {row:?}"
    );

    let calls = rows(
        &repo,
        &cache,
        "SELECT channel, tool_name, terminal_verb, tokens_returned, tokens_avoided \
         FROM serve_call_event",
    );
    let [call] = calls.as_slice() else {
        panic!("one decision, one call row, got {calls:?}");
    };
    assert_eq!(call[0], "hook_inject", "the channel the decision travelled");
    assert_eq!(
        call[1], "claude-code:read-gate",
        "the family and the step, the only per-family axis these relations have"
    );
    assert_eq!(call[2], "memd.read_intercepted");
    assert_eq!(call[3], row[3], "the same figures, in the other relation");
    assert_eq!(call[4], row[4]);
}

/// ★ **A read Rift caused not to happen is booked, and shows up in the
/// figure.** Driven through the shipped binary, one process per hook:
///
/// 1. the window reads a file, so the ledger knows it holds it;
/// 2. the context is compacted and the boundary hands it back the map of what
///    it stopped holding — Rift telling the window, in so many words, that it
///    held this file at this revision and these lines;
/// 3. the window then **edits** that file without reading it again.
///
/// The read at step 3 never happened, and the row that says so lands in the
/// audit relation under its own verb and reaches `rift savings --json` as the
/// prevented part of the avoided total.
#[test]
fn an_announcement_the_window_acts_on_books_a_read_that_never_happened() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    // The boundary tiers are Recall's, so the repository has to have asked
    // for Recall as an operator would.
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"gate\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    let cache = tmp.path().join("cache");
    let env = gate_env(&cache);
    let file = repo.join("held.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();

    // 1. the window is handed the file.
    hook(
        &repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        1,
        &read_of(&file, 1, 40),
        &env,
    );
    // 2. it is compacted, and the boundary hands back what it stopped holding.
    hook(&repo, "claude-code", "PreCompact", "gate-1", 2, "{}", &env);
    let map = native_hook(
        &repo,
        "claude-code",
        "SessionStart",
        r#"{"session_id":"gate-1","source":"compact"}"#,
        &env,
    );
    assert!(
        map.contains("held.txt"),
        "the map names the file it says the window held: {map}"
    );
    let names = windows(&repo);
    let [only] = names.as_slice() else {
        panic!("one context window, got {names:?}");
    };
    assert!(
        kinds_of(&repo, only).contains(&"hold".to_owned()),
        "and the window's ledger records having been told: {:?}",
        kinds_of(&repo, only)
    );

    // 3. the window acts on the file without reading it again.
    let edit = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":{},"old_string":"line 7\n","new_string":"line seven\n"}}}}"#,
        json_str(&file)
    );
    hook(&repo, "claude-code", "PreToolUse", "gate-1", 3, &edit, &env);

    let booked = rows(
        &repo,
        &cache,
        "SELECT verb, handle, reason, tokens_returned, tokens_avoided FROM memd_audit_event \
         WHERE verb = 'memd.read_prevented'",
    );
    let [row] = booked.as_slice() else {
        panic!("one act on one announcement, one row, got {booked:?}");
    };
    assert!(
        row[1].starts_with("file:held.txt@sha256:") && row[1].ends_with(":L1-40"),
        "the handle names the file, the revision and the announced lines: {row:?}"
    );
    assert_eq!(row[2], "prevented");
    assert_eq!(row[3], "0", "nothing was put in front of the model");
    let avoided: u64 = row[4].parse().unwrap();
    assert!(avoided > 0, "the read it did not make: {row:?}");

    // And the figure a person reads.
    let out = rift_cmd(&repo)
        .args(["savings", "--json"])
        .envs(env.iter().map(|(k, v)| (k.clone(), v.clone())))
        .output()
        .unwrap();
    assert!(out.status.success(), "rift savings failed");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let prevented = json["overall"]["tokens_prevented"].as_u64().unwrap();
    let total = json["overall"]["tokens_avoided"].as_u64().unwrap();
    assert_eq!(prevented, avoided, "the row, in the published figure");
    assert!(
        prevented > 0 && prevented <= total,
        "inside the avoided total, never beside it: {prevented} of {total}"
    );
}

/// The slice ceiling, spelled as a **number** rather than as the constant it
/// pins.
///
/// A read whose slice is larger than this is left alone whatever the window
/// holds. Written here as `96 * 1024` for the reason [`INLINE_HASH_CEILING`]
/// gives, and used only to say how much of the measured pool sits on each side
/// of it — so a move shows up as a number rather than as a silent re-scoping.
const SLICE_CEILING: usize = 96 * 1024;

/// The line floor, spelled as a **number** for the same reason
/// [`SLICE_CEILING`] is.
///
/// A repeat shorter than this is decided and recorded but never declined,
/// because under it the reason frame costs more than the range it replaces.
/// Used here only to say how many of the pool's tokens the floor forgoes — a
/// figure in its own right, so moving the floor shows up as a number rather
/// than as a quiet change in what the share is a share of.
const LINE_FLOOR: u32 = 24;

/// What this repository's own capture measured a re-read to cost, in tokens:
/// 98 re-reads of an unchanged revision over 500,428 tokens, and 58 re-reads
/// of a changed path over 866,705 (first reads, for scale, were 307 over
/// 2,011,810).
///
/// The fixture is sized from these two means rather than from a round number,
/// because the share a reason frame removes depends on the size of
/// the read it replaces: a frame of ~87 tokens against a 160-token read
/// removes barely half of it and against a 5,106-token read removes ~98 %. A
/// fixture of uniform small files therefore measures the frame, not the lever.
const UNCHANGED_REREAD_TOKENS: u32 = 5_106;
/// The mean of the changed class — the read that verifies an edit.
const CHANGED_REREAD_TOKENS: u32 = 14_943;
/// The spread the plain re-reads are drawn from, in tenths of
/// [`UNCHANGED_REREAD_TOKENS`]: eleven multipliers whose mean is exactly the
/// class mean, with a twelfth member sized instead at [`CROSSOVER_LINES`].
const PLAIN_SPREAD: [u32; 11] = [6, 7, 8, 9, 10, 10, 10, 11, 12, 13, 14];
/// The spread the verification reads are drawn from, in tenths of
/// [`CHANGED_REREAD_TOKENS`]: the class mean, one below it and one above, and
/// unbiased on purpose — the three multipliers sum to three times the middle
/// one, so the spread cannot flatter the lever by leaning either way.
///
/// **No member of this fixture reaches [`SLICE_CEILING`]** at the value the
/// ceiling now has: the largest is about 39.8 KB against a ceiling of 96 KiB,
/// and the figures say so by printing `past_ceiling` as zero. The spread is
/// not stretched to reach it, because a member sized to clear a ceiling is a
/// member drawn from no measured class and it would move a published
/// denominator. The ceiling is driven from both sides instead by
/// [`a_slice_past_the_ceiling_runs_while_one_under_it_is_declined`], which is
/// where a claim about the ceiling belongs.
const VERIFY_SPREAD: [u32; 3] = [7, 10, 13];
/// The smallest member: the length at which a plain frame stops paying for
/// itself (87 tokens of frame against 4 tokens a line breaks even at 21.75
/// lines). It sits under [`LINE_FLOOR`], so the lever now observes it and
/// lets it run, and its tokens are counted as received like any other member
/// the lever let through. It stays in the fixture for exactly that reason: the
/// floor is a lever that forgoes pool on purpose, and a fixture that dropped
/// the member the floor acts on would stop being able to say what it costs.
const CROSSOVER_LINES: u32 = 22;

/// The file lengths one run of the script uses, in lines, and the row its
/// figures are published under.
struct Sizing {
    /// One length per plain re-read member.
    plain: Vec<u32>,
    /// One length per file that is edited and then read back.
    verified: Vec<u32>,
    label: &'static str,
}

/// How many numbered lines the real tokenizer prices at about `tokens`.
///
/// Derived, never tabulated: bisected with the tokenizer the product prices
/// reads with, so a tokenizer change moves the fixture's sizes with it instead
/// of quietly changing what the fixture claims to measure.
fn lines_for_tokens(tokens: u32) -> u32 {
    let (mut lo, mut hi) = (1_u32, tokens.max(2));
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if rift_tokens::count(&numbered(mid, None)) < tokens {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The fixture sized from the measured classes.
///
/// The mapping, stated: each plain re-read is a file the tokenizer prices at
/// its multiplier's tenths of [`UNCHANGED_REREAD_TOKENS`], and each
/// verification read at its multiplier's tenths of [`CHANGED_REREAD_TOKENS`],
/// with one plain member sized instead at [`CROSSOVER_LINES`]. Nothing here is
/// chosen to clear a bar: the changed class's mean member is larger than
/// [`SLICE_CEILING`] and is scripted at its measured size anyway, which is why
/// the figures say how much of the pool the product's own ceiling leaves
/// alone.
fn class_sizes() -> Sizing {
    let tenths = |mean: u32, n: u32| lines_for_tokens(mean * n / 10);
    let mut plain: Vec<u32> = PLAIN_SPREAD
        .iter()
        .map(|n| tenths(UNCHANGED_REREAD_TOKENS, *n))
        .collect();
    plain.push(CROSSOVER_LINES);
    Sizing {
        plain,
        verified: VERIFY_SPREAD
            .iter()
            .map(|n| tenths(CHANGED_REREAD_TOKENS, *n))
            .collect(),
        label: "recall-intercept-savings",
    }
}

/// The same script over uniform forty-line files — the shape the first sitting
/// measured, and now a length just above [`LINE_FLOOR`]. Published beside the
/// class-sized figure so the minimum-slice question keeps its own number, and
/// held to no bar: forty lines is not a size this repository's reads have.
fn small_file_sizes() -> Sizing {
    Sizing {
        plain: vec![40; 12],
        verified: vec![40; 3],
        label: "recall-intercept-savings-small-files",
    }
}

/// What one call would have delivered: the real tokenizer's count of exactly
/// the lines it asked for, and those lines' bytes.
///
/// The slice is cut the way the product cuts it, so this count and the figure
/// the product books for the same key are two independent measurements of one
/// quantity — and the fixture asserts they agree, which is what keeps a price
/// a measurement rather than an estimate. It also prices a member the product
/// never decided about, which a booked figure cannot.
fn price_slice(text: &str, lo: u32, hi: u32) -> (u32, usize) {
    let slice: String = text
        .split_inclusive('\n')
        .skip(lo as usize - 1)
        .take((hi.saturating_sub(lo) as usize) + 1)
        .collect();
    (rift_tokens::count(&slice), slice.len())
}

/// Which measured class a pool member was drawn from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PoolClass {
    /// A re-read of bytes the window already holds at the revision it holds.
    Plain,
    /// The read that verifies the agent's own edit.
    Verification,
    /// A later turn asking again for a key this window has declined once.
    NextTurn,
}

/// One call in the script that asked for lines the window already held: a
/// member of the addressable repeat pool, with the lever's answer to it.
struct PoolMember {
    /// The file's name, as a booked handle spells it.
    name: String,
    /// The lines the call asked for, as the ledger numbers them.
    lo: u32,
    hi: u32,
    class: PoolClass,
    /// The real tokenizer's count of the slice the call would have delivered:
    /// the tokens the agent would have spent without the lever.
    price: u32,
    /// The bytes of that same slice, which is what [`SLICE_CEILING`] is
    /// spelled in.
    slice_bytes: usize,
    /// Whether the lever left the call alone, so its tokens reached the model.
    escaped: bool,
}

/// The scripted session itself: reads, re-reads, edits with the reads that
/// verify them, and a next turn that comes back to one file.
///
/// Returns EVERY member of the addressable repeat pool, each with the lever's
/// actual answer to it — not only the ones that escaped. A share counted
/// against a denominator the script assembles afterwards is a share of
/// whatever the script chose to admit; this one is counted against what the
/// session asked for.
fn script_the_session(repo: &Path, env: &[(String, String)], sizing: &Sizing) -> Vec<PoolMember> {
    let named = |prefix: &str, lengths: &[u32]| -> Vec<(PathBuf, u32)> {
        lengths
            .iter()
            .enumerate()
            .map(|(n, lines)| (repo.join(format!("{prefix}{n:02}.txt")), *lines))
            .collect()
    };
    let plain = named("p", &sizing.plain);
    let edited = named("e", &sizing.verified);
    for (path, lines) in plain.iter().chain(edited.iter()) {
        std::fs::write(path, numbered(*lines, None)).unwrap();
    }
    let mut turn = 0;

    // Turn one: the agent reads each file. Every one of these is the saving
    // the mechanism exists to protect, and none of them is pool.
    for (path, lines) in plain.iter().chain(edited.iter()) {
        let declined = read_whole(repo, env, &mut turn, path, *lines);
        assert!(!declined, "a first read is never declined");
    }

    // Turn two: it re-reads each of them, unchanged. Pool, every one.
    let mut pool: Vec<PoolMember> = Vec::new();
    for (path, lines) in &plain {
        let declined = read_whole(repo, env, &mut turn, path, *lines);
        let text = numbered(*lines, None);
        pool.push(member(path, &text, *lines, PoolClass::Plain, declined));
    }

    // Turn three: it edits the others and reads each back to check.
    pool.extend(edit_and_verify(repo, env, &mut turn, &edited));

    // The next turn: it comes back to one file it is still working on and
    // asks for it again. The key has been declined once already, so this one
    // runs — pool the mechanism does not remove.
    let (working, lines) = &plain[5];
    let declined = read_whole(repo, env, &mut turn, working, *lines);
    assert!(!declined, "one decline per key: the same call again runs");
    let text = numbered(*lines, None);
    pool.push(member(working, &text, *lines, PoolClass::NextTurn, false));

    pool
}

/// One whole-file read of `path`, in the next turn, and whether the lever
/// declined it.
fn read_whole(
    repo: &Path,
    env: &[(String, String)],
    turn: &mut u32,
    path: &Path,
    lines: u32,
) -> bool {
    *turn += 1;
    let payload = read_of(path, 1, lines + 10);
    let out = hook(
        repo,
        "claude-code",
        "PreToolUse",
        "gate-1",
        *turn,
        &payload,
        env,
    );
    declined_with(&out).is_some()
}

/// One call that asked for lines the window already held, priced.
fn member(path: &Path, text: &str, hi: u32, class: PoolClass, declined: bool) -> PoolMember {
    let (price, slice_bytes) = price_slice(text, 1, hi);
    PoolMember {
        name: file_name(path),
        lo: 1,
        hi,
        class,
        price,
        slice_bytes,
        escaped: !declined,
    }
}

/// The edits and the reads that verify them: the largest measured class, and
/// the one this mechanism exists for.
fn edit_and_verify(
    repo: &Path,
    env: &[(String, String)],
    turn: &mut u32,
    files: &[(PathBuf, u32)],
) -> Vec<PoolMember> {
    let mut pool = Vec::new();
    for (path, lines) in files {
        let edit = format!(
            r#"{{"tool_name":"Edit","tool_input":{{"file_path":{},"old_string":"line 7\n","new_string":"line 7\nline 7b\n","replace_all":false}}}}"#,
            json_str(path)
        );
        let after = numbered(*lines, Some("line 7b"));
        std::fs::write(path, &after).unwrap();
        *turn += 1;
        hook(
            repo,
            "claude-code",
            "PostToolUse",
            "gate-1",
            *turn,
            &edit,
            env,
        );
        let declined = read_whole(repo, env, turn, path, *lines);
        pool.push(member(
            path,
            &after,
            *lines + 1,
            PoolClass::Verification,
            declined,
        ));
    }
    pool
}

/// ★ The scripted session's own numbers: what its addressable repeat pool was
/// worth, and how much of it the lever removed.
///
/// **What the pool is.** Every call in this script that asks for lines the
/// window already holds at the revision it holds them at — the plain re-reads
/// of turn two, the reads that verify the agent's own edits in turn three, and
/// the next turn's re-read of a file it is still working on. Each member is
/// priced by the real tokenizer's count of the slice the call would have
/// delivered, and every member the lever decided about is cross-checked
/// against the figure the product booked for that exact key — two
/// measurements of one quantity, so no price here is estimated.
///
/// **What removed is.** The pool minus what the agent received instead: the
/// reason frames it was answered with, and every pool member the lever let
/// through. It is NOT the avoided column. A mechanism that saved a thousand
/// tokens by spending nine hundred of them on reasons has removed a hundred,
/// and the two figures are published side by side so that neither can stand in
/// for the other. The avoided column is kept because the honesty gate is a
/// statement about it and the frames — avoided greater than returned — which
/// is a different claim from the share.
///
/// **What it is not.** One addressable shape is deliberately outside this
/// fixture: the read that verifies an edit whose sites v1 does not derive
/// (`MultiEdit` and its siblings), which is pool the mechanism does not remove
/// today and which the dogfood window sizes. The slices past
/// `max_slice_bytes` are NOT outside it — the changed class's own mean is one
/// of them — so they are scripted, priced and counted against the mechanism
/// like any other member, with the figures saying how much of the pool they
/// are.
///
/// **The residual that is here.** Two members run untouched, both by design,
/// and both counted against the mechanism rather than excused. One is a
/// next-turn re-read of a key already declined once, because one decline per
/// key is what bounds a model that answers a decline by asking again. The
/// other is the twenty-two-line member, which is under the line floor: the
/// lever reaches the decision, records it and lets the call run, because a
/// reason frame would have cost more than the range. The tokens each forgoes
/// are published as their own figures.
fn measure_the_repeat_pool(sizing: &Sizing) -> Figures {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    enforce_at_the_shipped_floor(&repo, &["claude-code"]);
    let cache = tmp.path().join("cache");
    let env = gate_env(&cache);
    let members = script_the_session(&repo, &env, sizing);
    assert_the_pools_composition(&members);

    // The product's own books, by the key each decision was about.
    let booked = rows(
        &repo,
        &cache,
        "SELECT handle, tokens_avoided, tokens_returned FROM memd_audit_event \
         WHERE verb = 'memd.read_intercepted'",
    );
    let avoided = cross_check_the_bookings(&members, &booked);
    let returned: u64 = booked.iter().map(|r| r[2].parse::<u64>().unwrap()).sum();
    let priced = |m: &PoolMember| u64::from(m.price);
    let pool: u64 = members.iter().map(priced).sum();
    let delivered: u64 = members.iter().filter(|m| m.escaped).map(priced).sum();
    let received = returned + delivered;
    let removed = pool.saturating_sub(received);
    // What the product's own slice ceiling leaves alone, published beside the
    // share and never taken out of it: the bar is held against the whole pool,
    // and this line says how much of a shortfall is the ceiling rather than
    // the frames.
    let past_ceiling: u64 = members
        .iter()
        .filter(|m| m.slice_bytes > SLICE_CEILING)
        .map(priced)
        .sum();
    // What the line floor forgoes, on the same terms: pool the lever reached a
    // decision about, recorded, and deliberately did not act on because the
    // reason would have cost more than the range. Published as its own figure
    // and, like the ceiling's, never taken out of the denominator.
    let under_floor: u64 = members
        .iter()
        .filter(|m| m.hi - m.lo + 1 < LINE_FLOOR)
        .map(priced)
        .sum();
    let share = |part: u64, whole: u64| {
        if whole == 0 {
            0.0
        } else {
            part as f64 * 100.0 / whole as f64
        }
    };

    println!(
        "{}: pool {pool} · received {received} · removed {removed} · avoided {avoided} · \
         returned {returned} · removed {:.2} % · declines {} · pool members {} · \
         escaped {delivered}",
        sizing.label,
        share(removed, pool),
        booked.len(),
        members.len(),
    );
    println!(
        "{}: of that pool {past_ceiling} tokens sit in slices larger than the product's \
         own ceiling of {SLICE_CEILING} bytes, which the lever never acts on; over \
         the rest the fixture removes {:.2} %",
        sizing.label,
        share(removed, pool - past_ceiling),
    );
    println!(
        "{}: and {under_floor} tokens sit in repeats shorter than the floor of \
         {LINE_FLOOR} lines, which the lever decides about, records and lets run \
         because a reason would cost more than the range — {:.2} % of the pool, \
         forgone on purpose",
        sizing.label,
        share(under_floor, pool),
    );
    Figures {
        pool,
        received,
        removed,
        avoided,
        returned,
    }
}

/// The pool's composition, asserted by class: a member dropped from the
/// denominator would raise the share with nothing going red, which is the one
/// way this figure could flatter itself.
fn assert_the_pools_composition(members: &[PoolMember]) {
    let of_class = |class| members.iter().filter(|m| m.class == class).count();
    assert_eq!(
        (
            members.len(),
            of_class(PoolClass::Plain),
            of_class(PoolClass::Verification),
            of_class(PoolClass::NextTurn)
        ),
        (16, 12, 3, 1),
        "the script's own pool: twelve plain repeats, three verification reads, \
         one next-turn repeat"
    );
}

/// Reconcile the script's own pool against the product's books, and return
/// what the lever avoided.
///
/// A declined member must be booked, its booked avoided figure must equal this
/// fixture's own count of the same slice, and its returned figure must be
/// non-zero: a frame the model reads is never free, and booking it free would
/// move the whole cost of this mechanism out of sight. A member the lever let
/// through delivered its price to the model whatever the books say about a key
/// it shares with an earlier decline.
fn cross_check_the_bookings(members: &[PoolMember], booked: &[Vec<String>]) -> u64 {
    let mut avoided = 0_u64;
    for m in members {
        assert!(
            m.price > 0,
            "a member with no price prices nothing: {}",
            m.name
        );
        if m.escaped {
            continue;
        }
        let row = booking_for(booked, m)
            .unwrap_or_else(|| panic!("{} was declined and nothing was booked", m.name));
        assert_eq!(
            row[1].parse::<u64>().unwrap(),
            u64::from(m.price),
            "the product's count of the slice it did not deliver and this fixture's \
             count of the same slice are one quantity: {row:?}"
        );
        assert!(
            row[2].parse::<u64>().unwrap() > 0,
            "a frame the model reads is never free, and booking it free would move \
             the whole cost of this mechanism out of sight: {row:?}"
        );
        avoided += u64::from(m.price);
    }
    assert_eq!(
        booked.len(),
        members.iter().filter(|m| !m.escaped).count(),
        "every booked row is one declined member of this pool and no other"
    );
    avoided
}

/// The one row booked for a member's key, or `None`. Two rows for one key
/// would price it twice, and that is a defect, not a sum.
fn booking_for<'a>(booked: &'a [Vec<String>], m: &PoolMember) -> Option<&'a Vec<String>> {
    let prefix = format!("file:{}@sha256:", m.name);
    let suffix = format!(":L{}-{}", m.lo, m.hi);
    let mut rows = booked
        .iter()
        .filter(|row| row[0].starts_with(&prefix) && row[0].ends_with(&suffix));
    let row = rows.next();
    assert!(
        rows.next().is_none(),
        "two bookings for one key would price {} twice",
        m.name
    );
    row
}

/// The four figures one run of the script publishes.
struct Figures {
    /// The tokens the agent would have spent without the lever.
    pool: u64,
    /// What it received instead: the reason frames, plus every pool member the
    /// lever let through.
    received: u64,
    /// `pool - received`.
    removed: u64,
    /// What the lever avoided, cross-checked against the product's books.
    avoided: u64,
    /// What the frames cost, out of the same rows.
    returned: u64,
}

/// ★ The class-sized fixture is held to the nine-tenths bar.
#[test]
fn the_intercept_fixture_removes_at_least_ninety_percent_of_its_pool() {
    let f = measure_the_repeat_pool(&class_sizes());
    assert!(f.pool > 0, "a fixture with no pool measures nothing");
    assert!(
        f.avoided > f.returned,
        "the honesty gate: what the declines saved must exceed what their \
         reasons cost — avoided {}, returned {}",
        f.avoided,
        f.returned
    );
    assert!(
        f.removed * 100 >= f.pool * 90,
        "the fixture removed {} of a {}-token pool ({:.2} per cent), under the \
         nine-tenths bar; publish this number and raise it as a decision, never \
         tune the fixture",
        f.removed,
        f.pool,
        f.removed as f64 * 100.0 / f.pool as f64
    );
}

/// The same script over uniform forty-line files, published and held to no
/// bar.
///
/// Forty lines is not a size this repository's reads have; the row exists
/// because the minimum-slice question — the length below which a reason frame
/// costs more than the read it replaces — is decided on a number, and this is
/// that number. It sits just above [`LINE_FLOOR`], so every member of it is
/// still declined, and what it publishes is what a decline is worth at the
/// short end: a share far under the class-sized one, for the single reason
/// that the frame is a large fraction of a small read. That is the figure an observe
/// window has to weigh before the floor moves. Only the claims that hold at
/// every size are asserted here.
#[test]
fn the_small_file_fixture_publishes_its_figure_without_a_bar() {
    let f = measure_the_repeat_pool(&small_file_sizes());
    assert!(f.pool > 0, "a fixture with no pool measures nothing");
    assert!(f.received > 0, "the frames the model read are never free");
    assert!(
        f.avoided > f.returned,
        "even at forty lines the declines must save more than their reasons \
         cost — avoided {}, returned {}",
        f.avoided,
        f.returned
    );
}

/// A path's own file name — the spelling a booked handle carries.
fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

/// A `Read` with no `limit` — the shape almost every real read has.
fn read_whole_of(path: &Path) -> String {
    format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{}}}}}"#,
        json_str(path)
    )
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
    assert!(windows(&repo).is_empty(), "no coverage ledger is created");
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

/// ★ An existing repository's explicit `observe` remains authoritative, and a
/// repeated limit-less read still sizes the whole-file pool truthfully.
///
/// The figure is the point. Before the delivery was pinned, such a repeat was
/// priced at the one line it was credited with, about twenty tokens against
/// the thousands the tool actually returns, so an observe window would have
/// reported a mechanism working perfectly at nearly no saving.
#[test]
fn an_explicit_observe_policy_prices_a_limit_less_repeat_as_the_whole_file() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_repo(tmp.path());
    let policies = repo.join(".rift/policies");
    std::fs::create_dir_all(&policies).unwrap();
    std::fs::write(
        policies.join("serve.toml"),
        "[interception]\nmode = \"observe\"\nmin_slice_lines = 1\n",
    )
    .unwrap();
    let cache = tmp.path().join("cache");
    let env = gate_env(&cache);
    let file = repo.join("observed.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();
    let whole = read_whole_of(&file);

    for turn in [1, 2] {
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
            "the repository explicitly requested observe"
        );
    }
    assert_eq!(kinds(&repo), vec!["read", "observe"]);

    // The audit row a recorded-but-unacted decision books. It carries no
    // figures by design — nothing was sent to the model, so nothing may be
    // claimed as saved — and what it does carry is the key the pool is sized
    // by: the whole file, not the line the read began on.
    let booked = rows(
        &repo,
        &cache,
        "SELECT handle, tokens_avoided, tokens_returned FROM memd_audit_event \
         WHERE verb = 'memd.read_repeat_observed'",
    );
    let [row] = booked.as_slice() else {
        panic!("one decision, one audit row, got {booked:?}");
    };
    assert!(
        row[0].ends_with(":L1-40"),
        "the pool is keyed by the whole file the read holds: {row:?}"
    );
    assert_eq!(row[1], "0", "an unacted decision saved nothing");
    assert_eq!(row[2], "0", "and cost nothing");

    // And the window's own line, which is what the pool report prices: forty
    // lines of it, not the one line an unpinned window would have recorded.
    let names = windows(&repo);
    let observed: serde_json::Value = serde_json::from_str(
        std::fs::read_to_string(
            repo.join(".rift")
                .join("state")
                .join("reads")
                .join(&names[0]),
        )
        .unwrap()
        .lines()
        .nth(1)
        .unwrap(),
    )
    .unwrap();
    assert_eq!(observed["k"], "observe");
    assert_eq!(observed["lo"], 1);
    assert_eq!(observed["hi"], 40, "the pool a decline would have removed");
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

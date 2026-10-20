//! How every read-gate walk is driven, in one module: the throwaway
//! repository, the policy file that asks for declines, the environment each
//! call runs under, the one-process-per-hook spawn, and the readers that look
//! at what a walk left in the window's journal.
//!
//! Every walk is driven the way an agent drives one — one process per hook,
//! the payload on stdin, the decision read off stdout — against a throwaway
//! repository whose own `.rift/policies/serve.toml` names the family. Never
//! this repository: the file is written into a tempdir, and the spawn
//! chokepoint (`common::rift_cmd`) refuses a working directory inside the
//! tree under test.
//!
//! The claims the suites make on top of this are about **shapes**, never
//! about bytes of a path: a decision envelope, the clauses a reason carries,
//! the kinds of line the window's journal ends up holding. A test that pinned
//! the tempdir's own spelling would be pinning the machine it ran on.
//!
//! Each read-gate suite includes this file by `#[path]` as its own module and
//! uses a subset of it, which is why each declaration carries
//! `#[allow(dead_code)]`: the allow names a fact about the include, not a
//! suppression.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::common::rift_cmd;

/// The slice ceiling, spelled as a **number** rather than as the constant it
/// pins.
///
/// A read whose slice is larger than this is left alone whatever the window
/// holds. Written here as `96 * 1024` rather than imported, so that a raise on
/// the product's side has to red a test rather than quietly widen what may be
/// declined, and used only to say how much of the measured pool sits on each
/// side of it — so a move shows up as a number rather than as a silent
/// re-scoping.
pub(crate) const SLICE_CEILING: usize = 96 * 1024;

/// A path as a JSON string literal (quotes included): a Windows path carries
/// backslashes, which a raw `Display` splice turns into invalid escapes.
pub(crate) fn json_str(path: &Path) -> String {
    serde_json::Value::String(path.display().to_string()).to_string()
}

/// A hermetic repository with one commit and a live native-hook session, so
/// every hook below lands in a real family with a real journal.
pub(crate) fn seed_repo(tmp: &Path) -> PathBuf {
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
    let sessions = super::common::seed_sessions_dir(&repo.join(".git"));
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
pub(crate) fn enforce_for(repo: &Path, families: &[&str]) {
    write_policy(repo, families, "min_slice_lines = 1\n");
}

/// Turn the lever on for `families` at whatever line floor ships.
///
/// Only the savings fixture uses this. Every other walk here asserts a
/// *shape* — what is declined, what is recorded, what a reason says — over
/// files a handful of lines long, far under the shipped floor and nothing to
/// do with it, so [`enforce_for`] holds the floor at one and leaves those
/// walks testing what they are written about.
pub(crate) fn enforce_at_the_shipped_floor(repo: &Path, families: &[&str]) {
    write_policy(repo, families, "");
}

pub(crate) fn write_policy(repo: &Path, families: &[&str], extra: &str) {
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
pub(crate) fn gate_env(cache: &Path) -> [(String, String); 3] {
    [
        ("RIFT_CACHE_DIR".to_owned(), cache.display().to_string()),
        ("RIFT_NO_KEYCHAIN".to_owned(), "1".to_owned()),
        ("RIFT_CAPTURE_SYNC".to_owned(), "1".to_owned()),
    ]
}

/// Drive one real agent hook and keep its stdout — the one channel a decision
/// about a tool call travels on.
pub(crate) fn hook(
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
pub(crate) fn native_hook(
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

/// The reason a call was declined with, or `None` where it was left alone.
///
/// Both pinned envelopes are read, because a reader that knew only one of
/// them would quietly pass on the other family however it answered.
pub(crate) fn declined_with(stdout: &str) -> Option<String> {
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

/// A `Read` of `path`, as the pinned fields spell one.
pub(crate) fn read_of(path: &Path, offset: u32, limit: u32) -> String {
    format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":{offset},"limit":{limit}}}}}"#,
        json_str(path)
    )
}

/// A `Read` with no `limit` — the shape almost every real read has.
pub(crate) fn read_whole_of(path: &Path) -> String {
    format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{}}}}}"#,
        json_str(path)
    )
}

/// The `Edit` this suite drives, as the pinned fields spell one: `line 7b`
/// inserted after line 7 of `path`.
///
/// Two walks make the same edit — the one that reads what a verification read
/// is declined with, and the one that prices the class that read belongs to —
/// so the payload is written once rather than kept in step by eye.
pub(crate) fn edit_inserting_line_7b(path: &Path) -> String {
    format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":{},"old_string":"line 7\n","new_string":"line 7\nline 7b\n","replace_all":false}}}}"#,
        json_str(path)
    )
}

/// A file of `lines` numbered lines, with `extra` spliced in after line 7 when
/// one is given — the shape the edit walk needs before and after its change.
pub(crate) fn numbered(lines: u32, extra: Option<&str>) -> String {
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

/// The suffix of a window's call sidecar, which sits beside its journal and
/// is not one.
const CALLS_SIDECAR: &str = ".calls.jsonl";

/// The names of the journal files one repository holds — one per context
/// window, named for the window rather than for the order a directory lists.
pub(crate) fn windows(repo: &Path) -> Vec<String> {
    let dir = repo.join(".rift").join("state").join("reads");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.ends_with(CALLS_SIDECAR))
        .collect();
    names.sort();
    names
}

/// The kinds of line one window's journal holds, in order.
pub(crate) fn kinds_of(repo: &Path, window: &str) -> Vec<String> {
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
pub(crate) fn kinds(repo: &Path) -> Vec<String> {
    let names = windows(repo);
    let [only] = names.as_slice() else {
        panic!("one context window, got {names:?}");
    };
    kinds_of(repo, only)
}

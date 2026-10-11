//! End to end, through the shipped binary: what a subagent's window inherits
//! from the window that spawned it, and what a decision does with it.
//!
//! The spawning call leaves its line in the session's main journal, the
//! subagent's opening claims it, and the lines its spawner held of a path the
//! spawning prompt named land in the subagent's own journal, marked by name.
//! At the shipped `observe` a read of those lines runs and is recorded as an
//! observation of that class; only a repository that writes `enforce` for the
//! class has it declined, once. No client signals a fork, so no line is ever
//! inherited as one.
//!
//! The ids every walk uses are fixed strings, and nothing asserted here reads
//! the clock: what a line was written at is not part of any answer below.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};

mod common;

use rift_store::CanonicalStore as _;

#[allow(dead_code)] // The read-gate suites share these builders; each file uses a subset.
#[path = "common/read_gate_fixture.rs"]
mod read_gate_fixture;
use read_gate_fixture::{
    declined_with, gate_env, hook, json_str, numbered, seed_repo, write_policy,
};

const SESSION: &str = "gate-1";

/// The repository's policy: declines on for Claude Code at a floor of one
/// line, and the inherited-by-name lever as `by_name` says.
fn policy(repo: &Path, by_name: &str) {
    write_policy(
        repo,
        &["claude-code"],
        &format!("min_slice_lines = 1\ninherit_by_name = \"{by_name}\"\n"),
    );
}

/// A whole-file read by the window `agent` (the main window when `None`).
fn read_by(file: &Path, agent: Option<&str>) -> String {
    let window = agent.map_or_else(String::new, |a| format!(r#","agent_id":"{a}""#));
    format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{}}}{window}}}"#,
        json_str(file)
    )
}

/// The spawning call `call`, made by `agent` (the main window when `None`),
/// for a subagent of `agent_type` with `prompt`.
fn spawn_by(call: &str, agent_type: &str, prompt: &str, agent: Option<&str>) -> String {
    let window = agent.map_or_else(String::new, |a| format!(r#","agent_id":"{a}""#));
    let prompt = serde_json::Value::String(prompt.to_owned());
    format!(
        r#"{{"tool_name":"Agent","tool_use_id":"{call}","prompt_id":"p-1","tool_input":{{"subagent_type":"{agent_type}","prompt":{prompt}}}{window}}}"#
    )
}

/// A subagent window opening.
fn opening(agent: &str, agent_type: &str) -> String {
    format!(
        r#"{{"hook_event_name":"SubagentStart","agent_id":"{agent}","agent_type":"{agent_type}","prompt_id":"p-1"}}"#
    )
}

/// The rows of one window's journal, parsed.
fn journal(repo: &Path, agent: Option<&str>) -> Vec<serde_json::Value> {
    let mut bytes = SESSION.as_bytes().to_vec();
    bytes.push(0);
    bytes.extend_from_slice(agent.unwrap_or_default().as_bytes());
    let key = rift_hashchain::leaf_hash(&bytes).to_hex();
    let file = repo.join(".rift/state/reads").join(format!("{key}.jsonl"));
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect()
}

/// The `(kind, class)` of every row of one window's journal.
fn kinds(repo: &Path, agent: Option<&str>) -> Vec<(String, Option<String>)> {
    journal(repo, agent)
        .iter()
        .map(|row| {
            (
                row["k"].as_str().unwrap().to_owned(),
                row["c"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

fn kind(k: &str, c: Option<&str>) -> (String, Option<String>) {
    (k.to_owned(), c.map(str::to_owned))
}

/// A repository whose main window read `a.txt` and spawned an `Explore`
/// subagent `agent-7` with a prompt naming it, which has opened.
fn spawned(tmp: &Path, by_name: &str) -> (PathBuf, PathBuf, Vec<(String, String)>) {
    let repo = seed_repo(tmp);
    policy(&repo, by_name);
    let env = gate_env(&tmp.join("cache")).to_vec();
    let file = repo.join("a.txt");
    std::fs::write(&file, numbered(40, None)).unwrap();
    let send = |verb: &str, turn: u32, payload: &str| {
        hook(&repo, "claude-code", verb, SESSION, turn, payload, &env)
    };
    send("PreToolUse", 1, &read_by(&file, None));
    send(
        "PreToolUse",
        2,
        &spawn_by("toolu_1", "Explore", "Summarise a.txt.", None),
    );
    send("SubagentStart", 2, &opening("agent-7", "Explore"));
    (repo, file, env)
}

/// The family's recall projection, resolved the way the product resolves it.
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

/// The reasons of every observed repeat the audit relation holds.
fn observed_reasons(repo: &Path, cache: &Path) -> Vec<String> {
    let store = rift_store::open_migrated(recall_db(repo, cache), &rift_store::recall_migrations())
        .unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(store.query(
        "SELECT reason FROM memd_audit_event WHERE verb = 'memd.read_repeat_observed'",
        &[],
    ))
    .unwrap()
    .iter()
    .map(|row| row.get_text(0).unwrap())
    .collect()
}

/// ★ At the shipped `observe`, the subagent inherits its spawner's lines of
/// the path its prompt named, its read of them runs, and the window records
/// the read it was handed and an observation marked by name — never a decline.
#[test]
fn a_named_path_is_inherited_and_its_read_is_observed_never_declined() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, file, env) = spawned(tmp.path(), "observe");
    assert_eq!(
        kinds(&repo, Some("agent-7")),
        [kind("inherit", Some("by_name"))]
    );
    let main = kinds(&repo, None);
    assert!(main.contains(&kind("spawn", None)) && main.contains(&kind("claim", None)));

    let read = read_by(&file, Some("agent-7"));
    let out = hook(&repo, "claude-code", "PreToolUse", SESSION, 3, &read, &env);
    assert_eq!(declined_with(&out), None, "observe never declines");
    assert_eq!(
        kinds(&repo, Some("agent-7")),
        [
            kind("inherit", Some("by_name")),
            kind("read", None),
            kind("observe", Some("by_name")),
        ]
    );
    assert_eq!(
        observed_reasons(&repo, &tmp.path().join("cache")),
        ["inherited:by_name"]
    );
    assert!(
        kinds(&repo, Some("agent-7"))
            .iter()
            .all(|(_, c)| c.as_deref() != Some("fork")),
        "no client signals a fork, so nothing is inherited as one"
    );
}

/// With the lever written `enforce`, the inherited read is declined once and
/// its repeat runs, the one-decline rule holding for an inherited key too.
#[test]
fn an_enforced_class_declines_the_inherited_read_once() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, file, env) = spawned(tmp.path(), "enforce");
    let read = read_by(&file, Some("agent-7"));
    let first = hook(&repo, "claude-code", "PreToolUse", SESSION, 3, &read, &env);
    assert!(
        declined_with(&first).is_some(),
        "declined on the inherited lines"
    );
    let again = hook(&repo, "claude-code", "PreToolUse", SESSION, 3, &read, &env);
    assert_eq!(declined_with(&again), None, "the repeat is honoured");
}

/// With the lever off, nothing is inherited and the read simply runs.
#[test]
fn the_lever_off_inherits_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, _file, _env) = spawned(tmp.path(), "off");
    assert!(kinds(&repo, Some("agent-7")).is_empty());
}

/// A subagent started by a subagent inherits from that subagent's window.
#[test]
fn a_nested_subagent_inherits_from_its_own_spawner() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, _file, env) = spawned(tmp.path(), "observe");
    let other = repo.join("b.txt");
    std::fs::write(&other, numbered(30, None)).unwrap();
    let send =
        |verb: &str, payload: &str| hook(&repo, "claude-code", verb, SESSION, 3, payload, &env);
    send("PreToolUse", &read_by(&other, Some("agent-7")));
    send(
        "PreToolUse",
        &spawn_by("toolu_2", "Plan", "Check b.txt and a.txt.", Some("agent-7")),
    );
    send("SubagentStart", &opening("agent-8", "Plan"));
    let inherited: Vec<String> = journal(&repo, Some("agent-8"))
        .iter()
        .filter(|row| row["k"] == "inherit")
        .map(|row| row["p"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        inherited,
        ["b.txt"],
        "the spawner's own read, not the main window's"
    );
}

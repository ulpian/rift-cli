use rift_agent::window_journal::{Interval, Line};
use rift_shadow::hooks::RawHook;
use serde_json::json;

use super::*;

const SESSION: &str = "s-1";

fn hash(seed: u8) -> rift_core::Sha256Id {
    rift_hashchain::leaf_hash(&[seed])
}

fn read(path: &str, seed: u8) -> Line {
    Line::Read {
        turn: 1,
        path: path.to_owned(),
        hash: hash(seed),
        range: Interval::new(1, 40).unwrap(),
        total_lines: 40,
        at_ms: 1,
    }
}

fn hook(verb: &str, payload: serde_json::Value) -> RawHook {
    RawHook {
        agent: rift_core::AgentId::CLAUDE_CODE,
        verb: verb.to_owned(),
        session: SESSION.to_owned(),
        turn: Some(3),
        occurred_at_ms: 1_700_000_000_000,
        payload,
    }
}

fn spawning(call: &str, agent_type: &str, prompt: &str) -> RawHook {
    hook(
        "PreToolUse",
        json!({
            "session_id": SESSION,
            "prompt_id": "p-1",
            "tool_name": "Agent",
            "tool_use_id": call,
            "tool_input": { "subagent_type": agent_type, "prompt": prompt },
        }),
    )
}

fn opening(agent_type: &str) -> RawHook {
    hook(
        "SubagentStart",
        json!({ "session_id": SESSION, "prompt_id": "p-1", "agent_type": agent_type }),
    )
}

fn step(work_tree: &Path, raw: &RawHook, window: Option<&str>, by_name: Mode) {
    spawn_step(&SpawnHook {
        raw,
        work_tree,
        window,
        by_name,
        turn: 3,
        at_ms: 99,
    });
}

fn lines(work_tree: &Path, window: Option<&str>) -> Vec<Line> {
    WindowJournal::open(work_tree, SESSION, window)
        .load()
        .journal
        .lines()
        .to_vec()
}

fn append(work_tree: &Path, window: Option<&str>, line: &Line) {
    WindowJournal::open(work_tree, SESSION, window)
        .append(line)
        .unwrap();
}

fn inherited_paths(work_tree: &Path, window: &str) -> Vec<String> {
    lines(work_tree, Some(window))
        .into_iter()
        .filter_map(|line| match line {
            Line::Inherit {
                class: InheritClass::ByName,
                path,
                ..
            } => Some(path),
            _ => None,
        })
        .collect()
}

/// The spawning call names, of what its window holds, exactly the paths its
/// prompt names — in either spelling — and nothing of the prompt itself.
#[test]
fn a_spawn_line_names_the_held_paths_its_prompt_names() {
    let repo = tempfile::tempdir().unwrap();
    append(repo.path(), None, &read("src/a.rs", 1));
    append(repo.path(), None, &read("src/b.rs", 2));
    let prompt = "Fix /abs/repo/src/a.rs, keep src/c.rs, ignore xsrc/b.rs.";
    step(
        repo.path(),
        &spawning("toolu_1", "Explore", prompt),
        None,
        Mode::Observe,
    );
    let spawn = lines(repo.path(), None).into_iter().last().unwrap();
    assert_eq!(
        spawn,
        Line::Spawn {
            turn: 3,
            call: "toolu_1".to_owned(),
            prompt: "p-1".to_owned(),
            agent_type: "Explore".to_owned(),
            spawner: None,
            names: vec!["src/a.rs".to_owned()],
            at_ms: 99,
        }
    );
    assert!(
        !spawn.to_json_line().contains("Fix"),
        "no prompt text is kept"
    );
}

/// A call that is not a spawn, or a spawn without the prompt id an opening
/// matches it by, writes nothing.
#[test]
fn only_a_spawn_with_a_prompt_id_leaves_a_line() {
    let repo = tempfile::tempdir().unwrap();
    let mut read_call = spawning("toolu_1", "Explore", "x");
    read_call.payload["tool_name"] = json!("Read");
    step(repo.path(), &read_call, None, Mode::Observe);
    let mut anonymous = spawning("toolu_2", "Explore", "x");
    anonymous
        .payload
        .as_object_mut()
        .unwrap()
        .remove("prompt_id");
    step(repo.path(), &anonymous, None, Mode::Observe);
    assert!(lines(repo.path(), None).is_empty());
}

/// The opening claims its sole spawn once, and inherits the spawner's live
/// reads of the named paths by name; a second opening of the same window — a
/// resume — writes nothing more.
#[test]
fn an_opening_claims_its_spawn_once_and_inherits_by_name() {
    let repo = tempfile::tempdir().unwrap();
    append(repo.path(), None, &read("src/a.rs", 1));
    append(repo.path(), None, &read("src/z.rs", 9));
    step(
        repo.path(),
        &spawning("toolu_1", "Explore", "read src/a.rs"),
        None,
        Mode::Observe,
    );
    step(
        repo.path(),
        &opening("Explore"),
        Some("agent-7"),
        Mode::Observe,
    );
    assert_eq!(inherited_paths(repo.path(), "agent-7"), ["src/a.rs"]);
    let claimed = lines(repo.path(), None);
    assert!(claimed.contains(&Line::Claim {
        turn: 3,
        agent: "agent-7".to_owned(),
        call: Some("toolu_1".to_owned()),
        at_ms: 99,
    }));
    let before = (
        lines(repo.path(), None).len(),
        lines(repo.path(), Some("agent-7")).len(),
    );
    step(
        repo.path(),
        &opening("Explore"),
        Some("agent-7"),
        Mode::Observe,
    );
    let after = (
        lines(repo.path(), None).len(),
        lines(repo.path(), Some("agent-7")).len(),
    );
    assert_eq!(before, after);
}

/// Two spawns one opening could answer are never guessed between: the
/// opening claims no spawn and inherits nothing.
#[test]
fn twin_spawns_are_never_guessed_between() {
    let repo = tempfile::tempdir().unwrap();
    append(repo.path(), None, &read("src/a.rs", 1));
    step(
        repo.path(),
        &spawning("toolu_1", "Explore", "src/a.rs"),
        None,
        Mode::Observe,
    );
    step(
        repo.path(),
        &spawning("toolu_2", "Explore", "src/a.rs"),
        None,
        Mode::Observe,
    );
    step(
        repo.path(),
        &opening("Explore"),
        Some("agent-7"),
        Mode::Observe,
    );
    assert!(inherited_paths(repo.path(), "agent-7").is_empty());
    assert!(lines(repo.path(), None).contains(&Line::Claim {
        turn: 3,
        agent: "agent-7".to_owned(),
        call: None,
        at_ms: 99,
    }));
}

/// With the lever off a spawn names nothing and no line is inherited, while
/// the claim that links the windows is still made.
#[test]
fn the_lever_off_inherits_nothing() {
    let repo = tempfile::tempdir().unwrap();
    append(repo.path(), None, &read("src/a.rs", 1));
    step(
        repo.path(),
        &spawning("toolu_1", "Explore", "src/a.rs"),
        None,
        Mode::Off,
    );
    step(repo.path(), &opening("Explore"), Some("agent-7"), Mode::Off);
    assert!(inherited_paths(repo.path(), "agent-7").is_empty());
    assert!(lines(repo.path(), None)
        .iter()
        .any(|line| matches!(line, Line::Spawn { names, .. } if names.is_empty())));
}

/// A subagent started by another subagent inherits from that subagent's
/// window, never from the main window's.
#[test]
fn a_nested_spawn_inherits_from_its_own_spawner() {
    let repo = tempfile::tempdir().unwrap();
    append(repo.path(), None, &read("src/main.rs", 1));
    append(repo.path(), Some("agent-parent"), &read("src/a.rs", 2));
    let prompt = "look at src/a.rs and src/main.rs";
    step(
        repo.path(),
        &spawning("toolu_9", "Plan", prompt),
        Some("agent-parent"),
        Mode::Observe,
    );
    step(
        repo.path(),
        &opening("Plan"),
        Some("agent-child"),
        Mode::Observe,
    );
    assert_eq!(inherited_paths(repo.path(), "agent-child"), ["src/a.rs"]);
}

/// A spawn naming more paths than one row can carry keeps as many as fit.
#[test]
fn an_oversized_spawn_line_drops_names_until_it_fits() {
    let names: Vec<String> = (0..32)
        .map(|i| format!("src/{}/{i}.rs", "d".repeat(150)))
        .collect();
    let line = fitted(Line::Spawn {
        turn: 1,
        call: "c".to_owned(),
        prompt: "p".to_owned(),
        agent_type: "t".to_owned(),
        spawner: None,
        names,
        at_ms: 1,
    });
    assert!(line.to_json_line().len() < MAX_APPEND_BYTES);
    assert!(matches!(line, Line::Spawn { names, .. } if !names.is_empty()));
}

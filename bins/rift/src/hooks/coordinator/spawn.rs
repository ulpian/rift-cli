//! The two ends of a spawn: the line a spawning window's call leaves in its
//! session's main journal, and the claim a subagent's opening makes on it —
//! with the lines the subagent inherits by name when the claim names one.
//!
//! The opening hook carries no prompt, only the id of the user prompt being
//! processed and the subagent's type; the spawning call carries both, its own
//! call id and the prompt itself. So the spawning call writes what it knows
//! where the opening can find it — the session's main journal, the one window
//! every subagent of the session can name without knowing which window started
//! it — and the opening claims the one spawn that matches. A claim is exact or
//! it names nothing: see [`Journal::sole_spawn`].
//!
//! Files only, through the validating journal: no store, no tokenizer, and no
//! byte of any file the paths name. The prompt is scanned for names in memory
//! and never written anywhere.

use std::collections::BTreeSet;
use std::path::Path;

use rift_agent::window_journal::{
    inherited_from_read, is_ident, InheritClass, Journal, Line, MAX_SPAWN_NAMES,
};
use rift_recall_policy::Mode;

use crate::window_journal::{WindowJournal, MAX_APPEND_BYTES};

/// The tool names Claude Code spawns a subagent window with.
const SPAWNING_TOOLS: [&str; 2] = ["Agent", "Task"];

/// The subagent type a spawning call that names none starts.
const DEFAULT_AGENT_TYPE: &str = "general-purpose";

/// The longest spawning prompt scanned for names. A prompt past it inherits
/// nothing by name: the scan is on the spawning call's critical path.
const MAX_PROMPT_BYTES: usize = 256 * 1024;

/// The most inherited lines one opening writes.
const MAX_INHERITED_LINES: usize = 256;

/// One hook, as the two ends of a spawn read it.
pub(in crate::hooks) struct SpawnHook<'a> {
    /// The hook itself.
    pub(in crate::hooks) raw: &'a rift_shadow::hooks::RawHook,
    /// The checkout whose journals are written.
    pub(in crate::hooks) work_tree: &'a Path,
    /// The window the hook fired in: the spawner on a spawning call, the new
    /// subagent at its opening.
    pub(in crate::hooks) window: Option<&'a str>,
    /// How far a decline may rest on lines inherited by name; `off` writes
    /// no names and no inherited lines.
    pub(in crate::hooks) by_name: Mode,
    /// The turn every line written here carries.
    pub(in crate::hooks) turn: u32,
    /// The moment it carries.
    pub(in crate::hooks) at_ms: u64,
}

/// Record whichever end of a spawn this hook is, if it is one.
pub(in crate::hooks) fn spawn_step(hook: &SpawnHook<'_>) {
    if hook.raw.agent != rift_core::AgentId::CLAUDE_CODE || hook.raw.session.is_empty() {
        return;
    }
    match hook.raw.verb.as_str() {
        "PreToolUse" => record_spawn(hook),
        "SubagentStart" => record_opening(hook),
        _ => {}
    }
}

/// The spawning window's end: one spawn line in the session's main journal.
fn record_spawn(hook: &SpawnHook<'_>) {
    let payload = &hook.raw.payload;
    let tool = payload.get("tool_name").and_then(serde_json::Value::as_str);
    if !tool.is_some_and(|tool| SPAWNING_TOOLS.contains(&tool)) {
        return;
    }
    let (Some(call), Some(prompt)) = (ident(payload, "tool_use_id"), ident(payload, "prompt_id"))
    else {
        return;
    };
    let input = payload.get("tool_input");
    let field = |name: &str| {
        input
            .and_then(|i| i.get(name))
            .and_then(serde_json::Value::as_str)
    };
    let agent_type = field("subagent_type").unwrap_or(DEFAULT_AGENT_TYPE);
    if !is_ident(agent_type) {
        return;
    }
    let names = match hook.by_name {
        Mode::Off => Vec::new(),
        _ => named_holdings(hook, field("prompt").unwrap_or_default()),
    };
    let line = fitted(Line::Spawn {
        turn: hook.turn,
        call: call.to_owned(),
        prompt: prompt.to_owned(),
        agent_type: agent_type.to_owned(),
        spawner: hook.window.map(str::to_owned),
        names,
        at_ms: hook.at_ms,
    });
    let _ = main_journal(hook).append(&line);
}

/// The paths the spawning window holds whose names the prompt carries, in
/// path order, at most [`MAX_SPAWN_NAMES`] of them.
fn named_holdings(hook: &SpawnHook<'_>, prompt: &str) -> Vec<String> {
    if prompt.is_empty() || prompt.len() > MAX_PROMPT_BYTES {
        return Vec::new();
    }
    let spawner = WindowJournal::open(hook.work_tree, &hook.raw.session, hook.window).load();
    let held: BTreeSet<&str> = spawner
        .journal
        .live()
        .iter()
        .filter_map(|line| match line {
            Line::Read { path, .. } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    held.into_iter()
        .filter(|path| names_the_path(prompt, path))
        .take(MAX_SPAWN_NAMES)
        .map(str::to_owned)
        .collect()
}

/// The spawn line with names dropped from its tail until it fits one atomic
/// append: a spawn that names fewer paths beats one that is never written.
fn fitted(mut line: Line) -> Line {
    while line.to_json_line().len() >= MAX_APPEND_BYTES {
        let Line::Spawn { names, .. } = &mut line else {
            break;
        };
        if names.pop().is_none() {
            break;
        }
    }
    line
}

/// The subagent's end: a claim in the session's main journal, once per
/// window, and — when it names a spawn that named paths — the lines its
/// spawner held of them, in its own journal, marked as inherited by name.
///
/// A window whose journal already holds anything, or that already claimed,
/// has opened before: the client fires this verb again when a subagent is
/// resumed or a teammate takes a new message, and neither is a spawn.
fn record_opening(hook: &SpawnHook<'_>) {
    let Some(agent) = hook.window else {
        return;
    };
    let own = WindowJournal::open(hook.work_tree, &hook.raw.session, Some(agent));
    if !own.load().journal.lines().is_empty() {
        return;
    }
    let main = main_journal(hook);
    let held = main.load().journal;
    if held.claim_of(agent).is_some() {
        return;
    }
    let spawn = sole_spawn_of(&held, &hook.raw.payload);
    let call = match spawn {
        Some(Line::Spawn { call, .. }) => Some(call.clone()),
        _ => None,
    };
    let claim = Line::Claim {
        turn: hook.turn,
        agent: agent.to_owned(),
        call,
        at_ms: hook.at_ms,
    };
    if main.append(&claim).is_err() || hook.by_name == Mode::Off {
        return;
    }
    if let Some(Line::Spawn { spawner, names, .. }) = spawn {
        inherit_by_name(hook, &own, spawner.as_deref(), names);
    }
}

/// The one unclaimed spawn the opening payload's prompt and type match.
fn sole_spawn_of<'a>(held: &'a Journal, payload: &serde_json::Value) -> Option<&'a Line> {
    let prompt = ident(payload, "prompt_id")?;
    let agent_type = ident(payload, "agent_type")?;
    held.sole_spawn(prompt, agent_type)
}

/// Copy the spawner's live reads of the named paths into the subagent's own
/// journal as lines inherited by name.
///
/// Read at the opening, which follows the spawning call by the time it takes
/// the client to start a window: what the spawner held then is what "live at
/// the spawn" means without a clock.
fn inherit_by_name(
    hook: &SpawnHook<'_>,
    own: &WindowJournal,
    spawner: Option<&str>,
    names: &[String],
) {
    if names.is_empty() {
        return;
    }
    let from = WindowJournal::open(hook.work_tree, &hook.raw.session, spawner).load();
    let inherited = from
        .journal
        .live()
        .iter()
        .filter(|line| matches!(line, Line::Read { path, .. } if names.contains(path)))
        .filter_map(|line| inherited_from_read(line, InheritClass::ByName, hook.turn, hook.at_ms))
        .take(MAX_INHERITED_LINES);
    for line in inherited {
        let _ = own.append(&line);
    }
}

/// The session's main journal, where spawns and claims meet.
fn main_journal(hook: &SpawnHook<'_>) -> WindowJournal {
    WindowJournal::open(hook.work_tree, &hook.raw.session, None)
}

/// An identifier member of the payload, or `None` when it is absent or not
/// an identifier's shape.
fn ident<'a>(payload: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|text| is_ident(text))
}

/// Whether `prompt` names `path` with no name character on either side of it.
///
/// The journal's spelling is checkout-relative, and an absolute spelling ends
/// in it after a `/`, so one bounded match finds both. The same rule the
/// replay counts this class by, so the two cannot disagree about a name.
fn names_the_path(prompt: &str, path: &str) -> bool {
    let is_name_char = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    prompt.match_indices(path).any(|(at, _)| {
        let before = prompt.get(..at).and_then(|head| head.chars().next_back());
        let after = prompt
            .get(at.saturating_add(path.len())..)
            .and_then(|tail| tail.chars().next());
        !before.is_some_and(is_name_char) && !after.is_some_and(is_name_char)
    })
}

#[cfg(test)]
mod tests;

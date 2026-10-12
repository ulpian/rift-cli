//! The link between a spawning call and the window it opens, made the way the
//! hook makes it, and the lines the window inherits by name through it.
//!
//! The hook is handed the spawning call's id, its prompt id and the type it
//! asks for, and at the opening only the prompt id and the type. So the
//! replay claims the same way — the one unclaimed spawn under the window's
//! prompt id and type, or none — and checks every claim against the client's
//! own sidecar, which names the spawning call outright. A claim the sidecar
//! contradicts is counted on its own line, because it is the line that has to
//! stay at zero.

use rift_agent::window_journal::{inherited_from_read, InheritClass, Journal, Line};

use super::inheritance::names_the_path;
use super::replay::{Ctx, TreeState, WindowState};
use super::report::Report;
use super::transcript::{Call, Window};

/// The subagent type a spawning call that names none starts.
const DEFAULT_AGENT_TYPE: &str = "general-purpose";

/// One spawning call, as the session's main journal would hold its line.
pub(super) struct PendingSpawn {
    call: String,
    prompt: String,
    agent_type: String,
    claimed: bool,
    /// The spawner's live reads of the paths the prompt named, as the lines
    /// the opening window would inherit.
    rows: Vec<Line>,
}

/// What a window's opening hook is handed, and what the sidecar says.
pub(super) struct Opening {
    under: Option<(String, String)>,
    call: Option<String>,
    opened: bool,
    /// The spawn this window claimed at its opening, by position.
    linked: Option<usize>,
}

/// Every window's opening facts, in tree order.
pub(super) fn openings(windows: &[Window]) -> Vec<Opening> {
    windows
        .iter()
        .map(|window| Opening {
            under: window.opened_under.clone(),
            call: window.spawning_call.clone(),
            opened: false,
            linked: None,
        })
        .collect()
}

/// The spawning end: the line a spawning call leaves, with what the spawner
/// held of the paths its prompt names.
pub(super) fn note_the_spawn(ctx: &mut Ctx<'_>, call: &Call) {
    let Some(prompt_id) = &call.prompt_id else {
        return;
    };
    let field = |name: &str| call.input.get(name).and_then(serde_json::Value::as_str);
    let prompt = field("prompt").unwrap_or_default();
    ctx.report.spawn_links.spawns = ctx.report.spawn_links.spawns.saturating_add(1);
    ctx.tree.spawns.push(PendingSpawn {
        call: call.id.clone(),
        prompt: prompt_id.clone(),
        agent_type: field("subagent_type")
            .unwrap_or(DEFAULT_AGENT_TYPE)
            .to_owned(),
        claimed: false,
        rows: named_rows(&ctx.state.journal, prompt),
    });
}

/// The spawner's live reads of every path `prompt` names, as inherited lines.
fn named_rows(journal: &Journal, prompt: &str) -> Vec<Line> {
    journal
        .live()
        .iter()
        .filter(|line| matches!(line, Line::Read { path, .. } if names_the_path(prompt, path)))
        .filter_map(|line| inherited_from_read(line, InheritClass::ByName, 0, 0))
        .collect()
}

/// The opening end, at a window's first event: claim the sole unclaimed spawn
/// under its prompt id and type, check it against the sidecar, and hand the
/// window what that spawn named.
pub(super) fn open_the_window(
    state: &mut WindowState,
    tree: &mut TreeState,
    report: &mut Report,
    index: usize,
) {
    let Some(opening) = tree.openings.get_mut(index) else {
        return;
    };
    if opening.opened {
        return;
    }
    opening.opened = true;
    let Some((prompt, agent_type)) = opening.under.clone() else {
        return;
    };
    let truth = opening.call.clone();
    let links = &mut report.spawn_links;
    links.openings = links.openings.saturating_add(1);
    let mut matching = tree.spawns.iter_mut().enumerate().filter(|(_, spawn)| {
        !spawn.claimed && spawn.prompt == prompt && spawn.agent_type == agent_type
    });
    let (Some((at, sole)), None) = (matching.next(), matching.next()) else {
        links.unnamed = links.unnamed.saturating_add(1);
        return;
    };
    sole.claimed = true;
    if truth.as_deref() == Some(sole.call.as_str()) {
        links.exact = links.exact.saturating_add(1);
    } else {
        links.wrong = links.wrong.saturating_add(1);
    }
    let class = class_index(InheritClass::ByName);
    if let Some(lines) = report.inherited_lines.get_mut(class) {
        *lines = lines.saturating_add(u64::try_from(sole.rows.len()).unwrap_or(u64::MAX));
    }
    for row in &sole.rows {
        state.journal.push(row.clone());
    }
    if let Some(opening) = tree.openings.get_mut(index) {
        opening.linked = Some(at);
    }
}

/// Whether the spawn `window` opened under left an inherited line for `key`:
/// the spawner held some of the path and its prompt named it.
pub(super) fn handed_down(tree: &TreeState, window: usize, key: &str) -> bool {
    tree.openings
        .get(window)
        .and_then(|opening| opening.linked)
        .and_then(|at| tree.spawns.get(at))
        .is_some_and(|spawn| {
            spawn
                .rows
                .iter()
                .any(|line| matches!(line, Line::Inherit { path, .. } if path == key))
        })
}

/// Where a class sits in the report's per-class arrays.
pub(super) fn class_index(class: InheritClass) -> usize {
    InheritClass::ALL
        .iter()
        .position(|c| *c == class)
        .unwrap_or_default()
}

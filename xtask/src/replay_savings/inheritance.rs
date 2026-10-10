//! What a window was handed by the window that started it, and so why it read
//! a revision its sibling had already read.
//!
//! The report has always said how much of the first-read floor is a second
//! window reading what a first one already held. This splits that pool by the
//! route the second window could have inherited the bytes through, because
//! each route is a different lever: a fork already carries its parent's
//! history, a spawning prompt can carry the lines themselves, and a prompt
//! that only names the file is where a handed-down coverage would have to
//! start. Only counts leave this module — never a prompt, never a path.
//!
//! The unit is one later window's first read of a revision a sibling had
//! read first. A revision five windows read is one pair and four such reads,
//! so the routes sum to the reads, not to the pairs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rift_agent::window_journal::{Journal, Line};
use rift_core::Sha256Id;
use serde_json::Value;

use super::transcript::{Event, Window};

/// The route a sibling window's re-read could have been inherited through.
///
/// Counted once per later window's first read of a revision a sibling window
/// had read first — not once per `(path, revision)` pair — so the four routes
/// sum to the later readers' calls and tokens.
///
/// Applied with precedence fork, then paste, then by name, then none: the
/// first route that holds is the one the read is counted under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Inheritance {
    /// The window's transcript starts from another window's history: its
    /// first record continues, or repeats, a record another window of the tree
    /// holds. A forked window carries its parent's leading records, so the
    /// evidence is a record identity shared across two windows. A client that
    /// forks by copying the history under fresh identities leaves no such
    /// evidence, and a corpus without forks counts zero here.
    Fork,
    /// The prompt that spawned the window carries a run of at least the
    /// decision floor's line count of consecutive lines of what the parent's
    /// own read of that revision returned, compared with the read tool's
    /// line-number gutter removed and surrounding whitespace trimmed.
    Paste,
    /// The spawning prompt names the path, repository-relative or absolute —
    /// the relative spelling is the tail of the absolute one, so one match
    /// with a boundary on each side covers both.
    ByName,
    /// None of the above.
    None,
}

/// What one window of a session tree inherited, established before any of
/// its calls is replayed.
#[derive(Clone, Debug, Default)]
pub(super) struct WindowLead {
    /// Whether the window starts from another window's history.
    fork: bool,
    /// The text of the prompt that spawned it, where it has one.
    prompt: Option<String>,
    /// The window whose tool call spawned it, where the client recorded which.
    pub(super) parent: Option<usize>,
}

/// The tool names a client spawns a subagent window with.
pub(super) const SPAWNING_TOOLS: [&str; 2] = ["Agent", "Task"];

impl Inheritance {
    /// Every class, in the order the table prints them.
    pub const ALL: [Self; 4] = [Self::Fork, Self::Paste, Self::ByName, Self::None];

    /// The key this class is published under.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Fork => "fork",
            Self::Paste => "paste",
            Self::ByName => "by_name",
            Self::None => "none",
        }
    }

    /// The sentence the table prints for this class.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fork => "    fork: began from another window's history",
            Self::Paste => "    paste: the spawning prompt carried the lines",
            Self::ByName => "    by name: the spawning prompt named the path",
            Self::None => "    none: nothing handed down",
        }
    }

    /// Where this class sits in [`Self::ALL`].
    pub(super) const fn index(self) -> usize {
        match self {
            Self::Fork => 0,
            Self::Paste => 1,
            Self::ByName => 2,
            Self::None => 3,
        }
    }
}

/// What every window of one tree inherited.
///
/// `paths` and `windows` are the tree's window files and what was read out of
/// them, in the same order. A subagent's spawning prompt is the input of the
/// tool call its sidecar names; where there is no sidecar, or it names a call
/// the tree does not hold, the window's own leading prompt stands in, which is
/// the same text as the client delivers it.
pub(super) fn leads(paths: &[&Path], windows: &[Window]) -> Vec<WindowLead> {
    let mut spawns: BTreeMap<&str, (usize, String)> = BTreeMap::new();
    for (index, window) in windows.iter().enumerate() {
        for (_, _, event) in &window.events {
            let Event::Call(call) = event else { continue };
            if SPAWNING_TOOLS.contains(&call.name.as_str()) {
                spawns.insert(call.id.as_str(), (index, input_text(&call.input)));
            }
        }
    }
    let mut out = Vec::with_capacity(windows.len());
    for (index, (path, window)) in paths.iter().zip(windows).enumerate() {
        let spawned_by = window
            .spawning_call
            .as_deref()
            .and_then(|id| spawns.get(id));
        let is_subagent = path
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "subagents");
        let prompt = match spawned_by {
            Some((_, text)) => Some(text.clone()),
            None if is_subagent => window.lead.prompt.clone(),
            None => None,
        };
        out.push(WindowLead {
            fork: starts_from_another(index, windows),
            prompt,
            parent: spawned_by.map(|(parent, _)| *parent),
        });
    }
    out
}

/// Which route one sibling re-read was inherited through.
///
/// `parent_text` is what the spawning window's own first read of the same
/// revision returned, where it read it at all.
pub(super) fn inheritance_of(
    lead: &WindowLead,
    parent_text: Option<&str>,
    key: &str,
    min_lines: usize,
) -> Inheritance {
    if lead.fork {
        return Inheritance::Fork;
    }
    let Some(prompt) = lead.prompt.as_deref() else {
        return Inheritance::None;
    };
    if parent_text.is_some_and(|text| carries_a_run(prompt, text, min_lines)) {
        return Inheritance::Paste;
    }
    if names_the_path(prompt, key) {
        return Inheritance::ByName;
    }
    Inheritance::None
}

/// The revisions the section a subagent's window opens with would name, read
/// off the spawning window's journal at the instant of the spawn.
///
/// The latest revision of each path the journal still holds, newest first,
/// through the shipped renderer, so its cap and its refusals are the
/// section's own rather than a second statement of them. The subagent holds
/// nothing at its spawn, so nothing is subtracted.
pub(super) fn opening_section(journal: &Journal) -> BTreeSet<(String, String)> {
    let mut latest: BTreeMap<&str, (usize, Sha256Id)> = BTreeMap::new();
    for (order, line) in journal.live().iter().enumerate() {
        if let Line::Read { path, hash, .. } = line {
            latest.insert(path.as_str(), (order, *hash));
        }
    }
    let mut newest_first: Vec<(&str, usize, Sha256Id)> = latest
        .into_iter()
        .map(|(path, (order, hash))| (path, order, hash))
        .collect();
    newest_first.sort_by_key(|(_, order, _)| std::cmp::Reverse(*order));
    let hexes: Vec<String> = newest_first
        .iter()
        .map(|(_, _, hash)| hash.to_hex())
        .collect();
    let reads: Vec<rift_recall_validity::trust::CoordinatorRead<'_>> = newest_first
        .iter()
        .zip(&hexes)
        .map(|((path, _, _), hash)| rift_recall_validity::trust::CoordinatorRead { path, hash })
        .collect();
    let Some((_, named)) = rift_recall_validity::trust::render_coordinator_reads_naming(&reads, 0)
    else {
        return BTreeSet::new();
    };
    named
        .iter()
        .filter_map(|at| newest_first.get(*at).zip(hexes.get(*at)))
        .map(|((path, _, _), hex)| ((*path).to_owned(), hex.clone()))
        .collect()
}

/// The text of a spawning call's input: its prompt, or every string it
/// carries when it has no prompt field.
fn input_text(input: &Value) -> String {
    if let Some(prompt) = input.get("prompt").and_then(Value::as_str) {
        return prompt.to_owned();
    }
    let Some(fields) = input.as_object() else {
        return String::new();
    };
    let strings: Vec<&str> = fields.values().filter_map(Value::as_str).collect();
    strings.join("\n")
}

/// Whether window `index`'s first record continues or repeats a record
/// another window of the tree holds.
fn starts_from_another(index: usize, windows: &[Window]) -> bool {
    let Some(window) = windows.get(index) else {
        return false;
    };
    let leading: BTreeSet<&str> = [
        window.lead.uuid.as_deref(),
        window.lead.parent_uuid.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();
    windows
        .iter()
        .enumerate()
        .filter(|(other, _)| *other != index)
        .any(|(_, other)| leading.iter().any(|uuid| other.lead.uuids.contains(*uuid)))
}

/// Whether `prompt` carries `run` consecutive lines of `text`, each compared
/// with the read tool's gutter removed and surrounding whitespace trimmed.
fn carries_a_run(prompt: &str, text: &str, run: usize) -> bool {
    if run == 0 {
        return false;
    }
    let prompt_lines: Vec<&str> = prompt.lines().map(str::trim).collect();
    if prompt_lines.len() < run {
        return false;
    }
    let runs: BTreeSet<&[&str]> = prompt_lines.windows(run).collect();
    let text_lines: Vec<&str> = text.lines().map(without_gutter).collect();
    text_lines.windows(run).any(|lines| runs.contains(lines))
}

/// One result line without the read tool's line-number gutter — digits and
/// then an arrow or a tab — and trimmed.
fn without_gutter(line: &str) -> &str {
    let trimmed = line.trim_start();
    let rest = trimmed.trim_start_matches(|c: char| c.is_ascii_digit());
    if rest.len() == trimmed.len() {
        return trimmed.trim();
    }
    match rest.strip_prefix('→').or_else(|| rest.strip_prefix('\t')) {
        Some(body) => body.trim(),
        None => trimmed.trim(),
    }
}

/// Whether `prompt` names `key` with no name character on either side of it.
fn names_the_path(prompt: &str, key: &str) -> bool {
    let is_name_char = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    prompt.match_indices(key).any(|(at, _)| {
        let before = prompt.get(..at).and_then(|head| head.chars().next_back());
        let after = prompt
            .get(at.saturating_add(key.len())..)
            .and_then(|tail| tail.chars().next());
        !before.is_some_and(is_name_char) && !after.is_some_and(is_name_char)
    })
}

#[cfg(test)]
mod tests {
    use rift_agent::window_journal::{EvictReason, Interval, Journal, Line};

    use super::{carries_a_run, names_the_path, opening_section, without_gutter};

    fn read(path: &str, seed: u8) -> Line {
        Line::Read {
            turn: 1,
            path: path.to_owned(),
            hash: rift_agent::content_sha256(&[seed]),
            range: Interval::new(1, 9).unwrap(),
            total_lines: 9,
            at_ms: 0,
        }
    }

    /// What a spawned window would be told: the spawning window's live reads,
    /// the latest revision of each path, newest first, eight at most, and
    /// nothing the section would refuse to name.
    #[test]
    fn the_opening_section_names_the_newest_live_revisions_it_can_name() {
        let mut journal = Journal::default();
        journal.push(read("src/evicted.rs", 0));
        journal.push(Line::Evict {
            turn: 1,
            reason: EvictReason::Compact,
            at_ms: 0,
        });
        for seed in 1..=9_u8 {
            journal.push(read(&format!("src/m{seed}.rs"), seed));
        }
        journal.push(read("/etc/outside.rs", 10));
        journal.push(read("src/m9.rs", 11));
        let named = opening_section(&journal);
        let named_as = |path: &str, seed: u8| {
            (
                path.to_owned(),
                rift_agent::content_sha256(&[seed]).to_hex(),
            )
        };
        let mut expected: Vec<(String, String)> = (2..=8_u8)
            .map(|seed| named_as(&format!("src/m{seed}.rs"), seed))
            .collect();
        expected.push(named_as("src/m9.rs", 11));
        assert_eq!(named, expected.into_iter().collect());
    }

    #[test]
    fn a_paste_is_a_run_of_lines_without_the_gutter() {
        let text = "     1→alpha\n     2→beta\n     3→gamma\n";
        assert!(carries_a_run("see:\n  alpha\nbeta\ngamma  \n", text, 3));
        assert!(!carries_a_run("alpha\ngamma\nbeta\n", text, 3));
        assert!(!carries_a_run("alpha\nbeta\n", text, 3), "too short");
        assert!(!carries_a_run("alpha", text, 0), "an empty run is none");
        assert_eq!(without_gutter("12\tbody "), "body");
        assert_eq!(without_gutter("2026 was"), "2026 was");
    }

    #[test]
    fn a_path_is_named_only_with_a_boundary_on_each_side() {
        assert!(names_the_path("look at /repo/src/lib.rs.", "src/lib.rs"));
        assert!(names_the_path("`src/lib.rs`", "src/lib.rs"));
        assert!(!names_the_path("see src/lib.rs2", "src/lib.rs"));
        assert!(!names_the_path("see xsrc/lib.rs", "src/lib.rs"));
    }
}

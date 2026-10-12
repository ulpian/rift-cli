//! The sibling re-reads whose spawning prompt named the path, cut by what the
//! spawner had to hand down: nothing of the path, lines the read went beyond,
//! or lines that covered it. The three rows add up to the "by name" row.
//!
//! A module of its own because the by-name row says the prompt named the
//! path and nothing of what a child could have been given for it, and every
//! figure it adds is on rows of its own, so every row before it reads the
//! same with or without it.
//!
//! **The unit** is the by-name row's own: one later window's first read of a
//! revision a sibling window had read first, counted under the route "the
//! spawning prompt named the path".
//!
//! **The classes**, decided at that first read, a read in the first one it
//! fits:
//!
//! 1. *The read was answered from lines handed down.* The shipped decision,
//!    run over what the window inherited at its opening, found the lines held
//!    and would have declined it were that class enforced — the set the
//!    "reads it alone would decline" row counts, taken here per read.
//! 2. *The spawner handed down lines of the path, and the read went beyond
//!    them.* The spawn the window opened under left an inherited line for
//!    this path, and the decision did not find the read inside what was left
//!    of it.
//! 3. *The spawner held no line of the path.* The spawn the window opened
//!    under left no inherited line for this path — another window of the
//!    family had read it, or no spawn was linked to the window at all.

use serde_json::{json, Map, Value};

use super::render::row;
use super::report::{Report, Tally};

/// The classes, in the order the table prints them: a key and a label.
const CLASSES: [(&str, &str); 3] = [
    (
        "spawner_held_no_line",
        "  the spawner held no line of the path",
    ),
    (
        "beyond_inherited_lines",
        "  the read went beyond the lines handed down",
    ),
    ("inherited", "  lines handed down covered it (counted)"),
];

/// What the spawner handed a window for a path its prompt named.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ByNameOrigin {
    /// The spawner left no line of the path.
    #[default]
    SpawnerHeldNone,
    /// The spawner left lines, and the read was not inside them.
    BeyondTheRun,
    /// The inherited lines answered the read.
    Inherited,
}

/// The by-name re-reads by origin.
#[derive(Clone, Debug, Default)]
pub struct ByNameSplit {
    /// One tally per entry of `CLASSES`.
    classes: [Tally; 3],
}

impl ByNameOrigin {
    /// The origin of a first read, from what the decision found and whether
    /// the spawn handed down any line of the path.
    pub(super) fn of(decided_from_inherited_lines: bool, handed_down: bool) -> Self {
        match (decided_from_inherited_lines, handed_down) {
            (true, _) => Self::Inherited,
            (false, true) => Self::BeyondTheRun,
            (false, false) => Self::SpawnerHeldNone,
        }
    }

    /// Where this origin sits in `CLASSES`.
    const fn index(self) -> usize {
        match self {
            Self::SpawnerHeldNone => 0,
            Self::BeyondTheRun => 1,
            Self::Inherited => 2,
        }
    }
}

impl ByNameSplit {
    /// Count one by-name re-read.
    pub(super) fn note(&mut self, origin: ByNameOrigin, tokens: u64) {
        if let Some(tally) = self.classes.get_mut(origin.index()) {
            tally.add(tokens);
        }
    }
}

/// The table's rows for the by-name re-reads cut by origin.
pub(super) fn by_name_rows(report: &Report, denominator: u64) -> String {
    let mut out = String::from("\n  by name, cut by what the spawner handed down\n");
    for ((_, label), tally) in CLASSES.iter().zip(report.by_name_split.classes) {
        out.push_str(&row(label, tally, denominator));
    }
    out
}

/// The same rows for the machine-readable document.
pub(super) fn by_name_json(report: &Report) -> Value {
    let mut map = Map::new();
    for ((key, _), t) in CLASSES.iter().zip(report.by_name_split.classes) {
        map.insert(
            (*key).to_owned(),
            json!({ "calls": t.calls, "tokens": t.tokens }),
        );
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ★ A read answered from handed-down lines is counted inherited
    /// whatever else is true; one the spawner left lines for but that went
    /// beyond them is the second class; one with no line handed down, or no
    /// spawn at all, is the first. The three add up to the reads noted.
    #[test]
    fn a_by_name_read_is_cut_by_what_the_spawner_handed_down() {
        let mut split = ByNameSplit::default();
        split.note(ByNameOrigin::of(true, true), 1);
        split.note(ByNameOrigin::of(false, true), 10);
        split.note(ByNameOrigin::of(false, false), 100);
        split.note(ByNameOrigin::default(), 1_000);
        let tokens: Vec<u64> = split.classes.iter().map(|t| t.tokens).collect();
        assert_eq!(tokens, [1_100, 10, 1]);
        assert_eq!(split.classes.iter().map(|t| t.calls).sum::<u64>(), 4);
        let report = Report {
            by_name_split: split,
            ..Report::default()
        };
        let table = by_name_rows(&report, 10_000);
        assert!(table.contains("handed down"), "{table}");
        assert_eq!(by_name_json(&report)["beyond_inherited_lines"]["calls"], 1);
    }
}

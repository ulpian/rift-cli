//! Frecency: which files this developer's agents actually open, and when.
//!
//! Rift already records every file an agent was handed — that is what the
//! window journal is — and the projection already counts every read it has
//! ever ingested. Frecency is a **query over those facts**, not a store: there
//! is no access database to keep, to grow, to garbage-collect or to key by a
//! checkout path.
//!
//! ## Why this compounds across the family
//!
//! A worktree family is one repository: the developer opens a second checkout
//! to work on a branch, an agent spawns a subagent in a temporary one, and the
//! same handful of files are the interesting ones in all of them. The window
//! journals are per member — coverage is a claim about the bytes *that*
//! checkout held — but the *interest* they record is the family's. So the facts
//! are folded over every member handed in, keyed by the repository-relative
//! path each journal already uses, and a file opened ten times in a sibling
//! worktree
//! this morning is warm here this afternoon.
//!
//! ## What a missing fact does
//!
//! Nothing, in the safe direction. A journal that cannot be read, a row that
//! does not parse, a member that has been deleted, a projection nobody opened —
//! each leaves a path with fewer accesses than it really has, which ranks it
//! colder. Frecency is a tie-breaker over a fuzzy match, so a cold path is a
//! path ranked slightly lower, never a path that disappears.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rift_agent::window_journal::Line;

/// How many journal files one member's directory may contribute.
///
/// The directory is folded by its writer, so a member in ordinary use holds a
/// handful; a cap is here because this directory sits in a work tree an agent
/// can write to, and a rebuild must not become unbounded because something
/// filled it.
const MAX_JOURNAL_FILES: usize = 512;

/// How many bytes one journal file may contribute.
const MAX_JOURNAL_BYTES: u64 = 8 * 1_024 * 1_024;

/// What is known about one path's history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PathFacts {
    /// How many times an agent was handed, denied, or wrote this file.
    pub accesses: u32,
    /// The newest millisecond any of that happened at.
    pub last_at_ms: u64,
}

/// The frecency facts a ranking is allowed to use.
///
/// Built by whoever can afford to build it — the detached worker, a verb, a
/// test — and handed to the engine as an argument. That is deliberate: it is
/// what lets a caller that may not open a store still rank, and it makes the
/// ranking a pure function of its inputs, which is what the determinism test
/// pins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrecencyFacts {
    by_path: BTreeMap<String, PathFacts>,
}

impl FrecencyFacts {
    /// No facts at all: every path is equally cold.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The facts every one of these members' window journals carries.
    ///
    /// The members are the family's checkouts, this one included. Order does
    /// not matter and neither do duplicates: counting is addition and recency
    /// is a maximum, so the fold is the same however the members arrive.
    #[must_use]
    pub fn from_members<P: AsRef<Path>>(work_trees: &[P]) -> Self {
        let mut facts = Self::new();
        for work_tree in work_trees {
            facts.absorb_member(work_tree.as_ref());
        }
        facts
    }

    /// Fold one member's window journals in.
    pub fn absorb_member(&mut self, work_tree: &Path) {
        let dir = crate::reads_dir(work_tree);
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            return;
        };
        // The cap is applied to the *sorted* names, not to the directory's own
        // order. `read_dir` hands entries over in whatever order the
        // filesystem holds them, so stopping the iterator at the cap would
        // fold a different 512 files on each run and rank the same repository
        // two ways in two minutes. Keeping the lexicographically smallest
        // names as they arrive is the same set `sort` then `truncate` would
        // give, without holding a directory somebody filled in memory first.
        let mut files: BTreeSet<std::path::PathBuf> = BTreeSet::new();
        for path in read_dir
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        {
            files.insert(path);
            if files.len() > MAX_JOURNAL_FILES {
                files.pop_last();
            }
        }
        for file in files {
            self.absorb_journal(&file);
        }
    }

    /// Fold **one** journal file in.
    ///
    /// The whole directory is the family's interest; one file is one context
    /// window's own, and a caller on a latency budget can afford exactly that
    /// much: the file the window is already writing to is one bounded read of
    /// bytes the page cache is holding anyway. A caller with time folds the
    /// directory and gets the same facts plus its neighbours'.
    ///
    /// Every refusal is silence in the safe direction — an unreadable file, a
    /// file past the size bound, a line that does not parse — because a path
    /// with fewer accesses than it really has ranks colder and never
    /// disappears.
    pub fn absorb_journal(&mut self, file: &Path) {
        if std::fs::metadata(file).is_ok_and(|m| m.len() > MAX_JOURNAL_BYTES) {
            return;
        }
        let Ok(text) = std::fs::read_to_string(file) else {
            return;
        };
        for line in text.lines().filter_map(Line::from_json_line) {
            if let Some((path, at_ms)) = touched(&line) {
                self.record(path, at_ms);
            }
        }
    }

    /// Fold in the projection's own read counts.
    ///
    /// Taken as an argument rather than queried here, because the one caller
    /// that must never open a store is the one that most wants a ranking. The
    /// counts carry no time of their own — they are the whole history rather
    /// than an event — so they add accesses and leave recency alone.
    pub fn absorb_read_counts<I, S>(&mut self, counts: I)
    where
        I: IntoIterator<Item = (S, u32)>,
        S: Into<String>,
    {
        for (path, count) in counts {
            let entry = self.by_path.entry(path.into()).or_default();
            entry.accesses = entry.accesses.saturating_add(count);
        }
    }

    /// What is known about one path.
    #[must_use]
    pub fn get(&self, path: &str) -> PathFacts {
        self.by_path.get(path).copied().unwrap_or_default()
    }

    /// How many paths anything is known about.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_path.len()
    }

    /// Whether nothing is known about any path.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }

    /// Record one access, from a test or a caller with a fact of its own.
    pub fn record(&mut self, path: &str, at_ms: u64) {
        let entry = self.by_path.entry(path.to_owned()).or_default();
        entry.accesses = entry.accesses.saturating_add(1);
        entry.last_at_ms = entry.last_at_ms.max(at_ms);
    }
}

/// The path a journal row is about, and when it happened.
///
/// Every row that names a file counts, including a decline: a read Rift
/// declined is a file the agent was interested in and already holds, which is
/// exactly as strong a signal about *this file matters here* as the read that
/// was allowed — and so is a file Rift announced, or acted on without
/// reading. The one row with no path — the window being emptied — says
/// nothing about any file and is skipped.
fn touched(line: &Line) -> Option<(&str, u64)> {
    match line {
        Line::Read { path, at_ms, .. }
        | Line::Deny { path, at_ms, .. }
        | Line::Observe { path, at_ms, .. }
        | Line::Outline { path, at_ms, .. }
        | Line::Expect { path, at_ms, .. }
        | Line::Edit { path, at_ms, .. }
        | Line::Hold { path, at_ms, .. }
        | Line::Prevented { path, at_ms, .. }
        | Line::Family { path, at_ms, .. } => Some((path.as_str(), *at_ms)),
        // Inherited rows are a claim the spawner made, and spawn and claim
        // rows link windows: none of them is something this window touched.
        Line::Evict { .. } | Line::Inherit { .. } | Line::Spawn { .. } | Line::Claim { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_core::Sha256Id;

    fn hash(byte: u8) -> Sha256Id {
        Sha256Id::from_bytes([byte; 32])
    }

    fn read_row(path: &str, at_ms: u64) -> String {
        Line::Read {
            turn: 1,
            path: path.to_owned(),
            hash: hash(1),
            range: rift_agent::window_journal::Interval::new(1, 10).unwrap(),
            total_lines: 10,
            at_ms,
        }
        .to_json_line()
    }

    fn journal(work_tree: &Path, key: &str, rows: &[String]) {
        let dir = crate::reads_dir(work_tree);
        std::fs::create_dir_all(&dir).unwrap();
        let mut body = String::new();
        for row in rows {
            body.push_str(row);
            body.push('\n');
        }
        std::fs::write(dir.join(format!("{key}.jsonl")), body).unwrap();
    }

    /// Reads recorded in two members of one family are one history: the counts
    /// add and the newest time wins, because the interest is the repository's
    /// even though the coverage is each checkout's.
    #[test]
    fn the_facts_of_every_member_are_one_history() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("main");
        let sibling = dir.path().join("feature");
        journal(
            &here,
            "aaaa",
            &[read_row("src/bus.rs", 1_000), read_row("src/ui.rs", 900)],
        );
        journal(
            &sibling,
            "bbbb",
            &[read_row("src/bus.rs", 5_000), read_row("src/bus.rs", 4_000)],
        );

        let facts = FrecencyFacts::from_members(&[here, sibling]);
        assert_eq!(
            facts.get("src/bus.rs"),
            PathFacts {
                accesses: 3,
                last_at_ms: 5_000
            }
        );
        assert_eq!(
            facts.get("src/ui.rs"),
            PathFacts {
                accesses: 1,
                last_at_ms: 900
            }
        );
        assert_eq!(facts.get("src/never.rs"), PathFacts::default());
        assert_eq!(facts.len(), 2);
    }

    /// The directory an agent can write to is read defensively: a row that is
    /// not a journal row, a file that is not a journal, a member that is not
    /// there — each contributes nothing and none of them is an error.
    #[test]
    fn a_damaged_journal_costs_facts_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("main");
        journal(
            &here,
            "aaaa",
            &[
                "not json at all".to_owned(),
                "{\"kind\":\"read\"}".to_owned(),
                read_row("src/bus.rs", 10),
            ],
        );
        std::fs::write(crate::reads_dir(&here).join("aaaa.snapshot.json"), "{}").unwrap();

        let facts = FrecencyFacts::from_members(&[here, dir.path().join("gone")]);
        assert_eq!(facts.get("src/bus.rs").accesses, 1);
        assert_eq!(facts.len(), 1, "only the row that parsed");
    }

    /// The cap on one member's journal directory keeps the same files however
    /// the filesystem hands them over.
    ///
    /// `read_dir` has no defined order, so a cap taken from the iterator would
    /// fold whichever files the directory happened to list first — a different
    /// set after a rewrite, a compaction, or on another machine, and therefore
    /// a different ranking for the same repository. Two directories holding
    /// exactly the same journals, created in opposite orders, must fold to one
    /// history, and the names that survive must be the smallest ones.
    #[test]
    fn the_journal_cap_keeps_the_same_files_whatever_order_the_directory_lists() {
        let dir = tempfile::tempdir().unwrap();
        let ascending = dir.path().join("ascending");
        let descending = dir.path().join("descending");
        let over_the_cap = MAX_JOURNAL_FILES + 4;
        let mut keys: Vec<usize> = (0..over_the_cap).collect();
        for key in &keys {
            journal(
                &ascending,
                &format!("{key:04}"),
                &[read_row(&path_of(*key), 1_000)],
            );
        }
        keys.reverse();
        for key in &keys {
            journal(
                &descending,
                &format!("{key:04}"),
                &[read_row(&path_of(*key), 1_000)],
            );
        }

        let one_way = FrecencyFacts::from_members(&[ascending]);
        let the_other = FrecencyFacts::from_members(&[descending]);
        assert_eq!(one_way, the_other, "the cap bit a different set of files");
        assert_eq!(one_way.len(), MAX_JOURNAL_FILES);
        assert_eq!(
            one_way.get(&path_of(0)).accesses,
            1,
            "the first journal by name is inside the cap"
        );
        assert_eq!(
            one_way.get(&path_of(over_the_cap - 1)),
            PathFacts::default(),
            "the last journal by name is the one dropped"
        );
    }

    fn path_of(key: usize) -> String {
        format!("src/f{key:04}.rs")
    }

    /// Every row that names a file is interest in that file — a read, a
    /// decline, an observation, the hash recorded before a whole-file write,
    /// and the edit itself. The one row that names no file, the window being
    /// emptied, says nothing about any of them.
    #[test]
    fn every_row_that_names_a_file_counts_and_the_window_row_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("main");
        let range = rift_agent::window_journal::Interval::new(1, 10).unwrap();
        journal(
            &here,
            "aaaa",
            &[
                Line::Deny {
                    turn: 1,
                    path: "src/denied.rs".to_owned(),
                    hash: hash(2),
                    range,
                    at_ms: 10,
                }
                .to_json_line(),
                Line::Observe {
                    turn: 1,
                    path: "src/observed.rs".to_owned(),
                    hash: hash(3),
                    range,
                    at_ms: 20,
                    via: None,
                }
                .to_json_line(),
                Line::Expect {
                    turn: 2,
                    path: "src/written.rs".to_owned(),
                    hash: hash(4),
                    at_ms: 30,
                }
                .to_json_line(),
                Line::Edit {
                    turn: 2,
                    path: "src/written.rs".to_owned(),
                    hash_old: None,
                    hash_new: hash(5),
                    sites: vec![range],
                    shift: 0,
                    full: true,
                    at_ms: 40,
                }
                .to_json_line(),
                Line::Evict {
                    turn: 3,
                    reason: rift_agent::window_journal::EvictReason::Compact,
                    at_ms: 50,
                }
                .to_json_line(),
            ],
        );

        let facts = FrecencyFacts::from_members(&[here]);
        assert_eq!(
            facts.get("src/denied.rs"),
            PathFacts {
                accesses: 1,
                last_at_ms: 10
            },
            "a declined read is interest in the file"
        );
        assert_eq!(facts.get("src/observed.rs").accesses, 1);
        assert_eq!(
            facts.get("src/written.rs"),
            PathFacts {
                accesses: 2,
                last_at_ms: 40
            },
            "the expectation and the edit are two touches of one file"
        );
        assert_eq!(facts.len(), 3, "the window row named no file");
        assert!(!facts.is_empty());
    }

    /// The projection's counts are a whole history without a moment, so they
    /// make a path busier without making it newer.
    #[test]
    fn projection_counts_add_accesses_and_never_recency() {
        let mut facts = FrecencyFacts::new();
        facts.record("src/bus.rs", 42);
        facts.absorb_read_counts([("src/bus.rs", 9), ("src/cold.rs", 4)]);
        assert_eq!(
            facts.get("src/bus.rs"),
            PathFacts {
                accesses: 10,
                last_at_ms: 42
            }
        );
        assert_eq!(
            facts.get("src/cold.rs"),
            PathFacts {
                accesses: 4,
                last_at_ms: 0
            }
        );
    }

    /// One window's own file, folded on its own: the same facts the whole
    /// directory would have given for that window, and none of its
    /// neighbours' — which is what a caller on a hook's budget can afford.
    #[test]
    fn one_windows_journal_folds_without_its_neighbours() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        journal(root, "mine", &[read_row("src/mine.rs", 10)]);
        journal(root, "theirs", &[read_row("src/theirs.rs", 20)]);
        let reads = crate::reads_dir(root);

        let mut one = FrecencyFacts::new();
        one.absorb_journal(&reads.join("mine.jsonl"));
        assert_eq!(one.get("src/mine.rs").accesses, 1);
        assert_eq!(one.get("src/mine.rs").last_at_ms, 10);
        assert_eq!(one.get("src/theirs.rs"), PathFacts::default());

        // A file that is not there, and one that is not a journal, are silence
        // rather than an error: a colder ranking, never an absent one.
        one.absorb_journal(&reads.join("gone.jsonl"));
        std::fs::write(reads.join("odd.jsonl"), "not json\n").unwrap();
        one.absorb_journal(&reads.join("odd.jsonl"));
        assert_eq!(one.len(), 1);

        // The whole directory is still both windows.
        let all = FrecencyFacts::from_members(&[root]);
        assert_eq!(all.get("src/theirs.rs").accesses, 1);
        assert_eq!(all.len(), 2);
    }
}

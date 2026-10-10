//! What a subagent's coordinator has read that the subagent does not hold.
//!
//! The one question asked of a window other than the caller's own, which is
//! why it is a module apart from [`seen`](super::seen) and
//! [`denies`](super::denies): those read the caller's journal to decide about
//! the caller, and this reads a second journal only to **name** what is in
//! it. Nothing here feeds a decision. A revision the coordinator holds is a
//! hint the subagent may act on, never coverage the subagent has: the read
//! gate keeps deciding per window, so a hint can never become a decline.
//!
//! Files only, through the validating [`WindowJournal::load`]: no store, no
//! git object database, no tokenizer.

use std::collections::BTreeMap;
use std::path::Path;

use rift_agent::window_journal::{Journal, Line};
use rift_core::Sha256Id;
use rift_recall_serve::{neutral_pushed_span, render_recovery_map_naming, HeldEntry, HeldVerdict};

use super::WindowJournal;

/// One revision the coordinator read, which the asking window does not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CoordinatorRead {
    /// The path, in the journal's own checkout spelling.
    pub(crate) path: String,
    /// The revision the coordinator read.
    pub(crate) hash: Sha256Id,
    /// When the coordinator's latest read of this revision was written.
    pub(crate) at_ms: u64,
}

/// What the session's main window holds that the subagent `agent_id` of the
/// same session, in the same checkout, does not: the latest revision the
/// coordinator read of each path, at or after `since_ms` when one is given,
/// oldest first.
///
/// A subagent is spawned by a window that has usually read the files its task
/// is about, and it starts holding none of them; this is the list of those
/// files, by revision, so it can ask for what moved rather than read them
/// whole. Its one consumer is the section a subagent's window is opened
/// with, which is the boundary that hands it over.
///
/// **The coordinator's own window in the caller's checkout, and no other** —
/// the window whose spawning call the caller's opening claimed, which for a
/// subagent started by another subagent is that subagent, not the main window.
/// A subagent works for the window that spawned it, and that window's reads
/// are the ones it was briefed from; a sibling subagent's reads, or another
/// member's of the family, are bytes of some other task and, in another
/// checkout, possibly some other file.
///
/// **It decides nothing and is never a decline.** It writes nothing to either
/// journal, and the read gate keeps answering from the subagent's own window
/// alone, so a file the coordinator holds is never a reason to skip a read.
///
/// The agent id is required rather than optional because the main window has
/// no coordinator: asked for itself, this would name its own reads back to it.
///
/// A row comes back only when its path is a checkout path in the journal's
/// own spelling that the push guard passes unaltered; the loader has already
/// dropped every row whose hash is not hex. The journal is a file an agent can
/// write to, and every name returned here is bound for a model's context.
pub(crate) fn coordinator_reads(
    work_tree: &Path,
    session_id: &str,
    agent_id: &str,
    since_ms: Option<u64>,
) -> Vec<CoordinatorRead> {
    let main = WindowJournal::open(work_tree, session_id, None)
        .load()
        .journal;
    let coordinator = match spawner_of(&main, agent_id) {
        Some(spawner) => {
            WindowJournal::open(work_tree, session_id, Some(&spawner))
                .load()
                .journal
        }
        None => main,
    };
    let latest = latest_reads(&coordinator, since_ms);
    if latest.is_empty() {
        // The common case, answered without reading the caller's own journal.
        return Vec::new();
    }
    let own = WindowJournal::open(work_tree, session_id, Some(agent_id)).load();
    unheld_by(&own.journal, latest)
}

/// The subagent window that spawned `agent_id`, when the opening's claim in
/// the session's main journal names a spawn made from one — `None` when the
/// main window spawned it, or when no spawn could be named and the main
/// window, the only coordinator a claim-less window can have, stands in.
fn spawner_of(main: &Journal, agent_id: &str) -> Option<String> {
    let call = main.claim_of(agent_id)??;
    main.lines().iter().find_map(|line| match line {
        Line::Spawn {
            call: c, spawner, ..
        } if c == call => spawner.clone(),
        _ => None,
    })
}

/// The latest revision of each path the journal read and still holds, at or
/// after `since_ms`, keyed by path; rows that may not be named are skipped.
fn latest_reads(journal: &Journal, since_ms: Option<u64>) -> BTreeMap<String, (Sha256Id, u64)> {
    let mut latest = BTreeMap::new();
    for line in journal.live() {
        let Line::Read {
            path, hash, at_ms, ..
        } = line
        else {
            continue;
        };
        let too_early = since_ms.is_some_and(|since| *at_ms < since);
        if too_early || !is_nameable(path) {
            continue;
        }
        latest.insert(path.clone(), (*hash, *at_ms));
    }
    latest
}

/// The revisions of `latest` the window `own` holds no line of, oldest first.
fn unheld_by(own: &Journal, latest: BTreeMap<String, (Sha256Id, u64)>) -> Vec<CoordinatorRead> {
    let mut out: Vec<CoordinatorRead> = latest
        .into_iter()
        .filter(|(path, (hash, _))| own.read_coverage(path, *hash).is_empty())
        .map(|(path, (hash, at_ms))| CoordinatorRead { path, hash, at_ms })
        .collect();
    out.sort_by(|a, b| (a.at_ms, &a.path).cmp(&(b.at_ms, &b.path)));
    out
}

/// Whether a journal path may be named to a model at all.
fn is_nameable(path: &str) -> bool {
    is_checkout_path(path) && passes_the_inline_guard(path)
}

/// Relative, slash-separated, naming nothing above the checkout, and free of
/// control characters: the spelling the gate keys coverage by.
///
/// An absolute path or a `..` names a file outside the checkout, which no
/// coordinator in this checkout read through the gate, and a backslash is a
/// second spelling of a path the guard would otherwise rewrite.
fn is_checkout_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|part| part.is_empty() || part == "..")
}

/// Whether the push guard renders `path` unaltered inside a frame's line.
///
/// Asked of the guard itself rather than of a second word list, so this
/// cannot drift from what a frame refuses. The guard's inline half is reached
/// through the recovery map's naming list, the one place the dispatcher
/// already exports its answer; the pushed half says whether the span comes
/// back unaltered, because a path the guard truncates or flattens names some
/// other file.
fn passes_the_inline_guard(path: &str) -> bool {
    if neutral_pushed_span(path).as_deref() != Some(path) {
        return false;
    }
    let entry = HeldEntry {
        path,
        hash: "",
        held: &[],
        verdict: HeldVerdict::Unknown,
        shape: None,
    };
    render_recovery_map_naming(&[entry], 0, usize::MAX)
        .is_some_and(|(_, named)| named.as_slice() == [0])
}

#[cfg(test)]
mod tests {
    use rift_agent::window_journal::EvictReason;
    use serde_json::Value;

    use super::super::tests::{hash, read};
    use super::*;

    /// Rows no gate wrote, each with the spans that must never come back.
    const HOSTILE_ROWS: &str = include_str!("coordinator_hostile_rows.jsonl");

    fn at(mut line: Line, ms: u64) -> Line {
        if let Line::Read { at_ms, .. } = &mut line {
            *at_ms = ms;
        }
        line
    }

    fn write(work_tree: &Path, agent_id: Option<&str>, lines: &[Line]) {
        let journal = WindowJournal::open(work_tree, "s-1", agent_id);
        for line in lines {
            journal.append(line).unwrap();
        }
    }

    fn named(reads: &[CoordinatorRead]) -> Vec<(&str, Sha256Id)> {
        reads.iter().map(|r| (r.path.as_str(), r.hash)).collect()
    }

    /// One hostile row as the coordinator's journal would hold it: an honest
    /// read row with its path, and its hash when the case names one, replaced.
    fn hostile_row(case: &Value) -> String {
        let mut row: Value = serde_json::from_str(&read(1, "src/x.rs", 1, 1, 9).to_json_line())
            .expect("an honest row");
        row["p"] = case["path"].clone();
        if let Some(hash) = case.get("hash") {
            row["h"] = hash.clone();
        }
        format!("{row}\n")
    }

    fn hostile_cases() -> Vec<Value> {
        HOSTILE_ROWS
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with("//"))
            .map(|line| serde_json::from_str(line).expect("a corpus row"))
            .collect()
    }

    #[test]
    fn the_coordinators_latest_revision_of_each_path_is_named() {
        let repo = tempfile::tempdir().unwrap();
        write(
            repo.path(),
            None,
            &[
                at(read(1, "src/a.rs", 1, 1, 40), 10),
                at(read(2, "src/a.rs", 2, 1, 40), 20),
                at(read(3, "src/b.rs", 1, 1, 40), 15),
            ],
        );
        let reads = coordinator_reads(repo.path(), "s-1", "agent-7", None);
        assert_eq!(
            named(&reads),
            vec![("src/b.rs", hash(1)), ("src/a.rs", hash(2))]
        );
    }

    #[test]
    fn a_revision_the_subagent_already_read_is_not_named() {
        let repo = tempfile::tempdir().unwrap();
        write(repo.path(), None, &[read(1, "src/a.rs", 1, 1, 40)]);
        write(
            repo.path(),
            Some("agent-7"),
            &[read(1, "src/a.rs", 1, 1, 5)],
        );
        assert!(coordinator_reads(repo.path(), "s-1", "agent-7", None).is_empty());
        // Another revision of the same path is news.
        write(repo.path(), None, &[read(2, "src/a.rs", 2, 1, 40)]);
        let reads = coordinator_reads(repo.path(), "s-1", "agent-7", None);
        assert_eq!(named(&reads), vec![("src/a.rs", hash(2))]);
    }

    /// The scope is the coordinator's own window: a sibling subagent's reads,
    /// and another session's, are not the caller's coordinator's.
    #[test]
    fn only_the_coordinators_own_window_is_read() {
        let repo = tempfile::tempdir().unwrap();
        write(
            repo.path(),
            Some("agent-sibling"),
            &[read(1, "src/s.rs", 1, 1, 9)],
        );
        WindowJournal::open(repo.path(), "s-other", None)
            .append(&read(1, "src/o.rs", 1, 1, 9))
            .unwrap();
        assert!(coordinator_reads(repo.path(), "s-1", "agent-7", None).is_empty());
    }

    #[test]
    fn reads_before_the_instant_are_not_named() {
        let repo = tempfile::tempdir().unwrap();
        write(
            repo.path(),
            None,
            &[
                at(read(1, "src/old.rs", 1, 1, 40), 99),
                at(read(2, "src/new.rs", 1, 1, 40), 100),
            ],
        );
        let reads = coordinator_reads(repo.path(), "s-1", "agent-7", Some(100));
        assert_eq!(named(&reads), vec![("src/new.rs", hash(1))]);
        assert_eq!(reads.first().map(|r| r.at_ms), Some(100));
    }

    /// The coordinator no longer holds what it read before its context was
    /// emptied, so those revisions are not offered as held.
    #[test]
    fn reads_the_coordinator_lost_to_a_compaction_are_not_named() {
        let repo = tempfile::tempdir().unwrap();
        let evict = Line::Evict {
            turn: 2,
            reason: EvictReason::Compact,
            at_ms: 2,
        };
        write(repo.path(), None, &[read(1, "src/a.rs", 1, 1, 40), evict]);
        assert!(coordinator_reads(repo.path(), "s-1", "agent-7", None).is_empty());
    }

    /// The journal sits where an agent can write, so a row naming a file
    /// outside the checkout, spelling one the gate never would, reading as an
    /// instruction or a frame's own wording, or carrying a hash that is not
    /// hex never comes back — and an honest row beside it still does.
    #[test]
    fn no_hostile_row_ever_comes_back_from_the_reader() {
        let cases = hostile_cases();
        assert_eq!(cases.len(), 11, "the corpus is append-only");
        for case in &cases {
            let repo = tempfile::tempdir().unwrap();
            write(repo.path(), None, &[read(1, "src/honest.rs", 1, 1, 9)]);
            let journal = WindowJournal::open(repo.path(), "s-1", None);
            let mut live = std::fs::OpenOptions::new()
                .append(true)
                .open(journal.live_path())
                .unwrap();
            std::io::Write::write_all(&mut live, hostile_row(case).as_bytes()).unwrap();
            let reads = coordinator_reads(repo.path(), "s-1", "agent-7", None);
            assert_eq!(named(&reads), vec![("src/honest.rs", hash(1))], "{case}");
            for span in case["forbidden"].as_array().unwrap() {
                let span = span.as_str().unwrap();
                assert!(reads.iter().all(|r| !r.path.contains(span)), "{case}");
            }
        }
    }

    /// A subagent started by another subagent is told what **that** subagent
    /// held — the window its opening claimed a spawn from — and not what the
    /// main window holds.
    #[test]
    fn a_nested_subagent_is_named_its_spawners_reads_not_the_main_windows() {
        let repo = tempfile::tempdir().unwrap();
        let spawn = Line::Spawn {
            turn: 1,
            call: "toolu_1".to_owned(),
            prompt: "p-1".to_owned(),
            agent_type: "Plan".to_owned(),
            spawner: Some("agent-parent".to_owned()),
            names: Vec::new(),
            at_ms: 2,
        };
        let claim = Line::Claim {
            turn: 1,
            agent: "agent-7".to_owned(),
            call: Some("toolu_1".to_owned()),
            at_ms: 3,
        };
        write(
            repo.path(),
            None,
            &[read(1, "src/main.rs", 1, 1, 9), spawn, claim],
        );
        write(
            repo.path(),
            Some("agent-parent"),
            &[read(2, "src/a.rs", 2, 1, 9)],
        );
        let reads = coordinator_reads(repo.path(), "s-1", "agent-7", None);
        assert_eq!(named(&reads), vec![("src/a.rs", hash(2))]);
        // A window whose opening named no spawn keeps the main window.
        let main = coordinator_reads(repo.path(), "s-1", "agent-8", None);
        assert_eq!(named(&main), vec![("src/main.rs", hash(1))]);
    }

    #[test]
    fn a_coordinator_with_no_journal_names_nothing() {
        let repo = tempfile::tempdir().unwrap();
        assert!(coordinator_reads(repo.path(), "s-1", "agent-7", None).is_empty());
    }
}

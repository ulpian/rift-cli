//! The window journals a checkpoint freezes: every row a session's context
//! windows hold when the session is archived, mapped into the shard's own
//! `window_journal.jsonl` grammar.
//!
//! A module of its own because it is the one reader of these files that runs
//! on the archive path — the worker's condense and `prepare-commit-msg`'s
//! prediction — and never while a tool call waits on a decision. It reads the
//! journals exactly as a decision does, through [`WindowJournal::load`], so an
//! agent-written row that does not parse is dropped here too, and it writes
//! nothing: what the files hold at archive time is what is frozen, and a
//! journal already swept is simply absent.

use std::path::Path;

use rift_agent::window_journal::{InheritClass, Interval, Line};
use rift_shadow::backend::{ArchivedWindowFact, ArchivedWindowRow, WindowSliceKind};

use super::{key, WindowJournal};

/// Every freezable row of every context window of `session` in this
/// checkout: the session's own window and the window of each subagent the
/// capture projection links to one of its turns.
///
/// Cumulative, like the read facts and the usage beside it: an unchanged set
/// of journals yields the same rows, so a second archive of an unchanged
/// session derives the id the first one did. The trailer's prediction folds
/// the same rows and can still miss by one appended between
/// `prepare-commit-msg` and the condense, exactly as it can for a read fact or
/// a serve; the archive keeps its true id either way.
///
/// A row the shard cannot spell in its declared shapes — a path that is not a
/// `/`-spelled repository path, above all — is left out whole and counted in
/// one warning, never rewritten.
pub(crate) fn session_window_facts(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    work_tree: &Path,
    agent: &str,
    session: &str,
) -> Vec<ArchivedWindowFact> {
    let mut windows = vec![None];
    windows.extend(
        subagent_window_ids(rt, store, agent, session)
            .into_iter()
            .map(Some),
    );
    let facts: Vec<ArchivedWindowFact> = windows
        .iter()
        .flat_map(|subagent| journal_window_facts(work_tree, session, subagent.as_deref()))
        .collect();
    let refused = facts.iter().filter(|fact| !fact.is_freezable()).count();
    if refused > 0 {
        tracing::warn!(
            refused,
            "window-journal rows outside the frozen shapes were not archived"
        );
    }
    facts
        .into_iter()
        .filter(ArchivedWindowFact::is_freezable)
        .collect()
}

/// The subagents the capture projection links to a turn of this session —
/// each one a context window of its own under the session's id.
///
/// The projection rather than the per-window injection markers: a marker is
/// claimed for a subagent at the very `SubagentStart` that records the
/// projection's row, so the markers name no window the projection does not,
/// and the projection also covers the families no marker is written for.
/// Best-effort: a query failure archives the session's own window alone.
fn subagent_window_ids(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    agent: &str,
    session: &str,
) -> Vec<String> {
    use rift_store::CanonicalStore as _;
    let session_row = rift_shadow::ids::session_row_id(agent, session);
    let Ok(rows) = rt.block_on(store.query(
        "SELECT DISTINCT s.child_session_id FROM subagent_session s \
         JOIN turn t ON t.turn_id = s.parent_turn_id \
         WHERE t.session_id = ? ORDER BY s.child_session_id",
        &[rift_store::Value::text(session_row)],
    )) else {
        return Vec::new();
    };
    rows.iter().filter_map(|row| row.get_text(0).ok()).collect()
}

/// One window's validated rows, mapped for the shard.
fn journal_window_facts(
    work_tree: &Path,
    session: &str,
    subagent: Option<&str>,
) -> Vec<ArchivedWindowFact> {
    let window = key(session, subagent);
    let loaded = WindowJournal::open(work_tree, session, subagent).load();
    loaded
        .journal
        .lines()
        .iter()
        .filter_map(|line| {
            Some(ArchivedWindowFact {
                window: window.clone(),
                subagent: subagent.map(str::to_owned),
                turn: line.turn(),
                at: line_at_ms(line),
                row: archived_window_row(line)?,
            })
        })
        .collect()
}

/// The row a journal line freezes as: its kind and that kind's own members,
/// spelled in the shard's grammar rather than the journal's — or `None` for
/// the spawn and claim lines, which link two windows' journals at run time
/// and are a fact about neither window's context.
fn archived_window_row(line: &Line) -> Option<ArchivedWindowRow> {
    let slice = |kind, path: &str, hash: &rift_core::Sha256Id, range: &Interval, total| {
        ArchivedWindowRow::Slice {
            kind,
            path: path.to_owned(),
            hash: hash.to_hex(),
            range: (range.lo, range.hi),
            total_lines: total,
        }
    };
    Some(match line {
        Line::Read {
            path,
            hash,
            range,
            total_lines,
            ..
        } => slice(WindowSliceKind::Read, path, hash, range, Some(*total_lines)),
        Line::Inherit {
            class,
            path,
            hash,
            range,
            total_lines,
            ..
        } => slice(
            inherited_kind(*class),
            path,
            hash,
            range,
            Some(*total_lines),
        ),
        Line::Deny {
            path, hash, range, ..
        } => slice(WindowSliceKind::Deny, path, hash, range, None),
        Line::Observe {
            path, hash, range, ..
        } => slice(WindowSliceKind::Observe, path, hash, range, None),
        Line::Hold {
            path, hash, range, ..
        } => slice(WindowSliceKind::Hold, path, hash, range, None),
        Line::Prevented {
            path, hash, range, ..
        } => slice(WindowSliceKind::Prevented, path, hash, range, None),
        Line::Expect { path, hash, .. } => ArchivedWindowRow::Expect {
            path: path.clone(),
            hash: hash.to_hex(),
        },
        Line::Edit {
            path,
            hash_old,
            hash_new,
            sites,
            shift,
            full,
            ..
        } => ArchivedWindowRow::Edit {
            path: path.clone(),
            hash_old: hash_old.map(|old| old.to_hex()),
            hash_new: hash_new.to_hex(),
            sites: sites.iter().map(|site| (site.lo, site.hi)).collect(),
            shift: *shift,
            full: *full,
        },
        Line::Evict { reason, .. } => ArchivedWindowRow::Evict {
            reason: reason.as_str().to_owned(),
        },
        Line::Spawn { .. } | Line::Claim { .. } => return None,
    })
}

/// The frozen kind an inherited run of each class is spelled as.
const fn inherited_kind(class: InheritClass) -> WindowSliceKind {
    match class {
        InheritClass::Fork => WindowSliceKind::InheritFork,
        InheritClass::Paste => WindowSliceKind::InheritPaste,
        InheritClass::ByName => WindowSliceKind::InheritByName,
    }
}

/// When a line was written, whatever its kind.
fn line_at_ms(line: &Line) -> u64 {
    match line {
        Line::Read { at_ms, .. }
        | Line::Deny { at_ms, .. }
        | Line::Observe { at_ms, .. }
        | Line::Hold { at_ms, .. }
        | Line::Prevented { at_ms, .. }
        | Line::Expect { at_ms, .. }
        | Line::Edit { at_ms, .. }
        | Line::Evict { at_ms, .. }
        | Line::Inherit { at_ms, .. }
        | Line::Spawn { at_ms, .. }
        | Line::Claim { at_ms, .. } => *at_ms,
    }
}

#[cfg(test)]
mod tests {
    use rift_agent::window_journal::EvictReason;

    use super::super::tests::{hash, read};
    use super::*;

    const SESSION: &str = "s-frozen";
    const CHILD: &str = "explore-1";

    /// A capture projection in which `CHILD` was spawned from a turn of
    /// `SESSION`, and a second session spawned a child of its own.
    fn projection(dir: &Path) -> (tokio::runtime::Runtime, rift_store::SqliteStore) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let store = rift_store::open_capture_projection(dir.join("projection.db")).unwrap();
        for (session, child) in [(SESSION, CHILD), ("s-other", "other-child")] {
            let session_row = rift_shadow::ids::session_row_id("claude-code", session);
            let turn_id = rift_shadow::ids::turn_row_id(&session_row, 1);
            let turn = rift_store::TurnRow {
                turn_id: turn_id.clone(),
                session_id: session_row,
                ordinal: 1,
                started_at: 1,
                schema_version: 1,
            };
            rt.block_on(rift_store::record_turn(&store, &turn)).unwrap();
            let link = rift_store::SubagentSessionRow {
                child_session_id: child.to_owned(),
                parent_turn_id: turn_id,
                schema_version: 1,
            };
            rt.block_on(rift_store::record_subagent_session(&store, &link))
                .unwrap();
        }
        (rt, store)
    }

    fn evict(turn: u32) -> Line {
        let reason = EvictReason::PreCompact;
        Line::Evict {
            turn,
            reason,
            at_ms: u64::from(turn),
        }
    }

    /// The session's own window and its subagent's window are both frozen,
    /// each under its own key; a window of another session in the same
    /// directory — and that session's subagent — is not.
    #[test]
    fn the_sessions_own_and_subagent_windows_are_frozen_and_another_sessions_are_not() {
        let tmp = tempfile::tempdir().unwrap();
        let (rt, store) = projection(tmp.path());
        let append = |session: &str, agent: Option<&str>, line: Line| {
            WindowJournal::open(tmp.path(), session, agent)
                .append(&line)
                .unwrap();
        };
        append(SESSION, None, read(1, "src/a.rs", 1, 1, 40));
        append(SESSION, None, evict(2));
        append(SESSION, Some(CHILD), read(1, "src/b.rs", 2, 1, 9));
        append("s-other", None, read(1, "src/c.rs", 3, 1, 9));
        append("s-other", Some("other-child"), read(1, "src/d.rs", 4, 1, 9));

        let facts = session_window_facts(&rt, &store, tmp.path(), "claude-code", SESSION);
        let seen: Vec<(String, Option<&str>, &str)> = facts
            .iter()
            .map(|f| (f.window.clone(), f.subagent.as_deref(), f.row.kind()))
            .collect();
        let (own, child) = (key(SESSION, None), key(SESSION, Some(CHILD)));
        assert_eq!(
            seen,
            [
                (own.clone(), None, "read"),
                (own, None, "evict"),
                (child, Some(CHILD), "read"),
            ]
        );
    }

    /// An inherited run freezes under its class's token with the file's
    /// length, while the spawn and claim lines that linked the windows stay
    /// run state and reach no shard — and they are not counted as refused.
    #[test]
    fn an_inherited_run_freezes_and_the_link_rows_do_not() {
        let tmp = tempfile::tempdir().unwrap();
        let (rt, store) = projection(tmp.path());
        let own = WindowJournal::open(tmp.path(), SESSION, None);
        own.append(&Line::Spawn {
            turn: 1,
            call: "toolu_1".to_owned(),
            prompt: "p-1".to_owned(),
            agent_type: "Explore".to_owned(),
            spawner: None,
            names: vec!["src/a.rs".to_owned()],
            at_ms: 1,
        })
        .unwrap();
        own.append(&Line::Claim {
            turn: 1,
            agent: CHILD.to_owned(),
            call: Some("toolu_1".to_owned()),
            at_ms: 2,
        })
        .unwrap();
        let read = read(1, "src/a.rs", 1, 1, 40);
        let inherited =
            rift_agent::window_journal::inherited_from_read(&read, InheritClass::ByName, 1, 3)
                .unwrap();
        WindowJournal::open(tmp.path(), SESSION, Some(CHILD))
            .append(&inherited)
            .unwrap();
        let facts = session_window_facts(&rt, &store, tmp.path(), "claude-code", SESSION);
        let kinds: Vec<&str> = facts.iter().map(|f| f.row.kind()).collect();
        assert_eq!(kinds, ["inherit:by_name"]);
        let bytes = rift_shadow::backend::window_journal_jsonl_bytes(&facts).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["total_lines"], 400);
        assert_eq!(value["range"], serde_json::json!([1, 40]));
        assert_eq!(value["subagent"], CHILD);
    }

    /// A row with a revision that is not hex never parses; a row whose path is
    /// absolute or climbs out of the checkout parses and is refused on the way
    /// to the shard. None of them reaches the artifact.
    #[test]
    fn a_hostile_journal_row_never_reaches_the_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let (rt, store) = projection(tmp.path());
        let journal = WindowJournal::open(tmp.path(), SESSION, None);
        journal.append(&read(1, "src/a.rs", 1, 1, 40)).unwrap();
        let good = hash(5).to_hex();
        let row = |path: &str, hash: &str| {
            format!(r#"{{"v":1,"k":"deny","t":2,"p":"{path}","h":"{hash}","lo":1,"hi":2,"at":2}}"#)
        };
        let hostile = [
            row("src/a.rs", &"z".repeat(64)),
            row("/etc/passwd", &good),
            row("src/../../secret", &good),
        ];
        let live = journal.live_path();
        let mut text = std::fs::read_to_string(&live).unwrap();
        for line in hostile {
            text.push_str(&line);
            text.push('\n');
        }
        std::fs::write(&live, text).unwrap();
        assert_eq!(journal.load().journal.lines().len(), 3, "two of them parse");

        let facts = session_window_facts(&rt, &store, tmp.path(), "claude-code", SESSION);
        assert_eq!(facts.len(), 1, "{facts:?}");
        let bytes = rift_shadow::backend::window_journal_jsonl_bytes(&facts).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        for leaked in ["passwd", "..", "zzzz"] {
            assert!(
                !text.contains(leaked),
                "{leaked} reached the artifact: {text}"
            );
        }
    }
}

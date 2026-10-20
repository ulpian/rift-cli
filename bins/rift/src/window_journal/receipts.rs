//! The reads and the emitted call facts one checkout's live window journals
//! hold, as the read-stage report takes them.
//!
//! Read-only: every file is parsed in place and nothing is created. The hook
//! receipt rows in the projection carry no call id and no path, so they can
//! key nothing here. A journal read line carries no call id either, but the
//! answer line that declined it does carry the turn, and the call sidecar of
//! the same window names the call emitted in that turn: when exactly one
//! call was emitted there, that is the read's delivery call. Otherwise the
//! call id stays unknown and delivery rests on the path and hash join.

use std::path::Path;

use rift_agent::window_journal::{CallFact, Line};

use super::{is_call_sidecar, is_journal_key, journal_dir, CallSidecar, WindowJournal};

/// One read a live journal holds.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct WindowRead {
    pub(crate) window_key: String,
    pub(crate) path: String,
    pub(crate) content_sha256: String,
    pub(crate) received_at: i64,
    /// The call that delivered the answer to this read, when the window's
    /// answer line and its call sidecar tie to exactly one.
    pub(crate) invocation_id: Option<String>,
}

/// One emitted call fact a window's sidecar holds.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct WindowCall {
    pub(crate) invocation_id: String,
    pub(crate) wrote: bool,
    pub(crate) at_ms: i64,
}

/// The most recent reads of every window, newest first, and every emitted
/// call fact of those windows.
#[derive(Debug, Default)]
pub(crate) struct LiveReads {
    pub(crate) reads: Vec<WindowRead>,
    pub(crate) calls: Vec<WindowCall>,
}

/// The `limit` most recent reads across the checkout's windows, and the
/// emitted call facts beside them.
pub(crate) fn live_reads(work_tree: &Path, limit: usize) -> LiveReads {
    let dir = journal_dir(work_tree);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return LiveReads::default();
    };
    let mut keys: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_call_sidecar(&name) {
                return None;
            }
            let key = name.strip_suffix(".jsonl")?;
            is_journal_key(key).then(|| key.to_owned())
        })
        .collect();
    keys.sort();
    let mut live = LiveReads::default();
    for key in keys {
        let journal = WindowJournal {
            dir: dir.clone(),
            key: key.clone(),
        };
        let lines = journal.load().journal.lines().to_vec();
        let sidecar = CallSidecar::at(dir.clone(), key.clone()).load();
        let call_lines = sidecar.lines();
        live.reads.extend(lines.iter().filter_map(|line| {
            let mut read = read_of(&key, line)?;
            read.invocation_id = delivery_call(line, &lines, call_lines);
            Some(read)
        }));
        live.calls.extend(call_lines.iter().filter_map(emission_of));
    }
    // Newest first, ties in window and path order, so a bound cut is the
    // same on every run.
    live.reads.sort_by(|a, b| {
        b.received_at
            .cmp(&a.received_at)
            .then_with(|| (&a.window_key, &a.path).cmp(&(&b.window_key, &b.path)))
    });
    live.reads.truncate(limit);
    live
}

fn read_of(window_key: &str, line: &Line) -> Option<WindowRead> {
    let Line::Read {
        path, hash, at_ms, ..
    } = line
    else {
        return None;
    };
    Some(WindowRead {
        window_key: window_key.to_owned(),
        path: path.clone(),
        content_sha256: hash.to_hex(),
        received_at: epoch_ms(*at_ms),
        invocation_id: None,
    })
}

/// The call that carried the answer to `read`: the first later deny line on
/// the same path and hash names the turn, and the one call emitted in that
/// turn of the window is it. Two calls in that turn tie to neither.
fn delivery_call(read: &Line, lines: &[Line], calls: &[Line]) -> Option<String> {
    let Line::Read {
        path, hash, at_ms, ..
    } = read
    else {
        return None;
    };
    let answer_turn = lines.iter().find_map(|line| match line {
        Line::Deny {
            turn,
            path: p,
            hash: h,
            at_ms: at,
            ..
        } if p == path && h == hash && at >= at_ms => Some(*turn),
        _ => None,
    })?;
    let mut ids = calls.iter().filter_map(|line| match line {
        Line::Call {
            turn,
            call: Some(id),
            fact: CallFact::Emitted { .. },
            ..
        } if *turn == answer_turn => Some(id),
        _ => None,
    });
    let id = ids.next()?;
    ids.all(|other| other == id).then(|| id.clone())
}

fn emission_of(line: &Line) -> Option<WindowCall> {
    let Line::Call {
        call: Some(id),
        fact: CallFact::Emitted { wrote, .. },
        at_ms,
        ..
    } = line
    else {
        return None;
    };
    Some(WindowCall {
        invocation_id: id.clone(),
        wrote: *wrote,
        at_ms: epoch_ms(*at_ms),
    })
}

fn epoch_ms(at_ms: u64) -> i64 {
    i64::try_from(at_ms).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window_journal::tests::{hash, read};

    fn snapshot(dir: &Path) -> Vec<(String, u64, std::time::SystemTime)> {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| {
                let meta = e.metadata().unwrap();
                let name = e.file_name().to_string_lossy().into_owned();
                (name, meta.len(), meta.modified().unwrap())
            })
            .collect();
        files.sort();
        files
    }

    #[test]
    fn read_stages_live_reads_takes_the_newest_reads_and_the_emitted_calls_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = WindowJournal::open(tmp.path(), "s1", None);
        for turn in [3, 7, 5] {
            journal.append(&read(turn, "a.rs", 1, 1, 10)).unwrap();
        }
        let sidecar = CallSidecar::open(tmp.path(), "s1", None);
        let emitted = CallFact::Emitted {
            wrote: true,
            exit: 0,
        };
        for (call, fact) in [
            (Some("c1"), emitted),
            (None, emitted),
            (Some("c2"), CallFact::Planned),
        ] {
            let line = Line::Call {
                turn: 7,
                call: call.map(str::to_owned),
                fact,
                at_ms: 9,
            };
            sidecar.append(&line).unwrap();
        }
        let before = snapshot(&journal_dir(tmp.path()));

        let live = live_reads(tmp.path(), 2);

        let times: Vec<i64> = live.reads.iter().map(|r| r.received_at).collect();
        assert_eq!(times, [7, 5]);
        assert_eq!(live.reads[0].content_sha256, hash(1).to_hex());
        assert_eq!(
            live.reads[0].window_key,
            crate::window_journal::key("s1", None)
        );
        assert_eq!(
            live.calls,
            [WindowCall {
                invocation_id: "c1".to_owned(),
                wrote: true,
                at_ms: 9
            }]
        );
        assert_eq!(snapshot(&journal_dir(tmp.path())), before);
    }

    fn emitted(turn: u32, id: &str) -> Line {
        Line::Call {
            turn,
            call: Some(id.to_owned()),
            fact: CallFact::Emitted {
                wrote: true,
                exit: 0,
            },
            at_ms: 9,
        }
    }

    #[test]
    fn read_stages_live_reads_tie_a_declined_read_to_the_one_call_emitted_in_its_answer_turn() {
        let tmp = tempfile::tempdir().unwrap();
        for (session, calls) in [("s1", &["c1"][..]), ("s2", &["c2", "c3"][..])] {
            let journal = WindowJournal::open(tmp.path(), session, None);
            journal.append(&read(1, "a.rs", 1, 1, 10)).unwrap();
            let deny = Line::Deny {
                turn: 2,
                path: "a.rs".to_owned(),
                hash: hash(1),
                range: rift_agent::window_journal::Interval::new(1, 10).unwrap(),
                at_ms: 9,
            };
            journal.append(&deny).unwrap();
            let sidecar = CallSidecar::open(tmp.path(), session, None);
            sidecar.append(&emitted(1, "earlier")).unwrap();
            for id in calls {
                sidecar.append(&emitted(2, id)).unwrap();
            }
        }

        let live = live_reads(tmp.path(), 10);

        let tied = |session: &str| {
            let key = crate::window_journal::key(session, None);
            let read = live.reads.iter().find(|r| r.window_key == key).unwrap();
            read.invocation_id.clone()
        };
        assert_eq!(tied("s1").as_deref(), Some("c1"));
        assert_eq!(
            tied("s2"),
            None,
            "two calls in the answer turn tie to neither"
        );
    }
}

//! The reads and the emitted call facts one checkout's live window journals
//! hold, as the read-stage report takes them.
//!
//! Read-only: every file is parsed in place and nothing is created. The hook
//! receipt rows in the projection carry no call id and no path, so they can
//! key nothing here; a read's call id stays unknown and its delivery rests on
//! the path and hash join, which the report names.

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
        live.reads.extend(
            journal
                .load()
                .journal
                .lines()
                .iter()
                .filter_map(|line| read_of(&key, line)),
        );
        let sidecar = CallSidecar::at(dir.clone(), key);
        live.calls
            .extend(sidecar.load().lines().iter().filter_map(emission_of));
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
    })
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
}

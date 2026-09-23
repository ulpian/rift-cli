//! What a checkout's journal directory amounts to, and when a file in it is
//! finished with.
//!
//! The count and the sweep share one screen — the file names this plane owns
//! — and that screen is why they live together: a sweep that removed a file
//! the count did not recognise would be deleting somebody else's.

use std::path::{Path, PathBuf};

use super::{dir, ineffective_denies, is_key, WindowJournal};

/// What one checkout's window journals amount to, for the operator diagnostic.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Survey {
    /// Context windows with a journal here.
    pub(crate) windows: usize,
    /// Snapshot files across all of them.
    pub(crate) snapshots: usize,
    /// Lines a reader gets back across every window.
    pub(crate) lines: usize,
    /// Rows dropped for not parsing — forged, torn, or from another version.
    pub(crate) dropped: usize,
    /// Declines a client ran the call in spite of.
    pub(crate) ineffective: usize,
}

/// Which of this module's three file shapes a name is.
enum Kind {
    Live,
    Rolled,
    Snapshot,
}

/// Count this checkout's window journals and the rows that did not parse.
///
/// Read the way the decision reads them — through the snapshots, not as a
/// naive file walk — so that the count is what a window actually holds. Opens
/// no store and decides nothing: the diagnostic exists so that a directory
/// quietly filling up with rows nobody can read is visible before anyone
/// wonders why nothing is ever declined.
pub(crate) fn survey(work_tree: &Path) -> Survey {
    let root = dir(work_tree);
    let mut keys = std::collections::BTreeSet::new();
    let mut snapshots = 0usize;
    for (name, _) in entries(work_tree) {
        let Some((key, kind)) = classify(&name) else {
            continue;
        };
        keys.insert(key);
        if matches!(kind, Kind::Snapshot) {
            snapshots += 1;
        }
    }
    let mut survey = Survey {
        windows: keys.len(),
        snapshots,
        ..Survey::default()
    };
    for key in keys {
        let loaded = WindowJournal {
            dir: root.clone(),
            key,
        }
        .load();
        survey.lines += loaded.journal.lines().len();
        survey.dropped += loaded.dropped;
        survey.ineffective += ineffective_denies(&loaded.journal);
    }
    survey
}

/// Remove the window journals a checkout has finished with, and nothing else.
///
/// "Finished with" is the file's own age against the same time-to-live every
/// other transient is swept by. A window still being written to keeps its
/// file: only files whose last write is older than the cutoff go, so a live
/// session is never disarmed mid-conversation. Files this module does not own
/// are never touched, however they got into the directory.
pub(crate) fn sweep_expired(work_tree: &Path, now_ms: i64, ttl_ms: i64, dry_run: bool) -> usize {
    let cutoff = now_ms.saturating_sub(ttl_ms);
    let mut removed = 0usize;
    for (name, path) in entries(work_tree) {
        if classify(&name).is_none() {
            continue;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        // A file whose age cannot be read is kept. Removing on an unknown
        // age would delete a live window's journal on a filesystem that
        // reports no times, and the whole point of this sweep is disk, which
        // can wait.
        let age_known = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok());
        let Some(modified_ms) = age_known else {
            continue;
        };
        if modified_ms >= cutoff {
            continue;
        }
        if dry_run || std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// The `(file name, path)` pairs directly in this checkout's journal directory.
fn entries(work_tree: &Path) -> Vec<(String, PathBuf)> {
    std::fs::read_dir(dir(work_tree))
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.path().is_file())
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            )
        })
        .collect()
}

/// The window a file belongs to and its shape, or `None` for a name this
/// module did not write — the screen that keeps the sweep to its own files.
fn classify(name: &str) -> Option<(String, Kind)> {
    if let Some(stem) = name.strip_suffix(".jsonl") {
        if is_key(stem) {
            return Some((stem.to_owned(), Kind::Live));
        }
        let (key, ordinal) = stem.strip_suffix(".rolled")?.rsplit_once('.')?;
        return (is_key(key) && ordinal.parse::<u64>().is_ok())
            .then(|| (key.to_owned(), Kind::Rolled));
    }
    let (key, ordinal) = name.strip_suffix(".snapshot.json")?.rsplit_once('.')?;
    (is_key(key) && ordinal.parse::<u64>().is_ok()).then(|| (key.to_owned(), Kind::Snapshot))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;
    use crate::window_journal::tests::read;

    /// Age one file backwards, the way a file that has sat there since a
    /// conversation ended is aged.
    fn age(path: &Path, days: u64) {
        let when =
            std::time::SystemTime::now() - std::time::Duration::from_secs(days * 24 * 60 * 60);
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(when))
            .unwrap();
    }

    /// The sweep is by the file's own age, and by nothing else. A window that
    /// is still being written to keeps its journal — disarming a live
    /// conversation would cost every saving left in it — and a file this
    /// module did not write is left exactly where it is, however it got there.
    #[test]
    fn clean_removes_only_expired_state() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        rift_git::test_support::fake_repo(&root).unwrap();

        let old = WindowJournal::open(&root, "a-session-that-ended", None);
        old.append(&read(1, "a.rs", 1, 1, 40)).unwrap();
        old.fold().unwrap().expect("folded");
        old.append(&read(2, "a.rs", 1, 41, 60)).unwrap();
        let live = WindowJournal::open(&root, "a-session-in-progress", None);
        live.append(&read(1, "b.rs", 1, 1, 10)).unwrap();
        // Something this module did not write, in the same directory.
        let stranger = dir(&root).join("notes.txt");
        std::fs::write(&stranger, b"not ours").unwrap();

        for path in [
            old.live_path(),
            old.rolled_path(1),
            old.snapshot_path(1),
            stranger.clone(),
        ] {
            age(&path, 30);
        }

        let dry = crate::session_verbs::clean(&root, true, 14).unwrap();
        assert_eq!(dry.json["expired_window_journals"], 3);
        assert!(old.live_path().exists(), "a dry run removes nothing");

        let out = crate::session_verbs::clean(&root, false, 14).unwrap();
        assert_eq!(out.json["expired_window_journals"], 3);
        assert!(!old.live_path().exists());
        assert!(!old.rolled_path(1).exists());
        assert!(!old.snapshot_path(1).exists());
        assert!(
            live.live_path().exists(),
            "a window still being written to keeps its journal"
        );
        assert!(
            stranger.exists(),
            "a file this module did not write is never removed"
        );
    }

    /// The operator count reads the journals the way the decision reads them —
    /// through the snapshots — and says plainly how many rows it had to drop.
    #[test]
    fn the_survey_counts_windows_snapshots_and_rows_it_could_not_read() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(survey(tmp.path()), Survey::default(), "an empty checkout");

        let one = WindowJournal::open(tmp.path(), "s1", None);
        one.append(&read(1, "a.rs", 1, 1, 40)).unwrap();
        one.fold().unwrap().expect("folded");
        one.append(&read(2, "a.rs", 1, 41, 60)).unwrap();
        let two = WindowJournal::open(tmp.path(), "s1", Some("subagent"));
        two.append(&read(1, "b.rs", 1, 1, 10)).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(two.live_path())
            .unwrap();
        file.write_all(b"{\"v\":1,\"k\":\"read\",\"t\":9\n")
            .unwrap();
        drop(file);

        let survey = survey(tmp.path());
        assert_eq!(survey.windows, 2, "a subagent is its own window");
        assert_eq!(survey.snapshots, 1);
        assert_eq!(survey.lines, 3, "one folded, one after it, one elsewhere");
        assert_eq!(survey.dropped, 1, "and the row nobody can read is named");
    }
}

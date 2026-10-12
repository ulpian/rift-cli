//! One context window's journal as a place on disk: the file appends land
//! in, the fold that keeps it short, and the read that puts the pieces back
//! together.
//!
//! The rows themselves are read and condensed next door in [`fold`](super::fold);
//! what lives here is the file handling — the append that must be one atomic
//! write, the rename that claims a fold, and the walk that reads a snapshot,
//! its rolled files and the live file as one history.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use rift_agent::window_journal::{Journal, Line};

use super::fold::{
    absorb_file, absorb_row, emptied, merge, read_snapshot, Accounted, SNAPSHOT_SCHEMA,
};
use super::{journal_dir, key, MAX_APPEND_BYTES};

/// How many rows a live file may hold before it is folded.
pub(crate) const MAX_LIVE_LINES: usize = 4_096;

/// The shortest row this module writes, rounded down.
///
/// An emptying — the smallest of them — is over fifty bytes; forty is the safe
/// side of that.
const MIN_ROW_BYTES: u64 = 40;

/// The size at which a live file is folded.
///
/// The cap that matters is [`MAX_LIVE_LINES`], and this is that cap expressed
/// in the one measurement an append can take for free. Counting rows would
/// mean reading the whole file on every append — the file grows, so the cost
/// is quadratic in the length of a session — for a bound that is only ever an
/// upper one. No row is shorter than [`MIN_ROW_BYTES`], so a file that has not
/// reached this size cannot hold the cap in rows, and a file folded here has
/// never exceeded it. It errs towards folding early, which costs a snapshot
/// and nothing else.
const FOLD_AT_BYTES: u64 = MAX_LIVE_LINES as u64 * MIN_ROW_BYTES;

/// What one `load` found: the journal itself, and how many rows were dropped
/// for not parsing (the number the operator diagnostic reports).
pub(crate) struct Loaded {
    /// The lines, oldest first, as the decision reads them.
    pub(crate) journal: Journal,
    /// Rows dropped anywhere for not being a line this version wrote.
    pub(crate) dropped: usize,
}

/// One context window's journal, as a place on disk.
pub(crate) struct WindowJournal {
    pub(super) dir: PathBuf,
    pub(super) key: String,
}

impl WindowJournal {
    /// The journal of one context window in one checkout. Opens nothing and
    /// creates nothing; the first append makes the directory.
    pub(crate) fn open(work_tree: &Path, session_id: &str, agent_id: Option<&str>) -> Self {
        Self {
            dir: journal_dir(work_tree),
            key: key(session_id, agent_id),
        }
    }

    /// The file appends land in.
    pub(super) fn live_path(&self) -> PathBuf {
        self.dir.join(format!("{}.jsonl", self.key))
    }

    pub(super) fn rolled_path(&self, ordinal: u64) -> PathBuf {
        self.dir
            .join(format!("{}.{ordinal}.rolled.jsonl", self.key))
    }

    pub(super) fn snapshot_path(&self, ordinal: u64) -> PathBuf {
        self.dir
            .join(format!("{}.{ordinal}.snapshot.json", self.key))
    }

    /// This window's `(ordinal, path)` files of one kind, oldest first.
    fn ordinals(&self, suffix: &str) -> Vec<(u64, PathBuf)> {
        let prefix = format!("{}.", self.key);
        let mut out: Vec<(u64, PathBuf)> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .filter_map(std::result::Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let ordinal = name
                    .strip_prefix(&prefix)?
                    .strip_suffix(suffix)?
                    .parse::<u64>()
                    .ok()?;
                Some((ordinal, entry.path()))
            })
            .collect();
        out.sort_by_key(|(ordinal, _)| *ordinal);
        out
    }

    /// Everything this window has been told, oldest first.
    ///
    /// The newest snapshot stands for every rolled file below it; the rolled
    /// file it was made from is read past the byte it accounted for, which is
    /// where a straggler's append lands; rolled files above it (a fold that
    /// crashed before its snapshot) are read whole; the live file is read
    /// last.
    pub(crate) fn load(&self) -> Loaded {
        let mut lines = Vec::new();
        let mut dropped = 0usize;
        let _ = self.absorb_history(&mut lines, &mut dropped, None);
        absorb_file(&self.live_path(), 0, None, &mut lines, &mut dropped);
        Loaded {
            journal: Journal::from_lines(lines),
            dropped,
        }
    }

    /// Append one line, folding first if the live file has filled up.
    ///
    /// The error is the caller's signal to let the call it was deciding about
    /// proceed: a line that could not be written is a line the journal does not
    /// have, and everything downstream reads that as less coverage. Nothing
    /// here panics and nothing here blocks.
    pub(crate) fn append(&self, line: &Line) -> std::io::Result<()> {
        let mut row = line.to_json_line();
        row.push('\n');
        if row.len() > MAX_APPEND_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a journal row longer than one atomic append is refused",
            ));
        }
        std::fs::create_dir_all(&self.dir)?;
        self.fold_if_full();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.live_path())?;
        // One `write`, not `write_all`: the atomicity that keeps two appending
        // processes from interleaving belongs to the single call. A short
        // write is reported rather than continued, because continuing is
        // exactly how a row gets torn in half by someone else's.
        let written = file.write(row.as_bytes())?;
        if written == row.len() {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "a journal row was written in part",
            ))
        }
    }

    /// Fold when the live file has reached the cap, and never mind if it
    /// cannot: an unfolded journal is a slower one, not a wrong one.
    fn fold_if_full(&self) {
        if std::fs::metadata(self.live_path()).is_ok_and(|m| m.len() >= FOLD_AT_BYTES) {
            let _ = self.fold();
        }
    }

    /// Roll the live file aside and write the merged state beside it.
    ///
    /// The rename claims the fold: it is atomic, so of two processes that
    /// decide to fold at the same moment exactly one finds a file to move and
    /// the other finds nothing and stops. A process still holding the old file
    /// open keeps appending into the rolled file, past the byte the snapshot
    /// accounts for, and the reader picks those rows up from there.
    pub(crate) fn fold(&self) -> std::io::Result<Option<u64>> {
        let Some((ordinal, caps)) = self.roll()? else {
            return Ok(None);
        };
        let (merged, accounted) = self.merged_history(&caps);
        self.write_snapshot(ordinal, &accounted, &merged)?;
        Ok(Some(ordinal))
    }

    /// Move the live file aside and report the ordinal it took and how many
    /// bytes it held **at that moment**.
    ///
    /// Reading that length here, one syscall after the rename, is the whole
    /// safety of the arrangement, and it is the one ordering that works. A
    /// process that opened the live file before the rename goes on appending
    /// into it afterwards; the snapshot stands for the bytes below the length
    /// reported here and the reader continues from exactly that byte. Taking
    /// the length later — after the history has been read — would let a row
    /// that landed in between fall into the gap: too late to be in the
    /// snapshot, too early to be in the tail, and on disk where nothing will
    /// ever look for it again. A lost row that happens to be an emptying
    /// leaves the window holding coverage it no longer has, which is the one
    /// failure this module must not have.
    ///
    /// Under-stating is safe in a way over-stating is not: a row read into the
    /// snapshot and read again from the tail is absorbed twice, and every use
    /// of these lines is idempotent — coverage is a union, a decline is
    /// looked up with `any`, a whole-file write with `find`.
    fn roll(&self) -> std::io::Result<Option<(u64, Accounted)>> {
        let next = self
            .ordinals(".rolled.jsonl")
            .last()
            .map_or(0, |(ordinal, _)| *ordinal)
            .max(
                self.ordinals(".snapshot.json")
                    .last()
                    .map_or(0, |(ordinal, _)| *ordinal),
            )
            + 1;
        let rolled = self.rolled_path(next);
        if rolled.exists() {
            return Ok(None);
        }
        std::fs::rename(self.live_path(), &rolled)?;
        // Every rolled file's length, taken now: the one this roll just made
        // and every older one a straggler may have appended to since the last
        // fold read it. Measuring them all here, before a byte is read, is
        // what makes the snapshot's account of them exact.
        let caps = self
            .ordinals(".rolled.jsonl")
            .into_iter()
            .map(|(ordinal, path)| (ordinal, std::fs::metadata(&path).map_or(0, |m| m.len())))
            .collect();
        Ok(Some((next, caps)))
    }

    /// The merged state a roll closed off: the history up to and including the
    /// rolled file, read no further than the byte the snapshot will account
    /// for.
    ///
    /// The live file is deliberately left out: a process that started a fresh
    /// one in the meantime is still appending to it, and the reader reads it
    /// anyway, so folding it in would only duplicate rows.
    fn merged_history(&self, caps: &Accounted) -> (Vec<Line>, Accounted) {
        let mut lines = Vec::new();
        let mut dropped = 0usize;
        let accounted = self.absorb_history(&mut lines, &mut dropped, Some(caps));
        (merge(&lines), accounted)
    }

    /// Everything before the live file, oldest first — the walk `load` makes,
    /// without its last step, and the account of it a snapshot would carry.
    ///
    /// **Every** rolled file is read from the byte the newest snapshot
    /// accounts for, not just the one that snapshot was made from. A process
    /// holding a rolled file open goes on appending to it for as long as it
    /// lives, and a later fold does not close that file: an account kept only
    /// for the newest rolled file would leave a row that landed in an older
    /// one below a byte nothing ever reads again. The account is carried
    /// forward from snapshot to snapshot for exactly that reason.
    ///
    /// `caps` bounds each rolled file to a length its caller has already
    /// measured, which is how a fold reads exactly what its snapshot will
    /// claim to stand for and not one byte more. The map returned is that
    /// claim: for each rolled file, the byte this walk actually read up to.
    fn absorb_history(
        &self,
        lines: &mut Vec<Line>,
        dropped: &mut usize,
        caps: Option<&Accounted>,
    ) -> Accounted {
        let snapshot = self
            .ordinals(".snapshot.json")
            .pop()
            .map(|(ordinal, path)| (ordinal, read_snapshot(&path, ordinal)));
        let (from_ordinal, mut accounted) = match snapshot {
            Some((ordinal, Some(snap))) => {
                for row in &snap.lines {
                    absorb_row(row, lines, dropped);
                }
                (ordinal, snap.accounted)
            }
            Some((ordinal, None)) => {
                *dropped += 1;
                lines.push(emptied());
                // The history is gone, so the rolled file this snapshot was
                // made from is read whole: what cannot be shown cannot be
                // claimed, and reading it again is coverage recorded after an
                // emptying rather than coverage smuggled across one.
                (ordinal, Accounted::from([(ordinal, 0)]))
            }
            None => (0, Accounted::new()),
        };
        for (ordinal, path) in self.ordinals(".rolled.jsonl") {
            // Above the newest snapshot is a fold that did not finish: nothing
            // accounts for that file, so it is read whole. At or below it, the
            // account says where to resume — and a file no account mentions is
            // one an older writer's snapshot took in whole, which is left
            // where it is rather than read a second time out of order.
            let from = if ordinal > from_ordinal {
                0
            } else {
                let Some(byte) = accounted.get(&ordinal) else {
                    continue;
                };
                *byte
            };
            let to = caps.and_then(|caps| caps.get(&ordinal).copied());
            absorb_file(&path, from, to, lines, dropped);
            if let Some(read_to) = to {
                accounted.insert(ordinal, read_to);
            }
        }
        accounted
    }

    /// Write one snapshot into place through a temporary name, so a reader
    /// never opens a half-written one.
    fn write_snapshot(
        &self,
        ordinal: u64,
        accounted: &Accounted,
        lines: &[Line],
    ) -> std::io::Result<()> {
        let rows: Vec<String> = lines.iter().map(Line::to_json_line).collect();
        let sources: serde_json::Map<String, serde_json::Value> = accounted
            .iter()
            .map(|(ordinal, byte)| (ordinal.to_string(), serde_json::json!(byte)))
            .collect();
        let body = serde_json::json!({
            "schema_version": SNAPSHOT_SCHEMA,
            "ordinal": ordinal,
            "source": format!("{}.{ordinal}.rolled.jsonl", self.key),
            "source_bytes": accounted.get(&ordinal).copied().unwrap_or_default(),
            "sources": sources,
            "lines": rows,
        })
        .to_string();
        let tmp = self
            .dir
            .join(format!(".{}.{ordinal}.snapshot.tmp", self.key));
        {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)?;
            file.write_all(body.as_bytes())?;
        }
        std::fs::rename(&tmp, self.snapshot_path(ordinal))
    }
}

#[cfg(test)]
mod tests {
    use rift_agent::window_journal::{DenyKey, EvictReason, Interval, Line};

    use super::*;
    use crate::window_journal::fold::MAX_FILE_BYTES;
    use crate::window_journal::tests::{hash, read};

    /// Where the child process this suite spawns is told to write.
    const CHILD_DIR: &str = "RIFT_TEST_WINDOW_JOURNAL_DIR";

    /// Which name the child process writes its rows under.
    const CHILD_TAG: &str = "RIFT_TEST_WINDOW_JOURNAL_TAG";

    /// The test the child process is asked to run — itself, filtered to one.
    const CHILD_TEST: &str =
        "window_journal::journal::tests::an_appending_child_writes_every_row_it_is_asked_for";

    /// How many rows each appending process writes.
    const CHILD_ROWS: u32 = 300;

    /// Rows in the live file, counted from the file rather than from anything
    /// that parsed it: what is on disk, whether or not it is a line.
    fn rows_on_disk(journal: &WindowJournal) -> usize {
        std::fs::read_to_string(journal.live_path())
            .map(|text| text.lines().count())
            .unwrap_or_default()
    }

    fn deny(turn: u32, path: &str, revision: u8, lo: u32, hi: u32) -> Line {
        Line::Deny {
            turn,
            path: path.to_owned(),
            hash: hash(revision),
            range: Interval::new(lo, hi).expect("a run"),
            at_ms: u64::from(turn),
        }
    }

    fn deny_key(path: &str, revision: u8, lo: u32, hi: u32) -> DenyKey {
        DenyKey {
            path: path.to_owned(),
            hash: hash(revision),
            range: Interval::new(lo, hi).expect("a run"),
        }
    }

    /// A reader arrives while someone else is mid-append and finds a final row
    /// that stops in the middle. It drops that row and keeps every complete
    /// one before it — the whole reason a half-written row can never be
    /// mistaken for a claim about what a window holds.
    #[test]
    fn a_torn_final_line_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = WindowJournal::open(tmp.path(), "s", None);
        journal.append(&read(1, "a.rs", 1, 1, 40)).unwrap();
        journal.append(&read(2, "b.rs", 1, 1, 10)).unwrap();
        // Exactly what a torn write leaves: a prefix of a row, no newline.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(journal.live_path())
            .unwrap();
        file.write_all(br#"{"v":1,"k":"read","t":3,"p":"c.rs","h":"#)
            .unwrap();
        drop(file);

        let loaded = journal.load();
        assert_eq!(loaded.journal.lines().len(), 2, "the whole rows survive");
        assert_eq!(loaded.dropped, 1, "the torn one is counted, not read");
        assert_eq!(
            rows_on_disk(&journal),
            3,
            "the torn row is still a row on disk"
        );
        assert!(loaded
            .journal
            .coverage("a.rs", hash(1))
            .contains(Interval::new(1, 40).unwrap()));
        assert!(
            loaded.journal.coverage("c.rs", hash(1)).is_empty(),
            "a row that did not parse contributes nothing at all"
        );
    }

    /// A row too long to write in one call is refused outright. Splitting it
    /// would let another process's append land in the middle of it, and two
    /// half-rows are worse than none: the journal simply does not learn this
    /// line, and the read it was about goes ahead.
    #[test]
    fn a_row_that_cannot_be_written_atomically_is_refused_and_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = WindowJournal::open(tmp.path(), "s", None);
        let long = "x".repeat(MAX_APPEND_BYTES);
        let error = journal
            .append(&read(1, &long, 1, 1, 10))
            .expect_err("a row past the atomic bound is refused");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!journal.live_path().exists(), "nothing was written");
        assert!(journal.load().journal.lines().is_empty());
    }

    /// The fold is a change of representation, not of meaning. What the window
    /// holds and what it has already been declined survive it exactly — and
    /// the reads that made up the coverage come back as their union, not as
    /// four thousand rows.
    #[test]
    fn the_snapshot_preserves_coverage_and_denies() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = WindowJournal::open(tmp.path(), "s", None);
        // Contiguous halves of one revision, an unrelated file, and a decline.
        for turn in 0..200u32 {
            journal
                .append(&read(turn, "a.rs", 1, turn * 2 + 1, turn * 2 + 2))
                .unwrap();
        }
        journal.append(&read(200, "b.rs", 2, 10, 20)).unwrap();
        journal.append(&deny(201, "a.rs", 1, 1, 40)).unwrap();
        let before = journal.load();
        assert!(before
            .journal
            .coverage("a.rs", hash(1))
            .contains(Interval::new(1, 400).unwrap()));

        let ordinal = journal.fold().unwrap().expect("the fold rolled the file");
        assert_eq!(ordinal, 1);
        assert!(journal.snapshot_path(1).exists());
        assert!(journal.rolled_path(1).exists());
        assert!(!journal.live_path().exists(), "appends start a fresh file");

        let after = journal.load();
        assert_eq!(
            after.journal.coverage("a.rs", hash(1)).runs(),
            before.journal.coverage("a.rs", hash(1)).runs(),
            "the same lines are held after the fold as before it"
        );
        assert_eq!(
            after.journal.coverage("b.rs", hash(2)).runs(),
            before.journal.coverage("b.rs", hash(2)).runs()
        );
        assert!(
            after.journal.denied(&deny_key("a.rs", 1, 1, 40)),
            "a decline already made must not be made a second time"
        );
        assert!(
            after.journal.lines().len() < before.journal.lines().len() / 10,
            "the point of the fold is fewer rows: {} from {}",
            after.journal.lines().len(),
            before.journal.lines().len()
        );
        // And the folded journal keeps accepting rows.
        journal.append(&read(202, "c.rs", 3, 1, 5)).unwrap();
        let later = journal.load();
        assert!(later
            .journal
            .coverage("c.rs", hash(3))
            .contains(Interval::new(1, 5).unwrap()));
        assert!(later
            .journal
            .coverage("a.rs", hash(1))
            .contains(Interval::new(1, 400).unwrap()));
    }

    /// Coverage taken before the window was emptied and coverage taken after
    /// it are not the same claim. The fold merges reads only inside the
    /// stretch they belong to, so an emptying still empties everything before
    /// it once the rows have been folded together.
    #[test]
    fn the_fold_never_carries_coverage_across_an_emptying() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = WindowJournal::open(tmp.path(), "s", None);
        journal.append(&read(1, "a.rs", 1, 1, 40)).unwrap();
        journal
            .append(&Line::Evict {
                turn: 2,
                reason: EvictReason::PreCompact,
                at_ms: 2,
            })
            .unwrap();
        journal.append(&read(3, "a.rs", 1, 41, 60)).unwrap();
        journal.fold().unwrap().expect("folded");

        let after = journal.load();
        assert!(
            !after
                .journal
                .coverage("a.rs", hash(1))
                .contains(Interval::new(1, 40).unwrap()),
            "lines read before the emptying are no longer held"
        );
        assert!(
            after
                .journal
                .coverage("a.rs", hash(1))
                .contains(Interval::new(41, 60).unwrap()),
            "lines read after it are"
        );
        assert!(
            after
                .journal
                .recorded_coverage("a.rs", hash(1))
                .contains(Interval::new(1, 40).unwrap()),
            "and what was read before it is still on the record, for the map"
        );
    }

    /// A history written by an earlier run, in the three pieces a reader can
    /// find it in: a snapshot of everything that had been folded, the rolled
    /// file it was made from carrying rows that landed after it was read, and
    /// the live file. All three are one window, and the reader sees the lot.
    #[test]
    fn a_snapshot_and_the_rows_after_it_are_read_as_one_history() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = WindowJournal::open(tmp.path(), "s", None);
        std::fs::create_dir_all(&journal.dir).unwrap();
        // What the fold read, and what a straggler appended after it did.
        let folded = format!("{}\n", read(1, "folded.rs", 1, 1, 40).to_json_line());
        let straggler = format!("{}\n", read(2, "straggler.rs", 1, 1, 10).to_json_line());
        std::fs::write(
            journal.rolled_path(1),
            format!("{folded}{straggler}").as_bytes(),
        )
        .unwrap();
        std::fs::write(
            journal.snapshot_path(1),
            serde_json::json!({
                "schema_version": SNAPSHOT_SCHEMA,
                "ordinal": 1,
                "source": format!("{}.1.rolled.jsonl", journal.key),
                "source_bytes": folded.len(),
                "lines": [read(1, "folded.rs", 1, 1, 40).to_json_line()],
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        journal.append(&read(3, "live.rs", 1, 1, 5)).unwrap();

        let loaded = journal.load();
        assert_eq!(loaded.dropped, 0);
        for (path, hi) in [("folded.rs", 40), ("straggler.rs", 10), ("live.rs", 5)] {
            assert!(
                loaded
                    .journal
                    .coverage(path, hash(1))
                    .contains(Interval::new(1, hi).unwrap()),
                "{path}: a reader sees every piece of the history"
            );
        }
        assert_eq!(
            loaded.journal.lines().len(),
            3,
            "and the rows the snapshot already accounted for are not read twice"
        );
    }

    /// The row a sibling hook appends **while the fold is running** — after it
    /// has read the file it rolled aside, before its snapshot is on disk.
    ///
    /// That process opened the live file before the rename, so its append goes
    /// into the rolled file, and there are exactly two places the reader can
    /// find it afterwards: inside the snapshot, or in the tail past the byte
    /// the snapshot accounts for. A byte measured after the reading rather
    /// than at the rename puts it in neither — on disk, below the accounted
    /// byte, and never read again. This drives the fold's two steps in order
    /// and appends between them, and the row that lands there is an emptying,
    /// because that is the row whose loss is not a lost saving but a decline
    /// of lines the window no longer holds.
    #[test]
    fn a_row_appended_while_the_fold_runs_is_still_read_back() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = WindowJournal::open(tmp.path(), "s", None);
        journal.append(&read(1, "a.rs", 1, 1, 40)).unwrap();
        // A sibling hook that opened the live file before the roll. Its writes
        // follow the file, not the name.
        let mut sibling = std::fs::OpenOptions::new()
            .append(true)
            .open(journal.live_path())
            .unwrap();

        let (ordinal, caps) = journal.roll().unwrap().expect("the roll claimed the fold");
        let (merged, accounted) = journal.merged_history(&caps);
        // Here: the fold has read, and has not yet written its snapshot.
        let evicted = Line::Evict {
            turn: 2,
            reason: EvictReason::PreCompact,
            at_ms: 2,
        };
        let mut row = evicted.to_json_line();
        row.push('\n');
        sibling.write_all(row.as_bytes()).unwrap();
        drop(sibling);
        journal
            .write_snapshot(ordinal, &accounted, &merged)
            .unwrap();

        let loaded = journal.load();
        assert!(
            loaded.journal.lines().contains(&evicted),
            "the row landed on disk and the reader must still find it: {:?}",
            loaded.journal.lines(),
        );
        assert!(
            loaded.journal.coverage("a.rs", hash(1)).is_empty(),
            "and it empties the window, which is what makes losing it a wrong decline"
        );
    }

    /// A process that opened a rolled file goes on appending to it, and a
    /// later fold does not close it. The row it writes lands in a file two
    /// folds old — and the account of how far each rolled file has been read
    /// is what keeps that row reachable.
    ///
    /// The account travelling from snapshot to snapshot is the whole of the
    /// fix. An account kept only for the newest rolled file would leave this
    /// row below a byte nothing looks past again: on disk, in a file the
    /// newest snapshot stands for, never read. The row here is an emptying for
    /// the same reason as in the fold-race test above — losing one is not a
    /// lost saving but a decline of lines the window no longer holds.
    #[test]
    fn a_straggler_row_in_an_older_rolled_file_survives_a_later_fold() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = WindowJournal::open(tmp.path(), "s", None);
        journal.append(&read(1, "a.rs", 1, 1, 40)).unwrap();
        // A sibling hook that opened the live file before the first roll.
        let mut sibling = std::fs::OpenOptions::new()
            .append(true)
            .open(journal.live_path())
            .unwrap();

        assert_eq!(journal.fold().unwrap(), Some(1));
        journal.append(&read(2, "b.rs", 1, 1, 40)).unwrap();
        assert_eq!(journal.fold().unwrap(), Some(2), "a second fold, later");

        // Only now does the straggler write, into the file it still holds —
        // the one the first fold rolled aside, two folds ago.
        let evicted = Line::Evict {
            turn: 3,
            reason: EvictReason::PreCompact,
            at_ms: 3,
        };
        let mut row = evicted.to_json_line();
        row.push('\n');
        sibling.write_all(row.as_bytes()).unwrap();
        drop(sibling);

        let loaded = journal.load();
        assert!(
            loaded.journal.lines().contains(&evicted),
            "the row is in the oldest rolled file and must still be read: {:?}",
            loaded.journal.lines(),
        );
        assert!(
            loaded.journal.coverage("a.rs", hash(1)).is_empty()
                && loaded.journal.coverage("b.rs", hash(1)).is_empty(),
            "and it empties the window, which is what makes losing it a wrong decline"
        );
        // A third fold must not lose it again: the account it writes carries
        // every rolled file, not just the one it made.
        journal.append(&read(4, "c.rs", 1, 1, 5)).unwrap();
        assert_eq!(journal.fold().unwrap(), Some(3));
        assert!(
            journal.load().journal.lines().contains(&evicted),
            "the folded history keeps it"
        );
    }

    /// A snapshot nobody can read took the history with it. Read as nothing at
    /// all it would leave the rows written *after* it standing alone, which is
    /// harmless — but read as an emptying it also guarantees that no decline
    /// can rest on coverage whose emptying line was in the part that is gone.
    #[test]
    fn an_unreadable_snapshot_is_read_as_an_emptying() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = WindowJournal::open(tmp.path(), "s", None);
        std::fs::create_dir_all(&journal.dir).unwrap();
        std::fs::write(journal.snapshot_path(1), b"{ not json").unwrap();
        journal.append(&read(3, "live.rs", 1, 1, 5)).unwrap();

        let loaded = journal.load();
        assert_eq!(loaded.dropped, 1, "the loss is counted, not silent");
        assert_eq!(
            loaded.journal.lines().first().map(Line::kind),
            Some("evict"),
            "a history that cannot be shown empties what came before it"
        );
        assert!(
            loaded
                .journal
                .coverage("live.rs", hash(1))
                .contains(Interval::new(1, 5).unwrap()),
            "and what came after it is untouched"
        );
    }

    /// A file past the byte budget is not parsed at all: the directory is
    /// writable by whatever runs in this checkout, and the decision path needs
    /// a bound that does not depend on who wrote the file.
    #[test]
    fn a_file_past_the_budget_holds_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = WindowJournal::open(tmp.path(), "s", None);
        journal.append(&read(1, "a.rs", 1, 1, 40)).unwrap();
        let filler = vec![b'\n'; usize::try_from(MAX_FILE_BYTES).unwrap() + 1];
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(journal.live_path())
            .unwrap();
        file.write_all(&filler).unwrap();
        drop(file);
        assert!(journal.load().journal.coverage("a.rs", hash(1)).is_empty());
    }

    /// The live file folds itself once it is long enough, without anyone
    /// asking, and the window holds exactly what it held before.
    #[test]
    fn an_append_past_the_cap_folds_the_file_first() {
        let tmp = tempfile::tempdir().unwrap();
        let journal = WindowJournal::open(tmp.path(), "s", None);
        let rows = 2_000u32;
        for turn in 1..=rows {
            journal.append(&read(turn, "a.rs", 1, turn, turn)).unwrap();
        }
        assert_eq!(journal.ordinals(".snapshot.json").len(), 1, "folded once");

        let rolled = std::fs::read_to_string(journal.rolled_path(1)).unwrap();
        assert!(
            rolled.lines().count() <= MAX_LIVE_LINES,
            "a file is folded before it passes the cap in rows, never after"
        );
        let loaded = journal.load();
        assert_eq!(loaded.dropped, 0);
        assert!(
            rows_on_disk(&journal) < rows as usize,
            "the rows after the fold are in a fresh file"
        );
        assert!(
            loaded
                .journal
                .coverage("a.rs", hash(1))
                .contains(Interval::new(1, rows).unwrap()),
            "every line read before and after the fold is still held"
        );
    }

    /// One appending process, run two ways: on its own, from the ordinary
    /// suite, and as the child [`concurrent_appends_from_two_processes_never_corrupt_the_journal`]
    /// spawns into a journal another process is appending to at the same time.
    /// Either way the claim is the same — every row it was asked for is in the
    /// file afterwards, whole.
    #[test]
    fn an_appending_child_writes_every_row_it_is_asked_for() {
        let solo = tempfile::tempdir().unwrap();
        let (root, tag) = match (std::env::var(CHILD_DIR), std::env::var(CHILD_TAG)) {
            (Ok(dir), Ok(tag)) => (PathBuf::from(dir), tag),
            _ => (solo.path().to_path_buf(), "alone".to_owned()),
        };
        let name = format!("{tag}.rs");
        let journal = WindowJournal::open(&root, "one-window", None);
        for turn in 1..=CHILD_ROWS {
            journal
                .append(&read(turn, &name, 1, turn, turn))
                .expect("an append this process asked for");
        }
        let loaded = journal.load();
        assert!(
            loaded
                .journal
                .coverage(&name, hash(1))
                .contains(Interval::new(1, CHILD_ROWS).unwrap()),
            "every row this process wrote is in the file it wrote them to"
        );
    }

    /// Two real processes append to one window's journal at the same time.
    ///
    /// A client may run its hooks in parallel and a model may issue parallel
    /// tool calls, so this is the ordinary case, not a stress test. Every row
    /// must survive whole: none torn in half by the other process's write,
    /// none interleaved into an unparseable one, and none lost.
    #[test]
    fn concurrent_appends_from_two_processes_never_corrupt_the_journal() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = std::env::current_exe().expect("this test binary");
        let tags = ["alpha", "beta"];
        let children: Vec<_> = tags
            .iter()
            .map(|tag| {
                std::process::Command::new(&exe)
                    .args([CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"])
                    .env(CHILD_DIR, tmp.path())
                    .env(CHILD_TAG, tag)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .expect("a second process")
            })
            .collect();
        for mut child in children {
            assert!(
                child.wait().expect("the child finished").success(),
                "an appending process failed its own assertion"
            );
        }

        let window = WindowJournal::open(tmp.path(), "one-window", None);
        let loaded = window.load();
        assert_eq!(loaded.dropped, 0, "no row was torn or interleaved");
        assert_eq!(
            rows_on_disk(&window),
            2 * CHILD_ROWS as usize,
            "every row both processes wrote is on disk, once"
        );
        assert_eq!(loaded.journal.lines().len(), 2 * CHILD_ROWS as usize);
        for tag in tags {
            let name = format!("{tag}.rs");
            assert!(
                loaded
                    .journal
                    .coverage(&name, hash(1))
                    .contains(Interval::new(1, CHILD_ROWS).unwrap()),
                "{name}: every line one process recorded is held"
            );
        }
    }
}

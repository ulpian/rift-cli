//! The window journal on disk: one append-only file per context window, under
//! this checkout's `.rift/state/`.
//!
//! The decision — what a window has been handed, and what may be declined
//! because of it — is pure and lives in the agent SDK. This module is the
//! other half: where those lines are kept, how two processes may append to
//! them at the same time, and how a file that has grown long is folded into a
//! smaller equivalent one without a reader ever seeing a gap.
//!
//! **Local run state, never a record of anything.** Losing every file in this
//! directory costs a re-read that would have been declined. Nothing here is
//! joined to a canonical table, synced, or trusted to be well-formed: the
//! directory sits in a work tree an agent can write to, so every row is
//! validated on the way in and a row that fails is dropped rather than
//! interpreted.
//!
//! **Every failure leans one way.** A file that cannot be read, a snapshot
//! that will not parse, a row longer than one atomic write, a directory that
//! cannot be created — each ends as *less* coverage than really exists, which
//! costs a saving and can never produce a decline of lines the window does not
//! hold. The one direction that would be unsafe — keeping coverage while
//! losing the line that says the window was emptied — is why an unreadable
//! snapshot is read as an emptying rather than as nothing at all.
//!
//! ## The files
//!
//! ```text
//! .rift/state/reads/<key>.jsonl                 the live file: appended to
//! .rift/state/reads/<key>.<n>.rolled.jsonl      a live file that filled up
//! .rift/state/reads/<key>.<n>.snapshot.json     the state of everything ≤ n
//! ```
//!
//! `<key>` is one **context window**, not one session: a subagent runs under
//! its parent's session id and carries its own agent id, and the two windows
//! hold different bytes, so the key is the hash of both together.
//!
//! ## Appending, and two processes doing it at once
//!
//! A client may run its hooks in parallel and a model may issue parallel tool
//! calls, so two processes appending to one window's file is ordinary, not
//! exceptional. Each row is one `write` of at most [`MAX_APPEND_BYTES`] to a
//! file opened for append — atomic per call on POSIX below `PIPE_BUF` and on
//! Windows through `FILE_APPEND_DATA` — so rows never interleave. A reader
//! arriving mid-append sees a final row without its newline and drops it,
//! because a row that does not parse is a row that never happened.
//!
//! ## Reading a folded history
//!
//! A live file that fills up is *renamed* aside and a snapshot of the merged
//! state is written beside it; the writing half lands next. What a reader must
//! do with the result is here: the newest snapshot stands for every rolled
//! file below it, and each of those is read past the byte the snapshot
//! accounts for — which is where an append from a process still holding the
//! old file open lands, however many folds ago that file was rolled — while
//! rolled files above it belong to a fold that did not finish and are read
//! whole, and the live file comes last. Nothing is ever truncated, so no
//! concurrent append can be lost, and a snapshot that cannot be read is taken
//! as the window having been emptied rather than as nothing at all.
//!
//! Those bytes are measured **once**, immediately after the rename and before
//! a byte of any rolled file is read, and the fold reads no further than them.
//! That ordering is what makes "no concurrent append can be lost" true rather
//! than nearly true: an accounted byte measured after the reading would count
//! rows the snapshot had not taken in, and a row that landed in that gap would
//! be in neither place. The account names every rolled file rather than only
//! the newest, because a straggler writes into the file it opened for as long
//! as it lives, and a row it adds to a file two folds old has to stay
//! reachable too.
//!

use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};

use rift_agent::window_journal::{DenyKey, EvictReason, Interval, IntervalSet, Journal, Line};

/// How many rows a live file may hold before it is folded.
pub(crate) const MAX_LIVE_LINES: usize = 4_096;

/// The largest row this will write in one call.
///
/// Atomicity is what keeps two appending processes from interleaving, and it
/// is a property of a *single* write of a bounded size. A row that would not
/// fit is refused rather than split, and a refused row is one line of
/// coverage the journal does not have. Ordinary rows are a few hundred bytes;
/// only a pathological path or a long list of edited sites comes near this.
pub(crate) const MAX_APPEND_BYTES: usize = 4_096;

/// The most bytes this will parse out of any one file.
///
/// The directory is writable by whatever runs in this checkout, and a read on
/// the decision path needs a bound that does not depend on who wrote the file.
/// Past the bound the file is read as an emptying of the window: no coverage,
/// nothing declined.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

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

/// The schema every snapshot this writes carries, and the only one it reads.
const SNAPSHOT_SCHEMA: u32 = 1;

/// Where one checkout keeps its window journals.
///
/// Per checkout by design: the bytes at a path differ between members of a
/// worktree family, and coverage is a claim about bytes. The directory is
/// under the ignored run-state root, so nothing here is ever committed.
pub(crate) fn dir(work_tree: &Path) -> PathBuf {
    work_tree.join(".rift").join("state").join("reads")
}

/// The file name of one context window: the session it belongs to and the
/// agent inside it, hashed together.
///
/// A subagent's hooks carry the parent's session id and their own agent id.
/// Keying by session alone would let one window's reads decline another
/// window's calls, which is the one mistake that costs an agent bytes it does
/// not hold — so the agent id is part of the key, and its absence (an ordinary
/// main-thread call) is a distinct, empty part rather than a missing one.
pub(crate) fn key(session_id: &str, agent_id: Option<&str>) -> String {
    let agent = agent_id.unwrap_or_default();
    let mut bytes = Vec::with_capacity(session_id.len() + 1 + agent.len());
    bytes.extend_from_slice(session_id.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(agent.as_bytes());
    rift_hashchain::leaf_hash(&bytes).to_hex()
}

/// A 64-character lowercase-hex key, and nothing else — the shape every file
/// this module owns is named with, and the screen every sweep applies before
/// it removes anything.
fn is_key(s: &str) -> bool {
    s.len() == 64
        && s.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

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
    dir: PathBuf,
    key: String,
}

impl WindowJournal {
    /// The journal of one context window in one checkout. Opens nothing and
    /// creates nothing; the first append makes the directory.
    pub(crate) fn open(work_tree: &Path, session_id: &str, agent_id: Option<&str>) -> Self {
        Self {
            dir: dir(work_tree),
            key: key(session_id, agent_id),
        }
    }

    /// The file appends land in.
    fn live_path(&self) -> PathBuf {
        self.dir.join(format!("{}.jsonl", self.key))
    }

    fn rolled_path(&self, ordinal: u64) -> PathBuf {
        self.dir
            .join(format!("{}.{ordinal}.rolled.jsonl", self.key))
    }

    fn snapshot_path(&self, ordinal: u64) -> PathBuf {
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

/// A line saying the window no longer holds what it was given.
///
/// Used where the history itself has been lost: it empties coverage for
/// everything before it and leaves everything after it intact, which is
/// exactly what "we cannot show what was held" should mean.
fn emptied() -> Line {
    Line::Evict {
        turn: 0,
        reason: EvictReason::Clear,
        at_ms: 0,
    }
}

/// How far into each rolled file a snapshot has already read, by ordinal.
///
/// The byte a snapshot stands for in one file is not the whole account: a
/// straggler keeps appending to whichever file it opened, however many folds
/// happen afterwards, so the account names every rolled file and travels from
/// one snapshot to the next.
type Accounted = std::collections::BTreeMap<u64, u64>;

/// One snapshot, as it was written.
struct Snapshot {
    /// The rows of the merged state.
    lines: Vec<String>,
    /// How far into each rolled file this snapshot read.
    accounted: Accounted,
}

/// Read one snapshot, or `None` for anything this version did not write.
///
/// `ordinal` is the one the file is named with, and it is what an older
/// snapshot — written before the account named more than one file — is read
/// as accounting for.
fn read_snapshot(path: &Path, ordinal: u64) -> Option<Snapshot> {
    let text = read_bounded(path, 0, None)?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    if value.get("schema_version")?.as_u64()? != u64::from(SNAPSHOT_SCHEMA) {
        return None;
    }
    let source_bytes = value.get("source_bytes")?.as_u64()?;
    let accounted = value
        .get("sources")
        .and_then(serde_json::Value::as_object)
        .map_or_else(
            || Accounted::from([(ordinal, source_bytes)]),
            |sources| {
                sources
                    .iter()
                    .filter_map(|(key, byte)| Some((key.parse().ok()?, byte.as_u64()?)))
                    .collect()
            },
        );
    Some(Snapshot {
        lines: value
            .get("lines")?
            .as_array()?
            .iter()
            .filter_map(|row| row.as_str().map(str::to_owned))
            .collect(),
        accounted,
    })
}

/// A file's text from `from` to `to` (its end when that is `None`), or `None`
/// when it cannot be read or is past the bound this path will spend.
fn read_bounded(path: &Path, from: u64, to: Option<u64>) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let end = to.map_or(len, |end| end.min(len));
    let span = end.saturating_sub(from);
    if span > MAX_FILE_BYTES {
        return None;
    }
    if from > 0 {
        file.seek(std::io::SeekFrom::Start(from)).ok()?;
    }
    let mut text = String::new();
    file.take(span).read_to_string(&mut text).ok()?;
    Some(text)
}

/// Parse one row into `lines`, counting it if it is not a line this version
/// wrote — a torn final row, a forged one, a row from a later schema.
fn absorb_row(row: &str, lines: &mut Vec<Line>, dropped: &mut usize) {
    match Line::from_json_line(row) {
        Some(line) => lines.push(line),
        None => *dropped += 1,
    }
}

/// Parse one file's rows from `from` to `to`; returns how many rows it held.
///
/// A file that is missing contributes nothing. A file too large to spend the
/// budget on is read as an emptying, for the same reason an unreadable
/// snapshot is: what cannot be shown cannot be claimed.
fn absorb_file(
    path: &Path,
    from: u64,
    to: Option<u64>,
    lines: &mut Vec<Line>,
    dropped: &mut usize,
) -> usize {
    if !path.exists() {
        return 0;
    }
    let Some(text) = read_bounded(path, from, to) else {
        *dropped += 1;
        lines.push(emptied());
        return 0;
    };
    let mut rows = 0usize;
    for row in text.lines() {
        if row.is_empty() {
            continue;
        }
        rows += 1;
        absorb_row(row, lines, dropped);
    }
    rows
}

/// The same state in fewer rows.
///
/// Reads of one revision are merged into their union, because a union is what
/// coverage is; every other kind of row is kept, in order, once. The merge
/// happens strictly **within** the stretches between emptyings: a read taken
/// before the window was emptied and one taken after are not the same claim,
/// and folding them together would resurrect coverage the window has lost.
fn merge(lines: &[Line]) -> Vec<Line> {
    let mut out = Vec::new();
    let mut stretch: Vec<&Line> = Vec::new();
    for line in lines {
        if matches!(line, Line::Evict { .. }) {
            merge_stretch(&stretch, &mut out);
            stretch.clear();
            out.push(line.clone());
        } else {
            stretch.push(line);
        }
    }
    merge_stretch(&stretch, &mut out);
    out
}

/// One revision's reads, folded together.
struct Held {
    path: String,
    hash: rift_core::Sha256Id,
    lines: IntervalSet,
    /// The earliest turn any of them was taken in.
    turn: u32,
    /// The longest the file was seen to be at that revision.
    total_lines: u32,
    /// The latest moment any of them was written.
    at_ms: u64,
}

/// Fold one stretch between emptyings into `out`.
fn merge_stretch(stretch: &[&Line], out: &mut Vec<Line>) {
    // Reads first, one row per run of lines actually held, keyed by the
    // revision they were taken of. The turn kept is the earliest, because that
    // is the turn a reader is told the bytes have been in front of it since.
    let mut held: Vec<Held> = Vec::new();
    let mut rest: Vec<Line> = Vec::new();
    for line in stretch {
        if let Line::Read {
            turn,
            path,
            hash,
            range,
            total_lines,
            at_ms,
        } = line
        {
            if let Some(entry) = held
                .iter_mut()
                .find(|entry| entry.path == *path && entry.hash == *hash)
            {
                entry.lines.insert(*range);
                entry.turn = entry.turn.min(*turn);
                entry.total_lines = entry.total_lines.max(*total_lines);
                entry.at_ms = entry.at_ms.max(*at_ms);
            } else {
                let mut lines = IntervalSet::new();
                lines.insert(*range);
                held.push(Held {
                    path: path.clone(),
                    hash: *hash,
                    lines,
                    turn: *turn,
                    total_lines: *total_lines,
                    at_ms: *at_ms,
                });
            }
        } else if !rest.iter().any(|kept| kept == *line) {
            rest.push((*line).clone());
        }
    }
    for entry in held {
        for run in entry.lines.runs() {
            out.push(Line::Read {
                turn: entry.turn,
                path: entry.path.clone(),
                hash: entry.hash,
                range: *run,
                total_lines: entry.total_lines,
                at_ms: entry.at_ms,
            });
        }
    }
    out.append(&mut rest);
}

/// Whether this window's last word on exactly these lines is a decline that
/// nothing has answered since.
///
/// The question a tool's *result* asks: a call the journal declined has no
/// result, so one arriving means the client ran the call anyway. Read from the
/// newest line back, and only since the window was last emptied — a decline
/// before an emptying is spent, and the read that follows it is a legitimate
/// first read of bytes the window no longer holds.
pub(crate) fn deny_stands(journal: &Journal, key: &DenyKey) -> bool {
    journal.live().iter().rev().find_map(|line| match line {
        Line::Deny {
            path, hash, range, ..
        } if key.matches(path, *hash, *range) => Some(true),
        Line::Read {
            path, hash, range, ..
        } if key.matches(path, *hash, *range) => Some(false),
        _ => None,
    }) == Some(true)
}

/// Declines a client did not act on: a decline, and then a read of exactly
/// those lines, with no emptying between them.
///
/// A client too old to understand a decision on the channel it was sent on
/// runs the call regardless, and the read that follows is how that shows. It
/// is not a failure of the decision — the bytes did reach the window and the
/// journal records that they did — but a count of them is the difference
/// between "the model asked again" and "the client never asked the model".
pub(crate) fn ineffective_denies(journal: &Journal) -> usize {
    let mut standing: Vec<DenyKey> = Vec::new();
    let mut ineffective = 0usize;
    for line in journal.lines() {
        match line {
            Line::Deny {
                path, hash, range, ..
            } => standing.push(DenyKey {
                path: path.clone(),
                hash: *hash,
                range: *range,
            }),
            Line::Read {
                path, hash, range, ..
            } => {
                if let Some(at) = standing
                    .iter()
                    .position(|key| key.matches(path, *hash, *range))
                {
                    standing.remove(at);
                    ineffective += 1;
                }
            }
            // An emptying spends every decline before it: the window no longer
            // holds what it was declined for, so the next read of those lines
            // is a first read and not a decision ignored.
            Line::Evict { .. } => standing.clear(),
            _ => {}
        }
    }
    ineffective
}

/// The most an edit line will name before it names nothing.
///
/// A row is one atomic append of at most [`MAX_APPEND_BYTES`], and a list of
/// changed places long enough to threaten that is also a list no reader acts
/// on: "the file changed in thirty-three places" is "the file changed". Past
/// the cap the places are dropped and the change is recorded as one whose
/// places do not follow from its payload, which is where every other
/// underivable case ends too.
pub(crate) const MAX_EDIT_SITES: usize = 32;

/// How many lines a stretch of text spans — zero for the empty string, else
/// one more than the newlines it contains, whether or not it ends in one.
///
/// The same convention the adapters count a replaced stretch with, because the
/// shift below is the subtraction of the two counts and a subtraction of two
/// different conventions is a number about nothing.
fn line_span(text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    u32::try_from(text.matches('\n').count())
        .unwrap_or(u32::MAX)
        .saturating_add(1)
}

/// A file's text with Windows line endings written the way the payload writes
/// them, so that a needle carrying `\n` is looked for in a haystack that
/// carries `\n` too.
///
/// Only carriage returns before a newline go: the count of newlines is
/// untouched, so a line number taken from the normalised text is a line number
/// of the file.
fn normalised(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains('\r') {
        std::borrow::Cow::Owned(text.replace("\r\n", "\n"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

/// Where a stretch of text sits in a file, as runs of lines, left to right and
/// never overlapping.
///
/// The search is an exact byte match: the text an agent wrote is either in the
/// file it wrote it to or it is not, and a fuzzy answer to that question is
/// exactly the guess this whole mechanism exists to avoid.
fn locate(haystack: &str, needle: &str) -> Vec<Interval> {
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    // A stretch ending in a newline occupies the lines before that newline:
    // "fn f() {\n" is one line, not the first two.
    let span = line_span(needle).saturating_sub(u32::from(needle.ends_with('\n')));
    let mut cursor = 0usize;
    let mut line = 1u32;
    while let Some(offset) = haystack.get(cursor..).and_then(|rest| rest.find(needle)) {
        let start = cursor + offset;
        line = line.saturating_add(newlines_between(haystack, cursor, start));
        let Some(run) = Interval::new(line, line.saturating_add(span.saturating_sub(1))) else {
            break;
        };
        out.push(run);
        if out.len() > MAX_EDIT_SITES {
            return Vec::new();
        }
        let end = start + needle.len();
        line = line.saturating_add(newlines_between(haystack, start, end));
        cursor = end;
    }
    out
}

/// How many line breaks lie between two byte offsets of one string.
fn newlines_between(text: &str, from: usize, to: usize) -> u32 {
    let slice = text.get(from..to).unwrap_or_default();
    u32::try_from(slice.matches('\n').count()).unwrap_or(u32::MAX)
}

/// What this window already knew about a file, at the moment something
/// changed it.
///
/// The revision is the newest one the journal names for the path — by the
/// millisecond the line was written, never by where it sits in the file. Rows
/// from two processes appending at once arrive in whatever order the calls
/// land in, and a fold rewrites a stretch of them into merged reads followed
/// by everything else, so position says nothing about time and the clock the
/// hook was handed says it exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PriorRevision {
    /// The revision the file was last seen at.
    pub(crate) hash: rift_core::Sha256Id,
    /// How long it was then, where a read line said so.
    pub(crate) total_lines: Option<u32>,
    /// Whether this window holds every line of it.
    pub(crate) held_in_full: bool,
}

/// What the journal knew about `path` before this change, or `None` when it
/// knew nothing — or when two lines of the same millisecond disagree about
/// which revision was the last one, which is a tie nobody can break and the
/// safe answer to is "nothing is known".
pub(crate) fn prior_revision(journal: &Journal, path: &str) -> Option<PriorRevision> {
    let mut newest_at = 0u64;
    let mut hash: Option<rift_core::Sha256Id> = None;
    let mut tied = false;
    for line in journal.live() {
        let seen = match line {
            Line::Read {
                path: p,
                hash,
                at_ms,
                ..
            } if p == path => (*hash, *at_ms),
            Line::Edit {
                path: p,
                hash_new,
                at_ms,
                ..
            } if p == path => (*hash_new, *at_ms),
            _ => continue,
        };
        if hash.is_none() || seen.1 > newest_at {
            newest_at = seen.1;
            hash = Some(seen.0);
            tied = false;
        } else if seen.1 == newest_at && hash != Some(seen.0) {
            tied = true;
        }
    }
    if tied {
        return None;
    }
    let hash = hash?;
    let total_lines = journal
        .live()
        .iter()
        .filter_map(|line| match line {
            Line::Read {
                path: p,
                hash: h,
                total_lines,
                ..
            } if p == path && *h == hash => Some(*total_lines),
            _ => None,
        })
        .max();
    let held_in_full = match total_lines.and_then(|n| Interval::new(1, n)) {
        Some(whole) => journal.coverage(path, hash).contains(whole),
        // Nothing here says how long the file was, so the only way this window
        // can hold all of it is by having written all of it.
        None => journal.wrote_in_full(path, hash).is_some(),
    };
    Some(PriorRevision {
        hash,
        total_lines,
        held_in_full,
    })
}

/// The hash a whole-file write said its bytes would have, where one was
/// recorded before the write landed.
pub(crate) fn expected_write(journal: &Journal, path: &str) -> Option<rift_core::Sha256Id> {
    journal
        .live()
        .iter()
        .filter_map(|line| match line {
            Line::Expect {
                path: p,
                hash,
                at_ms,
                ..
            } if p == path => Some((*at_ms, *hash)),
            _ => None,
        })
        .max_by_key(|(at_ms, _)| *at_ms)
        .map(|(_, hash)| hash)
}

/// What an edit line will say about a change: where the file changed, by how
/// many lines it grew, and whether the window that made the change holds every
/// line of the result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EditFacts {
    /// Where the file changed, in the numbering of the **new** bytes. A
    /// superset is honest — "everything else is unchanged" stays true — and an
    /// empty list says the places do not follow from the payload.
    pub(crate) sites: Vec<Interval>,
    /// How many lines the file grew by, negative when it shrank.
    pub(crate) shift: i64,
    /// Whether the window provably holds every line of the new revision.
    pub(crate) full: bool,
}

impl EditFacts {
    /// Whether this says anything at all beyond the file's new hash.
    pub(crate) fn is_silent(&self) -> bool {
        self.sites.is_empty() && !self.full
    }
}

/// What follows from an edit-shaped payload and the bytes it left behind.
///
/// Three questions, answered in the order they can be answered at all:
///
/// * **Did the change land as the payload described it?** For a whole-file
///   write that is an equality — the file's hash against the hash of the bytes
///   the payload carried — and it is the check that separates "the agent wrote
///   these bytes" from "a formatter rewrote them on the way in". For a
///   replacement it is a location: the text the agent wrote is in the file
///   exactly where a replacement would have put it, or something else has
///   happened here and nothing is claimed.
/// * **Where did it change?** In the numbering of the bytes a reader is about
///   to see, computed from those bytes, never by moving an older line number
///   along by an offset.
/// * **Does the window hold the result?** Only if it held every line of what
///   was there before, and only if the arithmetic agrees: a file that grew by
///   more lines than the replacement added grew somewhere else as well, and
///   that somewhere else is a place this window has never seen.
pub(crate) fn edit_facts(
    kind: &rift_agent::EditKind,
    new_text: &str,
    new_lines: u32,
    new_hash: rift_core::Sha256Id,
    expected: Option<rift_core::Sha256Id>,
    prior: Option<PriorRevision>,
) -> EditFacts {
    let held_before = prior.is_some_and(|p| p.held_in_full);
    // The file's own growth, where the journal knows how long it was: two
    // measured lengths, not an estimate.
    let measured_shift = prior
        .and_then(|p| p.total_lines)
        .map(|n_old| i64::from(new_lines) - i64::from(n_old));
    let silent = |shift: i64| EditFacts {
        sites: Vec::new(),
        shift,
        full: false,
    };
    match kind {
        rift_agent::EditKind::Write { content_sha256 } => {
            let promised = expected.unwrap_or(*content_sha256);
            let shift = measured_shift.unwrap_or_default();
            if promised != new_hash {
                // Formatter drift: the bytes on disk are not the bytes the
                // payload carried, so the window holds neither the file nor a
                // description of how it differs.
                return silent(shift);
            }
            EditFacts {
                sites: Interval::new(1, new_lines).into_iter().collect(),
                shift,
                // The bytes came out of the agent's own payload, so a window
                // that never read the file still holds every line of it.
                full: new_lines > 0,
            }
        }
        rift_agent::EditKind::Edit {
            new_string,
            old_string_lines,
            replace_all,
        } => {
            let haystack = normalised(new_text);
            let needle = normalised(new_string);
            let per_site = i64::from(line_span(&needle)) - i64::from(*old_string_lines);
            // An empty stretch is nothing to look for, and `locate` finds
            // nothing, which lands on the underivable arm below: no places and
            // no coverage. That is deliberately the conservative answer and not
            // a special case — the adapters do not build an `Edit` out of an
            // absent or empty replacement in the first place, so a special case
            // here would be a claim about a payload nothing produces, on a
            // branch no test could reach honestly.
            let sites = locate(&haystack, &needle);
            let found = i64::try_from(sites.len()).unwrap_or(i64::MAX);
            if sites.is_empty() || (!*replace_all && sites.len() > 1) {
                // Not there at all, or there more than once where the tool
                // promised one: either way the site is not this payload's to
                // name and the old bytes stop counting.
                return silent(measured_shift.unwrap_or_default());
            }
            if let Some(measured) = measured_shift {
                if found * per_site != measured {
                    // The file grew by a different number of lines than these
                    // replacements account for, so something else changed it
                    // too — somewhere this window has not seen.
                    return silent(measured);
                }
            }
            EditFacts {
                sites,
                shift: measured_shift.unwrap_or(found * per_site),
                full: held_before,
            }
        }
        // Several edits in one call, a notebook cell, a patch document: the
        // change is real and worth recording, and where it landed does not
        // follow from the payload without the bytes that were there before.
        rift_agent::EditKind::Other => silent(measured_shift.unwrap_or_default()),
    }
}

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

/// Which of this module's three file shapes a name is.
enum Kind {
    Live,
    Rolled,
    Snapshot,
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
    use rift_agent::window_journal::Interval;

    use super::*;

    /// Where the child process this suite spawns is told to write.
    const CHILD_DIR: &str = "RIFT_TEST_WINDOW_JOURNAL_DIR";
    /// Which name the child process writes its rows under.
    const CHILD_TAG: &str = "RIFT_TEST_WINDOW_JOURNAL_TAG";
    /// The test the child process is asked to run — itself, filtered to one.
    const CHILD_TEST: &str =
        "window_journal::tests::an_appending_child_writes_every_row_it_is_asked_for";
    /// How many rows each appending process writes.
    const CHILD_ROWS: u32 = 300;

    /// Rows in the live file, counted from the file rather than from anything
    /// that parsed it: what is on disk, whether or not it is a line.
    fn rows_on_disk(journal: &WindowJournal) -> usize {
        std::fs::read_to_string(journal.live_path())
            .map(|text| text.lines().count())
            .unwrap_or_default()
    }

    /// A hash to hang a line on; the value is arbitrary, its shape is not.
    fn hash(seed: u8) -> rift_core::Sha256Id {
        rift_hashchain::leaf_hash(&[seed])
    }

    fn read(turn: u32, path: &str, revision: u8, lo: u32, hi: u32) -> Line {
        Line::Read {
            turn,
            path: path.to_owned(),
            hash: hash(revision),
            range: Interval::new(lo, hi).expect("a run"),
            total_lines: 400,
            at_ms: u64::from(turn),
        }
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

    /// One context window is a session **and** the agent inside it. A subagent
    /// runs under its parent's session id, so a key that ignored the agent
    /// would let the parent's reads decline the child's calls — bytes the
    /// child was never handed.
    #[test]
    fn a_subagent_gets_its_own_journal_under_its_parents_session() {
        let parent = key("session-1", None);
        let child = key("session-1", Some("agent-7"));
        assert_ne!(parent, child);
        assert_eq!(parent, key("session-1", None), "the key is a function");
        assert_eq!(parent.len(), 64, "a key names a file");
        assert!(is_key(&parent) && is_key(&child));
        // The key is exactly the documented derivation, not merely some hash
        // of the two parts: session, a zero byte, then the agent or nothing.
        assert_eq!(
            child,
            rift_hashchain::leaf_hash(b"session-1\0agent-7").to_hex()
        );
        assert_eq!(parent, rift_hashchain::leaf_hash(b"session-1\0").to_hex());
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

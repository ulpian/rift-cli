//! Rows on the way in, and the same rows in fewer of them.
//!
//! A separate module from the file handling because this half never touches
//! a path this module owns: it is given a file and a span of it to spend, and
//! it answers with lines. Every failure here leans towards less coverage —
//! what cannot be shown cannot be claimed — which is why a file it cannot
//! read is read as an emptying rather than as nothing at all.

use std::io::{Read as _, Seek as _};
use std::path::Path;

use rift_agent::window_journal::{EvictReason, IntervalSet, Line};

/// The most bytes this will parse out of any one file.
///
/// The directory is writable by whatever runs in this checkout, and a read on
/// the decision path needs a bound that does not depend on who wrote the file.
/// Past the bound the file is read as an emptying of the window: no coverage,
/// nothing declined.
pub(super) const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// The schema every snapshot this writes carries, and the only one it reads.
pub(super) const SNAPSHOT_SCHEMA: u32 = 1;

/// How far into each rolled file a snapshot has already read, by ordinal.
///
/// The byte a snapshot stands for in one file is not the whole account: a
/// straggler keeps appending to whichever file it opened, however many folds
/// happen afterwards, so the account names every rolled file and travels from
/// one snapshot to the next.
pub(super) type Accounted = std::collections::BTreeMap<u64, u64>;

/// One snapshot, as it was written.
pub(super) struct Snapshot {
    /// The rows of the merged state.
    pub(super) lines: Vec<String>,
    /// How far into each rolled file this snapshot read.
    pub(super) accounted: Accounted,
}

/// A line saying the window no longer holds what it was given.
///
/// Used where the history itself has been lost: it empties coverage for
/// everything before it and leaves everything after it intact, which is
/// exactly what "we cannot show what was held" should mean.
pub(super) fn emptied() -> Line {
    Line::Evict {
        turn: 0,
        reason: EvictReason::Clear,
        at_ms: 0,
    }
}

/// Read one snapshot, or `None` for anything this version did not write.
///
/// `ordinal` is the one the file is named with, and it is what an older
/// snapshot — written before the account named more than one file — is read
/// as accounting for.
pub(super) fn read_snapshot(path: &Path, ordinal: u64) -> Option<Snapshot> {
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
pub(super) fn absorb_row(row: &str, lines: &mut Vec<Line>, dropped: &mut usize) {
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
pub(super) fn absorb_file(
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
pub(super) fn merge(lines: &[Line]) -> Vec<Line> {
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

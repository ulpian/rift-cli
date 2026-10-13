//! The "no name or line count" bucket split by what the transcript itself
//! proves about the working tree, and by the size of the file.
//!
//! A module of its own because the bucket is a blind spot of the replay, not
//! of the gate: the gate hashes the bytes on disk whether or not git committed
//! them, while the replay reads a path's shape from the window's commit and
//! has nothing for a file no commit held. Every figure here is on rows of its
//! own, so every row before it reads the same with or without it.
//!
//! **The model** is the working tree as the transcript proves it, per session
//! tree and working directory: the bytes of a whole-file write, an edit applied
//! to bytes already known, and the full content a read delivered — a whole-file
//! read, or a numbered range spliced into bytes already known. An edit that
//! cannot be applied, or a read of a file never delivered whole, leaves the
//! path seen with no bytes.
//!
//! **The classes**, decided for the member that put the call in the bucket:
//!
//! 1. *Known, unchanged at re-read.* The model holds the file's bytes, no edit
//!    has changed them since a read or a write delivered them, and every line
//!    this call delivered agrees with them. Live, the gate hashes the same
//!    bytes and declines the re-read as held.
//! 2. *Known, changed.* The model holds the bytes, and an edit changed them
//!    since, or what the call delivered disagrees with them: a delta.
//! 3. *Unknown, a re-read.* The path was read or edited before, and the
//!    transcript holds no complete copy of its bytes.
//! 4. *Unknown, a first sight.* Nothing of the path was seen before.
//!
//! A shell call of several reads is matched against one file's model at a
//! time, so another member's lines read as a disagreement: the split errs
//! toward "changed", never toward "declinable".

use std::collections::{BTreeMap, HashSet};

use rift_agent::{HiKind, ReadRequest, ReadSpan};
use serde_json::{json, Map, Value};

use super::bucket::Bucket;
use super::delivery::PERSISTED_OUTPUT;
use super::render::row;
use super::replay::Ctx;
use super::report::{Report, Tally};
use super::transcript::Call;
use super::uncommitted_declines::{
    fold_the_read, uncommitted_json, uncommitted_rows, Left, Reread, Shape,
};
use super::worktree::path_key;

/// The size bands, in the order the table prints them: the band's upper
/// bound in bytes, a key, and a label.
const BANDS: [(u64, &str, &str); 5] = [
    (16 * 1024, "le_16_kib", "    ≤ 16 KiB"),
    (64 * 1024, "le_64_kib", "    ≤ 64 KiB"),
    (256 * 1024, "le_256_kib", "    ≤ 256 KiB"),
    (1024 * 1024, "le_1_mib", "    ≤ 1 MiB"),
    (u64::MAX, "over_1_mib", "    > 1 MiB"),
];

/// The classes, in the order the table prints them: a key and a label.
const CLASSES: [(&str, &str); 4] = [
    (
        "known_unchanged",
        "  known, unchanged at re-read (declinable)",
    ),
    ("known_changed", "  known, changed since (delta)"),
    (
        "unknown_reread",
        "  unknown: a re-read, no bytes in the transcript",
    ),
    (
        "unknown_first_sight",
        "  unknown: a first sight of the path",
    ),
];

/// The Read tool's default window: a limit-less read that delivered this many
/// lines may have stopped short of the file's end.
const READ_DEFAULT_WINDOW: usize = 2000;

/// What the transcript proves about one bucketed member's file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Standing {
    /// Known, and what the call delivered agrees with it.
    Unchanged,
    /// Known, and changed since it was delivered.
    Changed,
    /// Seen before, with no complete copy of its bytes.
    Reread,
    /// Never seen before.
    FirstSight,
}

/// The bucket's calls by class and by size band.
#[derive(Clone, Debug, Default)]
pub struct NoLineCountSplit {
    /// One row of bands per entry of `CLASSES`.
    cells: [[Tally; 5]; 4],
}

/// One file's bytes as the transcript proves them.
struct Known {
    /// The bytes, as text.
    text: String,
    /// One hash per line of `text`.
    lines: Vec<u64>,
    /// Whether an edit changed the bytes since a read or a write delivered
    /// them to the window.
    dirty: bool,
}

/// What one call put in front of the model.
pub(super) enum Delivered {
    /// The Read tool's numbered lines, by line number.
    Numbered(Vec<(usize, String)>),
    /// A shell read's output, line by line.
    Plain(Vec<String>),
}

/// The working tree a session tree's transcript proves, by working directory
/// and path. A path seen without a complete copy of its bytes maps to `None`.
#[derive(Default)]
pub(super) struct WorkingTree {
    files: BTreeMap<(String, String), Option<Known>>,
}

impl Standing {
    /// Where this standing sits in `CLASSES`.
    const fn index(self) -> usize {
        match self {
            Self::Unchanged => 0,
            Self::Changed => 1,
            Self::Reread => 2,
            Self::FirstSight => 3,
        }
    }
}

impl NoLineCountSplit {
    /// Count one call of the bucket.
    pub(super) fn note(&mut self, standing: Standing, bytes: u64, tokens: u64) {
        let band = BANDS
            .iter()
            .position(|(ceiling, _, _)| bytes <= *ceiling)
            .unwrap_or(4);
        if let Some(tally) = self
            .cells
            .get_mut(standing.index())
            .and_then(|row| row.get_mut(band))
        {
            tally.add(tokens);
        }
    }

    /// One class's total over every band.
    fn class(&self, at: usize) -> Tally {
        let mut total = Tally::default();
        for tally in self.cells.get(at).into_iter().flatten() {
            total.calls = total.calls.saturating_add(tally.calls);
            total.tokens = total.tokens.saturating_add(tally.tokens);
        }
        total
    }
}

impl Known {
    fn of(text: String, dirty: bool) -> Self {
        let lines = text.lines().map(line_hash).collect();
        Self { text, lines, dirty }
    }

    /// Whether every line a call delivered agrees with these bytes.
    fn agrees(&self, delivered: &Delivered, whole: bool) -> bool {
        match delivered {
            Delivered::Numbered(lines) => {
                let shorter = whole && lines.len() != self.lines.len();
                !lines.is_empty()
                    && !shorter
                    && lines.iter().all(|(n, line)| {
                        n.checked_sub(1).and_then(|at| self.lines.get(at)) == Some(&line_hash(line))
                    })
            }
            Delivered::Plain(lines) => {
                let held: HashSet<u64> = self.lines.iter().copied().collect();
                let mut telling = lines
                    .iter()
                    .filter(|line| line.trim().len() >= 4)
                    .peekable();
                telling.peek().is_some()
                    && telling.all(|line| {
                        held.contains(&line_hash(line))
                            || held.contains(&line_hash(strip_line_number(line)))
                    })
            }
        }
    }

    /// Put a numbered range into these bytes, or `None` where the range does
    /// not join them.
    fn splice(mut self, lines: &[(usize, String)]) -> Option<Self> {
        let mut text: Vec<String> = self.text.lines().map(str::to_owned).collect();
        for (n, line) in lines {
            let at = n.checked_sub(1)?;
            match at.cmp(&text.len()) {
                std::cmp::Ordering::Less => text.get_mut(at)?.clone_from(line),
                std::cmp::Ordering::Equal => text.push(line.clone()),
                std::cmp::Ordering::Greater => return None,
            }
        }
        self.text = joined(&text);
        self.lines = self.text.lines().map(line_hash).collect();
        Some(self)
    }
}

impl WorkingTree {
    /// What a change to `key` proves: a write's bytes, or an edit applied to
    /// bytes already known. An edit that failed proves nothing changed.
    pub(super) fn note_change(&mut self, call: &Call, key: &str) {
        let slot = (call.cwd.clone().unwrap_or_default(), key.to_owned());
        let result = call.result.as_deref().unwrap_or_default();
        if result.contains("<tool_use_error>") || result.starts_with("Error") {
            return;
        }
        let known = match call.name.as_str() {
            "Write" => content(call).map(|text| Known::of(text.to_owned(), false)),
            "Edit" | "MultiEdit" => self
                .files
                .remove(&slot)
                .flatten()
                .and_then(|known| applied(known, call)),
            _ => None,
        };
        self.files.insert(slot, known);
    }

    /// How the file behind one bucketed member stands, and its size in bytes:
    /// the known bytes, or what the call delivered when nothing is known.
    pub(super) fn standing(
        &self,
        call: &Call,
        key: &str,
        delivered: &Delivered,
        whole: bool,
    ) -> (Standing, Option<u64>) {
        let slot = (call.cwd.clone().unwrap_or_default(), key.to_owned());
        match self.files.get(&slot) {
            None => (Standing::FirstSight, None),
            Some(None) => (Standing::Reread, None),
            Some(Some(known)) => {
                let size = u64::try_from(known.text.len()).ok();
                if !known.dirty && known.agrees(delivered, whole) {
                    (Standing::Unchanged, size)
                } else {
                    (Standing::Changed, size)
                }
            }
        }
    }

    /// The shape of the bytes the model holds for a file, where it holds them.
    pub(super) fn shape(&self, call: &Call, key: &str) -> Option<Shape> {
        let slot = (call.cwd.clone().unwrap_or_default(), key.to_owned());
        let known = self.files.get(&slot)?.as_ref()?;
        Some(Shape::of(&known.text))
    }

    /// What one member's delivery proves: the whole file, a numbered range
    /// joined to bytes already known, or only that the path was seen.
    pub(super) fn note_delivery(
        &mut self,
        call: &Call,
        key: &str,
        delivered: Delivered,
        whole: bool,
    ) {
        let slot = (call.cwd.clone().unwrap_or_default(), key.to_owned());
        let prior = self.files.remove(&slot).flatten();
        let known = match (delivered, whole, prior) {
            (Delivered::Numbered(lines), true, _) => {
                let text: Vec<String> = lines.into_iter().map(|(_, line)| line).collect();
                Some(Known::of(joined(&text), false))
            }
            (Delivered::Plain(lines), true, _) => Some(Known::of(joined(&lines), false)),
            (Delivered::Numbered(lines), false, Some(known)) => known.splice(&lines),
            // A shell range names no line numbers to join by, so the model
            // keeps what it had and a later disagreement reads as a change.
            (Delivered::Plain(_), false, prior) => prior,
            (Delivered::Numbered(_), false, None) => None,
        };
        self.files.insert(slot, known);
    }
}

/// Count a call of the bucket by what the transcript proves about the member
/// that put it there, then fold what every member delivered into the model.
pub(super) fn note_the_working_tree(
    ctx: &mut Ctx<'_>,
    call: &Call,
    requests: &[ReadRequest],
    blocker: Option<usize>,
    bucket: Bucket,
    text: &str,
    tokens: u64,
) {
    let several = requests.len() > 1;
    let mut unchanged_key = None;
    if bucket == Bucket::NoLineCount {
        let request = blocker.and_then(|at| requests.get(at));
        let key = request.and_then(|request| path_key(&request.path, call.cwd.as_deref()));
        let (standing, size) = match (request, key.as_deref()) {
            (Some(request), Some(key)) => {
                let whole = !several && is_whole(call, request, text);
                let delivered = delivered(call, text);
                ctx.tree.working.standing(call, key, &delivered, whole)
            }
            _ => (Standing::FirstSight, None),
        };
        let size = size.unwrap_or_else(|| u64::try_from(text.len()).unwrap_or(u64::MAX));
        ctx.report.no_line_count_split.note(standing, size, tokens);
        if standing == Standing::Unchanged {
            unchanged_key = key;
            if several {
                ctx.report.uncommitted.note_left(Left::Several, tokens);
            }
        }
    }
    if text.contains("<tool_use_error>") || text.contains(PERSISTED_OUTPUT) {
        return;
    }
    for request in requests {
        let Some(key) = path_key(&request.path, call.cwd.as_deref()) else {
            continue;
        };
        let whole = !several && is_whole(call, request, text);
        let delivered = if several {
            Delivered::Plain(Vec::new())
        } else {
            delivered(call, text)
        };
        ctx.tree.working.note_delivery(call, &key, delivered, whole);
        if several {
            continue;
        }
        let unchanged = unchanged_key.as_deref() == Some(key.as_str());
        let at = Reread {
            call,
            request,
            key: &key,
            text,
            tokens,
            unchanged,
        };
        fold_the_read(ctx, &at);
    }
}

/// What a call delivered, as lines: the Read tool's numbered lines up to the
/// first line that is not one, or a shell call's output.
pub(super) fn delivered(call: &Call, text: &str) -> Delivered {
    if call.name != "Read" {
        return Delivered::Plain(text.lines().map(str::to_owned).collect());
    }
    let mut lines = Vec::new();
    for line in text.lines() {
        let lead = line.trim_start_matches(' ');
        let digits = lead.len() - lead.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let (number, rest) = lead.split_at(digits);
        let Ok(number) = number.parse::<usize>() else {
            break;
        };
        let Some(content) = rest
            .strip_prefix('\u{2192}')
            .or_else(|| rest.strip_prefix('\t'))
        else {
            break;
        };
        lines.push((number, content.to_owned()));
    }
    Delivered::Numbered(lines)
}

/// Whether one read delivered the whole file: a limit-less Read whose numbered
/// lines run from 1 and stop short of the default window, or a shell read of
/// the whole file whose output the client kept whole.
fn is_whole(call: &Call, request: &ReadRequest, text: &str) -> bool {
    if text.contains("truncated") || text.contains("[Truncated") {
        return false;
    }
    if call.name == "Read" {
        let bounded = call.input.get("offset").is_some() || call.input.get("limit").is_some();
        let Delivered::Numbered(lines) = delivered(call, text) else {
            return false;
        };
        let from_one = lines.iter().enumerate().all(|(at, (n, _))| *n == at + 1);
        return !bounded && from_one && !lines.is_empty() && lines.len() < READ_DEFAULT_WINDOW;
    }
    matches!(
        request.span,
        ReadSpan::Lines {
            lo: 1,
            hi_kind: HiKind::DefaultWindow(HiKind::END_OF_FILE)
        }
    )
}

/// An edit applied to known bytes, or `None` where a replacement's old text
/// is not in them.
fn applied(known: Known, call: &Call) -> Option<Known> {
    let input = &call.input;
    let one = [input.clone()];
    let edits = match call.name.as_str() {
        "MultiEdit" => input.get("edits")?.as_array()?.as_slice(),
        _ => one.as_slice(),
    };
    let mut text = known.text;
    for edit in edits {
        let old = edit.get("old_string")?.as_str()?;
        let new = edit.get("new_string")?.as_str()?;
        if old.is_empty() || !text.contains(old) {
            return None;
        }
        let every = edit.get("replace_all").and_then(Value::as_bool) == Some(true);
        text = if every {
            text.replace(old, new)
        } else {
            text.replacen(old, new, 1)
        };
    }
    Some(Known::of(text, true))
}

/// A write's content.
fn content(call: &Call) -> Option<&str> {
    call.input.get("content").and_then(Value::as_str)
}

/// Lines joined back into bytes, each terminated.
fn joined(lines: &[String]) -> String {
    let mut text = lines.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

/// A line without a `cat -n` or `nl` number in front of it.
fn strip_line_number(line: &str) -> &str {
    let lead = line.trim_start_matches(' ');
    let rest = lead.trim_start_matches(|c: char| c.is_ascii_digit());
    if rest.len() == lead.len() {
        return line;
    }
    rest.strip_prefix('\t').unwrap_or(line)
}

/// A 64-bit FNV-1a hash of a line, without a carriage return at its end.
fn line_hash(line: &str) -> u64 {
    line.trim_end_matches('\r')
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        })
}

/// The table's rows for the bucket's split.
pub(super) fn no_line_count_rows(report: &Report, denominator: u64) -> String {
    let split = &report.no_line_count_split;
    let mut out = String::from("\n  no name or line count, split by what the transcript proves\n");
    for (at, (_, label)) in CLASSES.iter().enumerate() {
        out.push_str(&row(label, split.class(at), denominator));
        let bands = split.cells.get(at).copied().unwrap_or_default();
        for ((_, _, band), tally) in BANDS.iter().zip(bands) {
            out.push_str(&row(band, tally, denominator));
        }
    }
    out.push_str(&uncommitted_rows(report, denominator));
    out
}

/// The same rows for the machine-readable document.
pub(super) fn no_line_count_json(report: &Report) -> Value {
    let split = &report.no_line_count_split;
    let mut map = Map::new();
    for (at, (key, _)) in CLASSES.iter().enumerate() {
        let total = split.class(at);
        let mut bands = Map::new();
        let cells = split.cells.get(at).copied().unwrap_or_default();
        for ((_, band, _), tally) in BANDS.iter().zip(cells) {
            bands.insert(
                (*band).to_owned(),
                json!({ "calls": tally.calls, "tokens": tally.tokens }),
            );
        }
        map.insert(
            (*key).to_owned(),
            json!({ "calls": total.calls, "tokens": total.tokens, "bands": bands }),
        );
    }
    map.insert("uncommitted_declines".to_owned(), uncommitted_json(report));
    Value::Object(map)
}

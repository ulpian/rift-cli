//! What this member's own window journals say.
//!
//! The journals are per-member run state, not a family store: everything here
//! reads the append files one checkout wrote and re-counts the bytes they
//! named. A read whose bytes have moved since is declared unmeasured, never
//! imputed.

mod compaction;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rift_agent::window_journal::{InheritClass, Interval, Line as JournalLine};

use super::panes::WindowPane;
use super::render::pad;

pub(super) use self::compaction::{render_compaction, Compaction};
use self::compaction::{session_of, CompactionFold, Storm};

/// How many turns after an eviction a read still counts as recovery.
///
/// Three: the turn the map arrives in, the turn that acts on it, and one
/// more. The first re-read after a compaction is deliberately allowed (the
/// window no longer holds the bytes), so the reading that follows an eviction
/// is recovery rather than repetition — and after three turns it is ordinary
/// work again, which is why the share is published with the number rather
/// than folded into a single "post-compaction" total.
const POST_COMPACTION_TURNS: u32 = 3;

/// The slice-length buckets the line floor is priced in: below the shipped
/// floor, and then the orders of magnitude above it.
const SLICE_LENGTH_BUCKETS: &[(&str, u32, u32)] = &[
    ("< 24 lines", 0, 23),
    ("24–99 lines", 24, 99),
    ("100–999 lines", 100, 999),
    ("≥ 1000 lines", 1000, u32::MAX),
];

/// The four slice-length buckets, all zero.
pub(super) fn empty_buckets() -> Vec<SliceBucket> {
    SLICE_LENGTH_BUCKETS
        .iter()
        .map(|(label, _, _)| SliceBucket {
            label: (*label).to_owned(),
            reads: 0,
            tokens: 0,
            unmeasured: 0,
        })
        .collect()
}

/// The bucket a slice of `lines` lines falls in.
fn bucket_of(lines: u32) -> usize {
    SLICE_LENGTH_BUCKETS
        .iter()
        .position(|(_, lo, hi)| lines >= *lo && lines <= *hi)
        .unwrap_or(0)
}

/// One slice-length bucket of the would-have-declined pool.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct SliceBucket {
    /// The bucket, as the report prints it.
    pub(super) label: String,
    /// Observed repeats whose slice falls in it.
    pub(super) reads: u64,
    /// Their measured tokens.
    pub(super) tokens: u64,
    /// Of those reads, the ones whose bytes could not be re-counted.
    pub(super) unmeasured: u64,
}

/// Denies of one frame kind, and how many were asked for again.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct RetryAfterDeny {
    /// The frame the decline carried.
    pub(super) frame: String,
    /// Declines of that shape.
    pub(super) denies: u64,
    /// Of those, the ones re-issued inside the same turn.
    pub(super) retried: u64,
}

/// One class of inherited line, as this member's journals hold it: how many
/// lines opening windows inherited, and the reads an inherited line alone
/// would have declined, which ran because the class is not enforced.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct InheritedPool {
    /// The class, as the journal spells it.
    pub(super) class: &'static str,
    /// Inherited lines written.
    pub(super) lines: u64,
    /// Reads they alone would have declined.
    pub(super) reads: u64,
    /// Those reads' measured tokens.
    pub(super) tokens: u64,
    /// Of those reads, the ones whose bytes could not be re-counted.
    pub(super) unmeasured: u64,
}

/// What this member's window journals say about the window that is open.
///
/// The journals are per-member run state, not a family store: this is what one
/// checkout's context windows did. Windows are not sessions — a subagent has
/// its own journal under its parent's session id — so both are counted and
/// neither is reported as the other.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(super) struct WindowJournals {
    /// The directory the rows came from.
    pub(super) dir: String,
    /// Why the numbers are absent, when they are.
    pub(super) note: Option<String>,
    /// Context windows with a journal file.
    pub(super) windows: u64,
    /// Lines across all of them.
    pub(super) lines: u64,
    /// Read-shaped calls recorded.
    pub(super) reads: u64,
    /// Their measured tokens.
    pub(super) read_tokens: u64,
    /// Reads whose bytes could not be re-counted under the identity the
    /// journal named them by.
    pub(super) reads_unmeasured: u64,
    /// Reads whose slice is past the cap, and so were never eligible for a
    /// decline at all.
    pub(super) reads_over_cap: u64,
    /// The cap they were judged against.
    pub(super) max_slice_bytes: u64,
    /// The line floor the pool below is judged against.
    pub(super) min_slice_lines: u32,
    /// Repeats the gate would have declined and did not.
    pub(super) pool_reads: u64,
    /// Their measured tokens.
    pub(super) pool_tokens: u64,
    /// Of those, the ones whose bytes could not be re-counted.
    pub(super) pool_unmeasured: u64,
    /// The pool by slice length — the distribution the floor is priced from.
    pub(super) buckets: Vec<SliceBucket>,
    /// Declines, by frame, with the repeats that followed them.
    pub(super) retries: Vec<RetryAfterDeny>,
    /// Consecutive reads of one path whose ranges abut.
    pub(super) adjacent_reads: u64,
    /// Evictions recorded.
    pub(super) evictions: u64,
    /// Read tokens inside [`POST_COMPACTION_TURNS`] of one.
    pub(super) post_compaction_tokens: u64,
    /// The same evictions per session, and the reading after each one.
    pub(super) compaction: Compaction,
}

impl WindowJournals {
    /// An empty result carrying the reason it is empty.
    fn unavailable(dir: String, note: String) -> Self {
        Self {
            dir,
            note: Some(note),
            windows: 0,
            lines: 0,
            reads: 0,
            read_tokens: 0,
            reads_unmeasured: 0,
            reads_over_cap: 0,
            max_slice_bytes: 0,
            min_slice_lines: 0,
            pool_reads: 0,
            pool_tokens: 0,
            pool_unmeasured: 0,
            buckets: Vec::new(),
            retries: Vec::new(),
            adjacent_reads: 0,
            evictions: 0,
            post_compaction_tokens: 0,
            compaction: Compaction::default(),
        }
    }
}

/// The slice a journal line named: the file, the revision it named the file
/// by, and the run of lines inside it.
///
/// Three fields that only ever travel together — a decline, a read and a
/// would-have-declined repeat are all judged by exactly this triple — so they
/// travel as one parameter rather than as three that could be passed in the
/// wrong order.
#[derive(Clone, Copy)]
struct Slice<'a> {
    path: &'a str,
    revision: &'a str,
    range: Interval,
}

/// The bytes a journal line named, re-counted under the identity it named them
/// by — or nothing.
///
/// The working file is the only source consulted, and only while it still
/// hashes to the revision the line recorded. A file edited since is NOT
/// re-counted under the new bytes: that would price a slice nobody read. Such
/// a read is counted and declared unmeasured, never imputed — the posture the
/// serving audit takes with the same problem.
struct SliceMeter {
    work_tree: PathBuf,
    /// `(path, revision)` → the file's text, or `None` once it is known not to
    /// hash to that revision any more.
    seen: BTreeMap<(String, String), Option<String>>,
}

impl SliceMeter {
    fn new(work_tree: &Path) -> Self {
        Self {
            work_tree: work_tree.to_path_buf(),
            seen: BTreeMap::new(),
        }
    }

    /// The text of `path` at `revision`, read at most once per pair.
    fn text(&mut self, path: &str, revision: &str) -> Option<&String> {
        let key = (path.to_owned(), revision.to_owned());
        if !self.seen.contains_key(&key) {
            let full = self.work_tree.join(path);
            let held = std::fs::read(&full).ok().and_then(|bytes| {
                (rift_agent::content_sha256(&bytes).to_hex() == revision)
                    .then(|| String::from_utf8_lossy(&bytes).into_owned())
            });
            self.seen.insert(key.clone(), held);
        }
        self.seen.get(&key).and_then(Option::as_ref)
    }

    /// `(bytes, tokens)` of the run of lines a line named, or `None` when the
    /// bytes are no longer the bytes it named.
    fn measure(&mut self, at: Slice<'_>) -> Option<(u64, u64)> {
        let text = self.text(at.path, at.revision)?;
        let slice: String = text
            .split_inclusive('\n')
            .skip(at.range.lo as usize - 1)
            .take(at.range.lines() as usize)
            .collect();
        Some((
            u64::try_from(slice.len()).unwrap_or(0),
            u64::from(rift_tokens::count(&slice)),
        ))
    }
}

/// Which frame a decline carried, worked out from the lines that came before
/// it — the same two facts the decision itself is made of.
///
/// A revision the window read is a plain decline; a revision it holds only by
/// having written it is a delta decline. Anything else is a decline this
/// reader cannot name, and it says so rather than guessing.
fn frame_before(lines: &[JournalLine], path: &str, revision: &str) -> &'static str {
    let mut frame = "unknown";
    for line in lines {
        match line {
            JournalLine::Read {
                path: p, hash: h, ..
            } if p == path && h.to_hex() == revision => frame = "plain",
            JournalLine::Edit {
                path: p,
                hash_new: h,
                ..
            } if p == path && h.to_hex() == revision && frame == "unknown" => frame = "delta",
            _ => {}
        }
    }
    frame
}

/// What a fold carries from one line of a window to the next.
///
/// Both are facts about the lines already walked, not about the line in hand:
/// range-walking is a claim about the previous read, and post-compaction
/// reading is a claim about the most recent eviction.
#[derive(Default)]
struct Rolling {
    /// The previous read's path and range.
    last_read: Option<(String, Interval)>,
    /// The turn the most recent eviction happened in.
    evicted_at: Option<u32>,
    /// The storms of this window's evictions, the most recent last.
    ///
    /// A read inside the turns of two evictions is credited to the most recent
    /// one only — the same eviction `evicted_at` names — so the storms
    /// partition the post-compaction total rather than overlap it.
    storms: Vec<Storm>,
}

impl Rolling {
    /// Whether a read in `turn` is recovery from the most recent eviction.
    fn recovering(&self, turn: u32) -> bool {
        self.evicted_at
            .is_some_and(|at| turn >= at && turn <= at.saturating_add(POST_COMPACTION_TURNS))
    }
}

/// Fold one window's lines into the running figures, and hand back the storm
/// of each of its evictions, in order.
fn fold_window(
    lines: &[JournalLine],
    meter: &mut SliceMeter,
    out: &mut WindowJournals,
) -> Vec<Storm> {
    let mut retries: BTreeMap<&'static str, (u64, u64)> = BTreeMap::new();
    let mut rolling = Rolling::default();
    for (i, line) in lines.iter().enumerate() {
        out.lines = out.lines.saturating_add(1);
        match line {
            JournalLine::Read {
                turn,
                path,
                hash,
                range,
                ..
            } => {
                let at = Slice {
                    path,
                    revision: &hash.to_hex(),
                    range: *range,
                };
                fold_read(*turn, at, meter, &mut rolling, out);
            }
            JournalLine::Observe {
                path,
                hash,
                range,
                via: None,
                ..
            } => fold_observe(
                Slice {
                    path,
                    revision: &hash.to_hex(),
                    range: *range,
                },
                meter,
                out,
            ),
            JournalLine::Deny {
                turn,
                path,
                hash,
                range,
                ..
            } => {
                let revision = hash.to_hex();
                let frame = frame_before(lines.get(..i).unwrap_or_default(), path, &revision);
                let entry = retries.entry(frame).or_insert((0, 0));
                entry.0 = entry.0.saturating_add(1);
                let at = Slice {
                    path,
                    revision: &revision,
                    range: *range,
                };
                if retried_in_turn(lines, i, *turn, at) {
                    entry.1 = entry.1.saturating_add(1);
                }
            }
            JournalLine::Evict { turn, reason, .. } => {
                out.evictions = out.evictions.saturating_add(1);
                rolling.evicted_at = Some(*turn);
                rolling.storms.push(Storm {
                    reason: reason.as_str(),
                    ..Storm::default()
                });
            }
            // Nothing else is a read, a decline or an emptying of this window's
            // own: announcements are counted where the savings are, and the
            // inherited classes by `inherited_pools`, never in this pool.
            _ => (),
        }
    }
    for (frame, (denies, retried)) in retries {
        merge_retry(frame, denies, retried, &mut out.retries);
    }
    rolling.storms
}

/// Count one read: its tokens, whether its slice was ever eligible for a
/// decline, whether it follows an eviction, and whether it walks on from the
/// read before it.
fn fold_read(
    turn: u32,
    at: Slice<'_>,
    meter: &mut SliceMeter,
    rolling: &mut Rolling,
    out: &mut WindowJournals,
) {
    out.reads = out.reads.saturating_add(1);
    let measured = meter.measure(at);
    match measured {
        Some((bytes, tokens)) => {
            out.read_tokens = out.read_tokens.saturating_add(tokens);
            if bytes > out.max_slice_bytes {
                out.reads_over_cap = out.reads_over_cap.saturating_add(1);
            }
            if rolling.recovering(turn) {
                out.post_compaction_tokens = out.post_compaction_tokens.saturating_add(tokens);
            }
        }
        None => out.reads_unmeasured = out.reads_unmeasured.saturating_add(1),
    }
    if rolling.recovering(turn) {
        credit_storm(
            rolling.storms.last_mut(),
            measured.map(|(_, tokens)| tokens),
        );
    }
    if rolling
        .last_read
        .as_ref()
        .is_some_and(|(p, prev)| p == at.path && at.range.lo == prev.hi.saturating_add(1))
    {
        out.adjacent_reads = out.adjacent_reads.saturating_add(1);
    }
    rolling.last_read = Some((at.path.to_owned(), at.range));
}

/// Credit one recovery read to the storm of the eviction it follows: its
/// tokens when it was measured, an unmeasured read when it was not.
fn credit_storm(storm: Option<&mut Storm>, tokens: Option<u64>) {
    let Some(storm) = storm else {
        return;
    };
    storm.reads = storm.reads.saturating_add(1);
    match tokens {
        Some(tokens) => storm.tokens = storm.tokens.saturating_add(tokens),
        None => storm.unmeasured = storm.unmeasured.saturating_add(1),
    }
}

/// Count one repeat the gate would have declined and did not, in the pool and
/// in the slice-length bucket the floor is priced from.
fn fold_observe(at: Slice<'_>, meter: &mut SliceMeter, out: &mut WindowJournals) {
    out.pool_reads = out.pool_reads.saturating_add(1);
    let measured = meter.measure(at);
    if let Some(slot) = out.buckets.get_mut(bucket_of(at.range.lines())) {
        slot.reads = slot.reads.saturating_add(1);
        match measured {
            Some((_, tokens)) => slot.tokens = slot.tokens.saturating_add(tokens),
            None => slot.unmeasured = slot.unmeasured.saturating_add(1),
        }
    }
    match measured {
        Some((_, tokens)) => out.pool_tokens = out.pool_tokens.saturating_add(tokens),
        None => out.pool_unmeasured = out.pool_unmeasured.saturating_add(1),
    }
}

/// Whether the repeat a decline bounds was asked for again: the same key,
/// inside the same turn. A later turn coming back to the file is ordinary work,
/// not a retry of a decline.
fn retried_in_turn(lines: &[JournalLine], denied_at: usize, turn: u32, at: Slice<'_>) -> bool {
    lines.iter().skip(denied_at + 1).any(|later| match later {
        JournalLine::Read {
            turn: t,
            path: p,
            hash: h,
            range: r,
            ..
        } => *t == turn && p == at.path && h.to_hex() == at.revision && *r == at.range,
        _ => false,
    })
}

/// Add one frame's declines to a set of rows, opening a row when the frame is
/// new to it.
fn merge_retry(frame: &'static str, denies: u64, retried: u64, into: &mut Vec<RetryAfterDeny>) {
    match into.iter_mut().find(|r| r.frame == frame) {
        Some(row) => {
            row.denies = row.denies.saturating_add(denies);
            row.retried = row.retried.saturating_add(retried);
        }
        None => into.push(RetryAfterDeny {
            frame: frame.to_owned(),
            denies,
            retried,
        }),
    }
}

/// Read every window journal this member holds and fold them into one account.
///
/// Only the append files are read — the live one and the rolled ones. A
/// snapshot is a fold of rolled files the decision path uses to answer
/// quickly; reading the rows themselves is what a report wants, and it cannot
/// double-count a row it never reads twice.
pub(super) fn window_journals(
    work_tree: &Path,
    max_slice_bytes: u64,
    min_slice_lines: u32,
) -> WindowJournals {
    let dir = crate::window_journal::journal_dir(work_tree);
    let shown = dir.display().to_string();
    // A directory this member cannot read is a NOTE with the reason, not a
    // silent zero: an empty pool and an unreadable one are different facts.
    if let Err(e) = std::fs::read_dir(&dir) {
        return WindowJournals::unavailable(shown, e.to_string());
    }
    let mut out = WindowJournals::unavailable(shown, String::new());
    out.note = None;
    out.max_slice_bytes = max_slice_bytes;
    out.min_slice_lines = min_slice_lines;
    out.buckets = empty_buckets();
    let mut meter = SliceMeter::new(work_tree);
    let mut compaction = CompactionFold::default();
    for (key, lines) in member_windows(&dir) {
        out.windows = out.windows.saturating_add(1);
        let storms = fold_window(&lines, &mut meter, &mut out);
        compaction.add_window(session_of(work_tree, &key), storms);
    }
    out.compaction = compaction.finish();
    out
}

/// Every context window this member holds, as its key and its lines in file
/// order.
///
/// One window is one key however many files it has rolled through, and only
/// the append files are read: a snapshot is a fold of rows this pass reads for
/// itself, so a pass that read both would count them twice.
fn member_windows(dir: &Path) -> Vec<(String, Vec<JournalLine>)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut keyed: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if Path::new(&name).extension().is_none_or(|e| e != "jsonl")
            || crate::window_journal::is_call_sidecar(&name)
        {
            continue;
        }
        let Some(key) = name.split('.').next() else {
            continue;
        };
        keyed.entry(key.to_owned()).or_default().push(entry.path());
    }
    let mut out = Vec::with_capacity(keyed.len());
    for (key, mut files) in keyed {
        files.sort();
        let mut lines: Vec<JournalLine> = Vec::new();
        for file in &files {
            if let Ok(text) = std::fs::read_to_string(file) {
                lines.extend(
                    rift_agent::window_journal::Journal::parse(&text)
                        .lines()
                        .to_vec(),
                );
            }
        }
        out.push((key, lines));
    }
    out
}

/// Each class of inherited line's pool across this member's windows, every
/// class listed whether or not it holds anything, in precedence order.
///
/// Kept apart from [`WindowJournals`]' own would-have-declined pool, which is
/// a figure about the lines a window was handed: a read only an inherited
/// line would have declined is a different lever's pool, and adding it to the
/// first would price a class nobody enforces as if it were the read lever's.
pub(super) fn inherited_pools(work_tree: &Path) -> Vec<InheritedPool> {
    let mut pools: Vec<InheritedPool> = InheritClass::ALL
        .iter()
        .map(|class| InheritedPool {
            class: class.as_str(),
            lines: 0,
            reads: 0,
            tokens: 0,
            unmeasured: 0,
        })
        .collect();
    let mut meter = SliceMeter::new(work_tree);
    for (_, lines) in member_windows(&crate::window_journal::journal_dir(work_tree)) {
        for line in &lines {
            fold_inherited(line, &mut meter, &mut pools);
        }
    }
    pools
}

/// Count one line into the pool of the class it is about, if it is about one.
fn fold_inherited(line: &JournalLine, meter: &mut SliceMeter, pools: &mut [InheritedPool]) {
    let pool_of = |pools: &mut [InheritedPool], class: InheritClass| {
        pools
            .iter_mut()
            .position(|pool| pool.class == class.as_str())
    };
    match line {
        JournalLine::Inherit { class, .. } => {
            if let Some(pool) = pool_of(pools, *class).and_then(|at| pools.get_mut(at)) {
                pool.lines = pool.lines.saturating_add(1);
            }
        }
        JournalLine::Observe {
            path,
            hash,
            range,
            via: Some(class),
            ..
        } => {
            let Some(pool) = pool_of(pools, *class).and_then(|at| pools.get_mut(at)) else {
                return;
            };
            pool.reads = pool.reads.saturating_add(1);
            let revision = hash.to_hex();
            let at = Slice {
                path,
                revision: &revision,
                range: *range,
            };
            match meter.measure(at) {
                Some((_, tokens)) => pool.tokens = pool.tokens.saturating_add(tokens),
                None => pool.unmeasured = pool.unmeasured.saturating_add(1),
            }
        }
        _ => {}
    }
}

/// The inherited classes' own lines, one per class, under a heading that
/// says no decline ever rested on them.
pub(super) fn render_inherited(pools: &[InheritedPool]) -> String {
    let mut out = String::from(
        "inherited lines (observed; nothing is declined on them):
",
    );
    for pool in pools {
        out.push_str(&format!(
            "  {}{} lines, {} reads would have been declined, {} tok ({} unmeasured)\n",
            pad(pool.class, 10),
            pool.lines,
            pool.reads,
            pool.tokens,
            pool.unmeasured
        ));
    }
    out
}

/// The window journals of the member `repo` is, judged against the caps this
/// repository's own policy resolves for Claude Code.
pub(super) fn member_window_journals(repo: &Path) -> WindowJournals {
    let policy = rift_recall_policy::ServePolicy::load(&repo.join(".rift"));
    let agent = rift_core::AgentId::CLAUDE_CODE;
    let effective = rift_recall_policy::resolve(
        &policy,
        agent.as_str(),
        crate::recall::family_capability(&agent),
    );
    window_journals(repo, effective.max_slice_bytes, effective.min_slice_lines)
}

/// Render the window-journal block (golden-locked).
pub(super) fn render_window_journals(w: &WindowJournals) -> String {
    let mut out = String::new();
    out.push_str("window journals (this member's own run state):\n");
    out.push_str(&format!("  directory:             {}\n", w.dir));
    if let Some(note) = &w.note {
        out.push_str(&format!("  not measured:          {note}\n"));
        return out;
    }
    out.push_str(&format!("  windows:               {}\n", w.windows));
    out.push_str(&format!("  lines:                 {}\n", w.lines));
    out.push_str(&format!(
        "  reads:                 {} ({} tok, {} unmeasured)\n",
        w.reads, w.read_tokens, w.reads_unmeasured
    ));
    out.push_str(&format!(
        "  past the {} B cap:  {} (never eligible for a decline)\n",
        w.max_slice_bytes, w.reads_over_cap
    ));
    out.push_str(&format!(
        "  would-have-declined:   {} reads, {} tok ({} unmeasured), floor {} lines\n",
        w.pool_reads, w.pool_tokens, w.pool_unmeasured, w.min_slice_lines
    ));
    out.push_str(&format!(
        "  {}{:>8}{:>12}{:>12}\n",
        pad("by slice length", 20),
        "reads",
        "tokens",
        "unmeasured"
    ));
    for b in &w.buckets {
        out.push_str(&format!(
            "    {}{:>8}{:>12}{:>12}\n",
            pad(&b.label, 18),
            b.reads,
            b.tokens,
            b.unmeasured
        ));
    }
    out.push_str(&format!(
        "  {}{:>8}{:>12}\n",
        pad("retry after decline", 22),
        "declines",
        "retried"
    ));
    if w.retries.is_empty() {
        out.push_str("    (no decline was recorded in this window)\n");
    }
    for r in &w.retries {
        let share = if r.denies == 0 {
            0.0
        } else {
            (r.retried as f64) * 100.0 / (r.denies as f64)
        };
        out.push_str(&format!(
            "    {}{:>8}{:>12}  {share:.1}%\n",
            pad(&r.frame, 20),
            r.denies,
            r.retried
        ));
    }
    out.push_str(&format!("  range-walking:         {}\n", w.adjacent_reads));
    let share = if w.read_tokens == 0 {
        0.0
    } else {
        (w.post_compaction_tokens as f64) * 100.0 / (w.read_tokens as f64)
    };
    out.push_str(&format!("  evictions:             {}\n", w.evictions));
    out.push_str(&format!(
        "  read within {POST_COMPACTION_TURNS} turns of one: {} of {} tok ({share:.1}%)\n",
        w.post_compaction_tokens, w.read_tokens
    ));
    out
}
/// When a journal line was written, in epoch milliseconds.
fn line_at_ms(line: &JournalLine) -> i64 {
    let ms = match line {
        JournalLine::Read { at_ms, .. }
        | JournalLine::Deny { at_ms, .. }
        | JournalLine::Observe { at_ms, .. }
        | JournalLine::Outline { at_ms, .. }
        | JournalLine::Expect { at_ms, .. }
        | JournalLine::Edit { at_ms, .. }
        | JournalLine::Hold { at_ms, .. }
        | JournalLine::Prevented { at_ms, .. }
        | JournalLine::Evict { at_ms, .. }
        | JournalLine::Inherit { at_ms, .. }
        | JournalLine::Spawn { at_ms, .. }
        | JournalLine::Claim { at_ms, .. }
        | JournalLine::Family { at_ms, .. }
        | JournalLine::Matched { at_ms, .. }
        | JournalLine::Call { at_ms, .. } => *at_ms,
    };
    i64::try_from(ms).unwrap_or(i64::MAX)
}

/// Fold this member's journal lines into the two windows.
///
/// A decline's frame and its retry are decided over the WHOLE window's lines,
/// not the half the cut left it: a decline is judged by everything its window
/// had read before it, whichever side of the boundary that reading was on.
pub(super) fn fold_split_journals(
    work_tree: &Path,
    boundary_ms: Option<i64>,
    observe: &mut WindowPane,
    enforce: &mut WindowPane,
) {
    let cut = boundary_ms.unwrap_or(i64::MAX);
    let mut meter = SliceMeter::new(work_tree);
    for (_, lines) in member_windows(&crate::window_journal::journal_dir(work_tree)) {
        let mut retries: BTreeMap<(bool, &'static str), (u64, u64)> = BTreeMap::new();
        let (mut in_observe, mut in_enforce) = (false, false);
        for (i, line) in lines.iter().enumerate() {
            let enforced = line_at_ms(line) >= cut;
            if enforced {
                in_enforce = true;
            } else {
                in_observe = true;
            }
            let pane: &mut WindowPane = if enforced {
                &mut *enforce
            } else {
                &mut *observe
            };
            if let Some((frame, retried)) = split_line(&lines, i, pane, &mut meter) {
                let entry = retries.entry((enforced, frame)).or_insert((0, 0));
                entry.0 = entry.0.saturating_add(1);
                if retried {
                    entry.1 = entry.1.saturating_add(1);
                }
            }
        }
        if in_observe {
            observe.windows = observe.windows.saturating_add(1);
        }
        if in_enforce {
            enforce.windows = enforce.windows.saturating_add(1);
        }
        for ((enforced, frame), (denies, retried)) in retries {
            let pane: &mut WindowPane = if enforced {
                &mut *enforce
            } else {
                &mut *observe
            };
            merge_retry(frame, denies, retried, &mut pane.retries);
        }
    }
}

/// Fold one line into the pane its instant places it in, and report the
/// decline it was, if it was one: the frame it carried and whether the repeat
/// it bounds was asked for again.
///
/// A decline's frame and its retry are read from the WHOLE window's lines, not
/// the half the cut left it — which is why this takes the window and an index
/// into it rather than the line alone.
fn split_line(
    lines: &[JournalLine],
    at: usize,
    pane: &mut WindowPane,
    meter: &mut SliceMeter,
) -> Option<(&'static str, bool)> {
    let line = lines.get(at)?;
    pane.lines = pane.lines.saturating_add(1);
    match line {
        JournalLine::Read { .. } => {
            pane.reads = pane.reads.saturating_add(1);
            None
        }
        JournalLine::Observe { via: None, .. } => {
            pane.observed = pane.observed.saturating_add(1);
            None
        }
        JournalLine::Deny {
            turn,
            path,
            hash,
            range,
            ..
        } => {
            pane.declined = pane.declined.saturating_add(1);
            let revision = hash.to_hex();
            let slice = Slice {
                path,
                revision: &revision,
                range: *range,
            };
            price_declined_slice(pane, meter, slice);
            let frame = frame_before(lines.get(..at).unwrap_or_default(), path, &revision);
            let retried = retried_in_turn(lines, at, *turn, slice);
            Some((frame, retried))
        }
        JournalLine::Expect { .. }
        | JournalLine::Edit { .. }
        | JournalLine::Evict { .. }
        | JournalLine::Hold { .. }
        | JournalLine::Prevented { .. }
        | JournalLine::Observe { via: Some(_), .. }
        | JournalLine::Outline { .. }
        | JournalLine::Inherit { .. }
        | JournalLine::Spawn { .. }
        | JournalLine::Claim { .. }
        | JournalLine::Family { .. }
        | JournalLine::Matched { .. }
        | JournalLine::Call { .. } => None,
    }
}

/// Price one declined slice into the pane's length buckets: under enforce the
/// would-have-declined pool no longer exists, so this is the distribution the
/// line floor is evaluated against in its place.
fn price_declined_slice(pane: &mut WindowPane, meter: &mut SliceMeter, at: Slice<'_>) {
    let measured = meter.measure(at);
    let Some(slot) = pane.declined_buckets.get_mut(bucket_of(at.range.lines())) else {
        return;
    };
    slot.reads = slot.reads.saturating_add(1);
    match measured {
        Some((_, tokens)) => slot.tokens = slot.tokens.saturating_add(tokens),
        None => slot.unmeasured = slot.unmeasured.saturating_add(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A work tree with one file of `lines` numbered lines, and its revision.
    pub(super) fn seeded_tree(lines: usize) -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().unwrap();
        let body = (1..=lines).fold(String::new(), |mut acc, n| {
            use core::fmt::Write as _;
            let _ = writeln!(acc, "line {n}");
            acc
        });
        std::fs::write(tmp.path().join("a.txt"), &body).unwrap();
        let revision = rift_agent::content_sha256(body.as_bytes()).to_hex();
        (tmp, revision)
    }

    /// Write one window's journal file.
    pub(super) fn window_file(root: &Path, key: &str, lines: &[JournalLine]) {
        let dir = root.join(".rift").join("state").join("reads");
        std::fs::create_dir_all(&dir).unwrap();
        let body = lines.iter().fold(String::new(), |mut acc, l| {
            use core::fmt::Write as _;
            let _ = writeln!(acc, "{}", l.to_json_line());
            acc
        });
        std::fs::write(dir.join(format!("{key}.jsonl")), body).unwrap();
    }

    pub(super) fn revision_id(hex: &str) -> rift_core::Sha256Id {
        rift_core::Sha256Id::parse_hex(hex).unwrap()
    }

    pub(super) fn read_line(turn: u32, hash: &str, lo: u32, hi: u32) -> JournalLine {
        JournalLine::Read {
            turn,
            path: "a.txt".to_owned(),
            hash: revision_id(hash),
            range: Interval::new(lo, hi).unwrap(),
            total_lines: 200,
            at_ms: 1,
        }
    }

    fn observe_line(turn: u32, hash: &str, lo: u32, hi: u32) -> JournalLine {
        JournalLine::Observe {
            turn,
            path: "a.txt".to_owned(),
            hash: revision_id(hash),
            range: Interval::new(lo, hi).unwrap(),
            at_ms: 2,
            via: None,
        }
    }

    /// ★ The pool the floor is priced from: every repeat the gate would have
    /// declined, counted in reads AND in tokens, distributed by the length of
    /// the slice it was about. Without the distribution the floor is a number
    /// nobody can move on evidence.
    #[test]
    fn the_would_have_declined_pool_is_counted_by_slice_length() {
        let (tmp, rev) = seeded_tree(200);
        window_file(
            tmp.path(),
            "w1",
            &[
                read_line(1, &rev, 1, 200),
                observe_line(2, &rev, 1, 10),
                observe_line(3, &rev, 1, 50),
                observe_line(4, &rev, 1, 150),
            ],
        );
        let w = window_journals(tmp.path(), 98_304, 24);
        assert_eq!(w.pool_reads, 3);
        assert!(w.pool_tokens > 0, "the pool is priced, not just counted");
        assert_eq!(w.pool_unmeasured, 0);
        let by_label: Vec<(String, u64)> = w
            .buckets
            .iter()
            .map(|b| (b.label.clone(), b.reads))
            .collect();
        assert_eq!(
            by_label,
            vec![
                ("< 24 lines".to_owned(), 1),
                ("24–99 lines".to_owned(), 1),
                ("100–999 lines".to_owned(), 1),
                ("≥ 1000 lines".to_owned(), 0),
            ]
        );
        let priced: u64 = w.buckets.iter().map(|b| b.tokens).sum();
        assert_eq!(priced, w.pool_tokens, "the buckets partition the pool");
    }

    /// ★ A slice whose bytes are no longer the bytes the journal named is
    /// declared unmeasured, never re-counted under the new bytes: pricing a
    /// slice nobody read is worse than admitting the count is missing.
    #[test]
    fn a_slice_whose_bytes_moved_is_declared_unmeasured_never_imputed() {
        let (tmp, rev) = seeded_tree(40);
        window_file(
            tmp.path(),
            "w1",
            &[read_line(1, &rev, 1, 40), observe_line(2, &rev, 1, 40)],
        );
        std::fs::write(tmp.path().join("a.txt"), "everything is different now\n").unwrap();
        let w = window_journals(tmp.path(), 98_304, 24);
        assert_eq!((w.reads, w.read_tokens, w.reads_unmeasured), (1, 0, 1));
        assert_eq!((w.pool_reads, w.pool_tokens, w.pool_unmeasured), (1, 0, 1));
    }

    /// A decline is answered by the same call inside the same turn or it is
    /// not answered at all, and each frame is reported on its own: the plain
    /// frame and the delta frame are different claims about what the window
    /// holds, so one retry rate over both would hide which one the model
    /// disbelieves.
    #[test]
    fn a_retry_is_counted_inside_the_turn_and_against_its_own_frame() {
        let (tmp, rev) = seeded_tree(200);
        let denied = |turn: u32, lo: u32, hi: u32| JournalLine::Deny {
            turn,
            path: "a.txt".to_owned(),
            hash: revision_id(&rev),
            range: Interval::new(lo, hi).unwrap(),
            at_ms: 3,
        };
        window_file(
            tmp.path(),
            "w1",
            &[
                // Read, then declined, then asked for again in the same turn.
                read_line(1, &rev, 1, 100),
                denied(2, 1, 100),
                read_line(2, &rev, 1, 100),
                // Held by having written it: a delta decline, not retried.
                JournalLine::Edit {
                    turn: 3,
                    path: "b.txt".to_owned(),
                    hash_old: None,
                    hash_new: revision_id(&rev),
                    sites: vec![Interval::new(4, 6).unwrap()],
                    shift: 0,
                    full: true,
                    at_ms: 4,
                },
                JournalLine::Deny {
                    turn: 4,
                    path: "b.txt".to_owned(),
                    hash: revision_id(&rev),
                    range: Interval::new(1, 100).unwrap(),
                    at_ms: 5,
                },
            ],
        );
        let w = window_journals(tmp.path(), 98_304, 24);
        let mut rows: Vec<(String, u64, u64)> = w
            .retries
            .iter()
            .map(|r| (r.frame.clone(), r.denies, r.retried))
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            vec![("delta".to_owned(), 1, 0), ("plain".to_owned(), 1, 1),]
        );
    }

    /// Two signals a summary rung would answer: reading a file a page at a
    /// time, and the reading that follows a compaction.
    #[test]
    fn range_walking_and_post_eviction_reading_are_both_counted() {
        let (tmp, rev) = seeded_tree(200);
        window_file(
            tmp.path(),
            "w1",
            &[
                read_line(1, &rev, 1, 50),
                read_line(1, &rev, 51, 100),
                read_line(1, &rev, 101, 150),
                JournalLine::Evict {
                    turn: 2,
                    reason: rift_agent::window_journal::EvictReason::Compact,
                    at_ms: 6,
                },
                read_line(3, &rev, 1, 50),
                read_line(9, &rev, 120, 170),
            ],
        );
        let w = window_journals(tmp.path(), 98_304, 24);
        assert_eq!(w.adjacent_reads, 2, "two abutting pairs, not three reads");
        assert_eq!(w.evictions, 1);
        assert!(
            w.post_compaction_tokens > 0 && w.post_compaction_tokens < w.read_tokens,
            "the turn three lines away counts; the one seven turns away does not"
        );
    }

    /// Windows are counted as windows. A subagent has a journal of its own
    /// under its parent's session id, so a count of windows is not a count of
    /// sessions and this report never prints one as the other.
    #[test]
    fn windows_are_counted_per_journal_and_rolled_files_are_one_window() {
        let (tmp, rev) = seeded_tree(200);
        window_file(tmp.path(), "w1", &[read_line(1, &rev, 1, 10)]);
        window_file(tmp.path(), "w2", &[read_line(1, &rev, 1, 10)]);
        let dir = tmp.path().join(".rift").join("state").join("reads");
        std::fs::write(
            dir.join("w1.1.rolled.jsonl"),
            format!("{}\n", read_line(1, &rev, 20, 30).to_json_line()),
        )
        .unwrap();
        // A snapshot is a fold of rows this pass reads for itself: reading it
        // too would count them twice.
        std::fs::write(dir.join("w1.1.snapshot.json"), "{}").unwrap();
        // A call sidecar is the window's correlation facts, never its reads.
        std::fs::write(
            dir.join("w1.calls.jsonl"),
            format!("{}\n", read_line(1, &rev, 40, 50).to_json_line()),
        )
        .unwrap();
        let w = window_journals(tmp.path(), 98_304, 24);
        assert_eq!((w.windows, w.reads), (2, 3));
    }

    /// ★ Under enforce the would-have-declined pool no longer exists, so the
    /// floor is priced from the DECLINED slices instead — each in the window
    /// its decline was booked in, with the raw retry that followed it.
    #[test]
    fn a_declined_slice_is_priced_by_length_in_the_window_it_was_booked_in() {
        let (tmp, rev) = seeded_tree(200);
        let deny = |turn: u32, lo: u32, hi: u32, at_ms: u64| JournalLine::Deny {
            turn,
            path: "a.txt".to_owned(),
            hash: revision_id(&rev),
            range: Interval::new(lo, hi).unwrap(),
            at_ms,
        };
        let mut first = read_line(1, &rev, 1, 40);
        if let JournalLine::Read { at_ms, .. } = &mut first {
            *at_ms = 10;
        }
        let mut retry = read_line(1, &rev, 1, 40);
        if let JournalLine::Read { at_ms, .. } = &mut retry {
            *at_ms = 30;
        }
        window_file(
            tmp.path(),
            "w1",
            &[first, deny(1, 1, 40, 20), retry, deny(4, 1, 150, 90)],
        );
        let (mut observe, mut enforce) = (WindowPane::new(), WindowPane::new());
        fold_split_journals(tmp.path(), Some(50), &mut observe, &mut enforce);

        assert_eq!((observe.windows, enforce.windows), (1, 1), "it straddles");
        assert_eq!((observe.reads, observe.declined), (2, 1));
        assert_eq!((enforce.reads, enforce.declined), (0, 1));
        assert_eq!(
            observe.declined_buckets.get(1).map(|b| b.reads),
            Some(1),
            "a 40-line decline is priced in the 24-99 bucket, before the cut"
        );
        assert_eq!(
            enforce.declined_buckets.get(2).map(|b| b.reads),
            Some(1),
            "a 150-line decline is priced in the 100-999 bucket, after it"
        );
        assert!(observe
            .declined_buckets
            .get(1)
            .is_some_and(|b| b.tokens > 0));
        let before = observe.retries.first().unwrap();
        assert_eq!(
            (before.frame.as_str(), before.denies, before.retried),
            ("plain", 1, 1)
        );
        let after = enforce.retries.first().unwrap();
        assert_eq!(
            (after.frame.as_str(), after.denies, after.retried),
            ("plain", 1, 0),
            "the frame is read from the whole window, the count lands in its half"
        );
    }
}

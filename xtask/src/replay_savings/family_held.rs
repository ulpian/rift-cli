//! The family-held rows: the first sights another live window of the same
//! session tree held at that revision, split by the route a decline would
//! have carried, net of the frame it would have sent and of the window's own
//! later reads of the same bytes — the follow-ups every such decline is
//! charged with.
//!
//! A module of its own because it asks a question no bucket does — what a
//! lever that is still observing would be worth — and every figure it adds is
//! on rows of its own, so every row before it reads the same with or without
//! it. Liveness is the gate's, restated on transcript time: a session's own
//! window is live while the tree is being worked in, and a subagent's while
//! its journal was written within the idle-sweep floor of the read. The
//! outline is bound by then: the worker binds one at every read of an
//! outlinable file, whatever its size, and the holder's read came first.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use rift_agent::window_journal::{held_runs, IntervalSet, Journal, Line, ResolvedRead};
use rift_core::Sha256Id;
use rift_recall_validity::trust::{
    render_held_lines_frame, render_held_outline_frame, OUTLINE_FRAME_MIN_PAGE_LINES,
};
use serde_json::{json, Value};

use super::family_held_uncommitted::UncommittedFamilyHeld;
use super::first_contact::{with_outline_facts, OutlineFacts};
use super::history::unix_seconds;
use super::render::{commas, row};
use super::report::{Report, Tally};

/// The family's idle-sweep floor as shipped, in seconds: past it a quiet
/// subagent's window is not live.
const LIVE_FLOOR_SECONDS: i64 = 300;

/// The routes, in the order the table prints them.
pub(super) const ROUTES: [(&str, &str); 3] = [
    ("outline", "(a) outline bound by the holder's read"),
    ("held", "(i) held lines, no outline yet"),
    ("runs", "(c) no cheaper route: the read runs"),
];

/// What one session tree's replay saw that the rows are folded from.
#[derive(Default)]
pub(super) struct FamilyLog {
    /// Whether each window is a subagent's.
    subagent: Vec<bool>,
    /// The stamp each window's journal line was written at, line for line.
    stamps: Vec<Vec<String>>,
    /// First sights a family decline may have answered, in replay order.
    candidates: Vec<Candidate>,
    /// Every read that ran, for the follow-ups.
    reads: Vec<RanRead>,
    /// Whether a window's whole-file write or edit holds the bytes it wrote,
    /// as a read does: true for the journal of files no commit holds, whose
    /// bytes a window often put there itself.
    writes_hold: bool,
}

/// A first sight a family decline may have answered.
struct Candidate {
    window: usize,
    stamp: String,
    read: ResolvedRead,
    tokens: u64,
    facts: Option<Arc<OutlineFacts>>,
}

/// A read that ran.
struct RanRead {
    window: usize,
    stamp: String,
    path: String,
    hash: Sha256Id,
    tokens: u64,
}

/// One first sight another live window held, with what each route would
/// have cost.
#[derive(Clone, Debug)]
pub(super) struct Case {
    tokens: u64,
    outline_frame: Option<u64>,
    held_frame: Option<u64>,
    follow_ups: Tally,
}

/// What the replay saw of family-held first sights, folded at render time.
#[derive(Clone, Debug, Default)]
pub struct FamilyHeldPool {
    pub(super) cases: Vec<Case>,
}

/// One route's figures.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct RouteFigures {
    pub(super) pool: Tally,
    pub(super) frames: u64,
    pub(super) frame_max: u64,
    pub(super) follow_ups: Tally,
}

/// Why a first sight is no family-held case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NoCase {
    /// A mid-file range, or a first page under the outline floor.
    Shape,
    /// Windows held the bytes, none of them live.
    QuietHolder,
    /// No other window's journal holds all the lines asked for.
    NoHolder,
}

/// A call, named by its window, its stamp and its path.
pub(super) type CallKey = (usize, String, String);

impl FamilyLog {
    /// A log for a tree whose windows are the files at `paths`, with their
    /// spawning windows where the client recorded one.
    pub(super) fn new(paths: &[&Path], parents: &[Option<usize>]) -> Self {
        let subagent = paths
            .iter()
            .zip(parents)
            .map(|(path, parent)| parent.is_some() || is_subagent_file(path))
            .collect();
        Self {
            subagent,
            stamps: vec![Vec::new(); paths.len()],
            ..Self::default()
        }
    }

    /// A log over the journals of files no commit holds, where a window's own
    /// write is as good a hold of the bytes as a read of them.
    pub(super) fn for_uncommitted(paths: &[&Path], parents: &[Option<usize>]) -> Self {
        Self {
            writes_hold: true,
            ..Self::new(paths, parents)
        }
    }

    /// The lines `window`'s journal holds now were all written by `stamp` at
    /// the latest: the ones the last event added are stamped with it.
    pub(super) fn note_journal_lines(&mut self, window: usize, lines: usize, stamp: &str) {
        if let Some(stamps) = self.stamps.get_mut(window) {
            stamps.resize(lines.max(stamps.len()), stamp.to_owned());
        }
    }

    /// A read that ran.
    pub(super) fn note_ran_read(
        &mut self,
        window: usize,
        stamp: &str,
        read: &ResolvedRead,
        tokens: u64,
    ) {
        self.reads.push(RanRead {
            window,
            stamp: stamp.to_owned(),
            path: read.path.clone(),
            hash: read.hash,
            tokens,
        });
    }

    /// A first sight of a one-read call.
    pub(super) fn note_family_candidate(
        &mut self,
        window: usize,
        stamp: &str,
        read: &ResolvedRead,
        tokens: u64,
        facts: Option<Arc<OutlineFacts>>,
    ) {
        self.candidates.push(Candidate {
            window,
            stamp: stamp.to_owned(),
            read: read.clone(),
            tokens,
            facts,
        });
    }
}

/// Fold one tree's log into the pool, over the windows' journals as the tree
/// left them.
pub(super) fn fold_family(log: &FamilyLog, journals: &[&Journal], pool: &mut FamilyHeldPool) {
    for candidate in &log.candidates {
        if let Some(case) = case_of(log, journals, candidate) {
            pool.cases.push(case);
        }
    }
}

/// The case a candidate is, or `None` when no family decline could have
/// answered it: not a whole-file or long-enough first-page read, or no other
/// live window held the revision.
fn case_of(log: &FamilyLog, journals: &[&Journal], candidate: &Candidate) -> Option<Case> {
    let read = &candidate.read;
    let whole = read.range.hi >= read.total_lines;
    if !has_case_shape(read) {
        return None;
    }
    let holders = live_holders(log, journals, candidate);
    if holders.is_empty() {
        return None;
    }
    let outline_frame = candidate.facts.as_ref().and_then(|facts| {
        with_outline_facts(read, facts, render_held_outline_frame)
            .map(|(frame, _)| u64::from(rift_tokens::count(&frame)))
    });
    let held_frame = held_runs(read, &holders).and_then(|runs| {
        let pairs: Vec<(u32, u32)> = runs.iter().map(|run| (run.lo, run.hi)).collect();
        let page_end = (!whole).then_some(read.range.hi);
        render_held_lines_frame(&read.path, &read.hash.to_hex(), &pairs, page_end)
            .map(|frame| u64::from(rift_tokens::count(&frame)))
    });
    let mut follow_ups = Tally::default();
    for later in &log.reads {
        let same =
            later.window == candidate.window && later.path == read.path && later.hash == read.hash;
        if same && later.stamp.as_str() > candidate.stamp.as_str() {
            follow_ups.add(later.tokens);
        }
    }
    Some(Case {
        tokens: candidate.tokens,
        outline_frame,
        held_frame,
        follow_ups,
    })
}

/// Whether a read is shaped for a family decline: a whole file, or a first
/// page long enough to be worth an outline.
fn has_case_shape(read: &ResolvedRead) -> bool {
    let whole = read.range.hi >= read.total_lines;
    read.range.lo == 1 && (whole || read.range.lines() >= OUTLINE_FRAME_MIN_PAGE_LINES)
}

/// What one other window had written, and when it last wrote, strictly
/// before a candidate was read, where it had touched the candidate's revision.
struct Prior<'a> {
    lines: &'a [Line],
    last: Option<&'a String>,
}

fn prior_touching<'a>(
    log: &'a FamilyLog,
    journals: &[&'a Journal],
    candidate: &Candidate,
    window: usize,
) -> Option<Prior<'a>> {
    let read = &candidate.read;
    let stamps = log
        .stamps
        .get(window)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let before = stamps
        .iter()
        .take_while(|stamp| stamp.as_str() < candidate.stamp.as_str())
        .count();
    let lines = journals.get(window)?.lines().get(..before)?;
    let touches = lines.iter().any(|line| match line {
        Line::Read { path, hash, .. } => *path == read.path && *hash == read.hash,
        Line::Edit { path, hash_new, .. } => {
            log.writes_hold && *path == read.path && *hash_new == read.hash
        }
        _ => false,
    });
    touches.then(|| Prior {
        lines,
        last: stamps.get(before.wrapping_sub(1)),
    })
}

/// What the other live windows of the tree held of the candidate's revision
/// when it was read: each window's lines written strictly before the read.
fn live_holders(log: &FamilyLog, journals: &[&Journal], candidate: &Candidate) -> IntervalSet {
    let read = &candidate.read;
    let mut held = IntervalSet::new();
    for window in (0..journals.len()).filter(|window| *window != candidate.window) {
        let Some(prior) = prior_touching(log, journals, candidate, window) else {
            continue;
        };
        if !is_holder_live(log, window, prior.last, &candidate.stamp) {
            continue;
        }
        for run in Journal::from_lines(prior.lines.to_vec())
            .coverage(&read.path, read.hash)
            .runs()
        {
            held.insert(*run);
        }
    }
    held
}

/// Why a candidate is no case, or `None` where it is one.
fn why_no_case(log: &FamilyLog, journals: &[&Journal], candidate: &Candidate) -> Option<NoCase> {
    if !has_case_shape(&candidate.read) {
        return Some(NoCase::Shape);
    }
    if !live_holders(log, journals, candidate).is_empty() {
        return None;
    }
    let read = &candidate.read;
    let whole_holder = (0..journals.len())
        .filter(|window| *window != candidate.window)
        .filter_map(|window| prior_touching(log, journals, candidate, window))
        .any(|prior| {
            Journal::from_lines(prior.lines.to_vec())
                .coverage(&read.path, read.hash)
                .contains(read.range)
        });
    Some(if whole_holder {
        NoCase::QuietHolder
    } else {
        NoCase::NoHolder
    })
}

/// The calls the family-held rows count as cases, by window, stamp and path.
pub(super) fn case_keys(log: &FamilyLog, journals: &[&Journal]) -> BTreeSet<CallKey> {
    log.candidates
        .iter()
        .filter(|candidate| case_of(log, journals, candidate).is_some())
        .map(key_of)
        .collect()
}

fn key_of(candidate: &Candidate) -> CallKey {
    (
        candidate.window,
        candidate.stamp.clone(),
        candidate.read.path.clone(),
    )
}

/// Fold the first reads of the journal of uncommitted files into the cases
/// and the reasons for no case, noting which of them today's rows count.
pub(super) fn fold_uncommitted_family(
    log: &FamilyLog,
    journals: &[&Journal],
    counted_today: &BTreeSet<CallKey>,
    out: &mut UncommittedFamilyHeld,
) {
    for candidate in &log.candidates {
        if counted_today.contains(&key_of(candidate)) {
            out.counted_today.add(candidate.tokens);
        }
        match why_no_case(log, journals, candidate) {
            Some(why) => out.note_no_case(why, candidate.tokens),
            None => {
                if let Some(case) = case_of(log, journals, candidate) {
                    out.pool.cases.push(case);
                }
            }
        }
    }
}

/// Whether a window is live at `stamp`: a session's own window always is
/// while its tree is worked in; a subagent's while its journal's last line
/// was written within the floor.
fn is_holder_live(log: &FamilyLog, window: usize, last: Option<&String>, stamp: &str) -> bool {
    if !log.subagent.get(window).copied().unwrap_or(true) {
        return true;
    }
    let (Some(last), Some(now)) = (last.and_then(|s| unix_seconds(s)), unix_seconds(stamp)) else {
        return false;
    };
    now.saturating_sub(last) <= LIVE_FLOOR_SECONDS
}

/// Whether a window's file is a subagent's: the client keeps those in a
/// directory of their own beside the session's.
fn is_subagent_file(path: &Path) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "subagents")
}

impl FamilyHeldPool {
    /// Each route's figures: the outline where one is bound, the held runs
    /// where none is, and the read that runs otherwise.
    pub(super) fn route_figures(&self) -> [RouteFigures; 3] {
        let mut out = [RouteFigures::default(); 3];
        for case in &self.cases {
            let (route, frame) = match (case.outline_frame, case.held_frame) {
                (Some(frame), _) => (0, frame),
                (None, Some(frame)) => (1, frame),
                (None, None) => (2, 0),
            };
            let Some(figures) = out.get_mut(route) else {
                continue;
            };
            figures.pool.add(case.tokens);
            figures.frames = figures.frames.saturating_add(frame);
            figures.frame_max = figures.frame_max.max(frame);
            figures.follow_ups.calls = figures
                .follow_ups
                .calls
                .saturating_add(case.follow_ups.calls);
            figures.follow_ups.tokens = figures
                .follow_ups
                .tokens
                .saturating_add(case.follow_ups.tokens);
        }
        out
    }
}

impl RouteFigures {
    /// What a decline on this route would have kept out of context: the
    /// reads it answered, less the frames and every later read of the bytes.
    pub(super) fn net_kept(self) -> Tally {
        Tally {
            calls: self.pool.calls,
            tokens: self
                .pool
                .tokens
                .saturating_sub(self.frames)
                .saturating_sub(self.follow_ups.tokens),
        }
    }
}

/// The table's family-held rows.
pub(super) fn family_held_rows(report: &Report, denominator: u64) -> String {
    let mut out = String::from(
        "\n  family held: first sights another live window held (observed; net of the frame and \
         the follow-ups)\n",
    );
    let figures = report.family_held.route_figures();
    for ((_, label), f) in ROUTES.iter().zip(figures) {
        out.push_str(&row(label, f.pool, denominator));
        if f.frames > 0 {
            let net_label = format!(
                "  net of frames (max {} tok) and follow-ups",
                commas(f.frame_max)
            );
            out.push_str(&row(&net_label, f.net_kept(), denominator));
        }
        out.push_str(&row(
            "  followed by another read",
            f.follow_ups,
            denominator,
        ));
    }
    out
}

/// The same rows for the machine-readable document.
pub(super) fn family_held_json(report: &Report) -> Value {
    let figures = report.family_held.route_figures();
    let mut map = serde_json::Map::new();
    for ((key, _), f) in ROUTES.iter().zip(figures) {
        map.insert(
            (*key).to_owned(),
            json!({
                "calls": f.pool.calls,
                "tokens": f.pool.tokens,
                "frame_tokens": f.frames,
                "frame_tokens_max": f.frame_max,
                "follow_up_calls": f.follow_ups.calls,
                "follow_up_tokens": f.follow_ups.tokens,
                // Only a route that declines keeps anything out; the read
                // that runs nets nothing.
                "net_tokens": if f.frames > 0 { f.net_kept().tokens } else { 0 },
            }),
        );
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use rift_agent::window_journal::Interval;
    use rift_agent::{HiKind, ReadChannel, ReadRequest, ReadSpan};

    use super::*;

    fn hash() -> Sha256Id {
        rift_agent::content_sha256(b"src/a.rs")
    }

    fn read(lo: u32, hi: u32) -> ResolvedRead {
        let request = ReadRequest {
            path: "src/a.rs".to_owned(),
            span: ReadSpan::lines(lo, HiKind::Explicit(hi)),
            channel: ReadChannel::Tool,
        };
        ResolvedRead::new(&request, hash(), 900).unwrap()
    }

    fn held(lo: u32, hi: u32) -> Line {
        Line::Read {
            turn: 1,
            path: "src/a.rs".to_owned(),
            hash: hash(),
            range: Interval::new(lo, hi).unwrap(),
            total_lines: 900,
            at_ms: 0,
        }
    }

    /// A tree of a main window and a subagent: the main window read lines
    /// 100-160 at ten o'clock, the subagent reads the file whole later.
    fn tree(subagent_reads_at: &str) -> (FamilyLog, Vec<Journal>) {
        let main = Path::new("s/abc.jsonl");
        let child = Path::new("s/abc/subagents/agent-1.jsonl");
        let mut log = FamilyLog::new(&[main, child], &[None, Some(0)]);
        let journals = vec![
            Journal::from_lines(vec![held(100, 160)]),
            Journal::default(),
        ];
        log.note_journal_lines(0, 1, "2026-09-01T10:00:00Z");
        log.note_family_candidate(1, subagent_reads_at, &read(1, 900), 5_000, None);
        log.note_ran_read(1, subagent_reads_at, &read(1, 900), 5_000);
        log.note_ran_read(1, "2026-09-01T11:30:00Z", &read(100, 160), 700);
        (log, journals)
    }

    /// ★ A first sight the session's own window held is a held-lines case,
    /// charged with the window's later read of the bytes; one the holder had
    /// not read yet is no case at all.
    #[test]
    fn a_first_sight_a_live_window_held_is_a_case_with_its_follow_ups() {
        let (log, journals) = tree("2026-09-01T11:00:00Z");
        let refs: Vec<&Journal> = journals.iter().collect();
        let mut pool = FamilyHeldPool::default();
        fold_family(&log, &refs, &mut pool);
        let [outline, held, runs] = pool.route_figures();
        assert_eq!((outline.pool.calls, runs.pool.calls), (0, 0));
        assert_eq!(held.pool.tokens, 5_000);
        assert_eq!(held.follow_ups.tokens, 700);
        assert!(held.frames > 0);
        assert_eq!(held.net_kept().tokens, 5_000 - held.frames - 700);

        let (log, journals) = tree("2026-09-01T09:00:00Z");
        let refs: Vec<&Journal> = journals.iter().collect();
        let mut pool = FamilyHeldPool::default();
        fold_family(&log, &refs, &mut pool);
        assert!(pool.cases.is_empty());
    }

    /// A subagent holder is live only while its journal was written within
    /// the floor of the read; a session's own window always is.
    #[test]
    fn a_quiet_subagent_holds_nothing_a_decline_may_rest_on() {
        let main = Path::new("s/abc.jsonl");
        let child = Path::new("s/abc/subagents/agent-1.jsonl");
        let journals = [
            Journal::default(),
            Journal::from_lines(vec![held(100, 160)]),
        ];
        let refs: Vec<&Journal> = journals.iter().collect();
        for (at, cases) in [("2026-09-01T10:04:59Z", 1), ("2026-09-01T10:05:01Z", 0)] {
            let mut log = FamilyLog::new(&[main, child], &[None, Some(0)]);
            log.note_journal_lines(1, 1, "2026-09-01T10:00:00Z");
            log.note_family_candidate(0, at, &read(1, 900), 5_000, None);
            let mut pool = FamilyHeldPool::default();
            fold_family(&log, &refs, &mut pool);
            assert_eq!(pool.cases.len(), cases, "{at}");
        }
        assert!(is_subagent_file(child) && !is_subagent_file(main));
    }
}

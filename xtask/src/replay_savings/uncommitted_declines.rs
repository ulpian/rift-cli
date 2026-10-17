//! The re-reads of files no commit holds that the gate declines, modelled with
//! no more credit than the gate gives.
//!
//! The gate keys coverage by the SHA-256 of the working-tree bytes, committed
//! or not, so an unchanged re-read of an uncommitted file is declined like any
//! other. The replay cannot see those bytes on disk; it can see what the
//! transcript proves of them (`no_line_count_split`). This module puts that
//! proof through the shipped decision: a journal of its own is fed by the
//! reads, writes and edits of such files, hashed over the proven bytes, and a
//! re-read the bucket classed as unchanged is credited only when that decision
//! declines it.
//!
//! The journal is kept apart from the window's main one so every row built on
//! the main journal reads as it did; the credit lands in the declined pool and
//! pays the same reason-frame cost the declined path does.
//!
//! What the gate would not do, this does not credit: a file past the
//! inline-hash cap is never hashed; a call of several reads is declined only
//! as a whole; a range the window does not hold, a key already declined once,
//! a slice past the credit bound or under the line floor are all let through.

use rift_agent::window_journal::{
    self, Decision, DenyShape, EditEcho, Interval, Line, Mode, ResolvedRead,
};
use rift_agent::{content_sha256, ReadRequest};
use rift_core::Sha256Id;
use serde_json::{json, Value};

use super::classify::{charge_a_frame, frame_for};
use super::first_contact::outline_facts;
use super::render::row;
use super::replay::Ctx;
use super::report::{Report, Tally};
use super::transcript::Call;

/// The most a gate hook hashes inline: a larger file is never read, so it is
/// never held. The shipped policy default, restated because the replay links
/// no policy crate.
pub(super) const MAX_INLINE_HASH_BYTES: u64 = 1024 * 1024;

/// Why an unchanged re-read is not credited, in the order the table prints
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Left {
    /// One of several reads in a call: declined only as a whole.
    Several,
    /// The file is past the inline-hash cap.
    OverTheHashCap,
    /// Nothing of these bytes was ever put in front of this window: the
    /// transcript proved them from another window of the session tree.
    NeverReadHere,
    /// This window held them before a compaction emptied it.
    EmptiedByACompaction,
    /// This window holds other lines of these bytes, not the ones asked for.
    LinesNotHeld,
    /// The window was already declined this exact read once.
    KeyAlreadyDeclined,
    /// The slice is past the size a delivery can be vouched for.
    PastTheCreditBound,
    /// Held, but the decision lets it run: under the line floor, or no
    /// honest frame to send.
    LetThrough,
}

/// What the gate model credits of the unchanged re-reads, and why not the rest.
#[derive(Clone, Debug, Default)]
pub struct UncommittedDeclines {
    /// Re-reads the gate declines.
    credited: Tally,
    /// The others, one tally per `LEFT` entry.
    left: [Tally; 8],
}

/// The reasons, in the order the table prints them: a key and a label.
const LEFT: [(&str, &str); 8] = [
    ("several_reads", "    left: one of several reads in a call"),
    ("over_hash_cap", "    left: file over the 1 MiB hashing cap"),
    (
        "never_read_here",
        "    left: first read of these bytes in this window",
    ),
    (
        "emptied_by_compaction",
        "    left: held before a compaction emptied the window",
    ),
    ("lines_not_held", "    left: other lines held, not these"),
    (
        "key_already_declined",
        "    left: this read was already declined once",
    ),
    ("past_credit_bound", "    left: slice past the credit bound"),
    ("let_through", "    left: held, decided not to decline"),
];

impl Left {
    const fn index(self) -> usize {
        match self {
            Self::Several => 0,
            Self::OverTheHashCap => 1,
            Self::NeverReadHere => 2,
            Self::EmptiedByACompaction => 3,
            Self::LinesNotHeld => 4,
            Self::KeyAlreadyDeclined => 5,
            Self::PastTheCreditBound => 6,
            Self::LetThrough => 7,
        }
    }
}

impl UncommittedDeclines {
    /// Count a re-read the bucket classed as unchanged and the gate would not
    /// decline.
    pub(super) fn note_left(&mut self, why: Left, tokens: u64) {
        if let Some(tally) = self.left.get_mut(why.index()) {
            tally.add(tokens);
        }
    }
}

/// One single-member call of a file the transcript proves bytes of.
pub(super) struct Reread<'a> {
    pub(super) call: &'a Call,
    pub(super) request: &'a ReadRequest,
    pub(super) key: &'a str,
    pub(super) text: &'a str,
    pub(super) tokens: u64,
    /// Whether the bucket classed this call as an unchanged re-read.
    pub(super) unchanged: bool,
}

/// The bytes a file has, as the gate would see them.
pub(super) struct Shape {
    pub(super) hash: Sha256Id,
    pub(super) lines: u32,
    pub(super) bytes: u64,
}

impl Shape {
    /// The shape of known bytes, lines counted the way the gate counts them.
    pub(super) fn of(text: &str) -> Self {
        let lines = text.split_inclusive('\n').count();
        Self {
            hash: content_sha256(text.as_bytes()),
            lines: u32::try_from(lines).unwrap_or(u32::MAX),
            bytes: u64::try_from(text.len()).unwrap_or(u64::MAX),
        }
    }
}

/// Note a change to a file, in the working-tree model and in the journal of
/// uncommitted files.
pub(super) fn note_an_edit(ctx: &mut Ctx<'_>, call: &Call, key: &str) {
    let before = ctx.tree.working.shape(call, key);
    ctx.tree.working.note_change(call, key);
    let Some(after) = ctx.tree.working.shape(call, key) else {
        return;
    };
    if before
        .as_ref()
        .is_some_and(|shape| shape.hash == after.hash)
    {
        return;
    }
    let wrote = call.name == "Write";
    let sites: Vec<Interval> = Interval::new(1, after.lines)
        .filter(|_| wrote)
        .into_iter()
        .collect();
    let old = before.as_ref().map(|shape| shape.lines).unwrap_or_default();
    ctx.state.uncommitted.push(Line::Edit {
        turn: ctx.state.turn,
        path: key.to_owned(),
        hash_old: before.map(|shape| shape.hash),
        hash_new: after.hash,
        sites,
        shift: i64::from(after.lines) - i64::from(old),
        full: wrote,
        at_ms: 0,
    });
}

/// Decide one read of a file the transcript proves bytes of, against the
/// journal of uncommitted files, count it where the bucket classed it as an
/// unchanged re-read, and record what the gate would have recorded.
///
/// Called after the delivery is folded into the model, so the hash is that of
/// the bytes the window now holds.
pub(super) fn fold_the_read(ctx: &mut Ctx<'_>, at: &Reread<'_>) {
    let Some(shape) = ctx.tree.working.shape(at.call, at.key) else {
        return;
    };
    if shape.bytes > MAX_INLINE_HASH_BYTES {
        note_left(ctx.report, at, Left::OverTheHashCap);
        return;
    }
    let request = ReadRequest {
        path: at.key.to_owned(),
        ..at.request.clone()
    };
    let Some(read) = ResolvedRead::new(&request, shape.hash, shape.lines) else {
        note_left(ctx.report, at, Left::LetThrough);
        return;
    };
    let limits = ctx.replay.limits;
    let slice = at.text.len();
    if slice > limits.max_slice_bytes || !read.delivery_is_complete(slice, limits) {
        note_left(ctx.report, at, Left::PastTheCreditBound);
        return;
    }
    let journal = &ctx.state.uncommitted;
    let held = journal.coverage(&read.path, read.hash).contains(read.range);
    let denied = journal.denied(&read.deny_key());
    let seen = Seen::of(journal, &read);
    let decision = window_journal::decide(
        &read,
        journal,
        Mode::Enforce,
        ctx.replay.channel,
        EditEcho::Pinned,
        slice,
        limits,
    );
    let frame = frame_for(journal, &read, &decision, limits.context_lines);
    let declined = matches!(decision, Decision::Deny(DenyShape::Plain)) && frame.is_some();
    let decision = if declined { decision } else { Decision::Allow };
    let on_a_write = journal.wrote_in_full(&read.path, read.hash).is_some();
    let line = window_journal::record(&read, &decision, slice, limits, ctx.state.turn, 0);
    ctx.state.uncommitted.push(line);
    if !declined {
        ctx.tree
            .uncommitted_family
            .note_ran_read(ctx.window, ctx.stamp, &read, at.tokens);
    }
    if !at.unchanged {
        return;
    }
    let (Some(frame), true) = (frame, declined) else {
        let why = match (held, denied) {
            (false, _) => seen.why_not_held(),
            (true, true) => Left::KeyAlreadyDeclined,
            (true, false) => Left::LetThrough,
        };
        note_left(ctx.report, at, why);
        if why == Left::NeverReadHere {
            note_a_family_candidate(ctx, at, &read);
        }
        return;
    };
    let report = &mut *ctx.report;
    report.declined.add(at.tokens);
    report.declinable.add(at.tokens);
    report.uncommitted.credited.add(at.tokens);
    charge_a_frame(report, &frame);
    if on_a_write {
        report.declines_resting_on_a_write = report.declines_resting_on_a_write.saturating_add(1);
    }
}

/// Note a window's first read of bytes it never held, for the family-held
/// share: the proven bytes give the outline a holder's read would have bound.
fn note_a_family_candidate(ctx: &mut Ctx<'_>, at: &Reread<'_>, read: &ResolvedRead) {
    let cwd = at.call.cwd.as_deref().unwrap_or_default();
    let facts = ctx
        .tree
        .working
        .proven_text(cwd, at.key)
        .and_then(|text| outline_facts(at.key, text.as_bytes()));
    ctx.tree
        .uncommitted_family
        .note_family_candidate(ctx.window, ctx.stamp, read, at.tokens, facts);
}

/// What a window's journal says of one revision of a file, before the read
/// being decided is written into it.
struct Seen {
    /// Some line of the revision is live.
    live: bool,
    /// Some line of the revision was ever written, a compaction aside.
    ever: bool,
}

impl Seen {
    fn of(journal: &window_journal::Journal, read: &ResolvedRead) -> Self {
        let of_it = |line: &Line| match line {
            Line::Read { path, hash, .. } => path == &read.path && *hash == read.hash,
            Line::Edit { path, hash_new, .. } => path == &read.path && *hash_new == read.hash,
            _ => false,
        };
        Self {
            live: journal.live().iter().any(of_it),
            ever: journal.lines().iter().any(of_it),
        }
    }

    /// Why a read the window does not wholly hold is not held.
    const fn why_not_held(&self) -> Left {
        match (self.live, self.ever) {
            (true, _) => Left::LinesNotHeld,
            (false, true) => Left::EmptiedByACompaction,
            (false, false) => Left::NeverReadHere,
        }
    }
}

/// Count an unchanged re-read the gate would not decline, where this call is
/// one.
pub(super) fn note_left(report: &mut Report, at: &Reread<'_>, why: Left) {
    if at.unchanged {
        report.uncommitted.note_left(why, at.tokens);
    }
}

/// The table's rows for what the gate model credits.
pub(super) fn uncommitted_rows(report: &Report, denominator: u64) -> String {
    let mut out = String::from(
        "\n  of the unchanged re-reads, what the gate declines (credited in the declined row)\n",
    );
    out.push_str(&row(
        "  declined: the gate holds the lines",
        report.uncommitted.credited,
        denominator,
    ));
    for ((_, label), tally) in LEFT.iter().zip(report.uncommitted.left) {
        out.push_str(&row(label, tally, denominator));
    }
    out
}

/// The same rows for the machine-readable document.
pub(super) fn uncommitted_json(report: &Report) -> Value {
    let tally = |t: Tally| json!({ "calls": t.calls, "tokens": t.tokens });
    let mut map = serde_json::Map::new();
    map.insert("credited".to_owned(), tally(report.uncommitted.credited));
    for ((key, _), t) in LEFT.iter().zip(report.uncommitted.left) {
        map.insert((*key).to_owned(), tally(t));
    }
    Value::Object(map)
}

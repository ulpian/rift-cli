//! What one context window has already been given, as lines of a revision —
//! and nothing else.
//!
//! This module is the pure half of the window journal: a line schema, an
//! interval set, and the decision taken over them. It touches no filesystem,
//! reads no clock and opens no store; every timestamp and every hash arrives
//! as an argument. The half that appends to a file and survives two processes
//! writing at once lives in the binary, beside the other local run state.
//!
//! Three rules shape everything here.
//!
//! 1. **A range is what the tool can have delivered, never what was asked.**
//!    Recording less than was really delivered costs a saving; recording more
//!    costs a wrong decision about content an agent does not hold. Only the
//!    first is acceptable, so an unpinned span does not resolve into this
//!    line journal at all.
//! 2. **Coverage belongs to a revision, not to a file.** Any byte change —
//!    an edit, a formatter, a branch switch — gives the file a new hash, and
//!    a new hash starts with no coverage at all. No interval is ever shifted
//!    or adjusted to follow an edit, which is why a line number recorded
//!    before a change can never be mistaken for one after it.
//! 3. **Lines carry identifiers, never content.** Paths, hashes, line
//!    numbers, counts. A line whose hash fields are not 64 lowercase hex
//!    characters, or whose shape does not match its kind, is dropped when the
//!    journal is read: the worst a malformed or forged line can cost is a
//!    saving that is not taken.

use rift_core::Sha256Id;
use serde_json::{json, Map, Value};

use crate::respond::Channel;
use crate::{ReadChannel, ReadRequest};

/// The schema version every line this module writes carries, and the only one
/// it reads. A later shape gets a later number rather than a tolerant reader.
pub const LINE_VERSION: u32 = 1;

/// The most bytes a read result can carry and still be recorded as delivered
/// in full, at the density this repository's own slices measure.
///
/// A read's result was measured to stop at a **25,000-token** cap and to be
/// truncated in place when it does (the recorded spike behind the read rows of
/// the Claude Code tool-field pin). Tokens are not bytes, and the conversion
/// is where a ceiling can go wrong in the expensive direction: the slices this
/// repository's fixtures price run **1.90 to 2.22 bytes a token**, so at the
/// smallest of those densities the cap is reached at `25_000 * 1.9` bytes.
/// Anything larger *may* have been cut off before the model saw the end of it,
/// and a range recorded for it would be a claim about a context that never
/// received the lines.
///
/// The smallest measured density is the right one to divide by, because the
/// question is not what a typical read delivers — it is what the largest slice
/// that could still have been truncated is, and that is the densest text.
///
/// A read over this size is recorded as one line rather than not at all: it
/// happened, the window holds its beginning, and the lines past that are the
/// ones nobody can vouch for.
///
/// **It is a bound on what density can vouch for, not on what a window may
/// hold.** Where the client states its own delivery — the fields
/// [`StatedDelivery`] is built from — that statement is the better evidence
/// and it is believed over this arithmetic, in both directions: a slice far
/// over this size is credited in full when the result says the call was
/// answered in full, and cut back to the run the result names when it says it
/// was cut short. The ceiling is what the journal falls back to when nothing
/// says anything, which is every family whose result shape no run has pinned.
pub const DEFAULT_MAX_DELIVERED_BYTES: usize = 47_500;

/// The most bytes a **terminal** result can carry and still be recorded as
/// delivered in full.
///
/// A different ceiling from [`DEFAULT_MAX_DELIVERED_BYTES`] because it is a
/// different mechanism, and it is measured in the unit it actually acts on.
/// A read tool's result is cut short in place at a token cap; terminal output
/// is not cut short at all — past a size in characters the client writes the
/// whole of it to a file and hands the model a pointer and a two-kilobyte
/// preview. The recorded spike walked the boundary a thousand characters at a
/// time: 29,999 characters came back inline, 30,999 came back as the pointer.
///
/// So the honest bound is the smaller side of that interval, and it is the
/// tighter of the two ceilings — which matters most for the commands that
/// state a range. `head -n 3000` and `sed -n '1,3000p'` name their lines, and
/// a range recorded from the command line alone would say a window holds
/// three thousand lines when what it holds is a filename.
///
/// A shell read over this size is recorded as one line rather than not at
/// all, for the same reason as the other ceiling: the preview is real, the
/// lines past it are the ones nobody can vouch for.
pub const DEFAULT_MAX_SHELL_DELIVERED_BYTES: usize = 30_000;

/// A run of lines, 1-based and inclusive at both ends — the same numbering a
/// reader sees in an editor and a tool reports in its output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Interval {
    /// The first line of the run.
    pub lo: u32,
    /// The last line of the run.
    pub hi: u32,
}

impl Interval {
    /// A run from `lo` to `hi`, or `None` when the pair is not a run: a line
    /// zero (the numbering starts at one) or an end before its start.
    ///
    /// The constructor is the only way in, so nothing downstream has to ask
    /// whether an interval it was handed makes sense.
    #[must_use]
    pub const fn new(lo: u32, hi: u32) -> Option<Self> {
        if lo == 0 || hi < lo {
            None
        } else {
            Some(Self { lo, hi })
        }
    }

    /// Whether every line of `other` is a line of this run.
    #[must_use]
    pub const fn contains(&self, other: Self) -> bool {
        self.lo <= other.lo && other.hi <= self.hi
    }

    /// Whether this run and `other` have a line in common.
    ///
    /// Touching counts: `L1-10` and `L10-20` share line ten, and a claim
    /// about line ten is the same claim whichever run it is made from.
    #[must_use]
    pub const fn intersects(&self, other: Self) -> bool {
        self.lo <= other.hi && other.lo <= self.hi
    }

    /// How many lines the run spans.
    #[must_use]
    pub const fn lines(&self) -> u32 {
        self.hi - self.lo + 1
    }

    /// Whether this run is the stand-in for *every* line of a file, however
    /// long it is — the run [`IntervalSet::whole_file`] is made of.
    ///
    /// A caller pricing the run has to tell the two apart: `L1-4294967295` is
    /// not a claim about four billion lines, it is the claim that no request
    /// can fall outside what the window holds.
    #[must_use]
    pub const fn is_whole_file(&self) -> bool {
        self.lo == 1 && self.hi == u32::MAX
    }
}

/// Why a context window stopped holding what it had been given.
///
/// Each of these is a signal some family publishes at the moment its context
/// is summarised, cleared or rebuilt. They are kept apart rather than folded
/// into one token because the share of reading that follows each is a
/// different number, and the journal is where that number comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvictReason {
    /// The client announced it is about to summarise the conversation.
    PreCompact,
    /// A session started from a summary of an earlier one.
    Compact,
    /// The conversation was cleared.
    Clear,
    /// A session was resumed from a transcript.
    Resume,
    /// The client announced a summary has been written.
    PostCompact,
    /// The client said outright that it handed the window less than the call
    /// asked for, and what this window was credited with is dropped back to
    /// what the result states it received.
    ///
    /// It is not a boundary the family announces about its conversation — it
    /// is a correction this journal makes about itself, and it is an eviction
    /// because coverage is a union of what was read and a union has no way to
    /// take a line back. It costs the window the savings its other reads had
    /// earned; it can never cost it bytes it was actually handed, because the
    /// delivered lines are written again straight after.
    Truncate,
}

impl EvictReason {
    /// The token this reason is written as.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreCompact => "pre_compact",
            Self::Compact => "compact",
            Self::Clear => "clear",
            Self::Resume => "resume",
            Self::PostCompact => "post_compact",
            Self::Truncate => "truncate",
        }
    }

    /// The reason a token names, or `None` for a token this version does not
    /// know — which drops the line rather than guessing at its meaning.
    #[must_use]
    pub fn from_str(token: &str) -> Option<Self> {
        match token {
            "pre_compact" => Some(Self::PreCompact),
            "compact" => Some(Self::Compact),
            "clear" => Some(Self::Clear),
            "resume" => Some(Self::Resume),
            "post_compact" => Some(Self::PostCompact),
            "truncate" => Some(Self::Truncate),
            _ => None,
        }
    }
}

/// One entry in a context window's journal.
///
/// The spelling of a path here is whatever the recorder keys coverage by, and
/// a recorder that had the bytes in hand keys it by the checkout's own
/// relative name: two absolute spellings of one file — a symlinked prefix, a
/// route through a link — are then one claim rather than two, and a name that
/// resolves to no file of this checkout is not recorded at all. This module
/// opens nothing and compares these strings for equality, so all it asks is
/// that one recorder spell one file one way.
///
/// Every variant carries the turn it happened in and the millisecond it was
/// written at, both supplied by the caller, so this module never reads a
/// clock and a test never has to freeze one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// Lines a read-shaped call can have delivered, and the file's own length
    /// at the time, so a later question about whole-file coverage has an
    /// answer without re-reading anything.
    Read {
        /// The turn the call was made in.
        turn: u32,
        /// The file, in the one spelling coverage is keyed by.
        path: String,
        /// The revision the bytes hashed to when the call was seen.
        hash: Sha256Id,
        /// The lines the tool can have delivered.
        range: Interval,
        /// How many lines the file had.
        total_lines: u32,
        /// When the line was written, in milliseconds.
        at_ms: u64,
    },
    /// A read that was declined once, with its reason delivered.
    Deny {
        /// The turn the call was made in.
        turn: u32,
        /// The file, in the one spelling coverage is keyed by.
        path: String,
        /// The revision the bytes hashed to.
        hash: Sha256Id,
        /// The lines that were asked for.
        range: Interval,
        /// When the line was written, in milliseconds.
        at_ms: u64,
    },
    /// A read that *would* have been declined, and was not — the line that
    /// sizes the pool while nothing is being changed about anyone's session.
    Observe {
        /// The turn the call was made in.
        turn: u32,
        /// The file, in the one spelling coverage is keyed by.
        path: String,
        /// The revision the bytes hashed to.
        hash: Sha256Id,
        /// The lines that were asked for.
        range: Interval,
        /// When the line was written, in milliseconds.
        at_ms: u64,
    },
    /// **Rift told this window it holds these lines of this revision.**
    ///
    /// Written where Rift pushes a statement about held state into the
    /// agent's context — a delta manifest entry, a recovery map entry — so
    /// that an action taken on the file afterwards has something to be
    /// evidence *against*. It is not coverage and never becomes coverage: a
    /// window holds what it was handed, and this line only records what it
    /// was told about what it was handed.
    ///
    /// Only what a hook can observe is written here. A citation of the file
    /// in the model's own prose is not a tool call, no family reports one,
    /// and nothing in this journal pretends otherwise.
    Hold {
        /// The turn the announcement was made in.
        turn: u32,
        /// The file, in the one spelling coverage is keyed by.
        path: String,
        /// The revision the announcement was about.
        hash: Sha256Id,
        /// The lines the announcement said the window held.
        range: Interval,
        /// When the line was written, in milliseconds.
        at_ms: u64,
    },
    /// **A read the announcement above stopped from happening**, booked once.
    ///
    /// Written when the agent acted on an announced file — changed it, or
    /// searched inside it — without reading it again. The line is what makes
    /// the booking once-per-hold: a second action on the same announcement
    /// finds this and books nothing more.
    Prevented {
        /// The turn the action was taken in.
        turn: u32,
        /// The file, in the one spelling coverage is keyed by.
        path: String,
        /// The revision the announcement was about.
        hash: Sha256Id,
        /// The lines the booking was priced over.
        range: Interval,
        /// When the line was written, in milliseconds.
        at_ms: u64,
    },
    /// The hash a whole-file write's own bytes have, recorded before the write
    /// lands, so that "the agent holds this file" becomes an equality checked
    /// afterwards rather than an assumption that nothing reformatted it.
    Expect {
        /// The turn the call was made in.
        turn: u32,
        /// The file, in the one spelling coverage is keyed by.
        path: String,
        /// The hash of the bytes the payload carried.
        hash: Sha256Id,
        /// When the line was written, in milliseconds.
        at_ms: u64,
    },
    /// A file changed, and what is known exactly about how.
    Edit {
        /// The turn the call was made in.
        turn: u32,
        /// The file, in the one spelling coverage is keyed by.
        path: String,
        /// The revision the file had before, where the journal knew it.
        hash_old: Option<Sha256Id>,
        /// The revision the file has now.
        hash_new: Sha256Id,
        /// Where it changed, in the numbering of the **new** bytes. Empty when
        /// the sites do not follow from the payload.
        sites: Vec<Interval>,
        /// How many lines the file grew by, negative when it shrank.
        shift: i64,
        /// Whether the agent provably holds every line of the new revision.
        full: bool,
        /// When the line was written, in milliseconds.
        at_ms: u64,
    },
    /// The context window stopped holding what it had been given.
    Evict {
        /// The turn the signal arrived in.
        turn: u32,
        /// Which signal it was.
        reason: EvictReason,
        /// When the line was written, in milliseconds.
        at_ms: u64,
    },
}

impl Line {
    /// The token naming this line's kind on the wire.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Read { .. } => "read",
            Self::Deny { .. } => "deny",
            Self::Observe { .. } => "observe",
            Self::Hold { .. } => "hold",
            Self::Prevented { .. } => "prevented",
            Self::Expect { .. } => "expect",
            Self::Edit { .. } => "edit",
            Self::Evict { .. } => "evict",
        }
    }

    /// The turn this line belongs to.
    #[must_use]
    pub const fn turn(&self) -> u32 {
        match self {
            Self::Read { turn, .. }
            | Self::Deny { turn, .. }
            | Self::Observe { turn, .. }
            | Self::Hold { turn, .. }
            | Self::Prevented { turn, .. }
            | Self::Expect { turn, .. }
            | Self::Edit { turn, .. }
            | Self::Evict { turn, .. } => *turn,
        }
    }

    /// The line as the single JSON object a journal file holds one of per row.
    ///
    /// No newline: the caller that appends decides how a row ends, and a
    /// writer that concatenates is not this module's business.
    #[must_use]
    pub fn to_json_line(&self) -> String {
        let mut object = Map::new();
        object.insert("v".to_owned(), json!(LINE_VERSION));
        object.insert("k".to_owned(), json!(self.kind()));
        object.insert("t".to_owned(), json!(self.turn()));
        match self {
            Self::Read {
                path,
                hash,
                range,
                total_lines,
                at_ms,
                ..
            } => {
                object.insert("p".to_owned(), json!(path));
                object.insert("h".to_owned(), json!(hash.to_hex()));
                object.insert("lo".to_owned(), json!(range.lo));
                object.insert("hi".to_owned(), json!(range.hi));
                object.insert("n".to_owned(), json!(total_lines));
                object.insert("at".to_owned(), json!(at_ms));
            }
            Self::Deny {
                path,
                hash,
                range,
                at_ms,
                ..
            }
            | Self::Observe {
                path,
                hash,
                range,
                at_ms,
                ..
            }
            | Self::Hold {
                path,
                hash,
                range,
                at_ms,
                ..
            }
            | Self::Prevented {
                path,
                hash,
                range,
                at_ms,
                ..
            } => {
                object.insert("p".to_owned(), json!(path));
                object.insert("h".to_owned(), json!(hash.to_hex()));
                object.insert("lo".to_owned(), json!(range.lo));
                object.insert("hi".to_owned(), json!(range.hi));
                object.insert("at".to_owned(), json!(at_ms));
            }
            Self::Expect {
                path, hash, at_ms, ..
            } => {
                object.insert("p".to_owned(), json!(path));
                object.insert("h".to_owned(), json!(hash.to_hex()));
                object.insert("at".to_owned(), json!(at_ms));
            }
            Self::Edit {
                path,
                hash_old,
                hash_new,
                sites,
                shift,
                full,
                at_ms,
                ..
            } => {
                object.insert("p".to_owned(), json!(path));
                if let Some(old) = hash_old {
                    object.insert("h_old".to_owned(), json!(old.to_hex()));
                }
                object.insert("h_new".to_owned(), json!(hash_new.to_hex()));
                let rows: Vec<Value> = sites.iter().map(|s| json!([s.lo, s.hi])).collect();
                object.insert("sites".to_owned(), Value::Array(rows));
                object.insert("shift".to_owned(), json!(shift));
                object.insert("full".to_owned(), json!(full));
                object.insert("at".to_owned(), json!(at_ms));
            }
            Self::Evict { reason, at_ms, .. } => {
                object.insert("reason".to_owned(), json!(reason.as_str()));
                object.insert("at".to_owned(), json!(at_ms));
            }
        }
        Value::Object(object).to_string()
    }

    /// The line a JSON row states, or `None` when the row is not one this
    /// version wrote.
    ///
    /// Every check here is a reason to drop rather than to repair: a version
    /// that is not this one, a kind that is not known, a hash that is not 64
    /// lowercase hex, a range that is not a run, a field of the wrong type or
    /// missing. Dropping costs at most a saving; repairing would cost a claim
    /// about content nobody verified.
    #[must_use]
    pub fn from_json_line(row: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(row).ok()?;
        let object = value.as_object()?;
        if u32_at(object, "v")? != LINE_VERSION {
            return None;
        }
        let turn = u32_at(object, "t")?;
        let at_ms = u64_at(object, "at")?;
        match object.get("k")?.as_str()? {
            "read" => Some(Self::Read {
                turn,
                path: text_at(object, "p")?.to_owned(),
                hash: hash_at(object, "h")?,
                range: range_at(object)?,
                total_lines: u32_at(object, "n")?,
                at_ms,
            }),
            "deny" => Some(Self::Deny {
                turn,
                path: text_at(object, "p")?.to_owned(),
                hash: hash_at(object, "h")?,
                range: range_at(object)?,
                at_ms,
            }),
            "observe" => Some(Self::Observe {
                turn,
                path: text_at(object, "p")?.to_owned(),
                hash: hash_at(object, "h")?,
                range: range_at(object)?,
                at_ms,
            }),
            "hold" => Some(Self::Hold {
                turn,
                path: text_at(object, "p")?.to_owned(),
                hash: hash_at(object, "h")?,
                range: range_at(object)?,
                at_ms,
            }),
            "prevented" => Some(Self::Prevented {
                turn,
                path: text_at(object, "p")?.to_owned(),
                hash: hash_at(object, "h")?,
                range: range_at(object)?,
                at_ms,
            }),
            "expect" => Some(Self::Expect {
                turn,
                path: text_at(object, "p")?.to_owned(),
                hash: hash_at(object, "h")?,
                at_ms,
            }),
            "edit" => Some(Self::Edit {
                turn,
                path: text_at(object, "p")?.to_owned(),
                hash_old: match object.get("h_old") {
                    None | Some(Value::Null) => None,
                    Some(_) => Some(hash_at(object, "h_old")?),
                },
                hash_new: hash_at(object, "h_new")?,
                sites: sites_at(object)?,
                shift: object.get("shift")?.as_i64()?,
                full: object.get("full")?.as_bool()?,
                at_ms,
            }),
            "evict" => Some(Self::Evict {
                turn,
                reason: EvictReason::from_str(text_at(object, "reason")?)?,
                at_ms,
            }),
            _ => None,
        }
    }
}

/// A string field, or `None` when it is absent or not a string.
fn text_at<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key)?.as_str()
}

/// A line-number-sized field, or `None` when it is absent, not a number, or
/// larger than a line number can be.
fn u32_at(object: &Map<String, Value>, key: &str) -> Option<u32> {
    u32::try_from(object.get(key)?.as_u64()?).ok()
}

/// A millisecond field, or `None` when it is absent or not a number.
fn u64_at(object: &Map<String, Value>, key: &str) -> Option<u64> {
    object.get(key)?.as_u64()
}

/// A hash field: 64 lowercase hex characters and nothing else.
///
/// Uppercase is refused even though it would parse, because a journal with two
/// spellings of one hash has two coverage sets for one revision.
fn hash_at(object: &Map<String, Value>, key: &str) -> Option<Sha256Id> {
    let text = text_at(object, key)?;
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    Sha256Id::parse_hex(text).ok()
}

/// The `lo`/`hi` pair of a read-shaped row, as a run.
fn range_at(object: &Map<String, Value>) -> Option<Interval> {
    Interval::new(u32_at(object, "lo")?, u32_at(object, "hi")?)
}

/// The `sites` array of an edit row: pairs of line numbers, each a run. An
/// entry that is not a run drops the whole line — a half-read site list would
/// name the wrong lines in a reason a model then acts on.
fn sites_at(object: &Map<String, Value>) -> Option<Vec<Interval>> {
    let mut out = Vec::new();
    for entry in object.get("sites")?.as_array()? {
        let pair = entry.as_array()?;
        if pair.len() != 2 {
            return None;
        }
        let lo = u32::try_from(pair.first()?.as_u64()?).ok()?;
        let hi = u32::try_from(pair.get(1)?.as_u64()?).ok()?;
        out.push(Interval::new(lo, hi)?);
    }
    Some(out)
}

/// A set of line runs, kept sorted, disjoint and non-adjacent, so that
/// "does this hold every line asked for?" is a walk rather than a search.
///
/// Two runs that meet end to end are one run: lines 1-5 and 6-10 leave no gap
/// between them, and a request for 1-10 is satisfied by having been given
/// both halves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IntervalSet {
    /// Sorted by start; no two entries overlap or touch.
    runs: Vec<Interval>,
}

impl IntervalSet {
    /// An empty set — the coverage of a revision nothing has been read of.
    #[must_use]
    pub const fn new() -> Self {
        Self { runs: Vec::new() }
    }

    /// Every line of a file, however long it is.
    ///
    /// This is what an agent holds of a file it wrote in full and nothing has
    /// rewritten since: the count of lines does not have to be known, because
    /// no request can fall outside it.
    #[must_use]
    pub fn whole_file() -> Self {
        Self {
            runs: vec![Interval {
                lo: 1,
                hi: u32::MAX,
            }],
        }
    }

    /// Add a run, merging it with everything it overlaps or touches.
    pub fn insert(&mut self, run: Interval) {
        let mut merged = run;
        let mut out: Vec<Interval> = Vec::with_capacity(self.runs.len() + 1);
        for existing in self.runs.drain(..) {
            if existing.hi.saturating_add(1) < merged.lo {
                out.push(existing);
            } else if merged.hi.saturating_add(1) < existing.lo {
                out.push(merged);
                merged = existing;
            } else {
                merged = Interval {
                    lo: merged.lo.min(existing.lo),
                    hi: merged.hi.max(existing.hi),
                };
            }
        }
        out.push(merged);
        self.runs = out;
    }

    /// Whether every line of `run` is in the set.
    #[must_use]
    pub fn contains(&self, run: Interval) -> bool {
        self.runs.iter().any(|held| held.contains(run))
    }

    /// Whether nothing is held at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// The runs, in order.
    #[must_use]
    pub fn runs(&self) -> &[Interval] {
        &self.runs
    }
}

/// The exact request a decline was recorded against: this file, at this
/// revision, for these lines.
///
/// A decline is refused a second time for the same key, which is what bounds
/// a model that answers a decline by asking again to one extra exchange. A
/// neighbouring range is a different key and gets its own single decline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DenyKey {
    /// The file, in the one spelling coverage is keyed by.
    pub path: String,
    /// The revision asked for.
    pub hash: Sha256Id,
    /// The lines asked for.
    pub range: Interval,
}

impl DenyKey {
    /// Whether a line about this file, revision and run of lines is a line
    /// about this key — the equality every question about one decline is
    /// asked with, in one place rather than at each of its callers.
    #[must_use]
    pub fn matches(&self, path: &str, hash: Sha256Id, range: Interval) -> bool {
        self.path == path && self.hash == hash && self.range == range
    }
}

/// The lines of one context window, in the order they were written.
#[derive(Clone, Debug, Default)]
pub struct Journal {
    lines: Vec<Line>,
}

impl Journal {
    /// A journal over lines already in hand.
    #[must_use]
    pub fn from_lines(lines: Vec<Line>) -> Self {
        Self { lines }
    }

    /// A journal read from a file's text, one row per line, dropping every row
    /// that is not a line this version wrote — including a torn last row,
    /// which is what a reader finds when it arrives mid-append.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        Self {
            lines: text.lines().filter_map(Line::from_json_line).collect(),
        }
    }

    /// The lines, in order.
    #[must_use]
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// Append a line.
    pub fn push(&mut self, line: Line) {
        self.lines.push(line);
    }

    /// The lines written since the context window last lost what it held —
    /// all of them when it never has.
    #[must_use]
    pub fn live(&self) -> &[Line] {
        let start = self
            .lines
            .iter()
            .rposition(|line| matches!(line, Line::Evict { .. }))
            .map_or(0, |at| at + 1);
        self.lines.get(start..).unwrap_or_default()
    }

    /// The lines of `(path, hash)` this context window was handed and still
    /// holds, from reads alone.
    #[must_use]
    pub fn read_coverage(&self, path: &str, hash: Sha256Id) -> IntervalSet {
        let mut set = IntervalSet::new();
        for line in self.live() {
            if let Line::Read {
                path: p,
                hash: h,
                range,
                ..
            } = line
            {
                if p == path && *h == hash {
                    set.insert(*range);
                }
            }
        }
        set
    }

    /// The lines of `(path, hash)` this context window holds, from reads **and**
    /// from having written the revision itself.
    ///
    /// An agent that wrote a whole file, and whose bytes the file still has,
    /// holds every line of it without having read any: the bytes came out of
    /// its own payload.
    #[must_use]
    pub fn coverage(&self, path: &str, hash: Sha256Id) -> IntervalSet {
        if self.wrote_in_full(path, hash).is_some() {
            return IntervalSet::whole_file();
        }
        self.read_coverage(path, hash)
    }

    /// Everything ever read of `(path, hash)`, across losses of context.
    ///
    /// A loss empties what may be declined, because the window no longer holds
    /// the bytes — but it does not forget what was read, because that record is
    /// exactly the map of what to offer the window back.
    #[must_use]
    pub fn recorded_coverage(&self, path: &str, hash: Sha256Id) -> IntervalSet {
        let mut set = IntervalSet::new();
        for line in &self.lines {
            if let Line::Read {
                path: p,
                hash: h,
                range,
                ..
            } = line
            {
                if p == path && *h == hash {
                    set.insert(*range);
                }
            }
        }
        set
    }

    /// The most recent still-held edit that left the file at `hash` with the
    /// agent holding every line of it.
    #[must_use]
    pub fn wrote_in_full(&self, path: &str, hash: Sha256Id) -> Option<&Line> {
        self.live().iter().rev().find(|line| {
            matches!(
                line,
                Line::Edit { path: p, hash_new, full: true, .. }
                    if p == path && *hash_new == hash
            )
        })
    }

    /// What this window held at the moment it last stopped holding anything.
    ///
    /// This is the recovery map's input and nothing else's: a context that has
    /// just been compacted is about to re-read wholesale, and this is the list
    /// of what it read, so it can be told which of those files still hash to
    /// what it saw. It is deliberately **not** coverage — coverage is what may
    /// be declined, and an emptied window may be declined nothing (a decline
    /// after an emptying would starve a context of bytes it no longer has).
    ///
    /// The stretch it is drawn from is the newest one **before** the last
    /// emptying that has a read in it. A boundary usually announces itself
    /// twice — once before the summary is written and once when the session
    /// starts again from it — and the stretch between those two signals holds
    /// nothing, so taking the last stretch outright would answer "nothing was
    /// held" at exactly the moment the question is asked. Older stretches stay
    /// out: they were recovered at their own boundary, and re-listing them
    /// would spend a compacted turn's budget on a map of a context two
    /// generations gone.
    ///
    /// Ranked by the millisecond of the newest read of each revision, newest
    /// first, so a caller that can only afford a prefix keeps the warm end.
    /// Ties break on the path, so two reads in one millisecond are still one
    /// order.
    #[must_use]
    pub fn held_before_eviction(&self) -> Vec<HeldFile> {
        let mut boundaries: Vec<usize> = self
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| matches!(line, Line::Evict { .. }))
            .map(|(at, _)| at)
            .collect();
        // A window that has never lost anything is not recovering from
        // anything, and a session with no boundary gets no map.
        let Some(mut end) = boundaries.pop() else {
            return Vec::new();
        };
        loop {
            let start = boundaries.last().map_or(0, |at| at + 1);
            let stretch = self.lines.get(start..end).unwrap_or_default();
            if stretch.iter().any(|line| matches!(line, Line::Read { .. })) {
                return held_of(stretch);
            }
            match boundaries.pop() {
                Some(previous) => end = previous,
                None => return Vec::new(),
            }
        }
    }

    /// Whether this exact request has already been declined once since the
    /// context window last lost what it held.
    #[must_use]
    pub fn denied(&self, key: &DenyKey) -> bool {
        self.live().iter().any(|line| {
            matches!(
                line,
                Line::Deny { path, hash, range, .. } if key.matches(path, *hash, *range)
            )
        })
    }

    /// Whether a booking this window has already taken claims **any** of
    /// these lines of this revision.
    ///
    /// The two levers reach one slice from opposite sides, and it is worth
    /// one read whichever side got there first. A decline that comes first
    /// answers the announcement, so no booking can follow it; a booking that
    /// comes first cannot un-book itself, so the decline that follows has to
    /// know to claim nothing. This is what it asks.
    ///
    /// **Overlap, not containment**, and the difference is the whole point.
    /// A decline books the run it was asked for, whole; it has no way to book
    /// the part of it nobody has claimed yet. So a booking of `L1-20` and a
    /// decline of the whole file are two claims that share twenty lines, and
    /// letting the wider one through books those lines twice — one revision
    /// paid for one and a half times. Asking about overlap sends the wider
    /// claim to zero instead: the slice keeps the figure the row that reached
    /// it first gave it, and the lines beyond it go unbooked. That is a
    /// saving nobody books, which is the direction this is allowed to fail
    /// in.
    ///
    /// The revision has to be the same one either way, because a booking
    /// about bytes that have since changed says nothing about the bytes being
    /// asked for now.
    #[must_use]
    pub fn prevented_intersects(&self, path: &str, hash: Sha256Id, range: Interval) -> bool {
        self.live().iter().any(|line| {
            matches!(
                line,
                Line::Prevented { path: p, hash: h, range: r, .. }
                    if p == path && *h == hash && r.intersects(range)
            )
        })
    }

    /// The announcement about `path` that is still standing: Rift said this
    /// window holds these lines of this revision, and nothing since has
    /// answered it.
    ///
    /// This is the whole of the "a read did not happen" test, and each way of
    /// answering an announcement is a way a read *did* happen, or was already
    /// counted:
    ///
    /// * a [`Line::Read`] of the file answers it — the bytes came back, so
    ///   nothing was avoided and the read is the measured fact;
    /// * a [`Line::Observe`] answers it for the same reason: a decline that
    ///   was reached and not made is a call that **ran**, so the bytes
    ///   reached the model exactly as a read's do. The line's name is about
    ///   what Rift did, never about what the tool delivered;
    /// * a [`Line::Deny`] answers it — that call was declined, and the
    ///   decline already books what the slice would have cost. Booking here
    ///   too would count one saving twice;
    /// * a [`Line::Prevented`] answers it — the announcement has been booked
    ///   once, which is all any announcement is worth;
    /// * an emptying answers everything, because [`Self::live`] starts after
    ///   it.
    ///
    /// Anything else — an edit, a search, a read of some other file — leaves
    /// it standing.
    #[must_use]
    pub fn standing_hold(&self, path: &str) -> Option<StandingHold> {
        let mut standing: Option<StandingHold> = None;
        for line in self.live() {
            match line {
                Line::Hold {
                    path: p,
                    hash,
                    range,
                    ..
                } if p == path => {
                    standing = Some(StandingHold {
                        hash: *hash,
                        range: *range,
                    });
                }
                Line::Read { path: p, .. }
                | Line::Observe { path: p, .. }
                | Line::Deny { path: p, .. }
                | Line::Prevented { path: p, .. }
                    if p == path =>
                {
                    standing = None;
                }
                _ => {}
            }
        }
        standing
    }
}

/// An announcement Rift pushed that nothing has answered: the revision it was
/// about, and the lines it said the window held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StandingHold {
    /// The revision the announcement named.
    pub hash: Sha256Id,
    /// The lines it said were held — [`Interval::is_whole_file`] when the
    /// announcement was about the whole of the file.
    pub range: Interval,
}

/// One file a context window held when it last stopped holding what it was
/// given: the revision, the lines, and when it last saw them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldFile {
    /// The file, in the one spelling coverage is keyed by.
    pub path: String,
    /// The revision the window read.
    pub hash: Sha256Id,
    /// The lines it was handed, merged.
    pub held: IntervalSet,
    /// The millisecond of its newest read of that revision.
    pub last_read_at_ms: u64,
}

/// Every revision one stretch of lines was handed, merged and ranked.
fn held_of(stretch: &[Line]) -> Vec<HeldFile> {
    let mut out: Vec<HeldFile> = Vec::new();
    for line in stretch {
        let Line::Read {
            path,
            hash,
            range,
            at_ms,
            ..
        } = line
        else {
            continue;
        };
        match out
            .iter_mut()
            .find(|held| held.path == *path && held.hash == *hash)
        {
            Some(held) => {
                held.held.insert(*range);
                held.last_read_at_ms = held.last_read_at_ms.max(*at_ms);
            }
            None => out.push(HeldFile {
                path: path.clone(),
                hash: *hash,
                held: {
                    let mut set = IntervalSet::new();
                    set.insert(*range);
                    set
                },
                last_read_at_ms: *at_ms,
            }),
        }
    }
    out.sort_by(|a, b| {
        b.last_read_at_ms
            .cmp(&a.last_read_at_ms)
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

/// How far interception is allowed to go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Nothing is decided and nothing is recorded. This is where an
    /// unreadable policy lands, because a lever whose settings cannot be read
    /// is a lever nobody is holding.
    Off,
    /// Everything is decided and recorded, and nothing is acted on — the mode
    /// that sizes the pool before anyone's session changes.
    #[default]
    Observe,
    /// A read the journal proves is already held may be declined, once.
    Enforce,
}

/// Whether a family's own edit tool is pinned to tell the model that the file
/// it just wrote is still current in its context.
///
/// A decline that rests on an `edit` line says, in effect, "you wrote these
/// bytes, so you hold them". That is a claim about the *family's* tooling, and
/// it is true only where a committed artifact says the edit result tells the
/// model so. Where it does not, the verification read is the agent's only
/// sight of its own edit, and taking that away buys a saving with the one
/// thing the read exists to provide — so a decline never rests on an edit line
/// there, in any shape.
///
/// [`Default`] is [`Self::Unpinned`], so a family nobody has pinned yet lands
/// on the safe side by omission rather than by anyone remembering to say so.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EditEcho {
    /// A committed pin quotes this family's edit result telling the model the
    /// file it just wrote is current in its context.
    Pinned,
    /// Nothing pinned says what this family's edit tools return.
    #[default]
    Unpinned,
}

/// The numbers the decision needs that are settings rather than facts.
///
/// They arrive as an argument because where they come from — a policy file,
/// a per-family override, a default — is not this module's business, and a
/// pure decision with a hidden source of truth is not a pure decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Above this many bytes a slice is recorded but never treated as held:
    /// past the point a read was measured to truncate, what the window kept of
    /// an earlier delivery is a page rather than the range, so a record saying
    /// the range arrived is true of the tool and false of the context.
    pub max_slice_bytes: usize,
    /// Below this many lines a slice is decided and recorded but never
    /// declined: the reason frame the model reads instead has a price of its
    /// own, and under some length it costs more than the range it replaces.
    pub min_slice_lines: u32,
    /// How many lines of surrounding context a suggested window adds on each
    /// side of a changed site.
    pub context_lines: u32,
    /// Above this many bytes a slice is not recorded as held at all: what a
    /// window is credited with collapses to the one line the read started on,
    /// the way an unpinned default window already does.
    ///
    /// Different in kind from [`Limits::max_slice_bytes`], and larger numbers
    /// are not safer here. That one bounds what may be **declined**: a slice
    /// over it is still recorded, and forgetting to record would only cost a
    /// saving. This one bounds what may be **recorded**, and a slice over it
    /// that is recorded anyway is a claim that a context holds lines a client
    /// may have cut off — the one direction of error a decline cannot survive.
    pub max_delivered_bytes: usize,
    /// The same bound for a read whose answer came back through the terminal,
    /// where the client's ceiling is a size in characters rather than a token
    /// cap and sits lower. Whichever of the two is tighter is what a shell
    /// read is held to.
    pub max_shell_delivered_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_slice_bytes: crate::DEFAULT_MAX_SLICE_BYTES,
            min_slice_lines: crate::DEFAULT_MIN_SLICE_LINES,
            context_lines: 3,
            max_delivered_bytes: DEFAULT_MAX_DELIVERED_BYTES,
            max_shell_delivered_bytes: DEFAULT_MAX_SHELL_DELIVERED_BYTES,
        }
    }
}

/// A read-shaped call with the two facts an adapter cannot supply — the
/// revision the file is at, and how many lines it has — filled in by whoever
/// had the bytes in hand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedRead {
    /// The file, in the one spelling coverage is keyed by.
    pub path: String,
    /// The revision the bytes hash to now.
    pub hash: Sha256Id,
    /// The lines the tool can have delivered, clamped by the file's length.
    pub range: Interval,
    /// How many lines the file has.
    pub total_lines: u32,
    /// Which channel carried the answer, carried through from the request
    /// because the ceiling a delivery is held to depends on it.
    pub channel: ReadChannel,
}

impl ResolvedRead {
    /// A request resolved against the file it names, or `None` when the range
    /// it asks for is not a range of that file: an empty file, a start past
    /// the end, a start before line one. Those cost nothing to allow and
    /// there is nothing to record about them.
    ///
    /// This is also the one place a window counted from the *bottom* becomes
    /// line numbers. [`crate::ReadSpan::LastLines`] names a count and not a start,
    /// because the start is `total - count + 1` and only here is the total
    /// known — the same division of labour [`crate::HiKind::END_OF_FILE`] already
    /// has, read the other way up. A count of nothing is not a range and is
    /// refused rather than rounded to one line.
    #[must_use]
    pub fn new(request: &ReadRequest, hash: Sha256Id, total_lines: u32) -> Option<Self> {
        if total_lines == 0 {
            return None;
        }
        let (lo, hi) = match request.span {
            crate::ReadSpan::LastLines { count } if count > 0 => {
                (total_lines.saturating_sub(count - 1).max(1), total_lines)
            }
            crate::ReadSpan::LastLines { .. } => return None,
            _ => request.conservative_lines()?,
        };
        if lo == 0 || lo > total_lines {
            return None;
        }
        let hi = hi.min(total_lines);
        Some(Self {
            path: request.path.clone(),
            hash,
            range: Interval::new(lo, hi)?,
            total_lines,
            channel: request.channel,
        })
    }

    /// The run of lines this read may be credited with, given how many bytes
    /// the slice it asked for holds.
    ///
    /// The whole run when the slice is small enough that the client's own cap
    /// cannot have cut it short; otherwise the single line it started on. The
    /// collapse is the same rule an unpinned default window already gets, and
    /// for the same reason: what a record must never do is answer for lines
    /// the model was not handed.
    ///
    /// Which ceiling applies is the read's channel. A terminal answer is held
    /// to the tighter of the two, because a client that spills terminal output
    /// to a file has handed the window a pointer, and a pointer is not the
    /// lines however precisely the command named them.
    #[must_use]
    pub fn delivery_is_complete(&self, slice_bytes: usize, limits: Limits) -> bool {
        // The two ceilings are compared differently because they are
        // different KINDS of number. The tool bound is arithmetic — a measured
        // token cap divided by a measured density — so the value itself is
        // inside what was measured and the whole run at exactly that size is
        // vouchable. The terminal bound is a boundary nobody has stood on: the
        // largest output ever watched arriving inline is 29,999 characters,
        // and results of exactly 30,000 were recorded carrying the client's
        // persisted-output field. So a terminal result is credited in full
        // only STRICTLY under its bound — one character wide, and on the side
        // the record supports.
        match self.channel {
            ReadChannel::Tool => slice_bytes <= limits.max_delivered_bytes,
            ReadChannel::Shell => {
                slice_bytes < limits.max_shell_delivered_bytes
                    && slice_bytes <= limits.max_delivered_bytes
            }
        }
    }

    /// The line range that can be credited under the legacy journal API.
    ///
    /// Hook callers now reject incomplete delivery before recording at all;
    /// the single-line fallback remains for replay compatibility with older
    /// journal records and direct users of this pure helper.
    #[must_use]
    pub fn range_delivered(&self, slice_bytes: usize, limits: Limits) -> Interval {
        if self.delivery_is_complete(slice_bytes, limits) {
            return self.range;
        }
        Interval {
            lo: self.range.lo,
            hi: self.range.lo,
        }
    }

    /// The key a decline of this request is remembered under.
    #[must_use]
    pub fn deny_key(&self) -> DenyKey {
        DenyKey {
            path: self.path.clone(),
            hash: self.hash,
            range: self.range,
        }
    }
}

/// What a family's own tool result states about the delivery it answered.
///
/// A range credited from the bytes is arithmetic over a measured density, and
/// [`Limits::max_delivered_bytes`] is where that arithmetic runs out: past it
/// a slice *may* have been cut short, so the window is credited with nothing.
/// This is the other source, and where it exists it is the better one — the
/// client says outright how much of the file it handed over, so a delivery
/// larger than any density argument could vouch for is nonetheless exactly
/// known.
///
/// Three cases, because three are what a pinned result object can say: the
/// call was answered in full, it was cut short at a named line, or it was cut
/// short and no line was named. **Silence is not one of them.** A result that
/// says nothing about itself is *absent* here rather than complete, which is
/// why every call site holds an `Option<StatedDelivery>` and an absent
/// statement leaves the byte ceiling as the only guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatedDelivery {
    /// The client said the call was answered in full, and named the run it
    /// handed over.
    Complete(Interval),
    /// The client said the call was cut short, and named how far it got.
    Truncated(Interval),
    /// The client said the call was cut short and did not say where, so
    /// nothing past the line the read started on can be vouched for.
    Unstated,
}

/// How a window's coverage has to move to agree with what a result states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconcile {
    /// The record and the statement already agree.
    Unchanged,
    /// The statement names more than the call was credited with, and coverage
    /// is a union, so this run is simply added to it. Nothing is lost and no
    /// boundary is written.
    Credit(Interval),
    /// The statement names less. A union has no subtraction in it, so the
    /// window is emptied and credited again with this run — or with nothing,
    /// where the statement leaves no line of this read standing at all.
    TakeBack(Option<Interval>),
}

/// What one result's own statement does to the coverage its call was credited
/// with.
///
/// **One rule, in both directions.** A statement that a call was answered in
/// full and one that it was cut short name the same thing — the run the client
/// says the window received — and the only difference between them is which
/// side of `recorded` that run falls on. Naming less than the record claims
/// takes lines back; naming more credits them, and that second direction is
/// the one a byte ceiling can never reach on its own: a slice over
/// [`Limits::max_delivered_bytes`] is credited with nothing because *density*
/// cannot vouch for it, and a client stating its own delivery has vouched for
/// it in the one way that is not a guess.
///
/// `recorded` is what the call was credited with before the result arrived —
/// [`ResolvedRead::range_delivered`] over the slice it asked for. Where the
/// ceiling refused that slice outright and nothing was written at all, it is
/// the read's own first line, and every line the statement names past it is a
/// credit rather than a correction.
///
/// Three clamps keep a statement from over-claiming:
///
/// - the run is intersected with [`ResolvedRead::range`], which is already
///   clamped to the file's own length. A client that counts the empty stretch
///   after a final newline as a line states one line more than the bytes hold,
///   and the intersection spends that line on a saving rather than on a claim;
/// - a run with no line in common with what this read could have delivered is
///   no run at all, and the window loses the claim rather than gaining a
///   strange one;
/// - a **terminal** answer is never credited from a statement. What a family
///   publishes is its read tool's result object; a command's result says only
///   that its output went to a file, so the bound a shell read is held to
///   stays [`Limits::max_shell_delivered_bytes`] whatever else arrives.
#[must_use]
pub fn reconcile_delivery(
    read: &ResolvedRead,
    recorded: Interval,
    stated: StatedDelivery,
) -> Reconcile {
    let run = match stated {
        StatedDelivery::Complete(run) | StatedDelivery::Truncated(run) => {
            Interval::new(read.range.lo.max(run.lo), read.range.hi.min(run.hi))
        }
        StatedDelivery::Unstated => Interval::new(recorded.lo, recorded.lo),
    };
    match run {
        None => Reconcile::TakeBack(None),
        Some(run) if run == recorded => Reconcile::Unchanged,
        Some(run) if run.contains(recorded) && read.channel == ReadChannel::Tool => {
            Reconcile::Credit(run)
        }
        Some(run) if run.contains(recorded) => Reconcile::Unchanged,
        Some(run) => Reconcile::TakeBack(Some(run)),
    }
}

/// What a decline would say, when there is one to make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DenyShape {
    /// These lines of this revision are the ones already in the window.
    Plain,
    /// The window holds the previous revision in full and the file changed
    /// only where it says, in the numbering the reader is about to see.
    Delta {
        /// Where the file changed, in the numbering of the current bytes.
        sites: Vec<Interval>,
        /// How many lines the file grew by, negative when it shrank.
        shift: i64,
        /// A range worth reading instead: the changed sites with a few lines
        /// of context around them.
        window: Interval,
    },
}

/// What is to be done about a read-shaped call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Let it run, and record what it will have delivered.
    Allow,
    /// It would have been declined, in this shape, and is not. The window is
    /// untouched; the record sizes what declining would be worth.
    Observe(DenyShape),
    /// Decline it once, with this to say for it.
    Deny(DenyShape),
}

/// Whether a family's channel is one a decline may travel on: an explanation
/// field the family documents **and** a recorded run showing the model
/// receiving it.
///
/// A decline whose reason the model never sees is an unexplained tool
/// failure, and retrying an unexplained failure is reasonable behaviour, so
/// an unconfirmed channel means the decision is recorded and never acted on.
fn reason_reaches_the_model(channel: Option<Channel>) -> bool {
    matches!(
        channel,
        Some(Channel::DenyWithReason {
            confirmed: true,
            ..
        })
    )
}

/// The smallest run holding every changed site.
fn hull(sites: &[Interval]) -> Option<Interval> {
    Interval::new(
        sites.iter().map(|s| s.lo).min()?,
        sites.iter().map(|s| s.hi).max()?,
    )
}

/// A range worth reading instead of the file: the changed sites taken
/// together, widened by the configured context on each side, clamped to the
/// file's own length.
fn suggested_window(hull: Interval, total_lines: u32, context_lines: u32) -> Option<Interval> {
    Interval::new(
        hull.lo.saturating_sub(context_lines).max(1),
        hull.hi.saturating_add(context_lines).min(total_lines),
    )
}

/// Which shape a decline of this read would take, or `None` when it must not
/// be declined at all.
fn shape_of(
    read: &ResolvedRead,
    journal: &Journal,
    limits: Limits,
    echo: EditEcho,
) -> Option<DenyShape> {
    if !journal.read_coverage(&read.path, read.hash).is_empty() {
        return Some(DenyShape::Plain);
    }
    // Nothing of this revision was read, so what the window holds it holds by
    // having written: the file changed since it last read, and the honest
    // thing to say is where.
    let Some(Line::Edit { sites, shift, .. }) = journal.wrote_in_full(&read.path, read.hash) else {
        return None;
    };
    if echo == EditEcho::Unpinned {
        // Unconditional, and deliberately not a mode: a family whose edit
        // result is not pinned to say the model holds what it wrote is never
        // declined on the strength of an edit line — neither with the sites,
        // nor plainly when the edit covered the whole file. The line stays:
        // coverage of the old bytes must still stop being coverage the moment
        // they change. What does not follow from it, here, is a decision.
        return None;
    }
    let span = hull(sites)?;
    if span.lo <= 1 && span.hi >= read.total_lines {
        // The change is the whole file — a write, not an edit. Naming every
        // line as a changed site tells a reader nothing it did not know.
        return Some(DenyShape::Plain);
    }
    let window = suggested_window(span, read.total_lines, limits.context_lines)?;
    if sites.iter().any(|site| site.contains(read.range)) {
        // Reading exactly the lines that changed is the right thing to do.
        return None;
    }
    Some(DenyShape::Delta {
        sites: sites.clone(),
        shift: *shift,
        window,
    })
}

/// What to do about a read-shaped call, given what this context window has
/// already been handed.
///
/// Every gate below is a reason **not** to decline, and they are checked in
/// the order that makes the cheapest and most certain ones first:
///
/// * the lever is off;
/// * the slice is larger than a client keeps inline, so what the window holds
///   of an earlier delivery may be a pointer rather than the bytes;
/// * the request asks for a line the window was not handed;
/// * this exact request was already declined once, so the repeat is honoured
///   — which is what bounds a model that answers a decline by asking again;
/// * the only thing the window holds this revision by is having written it,
///   and this family's edit result is not pinned to say the model holds what
///   it wrote;
/// * there is no honest shape for the decline to take;
/// * the slice is shorter than the length at which a reason frame stops
///   costing more than the range it replaces — the one gate here that still
///   reaches a decision and records it, because the pool a floor forgoes is a
///   figure the window has to be able to publish.
///
/// Only then does the mode and the family's channel decide between acting on
/// the decision and merely recording it. Whether the bytes are text, whether
/// they are small enough to hash inline, and whether the path resolves inside
/// the work tree are the caller's checks: they need the file, and this module
/// never opens one.
#[must_use]
pub fn decide(
    read: &ResolvedRead,
    journal: &Journal,
    mode: Mode,
    channel: Option<Channel>,
    echo: EditEcho,
    slice_bytes: usize,
    limits: Limits,
) -> Decision {
    if mode == Mode::Off || slice_bytes > limits.max_slice_bytes {
        return Decision::Allow;
    }
    if !journal.coverage(&read.path, read.hash).contains(read.range) {
        return Decision::Allow;
    }
    if journal.denied(&read.deny_key()) {
        return Decision::Allow;
    }
    let Some(shape) = shape_of(read, journal, limits, echo) else {
        return Decision::Allow;
    };
    // The floor, last: a slice too short for a decline to pay for itself is
    // still a decision this window made, and recording it is what lets the
    // observe window price the floor before anyone moves it.
    let pays_for_itself = read.range.lines() >= limits.min_slice_lines;
    if mode == Mode::Enforce && pays_for_itself && reason_reaches_the_model(channel) {
        Decision::Deny(shape)
    } else {
        Decision::Observe(shape)
    }
}

/// The line a decision leaves behind.
///
/// Recording happens on every path, including the one where nothing was
/// decided: an allowed read is what a later decision is made of, and a
/// decision that was only observed is what sizes the pool.
///
/// The three lines are not symmetric, which is why the slice's size is an
/// argument here. A `deny` and an `observe` record what was **asked for** —
/// that is what sizes the pool and what a repeat is matched against. A `read`
/// records what the call can have **delivered**, and a slice larger than
/// [`Limits::max_delivered_bytes`] cannot be vouched for past its first line
/// whatever the tool was asked for, so that is all it is credited with.
#[must_use]
pub fn record(
    read: &ResolvedRead,
    decision: &Decision,
    slice_bytes: usize,
    limits: Limits,
    turn: u32,
    at_ms: u64,
) -> Line {
    match decision {
        Decision::Allow => Line::Read {
            turn,
            path: read.path.clone(),
            hash: read.hash,
            range: read.range_delivered(slice_bytes, limits),
            total_lines: read.total_lines,
            at_ms,
        },
        Decision::Observe(_) => Line::Observe {
            turn,
            path: read.path.clone(),
            hash: read.hash,
            range: read.range,
            at_ms,
        },
        Decision::Deny(_) => Line::Deny {
            turn,
            path: read.path.clone(),
            hash: read.hash,
            range: read.range,
            at_ms,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HiKind, ReadChannel, ReadSpan};

    /// A hash with a distinguishable byte pattern, so a mixed-up hash in a
    /// failure message is obvious rather than a wall of zeroes.
    fn hash(seed: u8) -> Sha256Id {
        Sha256Id::from_bytes([seed; 32])
    }

    fn run(lo: u32, hi: u32) -> Interval {
        Interval::new(lo, hi).expect("a test interval is a run")
    }

    /// An announcement line, spelled once for the tests below.
    fn held_hold(path: &str, seed: u8, lo: u32, hi: u32) -> Line {
        Line::Hold {
            turn: 1,
            path: path.to_owned(),
            hash: hash(seed),
            range: run(lo, hi),
            at_ms: 10,
        }
    }

    /// Every announcement kind round-trips through the row it is written as.
    #[test]
    fn an_announcement_and_its_booking_round_trip() {
        for line in [
            held_hold("src/lib.rs", 3, 1, 40),
            Line::Prevented {
                turn: 7,
                path: "src/lib.rs".to_owned(),
                hash: hash(3),
                range: run(1, u32::MAX),
                at_ms: 99,
            },
        ] {
            let row = line.to_json_line();
            assert_eq!(Line::from_json_line(&row), Some(line.clone()), "{row}");
        }
    }

    /// An announcement nothing has answered is the evidence a later action is
    /// weighed against.
    #[test]
    fn an_unanswered_announcement_stands() {
        let journal = Journal::from_lines(vec![
            held_read("src/lib.rs", 3, 1, 40, 1),
            held_hold("src/lib.rs", 3, 1, 40),
        ]);
        assert_eq!(
            journal.standing_hold("src/lib.rs"),
            Some(StandingHold {
                hash: hash(3),
                range: run(1, 40)
            })
        );
        assert_eq!(journal.standing_hold("src/other.rs"), None);
    }

    /// Each of the three answers ends an announcement, and each is a way the
    /// read either happened or was already counted.
    #[test]
    fn a_read_a_decline_or_a_booking_answers_an_announcement() {
        let answers = [
            held_read("src/lib.rs", 3, 1, 40, 20),
            Line::Deny {
                turn: 2,
                path: "src/lib.rs".to_owned(),
                hash: hash(3),
                range: run(1, 40),
                at_ms: 20,
            },
            Line::Prevented {
                turn: 2,
                path: "src/lib.rs".to_owned(),
                hash: hash(3),
                range: run(1, 40),
                at_ms: 20,
            },
        ];
        for answer in answers {
            let journal =
                Journal::from_lines(vec![held_hold("src/lib.rs", 3, 1, 40), answer.clone()]);
            assert_eq!(
                journal.standing_hold("src/lib.rs"),
                None,
                "{} answered nothing",
                answer.kind()
            );
        }
    }

    /// ★ **An observed re-read answers the announcement.** `observe` names
    /// what Rift did — reach a decline and not make it — and never what the
    /// tool did: the call RAN, and the lines it asked for arrived in the
    /// window exactly as a read's would have. An announcement left standing
    /// through one would let the next edit book a read the model had just
    /// been handed.
    #[test]
    fn an_observed_reread_answers_the_announcement() {
        let journal = Journal::from_lines(vec![
            held_read("src/lib.rs", 3, 1, 40, 10),
            held_hold("src/lib.rs", 3, 1, 40),
            Line::Observe {
                turn: 2,
                path: "src/lib.rs".to_owned(),
                hash: hash(3),
                range: run(1, 40),
                at_ms: 20,
            },
        ]);
        assert_eq!(journal.standing_hold("src/lib.rs"), None);
        // And it answers only its own file: the observation says nothing
        // about a file nobody asked for.
        let elsewhere = Journal::from_lines(vec![
            held_hold("src/lib.rs", 3, 1, 40),
            Line::Observe {
                turn: 2,
                path: "src/other.rs".to_owned(),
                hash: hash(3),
                range: run(1, 40),
                at_ms: 20,
            },
        ]);
        assert!(elsewhere.standing_hold("src/lib.rs").is_some());
    }

    /// ★ **A booking is found by any decline that shares a line with it**,
    /// and only where those lines really are the same bytes: a booking of
    /// another revision, of another file, or of lines this request does not
    /// ask for leaves the decline free to book its own slice.
    ///
    /// The narrow-booking-wide-decline case is the one that matters. A
    /// decline books the run it was asked for and nothing smaller, so letting
    /// it through beside a booking of part of that run books those lines
    /// twice. It claims nothing instead.
    #[test]
    fn a_booking_answers_every_decline_that_shares_a_line_with_it() {
        let booked = |lo, hi| {
            Journal::from_lines(vec![Line::Prevented {
                turn: 2,
                path: "src/lib.rs".to_owned(),
                hash: hash(3),
                range: run(lo, hi),
                at_ms: 20,
            }])
        };
        assert!(booked(1, 40).prevented_intersects("src/lib.rs", hash(3), run(10, 20)));
        assert!(booked(1, u32::MAX).prevented_intersects("src/lib.rs", hash(3), run(10, 20)));
        assert!(
            booked(10, 20).prevented_intersects("src/lib.rs", hash(3), run(1, 40)),
            "a narrower booking already has part of a wider slice, and one \
             slice is worth one read"
        );
        // The edges, both ways round: sharing one line is sharing a claim,
        // and stopping one line short is not.
        assert!(booked(1, 10).prevented_intersects("src/lib.rs", hash(3), run(10, 20)));
        assert!(booked(11, 20).prevented_intersects("src/lib.rs", hash(3), run(1, 11)));
        assert!(
            !booked(1, 10).prevented_intersects("src/lib.rs", hash(3), run(11, 20)),
            "disjoint runs are two slices, and each is worth its own read"
        );
        assert!(!booked(11, 20).prevented_intersects("src/lib.rs", hash(3), run(1, 10)));
        assert!(
            !booked(1, 40).prevented_intersects("src/lib.rs", hash(4), run(10, 20)),
            "another revision is other bytes"
        );
        assert!(!booked(1, 40).prevented_intersects("src/other.rs", hash(3), run(10, 20)));
        // And an emptying takes the booking with it: the window no longer
        // holds what it was told it held.
        let mut lines = booked(1, 40).lines().to_vec();
        lines.push(Line::Evict {
            turn: 3,
            reason: EvictReason::Compact,
            at_ms: 30,
        });
        assert!(!Journal::from_lines(lines).prevented_intersects(
            "src/lib.rs",
            hash(3),
            run(10, 20)
        ));
    }

    /// An emptying takes the announcement with it: a window that no longer
    /// holds the bytes was never going to skip the read.
    #[test]
    fn an_emptying_ends_an_announcement() {
        let journal = Journal::from_lines(vec![
            held_hold("src/lib.rs", 3, 1, 40),
            Line::Evict {
                turn: 2,
                reason: EvictReason::Compact,
                at_ms: 30,
            },
        ]);
        assert_eq!(journal.standing_hold("src/lib.rs"), None);
    }

    /// A read line, spelled once for the tests that build a history out of
    /// them.
    fn held_read(path: &str, seed: u8, lo: u32, hi: u32, at_ms: u64) -> Line {
        Line::Read {
            turn: 1,
            path: path.to_owned(),
            hash: hash(seed),
            range: run(lo, hi),
            total_lines: 900,
            at_ms,
        }
    }

    fn evict(reason: EvictReason, at_ms: u64) -> Line {
        Line::Evict {
            turn: 1,
            reason,
            at_ms,
        }
    }

    /// The map's input is the generation that was just lost, whole, ranked
    /// warmest first — and a window that has lost nothing has no map.
    ///
    /// The double signal is the case that makes this more than a filter: a
    /// compaction announces itself before the summary is written and again
    /// when the session resumes from it, and nothing is read in between. The
    /// stretch between those two is empty, so an implementation that took the
    /// last stretch outright would answer "you held nothing" at the exact
    /// moment a compacted context asks what it held.
    #[test]
    fn the_map_is_the_generation_the_last_eviction_ended() {
        let mut journal = Journal::default();
        assert!(
            journal.held_before_eviction().is_empty(),
            "a fresh window is recovering from nothing"
        );
        journal.push(held_read("src/lib.rs", 0x3f, 1, 500, 10));
        assert!(
            journal.held_before_eviction().is_empty(),
            "a window that still holds its reads is not recovering either"
        );

        journal.push(held_read("src/hooks.rs", 0x40, 20, 30, 20));
        journal.push(held_read("src/lib.rs", 0x3f, 400, 600, 30));
        journal.push(evict(EvictReason::PreCompact, 40));
        journal.push(evict(EvictReason::Compact, 41));

        let held = journal.held_before_eviction();
        assert_eq!(
            held.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(),
            ["src/lib.rs", "src/hooks.rs"],
            "newest read first"
        );
        assert_eq!(
            held[0].held.runs(),
            [run(1, 600)],
            "two reads of one revision are the union of both"
        );
        assert_eq!(held[0].last_read_at_ms, 30);
        assert_eq!(held[1].hash, hash(0x40));

        // The generation after the boundary is its own: what it read is what
        // the NEXT boundary hands back, and the recovered generation is gone.
        journal.push(held_read("src/only-now.rs", 0x41, 1, 9, 50));
        journal.push(evict(EvictReason::Compact, 60));
        assert_eq!(
            journal
                .held_before_eviction()
                .iter()
                .map(|h| h.path.as_str())
                .collect::<Vec<_>>(),
            ["src/only-now.rs"],
            "a map of a context two generations gone is spend, not recovery"
        );
    }

    /// One revision per entry, never one file: a window that read a file,
    /// watched it change and read it again held two different sets of lines,
    /// and telling it "you hold lines 1-500" of a revision it no longer has
    /// is the one thing the map must never say.
    #[test]
    fn a_file_read_at_two_revisions_is_two_entries() {
        let mut journal = Journal::default();
        journal.push(held_read("src/lib.rs", 0x3f, 1, 10, 10));
        journal.push(held_read("src/lib.rs", 0x40, 1, 20, 20));
        journal.push(evict(EvictReason::Resume, 30));
        let held = journal.held_before_eviction();
        assert_eq!(held.len(), 2);
        assert_eq!(held[0].hash, hash(0x40), "the newer revision leads");
        assert_eq!(held[0].held.runs(), [run(1, 20)]);
        assert_eq!(held[1].hash, hash(0x3f));
        assert_eq!(held[1].held.runs(), [run(1, 10)]);
    }

    /// The map lists what was **read**, and a decline or an observation is not
    /// a read: the bytes of a call that was answered instead of run never
    /// reached the window.
    #[test]
    fn only_reads_are_held() {
        let mut journal = Journal::from_lines(one_of_each());
        journal.push(evict(EvictReason::Clear, 99));
        let held = journal.held_before_eviction();
        assert_eq!(held.len(), 1, "{held:?}");
        assert_eq!(held[0].path, "src/lib.rs");
        assert_eq!(held[0].held.runs(), [run(1, 500)]);
    }

    /// The property the whole boundary rests on: a decline is never re-armed
    /// by what the window held before it lost it. The map is drawn from those
    /// same lines, so this states both halves at once — the lines are still
    /// there to be listed, and they satisfy nothing.
    #[test]
    fn deny_is_not_rearmed_for_pre_eviction_coverage() {
        let mut journal = Journal::default();
        journal.push(held_read("src/lib.rs", 0x3f, 1, 500, 10));
        assert!(
            journal
                .coverage("src/lib.rs", hash(0x3f))
                .contains(run(1, 500)),
            "before the boundary the window holds the lines"
        );
        journal.push(evict(EvictReason::Compact, 20));

        assert!(
            journal.coverage("src/lib.rs", hash(0x3f)).is_empty(),
            "after it, nothing may be declined"
        );
        assert_eq!(
            journal.held_before_eviction()[0].held.runs(),
            [run(1, 500)],
            "and the same lines are exactly what the map lists"
        );

        // The re-read is a first read again, and re-arms the decline itself.
        journal.push(held_read("src/lib.rs", 0x3f, 1, 500, 30));
        assert!(journal
            .coverage("src/lib.rs", hash(0x3f))
            .contains(run(1, 500)));
    }

    #[test]
    fn a_run_starts_at_one_and_ends_at_or_after_its_start() {
        assert_eq!(Interval::new(0, 4), None, "line zero is not a line");
        assert_eq!(Interval::new(9, 8), None, "an end before its start");
        assert_eq!(run(3, 3).lines(), 1);
        assert_eq!(run(1, 10).lines(), 10);
        assert!(run(1, 10).contains(run(3, 4)));
        assert!(!run(1, 10).contains(run(3, 11)));
    }

    /// One line of every kind, for the assertions that walk them all.
    fn one_of_each() -> Vec<Line> {
        vec![
            Line::Read {
                turn: 2,
                path: "src/lib.rs".to_owned(),
                hash: hash(0x3f),
                range: run(1, 500),
                total_lines: 900,
                at_ms: 1_700_000_000_000,
            },
            Line::Deny {
                turn: 3,
                path: "src/lib.rs".to_owned(),
                hash: hash(0x3f),
                range: run(1, 500),
                at_ms: 12,
            },
            Line::Observe {
                turn: 4,
                path: "a b/c.rs".to_owned(),
                hash: hash(0x01),
                range: run(7, 7),
                at_ms: 13,
            },
            Line::Expect {
                turn: 5,
                path: "src/new.rs".to_owned(),
                hash: hash(0xab),
                at_ms: 14,
            },
            Line::Edit {
                turn: 6,
                path: "src/lib.rs".to_owned(),
                hash_old: Some(hash(0x3f)),
                hash_new: hash(0x40),
                sites: vec![run(37, 44), run(80, 81)],
                shift: -8,
                full: true,
                at_ms: 15,
            },
            Line::Edit {
                turn: 7,
                path: "src/new.rs".to_owned(),
                hash_old: None,
                hash_new: hash(0xab),
                sites: vec![],
                shift: 0,
                full: false,
                at_ms: 16,
            },
            Line::Evict {
                turn: 8,
                reason: EvictReason::PreCompact,
                at_ms: 17,
            },
        ]
    }

    #[test]
    fn every_kind_survives_a_round_trip() {
        for line in one_of_each() {
            let row = line.to_json_line();
            assert!(!row.contains('\n'), "a row is one line: {row}");
            assert_eq!(
                Line::from_json_line(&row),
                Some(line.clone()),
                "round trip of {row}"
            );
        }
    }

    #[test]
    fn a_row_carries_no_bytes_of_the_file_it_names() {
        // Nothing in the schema has a place to put content, and this is what
        // keeps it that way: a row is paths, hashes and numbers, and a new
        // field has to be added here before it can be added there.
        for line in one_of_each() {
            let row = line.to_json_line();
            let value: Value = serde_json::from_str(&row).expect("a row is JSON");
            for key in value.as_object().expect("a row is an object").keys() {
                assert!(
                    matches!(
                        key.as_str(),
                        "v" | "k"
                            | "t"
                            | "p"
                            | "h"
                            | "h_old"
                            | "h_new"
                            | "lo"
                            | "hi"
                            | "n"
                            | "at"
                            | "sites"
                            | "shift"
                            | "full"
                            | "reason"
                    ),
                    "an unexpected field {key} in {row}"
                );
            }
        }
    }

    #[test]
    fn a_hash_is_written_as_sixty_four_lowercase_hex_characters() {
        let row: Value = serde_json::from_str(
            &Line::Expect {
                turn: 1,
                path: "p".to_owned(),
                hash: hash(0xab),
                at_ms: 2,
            }
            .to_json_line(),
        )
        .expect("a row is JSON");
        let written = row["h"].as_str().expect("the hash is a string");
        assert_eq!(written.len(), 64);
        assert_eq!(written, written.to_lowercase());
    }

    #[test]
    fn every_reason_a_window_can_lose_its_context_for_has_one_spelling() {
        for reason in [
            EvictReason::PreCompact,
            EvictReason::Compact,
            EvictReason::Clear,
            EvictReason::Resume,
            EvictReason::PostCompact,
            EvictReason::Truncate,
        ] {
            assert_eq!(EvictReason::from_str(reason.as_str()), Some(reason));
        }
        assert_eq!(
            EvictReason::from_str("garbage-collected"),
            None,
            "a token this version does not know is not guessed at"
        );
    }

    /// **The bound on what may be recorded as held is the measured cap divided
    /// by the smallest measured density, and it is not the slice ceiling.**
    ///
    /// The recorded observation is a 25,000-**token** truncation cap on a read
    /// result. Bytes are not tokens: the slices this repository's own fixtures
    /// price run 1.90 to 2.22 bytes a token, and dividing at the smallest of
    /// those is what makes the bound hold for the densest text rather than for
    /// the average. Spelling the arithmetic here rather than the answer is the
    /// point — a raise has to change one of the two measured numbers.
    #[test]
    fn the_delivery_bound_is_the_measured_cap_at_the_smallest_measured_density() {
        let measured_read_token_cap: usize = "25000".parse().expect("the pin states a decimal");
        // 1.90 bytes a token, in tenths, so the arithmetic stays in integers.
        let smallest_measured_density_tenths = 19;
        assert_eq!(
            DEFAULT_MAX_DELIVERED_BYTES,
            measured_read_token_cap * smallest_measured_density_tenths / 10
        );
        assert!(
            DEFAULT_MAX_DELIVERED_BYTES < measured_read_token_cap * 4,
            "the slice ceiling divides the same cap at four bytes a token; \
             what may be recorded as held is the tighter of the two bounds, \
             because a ceiling derived at a density the text does not have is \
             no bound on what a read delivered"
        );
    }

    /// ★ **A shell read past the persistence threshold is credited with one
    /// line, whatever its command line claimed.**
    ///
    /// `sed -n '1,3000p'` names three thousand lines and the recogniser
    /// records them as an explicit range, which is right about the command
    /// and wrong about the window: past the measured threshold this client
    /// does not hand the model the output at all, it writes it to a file and
    /// hands over the path. The same bytes through a read tool are under the
    /// tool ceiling and stay whole — which is the point of keeping the two
    /// ceilings apart rather than taking the tighter one everywhere and
    /// giving up savings a read tool actually earned.
    #[test]
    fn a_shell_read_past_the_persistence_threshold_grants_one_line() {
        let limits = Limits::default();
        let slice = |channel| {
            ResolvedRead::new(
                &ReadRequest {
                    path: "src/long.rs".to_owned(),
                    span: ReadSpan::lines(1, HiKind::Explicit(3_000)),
                    channel,
                },
                hash(7),
                3_000,
            )
            .expect("a resolved read")
        };
        // A ~35 KB slice: over the shell threshold, under the tool ceiling.
        let bytes = 35_000;
        assert!(bytes > DEFAULT_MAX_SHELL_DELIVERED_BYTES);
        assert!(bytes < DEFAULT_MAX_DELIVERED_BYTES);
        assert_eq!(
            slice(ReadChannel::Shell).range_delivered(bytes, limits),
            run(1, 1),
            "the window holds a filename and a preview, not three thousand lines"
        );
        assert_eq!(
            slice(ReadChannel::Tool).range_delivered(bytes, limits),
            run(1, 3_000),
            "the same bytes inline are the lines they say they are"
        );
        // The recorded line follows, because that is the one kind of line
        // that grants a later decline something to rest on.
        assert_eq!(
            record(
                &slice(ReadChannel::Shell),
                &Decision::Allow,
                bytes,
                limits,
                1,
                1
            ),
            Line::Read {
                turn: 1,
                path: "src/long.rs".to_owned(),
                hash: hash(7),
                range: run(1, 1),
                total_lines: 3_000,
                at_ms: 1,
            }
        );
        // One character under the threshold is the largest output anyone has
        // watched arrive inline, and it is credited whole. AT the threshold
        // nobody has: every recorded result of exactly that size came back as
        // a filename, so that is where the credit stops.
        assert_eq!(
            slice(ReadChannel::Shell)
                .range_delivered(DEFAULT_MAX_SHELL_DELIVERED_BYTES - 1, limits),
            run(1, 3_000)
        );
        assert_eq!(
            slice(ReadChannel::Shell).range_delivered(DEFAULT_MAX_SHELL_DELIVERED_BYTES, limits),
            run(1, 1),
            "the bound itself was never seen delivered whole"
        );
    }

    /// The shell bound is the measured boundary, and it binds a shell read
    /// even when a policy raises the other one.
    ///
    /// The spike walked the boundary in thousand-character steps: 29,999
    /// characters inline, 30,999 persisted to a file. The bound is the
    /// smaller side, which is the largest size anyone watched arrive whole.
    #[test]
    fn the_shell_bound_is_the_measured_persistence_threshold() {
        let largest_seen_inline: usize = "29999".parse().expect("the pin states a decimal");
        let smallest_seen_persisted: usize = "30999".parse().expect("the pin states a decimal");
        assert_eq!(
            largest_seen_inline,
            DEFAULT_MAX_SHELL_DELIVERED_BYTES - 1,
            "the bound sits one character above the largest output seen whole, \
             and the comparison against it is strict"
        );
        assert!(DEFAULT_MAX_SHELL_DELIVERED_BYTES < smallest_seen_persisted);
        const {
            assert!(
                DEFAULT_MAX_SHELL_DELIVERED_BYTES < DEFAULT_MAX_DELIVERED_BYTES,
                "the terminal ceiling is the tighter of the two"
            );
        }
        // A policy that raises the tool ceiling does not raise this one.
        let raised = Limits {
            max_delivered_bytes: 1_000_000,
            ..Limits::default()
        };
        let read = ResolvedRead::new(
            &ReadRequest {
                path: "src/long.rs".to_owned(),
                span: ReadSpan::lines(10, HiKind::Explicit(3_000)),
                channel: ReadChannel::Shell,
            },
            hash(8),
            3_000,
        )
        .expect("a resolved read");
        assert_eq!(read.range_delivered(500_000, raised), run(10, 10));
    }

    /// A slice inside the bound is credited with every line it asked for; one
    /// byte over it is credited with the line it started on and nothing after,
    /// whatever the tool was asked for.
    #[test]
    fn a_slice_past_the_delivery_bound_is_credited_with_one_line() {
        let read = ResolvedRead::new(
            &ReadRequest {
                path: "src/big.rs".to_owned(),
                span: ReadSpan::lines(40, HiKind::Explicit(900)),
                channel: ReadChannel::Tool,
            },
            hash(1),
            900,
        )
        .expect("a resolved read");
        let limits = Limits {
            max_delivered_bytes: 1_000,
            ..Limits::default()
        };
        assert_eq!(read.range_delivered(1_000, limits), run(40, 900));
        assert_eq!(read.range_delivered(1_001, limits), run(40, 40));
        // The line the record leaves behind follows the same bound, and only
        // for the kind of line that grants coverage.
        assert_eq!(
            record(&read, &Decision::Allow, 1_001, limits, 1, 1),
            Line::Read {
                turn: 1,
                path: "src/big.rs".to_owned(),
                hash: hash(1),
                range: run(40, 40),
                total_lines: 900,
                at_ms: 1,
            }
        );
        let Line::Deny { range, .. } = record(
            &read,
            &Decision::Deny(DenyShape::Plain),
            1_001,
            limits,
            1,
            1,
        ) else {
            panic!("a declined call is recorded as a decline");
        };
        assert_eq!(
            range,
            run(40, 900),
            "a decline records what was asked for: that is what sizes the pool"
        );
    }

    /// ★ **A truncated read grants no coverage past what was delivered.**
    ///
    /// The correction as the gate makes it, in the pure core: the window is
    /// emptied and credited again with the lines the result stated. What it
    /// held of the same file before is gone, which costs a saving; what it
    /// never received is gone too, which is the point.
    #[test]
    fn a_truncated_read_grants_no_coverage_past_what_was_delivered() {
        let mut journal = Journal::from_lines(vec![held_read("src/a.rs", 1, 1, 900, 1)]);
        assert!(journal
            .coverage("src/a.rs", hash(1))
            .contains(run(500, 900)));
        journal.push(Line::Evict {
            turn: 2,
            reason: EvictReason::Truncate,
            at_ms: 2,
        });
        journal.push(held_read("src/a.rs", 1, 1, 40, 3));
        let coverage = journal.coverage("src/a.rs", hash(1));
        assert!(coverage.contains(run(1, 40)), "the delivery is held");
        assert!(
            !coverage.contains(run(41, 41)),
            "and the first line past it is not"
        );
        assert!(
            journal
                .recorded_coverage("src/a.rs", hash(1))
                .contains(run(500, 900)),
            "what was recorded is still recorded: a correction to coverage is \
             not a rewriting of the journal"
        );
    }

    #[test]
    fn a_row_naming_another_version_is_not_read() {
        let mut row: Value = serde_json::from_str(
            &Line::Evict {
                turn: 1,
                reason: EvictReason::Clear,
                at_ms: 2,
            }
            .to_json_line(),
        )
        .unwrap();
        row["v"] = json!(LINE_VERSION + 1);
        assert_eq!(Line::from_json_line(&row.to_string()), None);
    }

    #[test]
    fn a_hash_that_is_not_sixty_four_lowercase_hex_drops_its_line() {
        let good = Line::Read {
            turn: 1,
            path: "p".to_owned(),
            hash: hash(0x0a),
            range: run(1, 2),
            total_lines: 2,
            at_ms: 3,
        }
        .to_json_line();
        assert!(Line::from_json_line(&good).is_some());
        for spelling in [
            "0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A0A",
            "0a0a",
            "",
            "zz0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a",
        ] {
            let mut row: Value = serde_json::from_str(&good).unwrap();
            row["h"] = json!(spelling);
            assert_eq!(
                Line::from_json_line(&row.to_string()),
                None,
                "accepted the hash spelling {spelling}"
            );
        }
    }

    #[test]
    fn a_site_that_is_not_a_run_drops_its_edit_line() {
        let good = Line::Edit {
            turn: 1,
            path: "p".to_owned(),
            hash_old: None,
            hash_new: hash(0x0b),
            sites: vec![run(4, 9)],
            shift: 0,
            full: true,
            at_ms: 3,
        }
        .to_json_line();
        assert!(Line::from_json_line(&good).is_some());
        for sites in [json!([[9, 4]]), json!([[0, 4]]), json!([[1]]), json!("all")] {
            let mut row: Value = serde_json::from_str(&good).unwrap();
            row["sites"] = sites.clone();
            assert_eq!(
                Line::from_json_line(&row.to_string()),
                None,
                "accepted the site list {sites}"
            );
        }
    }

    #[test]
    fn an_unknown_kind_or_eviction_reason_is_not_read() {
        let mut row: Value = serde_json::from_str(
            &Line::Evict {
                turn: 1,
                reason: EvictReason::Resume,
                at_ms: 2,
            }
            .to_json_line(),
        )
        .unwrap();
        row["reason"] = json!("garbage-collected");
        assert_eq!(Line::from_json_line(&row.to_string()), None);
        row["reason"] = json!("resume");
        row["k"] = json!("forget");
        assert_eq!(Line::from_json_line(&row.to_string()), None);
    }

    /// A read line over a nine-hundred-line file, for the fixtures below.
    fn read_line(turn: u32, path: &str, seed: u8, lo: u32, hi: u32) -> Line {
        Line::Read {
            turn,
            path: path.to_owned(),
            hash: hash(seed),
            range: run(lo, hi),
            total_lines: 900,
            at_ms: u64::from(turn),
        }
    }

    #[test]
    fn runs_that_meet_end_to_end_are_one_run() {
        let mut set = IntervalSet::new();
        set.insert(run(6, 10));
        set.insert(run(1, 5));
        assert_eq!(set.runs(), [run(1, 10)], "1-5 and 6-10 leave no gap");
        assert!(set.contains(run(1, 10)));

        let mut gapped = IntervalSet::new();
        gapped.insert(run(1, 5));
        gapped.insert(run(7, 10));
        assert_eq!(gapped.runs(), [run(1, 5), run(7, 10)], "line 6 is missing");
        assert!(!gapped.contains(run(1, 10)));
        gapped.insert(run(6, 6));
        assert_eq!(gapped.runs(), [run(1, 10)], "line 6 closes the gap");
    }

    #[test]
    fn malformed_lines_are_dropped_on_read() {
        let good = read_line(1, "src/lib.rs", 0x3f, 1, 50);
        let text = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            good.to_json_line(),
            "",
            "{not json at all",
            r#"{"v":1,"k":"read","t":2,"p":"src/lib.rs","h":"nothex","lo":1,"hi":9,"n":9,"at":1}"#,
            r#"{"v":2,"k":"evict","t":3,"reason":"clear","at":1}"#,
            // a torn final append, cut mid-write
            r#"{"v":1,"k":"read","t":4,"p":"src/li"#,
        );
        let journal = Journal::parse(&text);
        assert_eq!(journal.lines(), [good], "only the well-formed row survives");
        assert!(
            journal
                .coverage("src/lib.rs", hash(0x3f))
                .contains(run(1, 50)),
            "and it still carries its coverage"
        );
    }

    #[test]
    fn a_write_of_the_whole_file_covers_it_without_a_read() {
        let journal = Journal::from_lines(vec![Line::Edit {
            turn: 1,
            path: "src/new.rs".to_owned(),
            hash_old: None,
            hash_new: hash(0x11),
            sites: vec![run(1, 40)],
            shift: 40,
            full: true,
            at_ms: 1,
        }]);
        assert!(journal.read_coverage("src/new.rs", hash(0x11)).is_empty());
        assert!(journal
            .coverage("src/new.rs", hash(0x11))
            .contains(run(1, 40)));
        assert!(
            journal
                .coverage("src/new.rs", hash(0x11))
                .contains(run(900, 1000)),
            "every line of it, however long the file turns out to be"
        );
    }

    #[test]
    fn an_edit_that_grants_no_coverage_grants_none() {
        let journal = Journal::from_lines(vec![Line::Edit {
            turn: 1,
            path: "src/lib.rs".to_owned(),
            hash_old: Some(hash(0x3f)),
            hash_new: hash(0x40),
            sites: vec![],
            shift: 0,
            full: false,
            at_ms: 1,
        }]);
        assert!(journal.coverage("src/lib.rs", hash(0x40)).is_empty());
        assert!(journal.wrote_in_full("src/lib.rs", hash(0x40)).is_none());
    }

    #[test]
    fn a_decline_is_remembered_for_its_exact_request_only() {
        let key = |lo, hi| DenyKey {
            path: "src/lib.rs".to_owned(),
            hash: hash(0x3f),
            range: run(lo, hi),
        };
        let journal = Journal::from_lines(vec![Line::Deny {
            turn: 3,
            path: "src/lib.rs".to_owned(),
            hash: hash(0x3f),
            range: run(1, 50),
            at_ms: 3,
        }]);
        assert!(journal.denied(&key(1, 50)));
        assert!(
            !journal.denied(&key(1, 51)),
            "a wider request is its own key"
        );
        assert!(!journal.denied(&key(2, 50)), "so is a narrower one");
    }

    proptest::proptest! {
        /// ★ Coverage never answers for a revision it was not recorded against.
        /// Whatever was read of one revision, the next revision of the same file
        /// starts with nothing held — which is what makes a line number recorded
        /// before a change harmless after it.
        #[test]
        fn a_hash_change_empties_coverage(
            reads in proptest::collection::vec((1u32..=800, 1u32..=100), 1..8),
            other_seed in 1u8..=255,
        ) {
            let lines: Vec<Line> = reads
                .iter()
                .enumerate()
                .map(|(i, (lo, span))| read_line(i as u32, "src/lib.rs", 0, *lo, lo + span))
                .collect();
            let journal = Journal::from_lines(lines);

            proptest::prop_assert!(
                !journal.coverage("src/lib.rs", hash(0)).is_empty(),
                "the revision that was read holds something"
            );
            proptest::prop_assert!(
                journal.coverage("src/lib.rs", hash(other_seed)).is_empty(),
                "and every other revision holds nothing"
            );
            proptest::prop_assert!(
                journal.coverage("other/file.rs", hash(0)).is_empty(),
                "as does every other path"
            );
        }

        /// ★ Losing the context empties what may be declined, and keeps what may
        /// be offered back. The two questions have different answers on purpose:
        /// a window that no longer holds the bytes must be allowed to read them,
        /// and the record of what it read is the map of what to offer it.
        #[test]
        fn an_eviction_empties_coverage_for_deny_and_keeps_it_for_the_map(
            lo in 1u32..=800,
            span in 1u32..=99,
            reason_pick in 0usize..5,
        ) {
            let reasons = [
                EvictReason::PreCompact,
                EvictReason::Compact,
                EvictReason::Clear,
                EvictReason::Resume,
                EvictReason::PostCompact,
            ];
            let range = run(lo, lo + span);
            let decline = |turn: u32| Line::Deny {
                turn,
                path: "src/lib.rs".to_owned(),
                hash: hash(0x3f),
                range,
                at_ms: u64::from(turn),
            };
            let lost = |turn: u32, pick: usize| Line::Evict {
                turn,
                reason: reasons[pick % reasons.len()],
                at_ms: u64::from(turn),
            };
            // Two losses, with a read and a decline between them: only the
            // last loss may bound what is still held. A journal that starts
            // reading at the first one would find both of the middle lines
            // and answer as though the window still held them.
            let journal = Journal::from_lines(vec![
                read_line(1, "src/lib.rs", 0x3f, range.lo, range.hi),
                decline(2),
                lost(3, reason_pick),
                read_line(4, "src/lib.rs", 0x3f, range.lo, range.hi),
                decline(5),
                lost(6, reason_pick + 1),
            ]);

            proptest::prop_assert!(
                journal.coverage("src/lib.rs", hash(0x3f)).is_empty(),
                "nothing is held once the window lost it"
            );
            proptest::prop_assert!(
                journal.recorded_coverage("src/lib.rs", hash(0x3f)).contains(range),
                "but the record of the read survives for the map"
            );
            proptest::prop_assert!(
                !journal.denied(&DenyKey {
                    path: "src/lib.rs".to_owned(),
                    hash: hash(0x3f),
                    range,
                }),
                "and the earlier decline is forgotten with it"
            );
        }
    }

    /// The one channel a decline may travel on: documented **and** observed
    /// reaching the model.
    const CONFIRMED: Channel = Channel::DenyWithReason {
        reason_field: "permissionDecisionReason",
        confirmed: true,
    };

    /// The same channel before anyone watched a model receive it.
    const UNCONFIRMED: Channel = Channel::DenyWithReason {
        reason_field: "permissionDecisionReason",
        confirmed: false,
    };

    fn request(path: &str, lo: u32, hi_kind: HiKind) -> ReadRequest {
        ReadRequest {
            path: path.to_owned(),
            span: ReadSpan::lines(lo, hi_kind),
            channel: ReadChannel::Tool,
        }
    }

    fn resolved(path: &str, seed: u8, lo: u32, hi: u32, total_lines: u32) -> ResolvedRead {
        ResolvedRead::new(
            &request(path, lo, HiKind::Explicit(hi)),
            hash(seed),
            total_lines,
        )
        .expect("a test read is a range of its file")
    }

    /// **A window counted from the bottom becomes line numbers exactly once**,
    /// here, where the file's length is already in hand for the hash.
    ///
    /// The three cases are the whole contract: a count inside the file is the
    /// run ending at its last line; a count past the file's end is the whole
    /// file and never a line before its first; and a count of nothing is not a
    /// range at all and is refused rather than rounded up to one line.
    #[test]
    fn a_window_counted_from_the_end_resolves_against_the_files_own_length() {
        let from_end = |count: u32| ReadRequest {
            path: "src/lib.rs".to_owned(),
            span: ReadSpan::LastLines { count },
            channel: ReadChannel::Shell,
        };
        let range = |count: u32, total: u32| {
            ResolvedRead::new(&from_end(count), hash(1), total).map(|read| read.range)
        };
        assert_eq!(range(40, 400).map(|r| (r.lo, r.hi)), Some((361, 400)));
        assert_eq!(range(1, 400).map(|r| (r.lo, r.hi)), Some((400, 400)));
        assert_eq!(range(400, 400).map(|r| (r.lo, r.hi)), Some((1, 400)));
        assert_eq!(range(9_000, 400).map(|r| (r.lo, r.hi)), Some((1, 400)));
        assert_eq!(range(u32::MAX, 400).map(|r| (r.lo, r.hi)), Some((1, 400)));
        assert_eq!(range(0, 400), None);
        assert_eq!(range(40, 0), None);
        // The request on its own still names no lines: the whole point of the
        // variant is that only this step may say which they are.
        assert_eq!(from_end(40).conservative_lines(), None);
    }

    /// ★ **A key already declined is never declined a second time**, however
    /// many times the same call is made.
    ///
    /// A3 §1 is a loop: a model that answers a decline by making the identical
    /// call again must not meet the same decline, or the conversation cannot
    /// leave the turn. The journal's memory of the decline is tested above;
    /// this is the rule that memory exists for — `decide` consulting it — and
    /// it is the test that reddens when that consultation is short-circuited.
    ///
    /// Both directions are asserted, because a guard that answered `Allow` for
    /// everything would satisfy the first half alone: a DIFFERENT key over the
    /// very same coverage is still declined, so what the guard remembers is
    /// one request and not one window.
    #[test]
    fn a_key_already_declined_is_never_declined_a_second_time() {
        let held = |lines: Vec<Line>| Journal::from_lines(lines);
        let decide_now = |journal: &Journal, read: &ResolvedRead| {
            decide(
                read,
                journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                1_000,
                Limits::default(),
            )
        };
        let repeat = resolved("src/lib.rs", 0x3f, 1, 50, 50);
        let narrower = resolved("src/lib.rs", 0x3f, 1, 40, 50);
        let mut lines = vec![read_line(1, "src/lib.rs", 0x3f, 1, 50)];

        // The first repeat is what a decline is for.
        let first = decide_now(&held(lines.clone()), &repeat);
        assert!(
            matches!(first, Decision::Deny(_)),
            "the repeat of a range the window holds is declined: {first:?}"
        );

        // The line that decision leaves behind is the whole mechanism: from
        // here on the same key is answered by the guard, not by coverage.
        lines.push(record(&repeat, &first, 1_000, Limits::default(), 2, 2));
        for again in 1..=3 {
            assert_eq!(
                decide_now(&held(lines.clone()), &repeat),
                Decision::Allow,
                "call {again} after the decline runs; the window is never held \
                 in a loop of its own answer"
            );
        }

        // And the guard is one key wide, not one window wide.
        let other = decide_now(&held(lines.clone()), &narrower);
        assert!(
            matches!(other, Decision::Deny(_)),
            "a different request over the same coverage is its own key: {other:?}"
        );
    }

    #[test]
    fn a_range_that_is_not_a_range_of_the_file_resolves_to_nothing() {
        assert_eq!(
            ResolvedRead::new(&request("p", 1, HiKind::Explicit(10)), hash(1), 0),
            None,
            "an empty file"
        );
        assert_eq!(
            ResolvedRead::new(&request("p", 40, HiKind::Explicit(50)), hash(1), 30),
            None,
            "a start past the end"
        );
        let clamped = ResolvedRead::new(&request("p", 5, HiKind::Explicit(500)), hash(1), 30)
            .expect("a start inside the file resolves");
        assert_eq!(clamped.range, run(5, 30), "the end is clamped to the file");
    }

    /// **A limit-less read holds the whole file it was delivered.**
    ///
    /// The one place the two halves of the rule meet: an adapter that knows
    /// the delivery runs to the end of the file states that and nothing more,
    /// and this step — the only one with the bytes in hand — turns it into the
    /// file's own last line and then asks whether a delivery that size can be
    /// vouched for at all.
    ///
    /// Derived from the recorded spike behind the Claude Code tool-field pin
    /// (observation O7): six limit-less reads of files from 25,000 to 100,000
    /// bytes each came back with `startLine` 1 and `numLines` equal to the
    /// file's own `totalLines`, and none of them carried the truncation flag
    /// that O5 pins for the reads that stopped short. So the whole file is
    /// what such a call delivers, up to the size at which the client's token
    /// cap could have cut it — and that size, not a line count, is the bound.
    #[test]
    fn a_limit_less_read_holds_the_whole_file_it_was_delivered() {
        let whole = ResolvedRead::new(
            &request("src/lib.rs", 1, HiKind::DefaultWindow(HiKind::END_OF_FILE)),
            hash(3),
            900,
        )
        .expect("the start is inside the file");
        assert_eq!(whole.range, run(1, 900), "the file's own extent");

        // Under the delivery bound, that whole extent is what the window is
        // credited with — the line a later decision is made of.
        let bytes = DEFAULT_MAX_DELIVERED_BYTES;
        let line = record(&whole, &Decision::Allow, bytes, Limits::default(), 0, 1);
        let Line::Read { range, .. } = &line else {
            panic!("an allowed read records a read line, got {line:?}");
        };
        assert_eq!(*range, run(1, 900));

        // And it answers for the repeat that follows, which is the whole point
        // of recording it: the same call again is inside coverage.
        let journal = Journal::from_lines(vec![line]);
        assert!(matches!(
            decide(
                &whole,
                &journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                bytes,
                Limits::default()
            ),
            Decision::Deny(_)
        ));

        // An offset with no cap is the same delivery starting later.
        let tail = ResolvedRead::new(
            &request(
                "src/lib.rs",
                700,
                HiKind::DefaultWindow(HiKind::END_OF_FILE),
            ),
            hash(3),
            900,
        )
        .expect("the start is inside the file");
        assert_eq!(tail.range, run(700, 900));
    }

    /// **A limit-less read past the delivery bound holds one line.**
    ///
    /// The other side of the same boundary, and the reason the rule is a size
    /// rather than a flag. A file large enough that the client's token cap
    /// *could* have stopped the read short is credited with the line it began
    /// on, whatever the range asked for — the same collapse an unbounded range
    /// has always had, now reached by measurement instead of by absence.
    ///
    /// One byte decides it, in both directions, so the bound is the bound and
    /// not a neighbourhood of one.
    #[test]
    fn a_limit_less_read_past_the_delivery_bound_holds_one_line() {
        let read = ResolvedRead::new(
            &request("src/lib.rs", 1, HiKind::DefaultWindow(HiKind::END_OF_FILE)),
            hash(3),
            900,
        )
        .expect("the start is inside the file");
        let limits = Limits::default();

        assert_eq!(
            read.range_delivered(DEFAULT_MAX_DELIVERED_BYTES, limits),
            run(1, 900),
            "at the bound the whole delivery is vouchable"
        );
        assert_eq!(
            read.range_delivered(DEFAULT_MAX_DELIVERED_BYTES + 1, limits),
            run(1, 1),
            "one byte past it, only the line it started on"
        );

        // What that costs is a saving, never a wrong decision: the repeat of a
        // read too large to vouch for runs.
        let line = record(
            &read,
            &Decision::Allow,
            DEFAULT_MAX_DELIVERED_BYTES + 1,
            limits,
            0,
            1,
        );
        let journal = Journal::from_lines(vec![line]);
        assert_eq!(
            decide(
                &read,
                &journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                DEFAULT_MAX_DELIVERED_BYTES + 1,
                limits
            ),
            Decision::Allow
        );
    }

    /// A limit-less read of a file `total` lines long — the shape almost every
    /// real read has, and the one the delivery rules are written about.
    fn whole_file(total: u32) -> ResolvedRead {
        ResolvedRead::new(
            &request("src/lib.rs", 1, HiKind::DefaultWindow(HiKind::END_OF_FILE)),
            hash(3),
            total,
        )
        .expect("a limit-less read of a file with lines in it is a range of it")
    }

    /// **The client's own statement credits a delivery no density could vouch
    /// for.**
    ///
    /// The size bound is arithmetic — a measured token cap over a measured
    /// density — and past it the window is credited with one line, so every
    /// later read of a repository's largest and most-read files starts from
    /// coverage that can never satisfy anything. This is the other evidence
    /// and it is not a guess: the result object names `startLine`, `numLines`
    /// and `totalLines`, and a run reaching the file's own length with no
    /// truncation flag beside it is the client saying it handed the file over
    /// (the recorded shape of O7 in the Claude Code tool-field pin). Believed,
    /// whatever the slice weighs.
    #[test]
    fn a_delivery_the_client_states_in_full_is_credited_past_the_size_bound() {
        let read = whole_file(900);
        let limits = Limits::default();
        let recorded = read.range_delivered(DEFAULT_MAX_DELIVERED_BYTES * 2, limits);
        assert_eq!(recorded, run(1, 1), "density vouches for one line of it");
        assert_eq!(
            reconcile_delivery(&read, recorded, StatedDelivery::Complete(run(1, 901))),
            Reconcile::Credit(run(1, 900)),
            "and the client's own word for the rest of it"
        );
    }

    /// **The client counts one line more than the bytes do, and that line is
    /// never credited.**
    ///
    /// O7: every file in the recorded spike ends in a newline and the client
    /// reports one `totalLines` more than it holds newline-terminated lines,
    /// counting the empty stretch after the final newline. A statement is
    /// therefore intersected with the run this read could have delivered —
    /// already clamped to Rift's own count — rather than believed outright.
    /// The direction of the difference is a line of saving forgone, never a
    /// line of coverage invented.
    #[test]
    fn the_line_the_client_counts_past_the_last_newline_is_never_credited() {
        let read = whole_file(250);
        assert_eq!(read.range, run(1, 250), "Rift's own count");
        assert_eq!(
            reconcile_delivery(&read, run(1, 1), StatedDelivery::Complete(run(1, 251))),
            Reconcile::Credit(run(1, 250)),
            "the client's 251st line is not a line of these bytes"
        );
    }

    /// **A statement that the call was cut short and one that it was answered
    /// in full are one rule**, and which way coverage moves is decided by the
    /// record rather than by the statement.
    ///
    /// The same delivery — forty lines, stated by the same result — takes
    /// eight hundred and sixty lines back from a record that claimed the file,
    /// and credits thirty-nine to a record the size bound had already cut to
    /// one. A statement that agrees with the record writes nothing at all.
    #[test]
    fn a_truncation_and_a_completion_are_one_rule_read_from_both_sides() {
        let read = whole_file(900);
        let limits = Limits::default();

        let whole = read.range_delivered(DEFAULT_MAX_DELIVERED_BYTES, limits);
        assert_eq!(whole, run(1, 900));
        assert_eq!(
            reconcile_delivery(&read, whole, StatedDelivery::Truncated(run(1, 40))),
            Reconcile::TakeBack(Some(run(1, 40))),
            "the record claimed more than arrived"
        );

        let one = read.range_delivered(DEFAULT_MAX_DELIVERED_BYTES + 1, limits);
        assert_eq!(one, run(1, 1));
        assert_eq!(
            reconcile_delivery(&read, one, StatedDelivery::Truncated(run(1, 40))),
            Reconcile::Credit(run(1, 40)),
            "and here it claimed less, from the very same sentence"
        );

        assert_eq!(
            reconcile_delivery(&read, whole, StatedDelivery::Complete(run(1, 901))),
            Reconcile::Unchanged,
            "a statement the record already agrees with costs a lookup"
        );
    }

    /// A truncation that names nothing keeps the line the read started on, and
    /// a statement with no line of this read in it leaves no claim standing.
    #[test]
    fn a_statement_that_names_no_line_of_this_read_leaves_nothing_standing() {
        let read = whole_file(900);
        assert_eq!(
            reconcile_delivery(&read, run(1, 900), StatedDelivery::Unstated),
            Reconcile::TakeBack(Some(run(1, 1))),
            "cut short with no line named is the smallest claim there is"
        );
        assert_eq!(
            reconcile_delivery(&read, run(1, 1), StatedDelivery::Unstated),
            Reconcile::Unchanged,
            "and a record already that small has nothing to lose"
        );
        let tail = resolved("src/lib.rs", 3, 700, 900, 900);
        assert_eq!(
            reconcile_delivery(&tail, run(700, 900), StatedDelivery::Complete(run(1, 40))),
            Reconcile::TakeBack(None),
            "a run sharing no line with this read is no run of it at all"
        );
    }

    /// **A terminal answer is never credited from a statement.**
    ///
    /// The fields a statement is read from belong to a read tool's result
    /// object. A command's result says something quite different: past a size
    /// in characters the client writes the output to a file and hands the model a
    /// pointer and a preview (O6), so no sentence a command's result carries
    /// can raise what a shell read is credited with above
    /// [`Limits::max_shell_delivered_bytes`]. The correcting direction still
    /// reaches it, because taking a claim back can only cost a saving.
    #[test]
    fn a_terminal_answer_is_never_credited_from_a_statement() {
        let shell = ResolvedRead::new(
            &ReadRequest {
                path: "src/lib.rs".to_owned(),
                span: ReadSpan::lines(1, HiKind::DefaultWindow(HiKind::END_OF_FILE)),
                channel: ReadChannel::Shell,
            },
            hash(3),
            900,
        )
        .expect("a shell read of a file with lines in it is a range of it");
        assert_eq!(
            reconcile_delivery(&shell, run(1, 1), StatedDelivery::Complete(run(1, 901))),
            Reconcile::Unchanged,
            "the shell bound stands whatever a result says about itself"
        );
        assert_eq!(
            reconcile_delivery(&shell, run(1, 900), StatedDelivery::Unstated),
            Reconcile::TakeBack(Some(run(1, 1))),
            "and the correction still reaches the terminal channel"
        );
    }

    /// **What the credit is worth, and the ceiling it does not move.**
    ///
    /// Coverage credited from a statement is coverage like any other: a later
    /// read wholly inside it is declinable under the ordinary rule. What one
    /// decline may cover is still [`Limits::max_slice_bytes`], which is a
    /// separate number about a separate thing — the size at which what a
    /// window kept of an earlier delivery may be a page rather than the range.
    #[test]
    fn credited_coverage_answers_a_later_read_up_to_the_decline_ceiling() {
        let read = whole_file(900);
        let limits = Limits::default();
        let Reconcile::Credit(range) =
            reconcile_delivery(&read, run(1, 1), StatedDelivery::Complete(run(1, 901)))
        else {
            panic!("the statement credits the file");
        };
        let journal = Journal::from_lines(vec![Line::Read {
            turn: 1,
            path: read.path.clone(),
            hash: read.hash,
            range,
            total_lines: 900,
            at_ms: 10,
        }]);
        let inside = resolved("src/lib.rs", 3, 100, 200, 900);
        let declining = |slice_bytes| {
            decide(
                &inside,
                &journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                slice_bytes,
                limits,
            )
        };
        assert!(
            matches!(declining(4_000), Decision::Deny(_)),
            "a range inside credited coverage is a range the window holds"
        );
        assert_eq!(
            declining(limits.max_slice_bytes + 1),
            Decision::Allow,
            "and the ceiling on what one decline may cover is unmoved"
        );
    }

    #[test]
    fn an_unknown_or_byte_span_never_resolves_to_line_coverage() {
        for span in [
            ReadSpan::Unknown,
            ReadSpan::VerifiedBytes {
                offset: 7,
                length: 60,
            },
        ] {
            let request = ReadRequest {
                path: "src/lib.rs".to_owned(),
                span,
                channel: ReadChannel::Tool,
            };
            assert_eq!(ResolvedRead::new(&request, hash(3), 900), None);
        }

        // With no fabricated seed line in the journal, a later pinned span is
        // allowed and cannot be observed or denied.
        let journal = Journal::default();
        let wider = resolved("src/lib.rs", 3, 7, 60, 900);
        assert_eq!(
            decide(
                &wider,
                &journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                64,
                Limits::default()
            ),
            Decision::Allow
        );
    }

    #[test]
    fn a_request_inside_a_single_edit_site_is_allowed() {
        let journal = Journal::from_lines(vec![
            read_line(1, "src/lib.rs", 0x3f, 1, 900),
            Line::Edit {
                turn: 2,
                path: "src/lib.rs".to_owned(),
                hash_old: Some(hash(0x3f)),
                hash_new: hash(0x40),
                sites: vec![run(37, 44)],
                shift: 8,
                full: true,
                at_ms: 2,
            },
        ]);
        let limits = Limits::default();

        let inside = resolved("src/lib.rs", 0x40, 38, 42, 908);
        assert_eq!(
            decide(
                &inside,
                &journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                64,
                limits
            ),
            Decision::Allow,
            "reading exactly what changed is the right thing to do"
        );

        let whole = resolved("src/lib.rs", 0x40, 1, 908, 908);
        assert_eq!(
            decide(
                &whole,
                &journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                64,
                limits
            ),
            Decision::Deny(DenyShape::Delta {
                sites: vec![run(37, 44)],
                shift: 8,
                window: run(34, 47),
            }),
            "and re-reading the file to check it is answered with where it changed"
        );
    }

    #[test]
    fn a_file_written_in_full_is_declined_without_naming_every_line_as_changed() {
        let journal = Journal::from_lines(vec![Line::Edit {
            turn: 1,
            path: "src/new.rs".to_owned(),
            hash_old: None,
            hash_new: hash(0x11),
            sites: vec![run(1, 40)],
            shift: 40,
            full: true,
            at_ms: 1,
        }]);
        let read = resolved("src/new.rs", 0x11, 1, 40, 40);
        assert_eq!(
            decide(
                &read,
                &journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                64,
                Limits::default()
            ),
            Decision::Deny(DenyShape::Plain)
        );
    }

    /// The edit-echo gate is a fact about a *family*, not a stand-down for
    /// everyone: on one journal and one read, both shapes an edit line can
    /// produce are still produced where the echo is pinned, and both are
    /// allowed where it is not.
    #[test]
    fn the_same_edit_is_decided_for_a_pinned_family_and_allowed_for_an_unpinned_one() {
        let edited = Journal::from_lines(vec![
            read_line(1, "src/lib.rs", 0x3f, 1, 900),
            Line::Edit {
                turn: 2,
                path: "src/lib.rs".to_owned(),
                hash_old: Some(hash(0x3f)),
                hash_new: hash(0x40),
                sites: vec![run(37, 44)],
                shift: 8,
                full: true,
                at_ms: 2,
            },
        ]);
        let written = Journal::from_lines(vec![Line::Edit {
            turn: 1,
            path: "src/new.rs".to_owned(),
            hash_old: None,
            hash_new: hash(0x11),
            sites: vec![run(1, 40)],
            shift: 40,
            full: true,
            at_ms: 1,
        }]);
        let verification = resolved("src/lib.rs", 0x40, 1, 908, 908);
        let read_back = resolved("src/new.rs", 0x11, 1, 40, 40);
        let at = |read: &ResolvedRead, journal: &Journal, echo| {
            decide(
                read,
                journal,
                Mode::Enforce,
                Some(CONFIRMED),
                echo,
                64,
                Limits::default(),
            )
        };

        assert_eq!(
            at(&verification, &edited, EditEcho::Pinned),
            Decision::Deny(DenyShape::Delta {
                sites: vec![run(37, 44)],
                shift: 8,
                window: run(34, 47),
            }),
        );
        assert_eq!(
            at(&verification, &edited, EditEcho::Unpinned),
            Decision::Allow,
            "the same verification read, on a family whose edit result says nothing"
        );

        assert_eq!(
            at(&read_back, &written, EditEcho::Pinned),
            Decision::Deny(DenyShape::Plain),
        );
        assert_eq!(
            at(&read_back, &written, EditEcho::Unpinned),
            Decision::Allow,
            "a whole-file write is still an edit line, and the gate covers it too"
        );
    }

    proptest::proptest! {
        /// ★ Coverage is exact at its edge: a request for one line more than
        /// was handed over is a request for a line the window does not hold,
        /// and it runs.
        #[test]
        fn a_request_one_line_past_coverage_is_allowed(
            lo in 1u32..=500,
            span in 0u32..=200,
            total_extra in 1u32..=100,
        ) {
            let hi = lo + span;
            let total_lines = hi + total_extra;
            let journal = Journal::from_lines(vec![Line::Read {
                turn: 1,
                path: "src/lib.rs".to_owned(),
                hash: hash(0x3f),
                range: run(lo, hi),
                total_lines,
                at_ms: 1,
            }]);
            // The floor is a different property, tested on its own below;
            // held out of the way here so a short generated span exercises
            // coverage rather than the trade.
            let limits = Limits { min_slice_lines: 1, ..Limits::default() };

            let exact = resolved("src/lib.rs", 0x3f, lo, hi, total_lines);
            proptest::prop_assert_eq!(
                decide(&exact, &journal, Mode::Enforce, Some(CONFIRMED), EditEcho::Pinned, 64, limits),
                Decision::Deny(DenyShape::Plain),
                "exactly what was handed over is held"
            );

            let one_past = resolved("src/lib.rs", 0x3f, lo, hi + 1, total_lines);
            proptest::prop_assert_eq!(
                decide(&one_past, &journal, Mode::Enforce, Some(CONFIRMED), EditEcho::Pinned, 64, limits),
                Decision::Allow,
                "one line further is not"
            );

            if lo > 1 {
                let one_before = resolved("src/lib.rs", 0x3f, lo - 1, hi, total_lines);
                proptest::prop_assert_eq!(
                    decide(&one_before, &journal, Mode::Enforce, Some(CONFIRMED), EditEcho::Pinned, 64, limits),
                    Decision::Allow,
                    "nor is one line earlier"
                );
            }
        }

        /// ★ A slice larger than a client keeps inline is never declined: what
        /// the window holds of the earlier delivery may be a pointer to a file
        /// rather than the bytes themselves.
        #[test]
        fn a_slice_over_the_cap_never_denies(
            over in proptest::prop_oneof![
                // The boundary itself, drawn on purpose: a range sampled
                // uniformly from a million values reaches its own first value
                // about never, and the byte that matters is the first one.
                proptest::strategy::Just(1usize),
                proptest::strategy::Just(2usize),
                3usize..=1_000_000,
            ],
            cap in 1usize..=64_000,
        ) {
            let journal = Journal::from_lines(vec![read_line(1, "src/lib.rs", 0x3f, 1, 900)]);
            let limits = Limits { max_slice_bytes: cap, min_slice_lines: 1, ..Limits::default() };
            let read = resolved("src/lib.rs", 0x3f, 1, 900, 900);
            let at = |slice: usize| decide(
                &read, &journal, Mode::Enforce, Some(CONFIRMED), EditEcho::Pinned, slice, limits
            );

            proptest::prop_assert_eq!(
                at(cap + over),
                Decision::Allow,
                "over the cap, nothing is decided"
            );
            proptest::prop_assert_eq!(
                at(cap + 1),
                Decision::Allow,
                "one byte over the cap is over the cap"
            );
            proptest::prop_assert_eq!(
                at(cap),
                Decision::Deny(DenyShape::Plain),
                "at the cap the decision is the ordinary one"
            );
        }

        /// ★ A slice shorter than the floor is observed and never declined —
        /// and, unlike the byte ceiling, it is still *decided*.
        ///
        /// The two conservative bounds differ on purpose. Past the byte
        /// ceiling nothing is known: what the window kept of the earlier
        /// delivery may be a page rather than the range, so there is no
        /// decision to make and the read is simply allowed. Under the line
        /// floor everything is known — the window holds the range, the repeat
        /// is covered — and only the trade is bad, so the decision is made,
        /// recorded as an observation, and the call runs. That is what lets
        /// the observe window price the pool the floor forgoes instead of
        /// arguing about where to put it.
        #[test]
        fn a_slice_under_the_line_floor_is_observed_never_denied(
            floor in 2u32..=400,
            under in 1u32..=200,
        ) {
            let span = |lines: u32| {
                let journal = Journal::from_lines(vec![read_line(1, "src/lib.rs", 0x3f, 1, 900)]);
                let read = resolved("src/lib.rs", 0x3f, 1, lines, 900);
                let limits = Limits {
                    min_slice_lines: floor,
                    ..Limits::default()
                };
                decide(
                    &read, &journal, Mode::Enforce, Some(CONFIRMED), EditEcho::Pinned, 64, limits
                )
            };

            proptest::prop_assert_eq!(
                span(floor.saturating_sub(under).max(1)),
                Decision::Observe(DenyShape::Plain),
                "under the floor the decision is reached and recorded, never acted on"
            );
            proptest::prop_assert_eq!(
                span(floor - 1),
                Decision::Observe(DenyShape::Plain),
                "one line under the floor is under the floor"
            );
            proptest::prop_assert_eq!(
                span(floor),
                Decision::Deny(DenyShape::Plain),
                "at the floor the decision is the ordinary one"
            );
            proptest::prop_assert_eq!(
                span(floor + 1),
                Decision::Deny(DenyShape::Plain),
                "and above it"
            );
        }

        /// ★ Observing is not enforcing. Every read that enforcing would
        /// decline is recorded and allowed to run, and the row it leaves says
        /// so — the one switch that keeps a measured lever from acting on
        /// anyone's session before the numbers are in.
        #[test]
        fn observe_records_and_never_denies(
            lo in 1u32..=800,
            span in 0u32..=99,
            edited in proptest::bool::ANY,
            slice in 0usize..=4_000,
        ) {
            let mut lines = vec![read_line(1, "src/lib.rs", 0x3f, 1, 900)];
            if edited {
                lines.push(Line::Edit {
                    turn: 2,
                    path: "src/lib.rs".to_owned(),
                    hash_old: Some(hash(0x3f)),
                    hash_new: hash(0x40),
                    sites: vec![run(37, 44)],
                    shift: 8,
                    full: true,
                    at_ms: 2,
                });
            }
            let journal = Journal::from_lines(lines);
            let limits = Limits::default();
            let seed = if edited { 0x40 } else { 0x3f };
            let read = resolved("src/lib.rs", seed, lo, lo + span, 900);

            let enforcing = decide(&read, &journal, Mode::Enforce, Some(CONFIRMED), EditEcho::Pinned, slice, limits);
            let observing = decide(&read, &journal, Mode::Observe, Some(CONFIRMED), EditEcho::Pinned, slice, limits);

            proptest::prop_assert!(
                !matches!(observing, Decision::Deny(_)),
                "observing never declines: {:?}",
                observing
            );
            match enforcing {
                Decision::Deny(shape) => proptest::prop_assert_eq!(
                    observing.clone(),
                    Decision::Observe(shape),
                    "what enforcing would decline, observing records in the same shape"
                ),
                other => proptest::prop_assert_eq!(
                    observing.clone(),
                    other,
                    "and everything else is decided identically"
                ),
            }

            // The row it leaves says it was observed, not declined.
            if let Decision::Observe(_) = observing {
                let line = record(&read, &observing, 0, Limits::default(), 9, 99);
                proptest::prop_assert_eq!(line.kind(), "observe");
            }
        }

        /// ★ With the lever off, nothing is ever declined, whatever the
        /// journal says and whatever the family's channel offers.
        #[test]
        fn off_never_denies(
            lo in 1u32..=800,
            span in 0u32..=99,
            slice in 0usize..=100_000,
            edited in proptest::bool::ANY,
        ) {
            let mut lines = vec![read_line(1, "src/lib.rs", 0x3f, 1, 900)];
            if edited {
                lines.push(Line::Edit {
                    turn: 2,
                    path: "src/lib.rs".to_owned(),
                    hash_old: Some(hash(0x3f)),
                    hash_new: hash(0x40),
                    sites: vec![run(37, 44)],
                    shift: 0,
                    full: true,
                    at_ms: 2,
                });
            }
            let journal = Journal::from_lines(lines);
            let seed = if edited { 0x40 } else { 0x3f };
            let read = resolved("src/lib.rs", seed, lo, lo + span, 900);

            proptest::prop_assert_eq!(
                decide(&read, &journal, Mode::Off, Some(CONFIRMED), EditEcho::Pinned, slice, Limits::default()),
                Decision::Allow
            );
        }

        /// ★ A family whose explanation has not been seen reaching a model is
        /// never declined on: an unexplained tool failure is a thing a model is
        /// right to retry, and a retry is the cost the whole design avoids.
        #[test]
        fn an_unpinned_channel_never_denies(
            lo in 1u32..=800,
            span in 0u32..=99,
            channel_pick in 0usize..4,
        ) {
            let channels = [
                None,
                Some(UNCONFIRMED),
                Some(Channel::Neutral),
                Some(Channel::AdditionalContext { limit_field: None }),
            ];
            let journal = Journal::from_lines(vec![read_line(1, "src/lib.rs", 0x3f, 1, 900)]);
            let read = resolved("src/lib.rs", 0x3f, lo, lo + span, 900);

            let decision = decide(
                &read,
                &journal,
                Mode::Enforce,
                channels[channel_pick % channels.len()],
                EditEcho::Pinned,
                64,
                Limits::default(),
            );
            proptest::prop_assert_eq!(
                decision,
                Decision::Observe(DenyShape::Plain),
                "recorded, never acted on"
            );
        }

        /// ★ A family whose edit result is not pinned to tell the model it
        /// holds what it just wrote is never declined on the strength of that
        /// edit — not with the changed sites, not plainly, and not even as a
        /// recorded `observe`, whose shape is what a later flip would act on.
        /// The read runs and is recorded as a read, which is the only sight of
        /// its own edit such an agent has.
        #[test]
        fn a_family_whose_edit_echo_is_not_pinned_is_never_declined_on_its_own_edit(
            lo in 1u32..=800,
            span in 0u32..=99,
            site_lo in 1u32..=800,
            site_span in 0u32..=99,
            shift in -50i64..=50,
            whole_file in proptest::bool::ANY,
            enforcing in proptest::bool::ANY,
            slice in 0usize..=4_000,
        ) {
            const TOTAL: u32 = 900;
            // The window read the old revision in full and then wrote it, so
            // everything it holds of the new revision it holds by having
            // written it — the one situation the delta arm exists for.
            let sites = if whole_file {
                vec![run(1, TOTAL)]
            } else {
                vec![run(site_lo, (site_lo + site_span).min(TOTAL))]
            };
            let journal = Journal::from_lines(vec![
                read_line(1, "src/lib.rs", 0x3f, 1, TOTAL),
                Line::Edit {
                    turn: 2,
                    path: "src/lib.rs".to_owned(),
                    hash_old: Some(hash(0x3f)),
                    hash_new: hash(0x40),
                    sites,
                    shift,
                    full: true,
                    at_ms: 2,
                },
            ]);
            let read = resolved("src/lib.rs", 0x40, lo, lo + span, TOTAL);
            let mode = if enforcing { Mode::Enforce } else { Mode::Observe };

            let decision = decide(
                &read,
                &journal,
                mode,
                Some(CONFIRMED),
                EditEcho::Unpinned,
                slice,
                Limits::default(),
            );
            proptest::prop_assert_eq!(
                decision.clone(),
                Decision::Allow,
                "an unpinned edit echo decides nothing, in any mode"
            );
            proptest::prop_assert_eq!(
                    record(&read, &decision, 0, Limits::default(), 3, 3).kind(),
                    "read"
                );
        }

        /// ★ However reads, declines and losses of context interleave, one
        /// request is declined at most once between two losses — the property
        /// that bounds a model answering a decline by asking again to a single
        /// extra exchange.
        #[test]
        fn at_most_one_deny_per_key_under_any_interleaving(
            steps in proptest::collection::vec(
                (0usize..=3, 1u32..=6, 0u32..=3, 0usize..=1),
                1..40,
            ),
        ) {
            const PATHS: [&str; 2] = ["src/lib.rs", "src/other.rs"];
            const TOTAL: u32 = 60;
            let limits = Limits::default();
            let mut journal = Journal::default();
            let mut epoch = 0usize;
            // A key is a path, a revision and a range: two revisions of one
            // file are two questions, and each gets its own single answer.
            let mut seen: Vec<(usize, String, u8, u32, u32)> = Vec::new();

            for (turn, (action, lo, span, which)) in steps.into_iter().enumerate() {
                let path = PATHS[which];
                if action == 3 {
                    journal.push(Line::Evict {
                        turn: turn as u32,
                        reason: EvictReason::Clear,
                        at_ms: turn as u64,
                    });
                    epoch += 1;
                    continue;
                }
                // Two revisions in play, so a decision sometimes rests on
                // coverage that a later step's hash change wipes out.
                let seed = if action == 2 { 0x40 } else { 0x3f };
                let read = resolved(path, seed, lo * 8, lo * 8 + span, TOTAL);
                let decision = decide(
                    &read,
                    &journal,
                    Mode::Enforce,
                    Some(CONFIRMED),
                    EditEcho::Pinned,
                    64,
                    limits,
                );
                if let Decision::Deny(_) = decision {
                    let key = (epoch, path.to_owned(), seed, read.range.lo, read.range.hi);
                    proptest::prop_assert!(
                        !seen.contains(&key),
                        "a second decline for {:?} in one context",
                        key
                    );
                    seen.push(key);
                }
                journal.push(record(
                    &read,
                    &decision,
                    0,
                    Limits::default(),
                    turn as u32,
                    turn as u64,
                ));
            }
        }
    }
}

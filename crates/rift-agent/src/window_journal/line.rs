//! The row grammar: one entry of a context window's journal, and the JSON
//! line it is written as and read back from.
//!
//! A separate module because the on-disk spelling is behaviour — the
//! journals on every developer's disk are read by the next binary — so the
//! codec and its field readers sit together, apart from the questions asked
//! over the rows.

use rift_core::Sha256Id;
use serde_json::{json, Map, Value};

use super::family::{family_line_from, insert_family_fields, FamilyRoute};
use super::inherit::{class_at, inherited_line_from, insert_inherited_fields, InheritClass};
use super::matched::{insert_matched_fields, matched_line_from};
use super::{Interval, LINE_VERSION};

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
        /// The class of inherited line the decision would have rested on, or
        /// `None` when it rested on this window's own lines. Its own field
        /// rather than its own kind, so a binary that predates it still reads
        /// the row as the observation it is.
        via: Option<InheritClass>,
    },
    /// A first read of a large file answered once with the file's bound
    /// outline instead of its lines.
    ///
    /// Not coverage: the window was handed the outline, not the file, so a
    /// later read of these lines is decided as one the window does not hold.
    /// A kind of its own rather than a `deny`, because what it answered with
    /// is a different act, and a binary that predates it drops a kind it does
    /// not know rather than reading it as a decline of lines the window held.
    Outline {
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
    /// **Lines this window holds because the window that started it held
    /// them**, marked with the class of evidence that says so.
    ///
    /// Never coverage by itself: a decision treats it as coverage only while
    /// its class's lever is at `enforce`, and below that it only sizes what
    /// the class would be worth. A kind of its own rather than a marked
    /// `read`, because a binary that predates it drops a kind it does not
    /// know and would otherwise read the row as lines this window was handed.
    Inherit {
        /// The turn the window was opened in.
        turn: u32,
        /// What the inheritance rests on.
        class: InheritClass,
        /// The file, in the one spelling coverage is keyed by.
        path: String,
        /// The revision the spawning window held.
        hash: Sha256Id,
        /// The lines it held of that revision.
        range: Interval,
        /// How many lines the file had.
        total_lines: u32,
        /// When the line was written, in milliseconds.
        at_ms: u64,
    },
    /// **A window started a subagent with this call**, written into the
    /// session's main journal so the subagent's opening can find it.
    ///
    /// Identifiers only: the call, the user prompt it was made under, the
    /// subagent type, the spawning window, and the paths of what the spawner
    /// held that its prompt names — never the prompt itself.
    Spawn {
        /// The turn the call was made in.
        turn: u32,
        /// The spawning tool call's id.
        call: String,
        /// The id of the user prompt the call was made under.
        prompt: String,
        /// The subagent type the call asked for.
        agent_type: String,
        /// The spawning window's agent id; `None` for the session's own.
        spawner: Option<String>,
        /// Paths the spawner held whose names the spawning prompt carries.
        names: Vec<String>,
        /// When the line was written, in milliseconds.
        at_ms: u64,
    },
    /// **A subagent's window opened**, and the spawn it is the answer to, in
    /// the session's main journal beside the spawn lines it is matched with.
    ///
    /// Written once per window, whether or not a spawn could be named: a
    /// window that opens again finds its own claim and claims nothing more.
    Claim {
        /// The turn the window opened in.
        turn: u32,
        /// The subagent's agent id.
        agent: String,
        /// The spawn it answers, when exactly one could be named.
        call: Option<String>,
        /// When the line was written, in milliseconds.
        at_ms: u64,
    },
    /// **A read declined once because another live window of the family
    /// holds these bytes**, with the cheaper route the decline carried — or,
    /// below `enforce`, would have carried.
    ///
    /// Not coverage: the window was handed an outline or a list of runs, not
    /// the lines. One per revision in a window: every later read of it is
    /// admitted, and one inside the offered runs is the follow-up the
    /// decline promised. A kind of its own, so a binary that predates it
    /// drops the row rather than reading a decline of lines the window held.
    Family {
        /// The turn the call was made in.
        turn: u32,
        /// The file, in the one spelling coverage is keyed by.
        path: String,
        /// The revision the bytes hashed to.
        hash: Sha256Id,
        /// The lines that were asked for.
        range: Interval,
        /// What the decline handed the window instead.
        route: FamilyRoute,
        /// The runs the frame named as the reads to make instead.
        offered: Vec<Interval>,
        /// Whether the decline was made, or only recorded.
        acted: bool,
        /// The size of the frame in bytes, sent or not: what an observed
        /// decline would have cost, measured without a tokenizer.
        frame_bytes: u32,
        /// When the line was written, in milliseconds.
        at_ms: u64,
    },
    /// **Lines of a file this window was shown in a terminal result**, found
    /// by matching the delivered lines against the file's own bytes at the
    /// revision on disk when the result arrived.
    ///
    /// Sparse, because output such as a search's names scattered lines. Never
    /// coverage by itself: a decision treats it as coverage only while its
    /// lever is at `enforce`. A kind of its own, so a binary that predates it
    /// drops the row rather than reading lines no grammar bounded as a read.
    Matched {
        /// The turn the result arrived in.
        turn: u32,
        /// The file, in the one spelling coverage is keyed by.
        path: String,
        /// The revision the bytes on disk hashed to.
        hash: Sha256Id,
        /// The runs of the file the output was found to carry, sorted and
        /// disjoint.
        runs: Vec<Interval>,
        /// How many lines the file had.
        total_lines: u32,
        /// Which member of the command line the output was aligned to,
        /// counted from zero.
        member: u32,
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
            Self::Outline { .. } => "outline",
            Self::Hold { .. } => "hold",
            Self::Prevented { .. } => "prevented",
            Self::Expect { .. } => "expect",
            Self::Edit { .. } => "edit",
            Self::Evict { .. } => "evict",
            Self::Inherit { .. } => "inherit",
            Self::Spawn { .. } => "spawn",
            Self::Claim { .. } => "claim",
            Self::Family { .. } => "family",
            Self::Matched { .. } => "matched",
        }
    }

    /// The turn this line belongs to.
    #[must_use]
    pub const fn turn(&self) -> u32 {
        match self {
            Self::Read { turn, .. }
            | Self::Deny { turn, .. }
            | Self::Observe { turn, .. }
            | Self::Outline { turn, .. }
            | Self::Hold { turn, .. }
            | Self::Prevented { turn, .. }
            | Self::Expect { turn, .. }
            | Self::Edit { turn, .. }
            | Self::Evict { turn, .. }
            | Self::Inherit { turn, .. }
            | Self::Spawn { turn, .. }
            | Self::Claim { turn, .. }
            | Self::Family { turn, .. }
            | Self::Matched { turn, .. } => *turn,
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
        self.insert_fields(&mut object);
        if let Self::Observe {
            via: Some(class), ..
        } = self
        {
            object.insert("c".to_owned(), json!(class.as_str()));
        }
        Value::Object(object).to_string()
    }

    /// The fields past the shared `v`/`k`/`t` header, one arm per shape a
    /// row can take. The spellings and their order are the on-disk grammar
    /// every journal already written is read back by, so each arm inserts
    /// exactly what its kind has always carried and nothing is derived.
    fn insert_fields(&self, object: &mut Map<String, Value>) {
        match self {
            Self::Read {
                path,
                hash,
                range,
                total_lines,
                at_ms,
                ..
            } => {
                insert_slice(object, path, *hash, *range);
                object.insert("n".to_owned(), json!(total_lines));
                object.insert("at".to_owned(), json!(at_ms));
            }
            Self::Deny { .. }
            | Self::Observe { .. }
            | Self::Outline { .. }
            | Self::Hold { .. }
            | Self::Prevented { .. } => insert_bare_slice(self, object),
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
                insert_change(object, path, *hash_old, *hash_new, sites);
                object.insert("shift".to_owned(), json!(shift));
                object.insert("full".to_owned(), json!(full));
                object.insert("at".to_owned(), json!(at_ms));
            }
            Self::Evict { reason, at_ms, .. } => {
                object.insert("reason".to_owned(), json!(reason.as_str()));
                object.insert("at".to_owned(), json!(at_ms));
            }
            Self::Inherit { .. } | Self::Spawn { .. } | Self::Claim { .. } => {
                insert_inherited_fields(self, object);
            }
            Self::Family { .. } => insert_family_fields(self, object),
            Self::Matched { .. } => insert_matched_fields(self, object),
        }
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
                via: class_at(object).ok()?,
            }),
            "outline" => Some(Self::Outline {
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
            kind => linking_line_from(kind, object, turn, at_ms),
        }
    }
}

/// The line one of the kinds whose grammar lives beside the decision it
/// serves states — inheritance between windows, and a decline resting on
/// another window — or `None` for a kind no module knows.
fn linking_line_from(
    kind: &str,
    object: &Map<String, Value>,
    turn: u32,
    at_ms: u64,
) -> Option<Line> {
    match kind {
        "family" => family_line_from(object, turn, at_ms),
        "matched" => matched_line_from(object, turn, at_ms),
        kind => inherited_line_from(kind, object, turn, at_ms),
    }
}

/// The fields of a line that is about one slice and carries nothing past it:
/// the slice and the moment. The kinds whose rows are exactly that share the
/// one spelling.
fn insert_bare_slice(line: &Line, object: &mut Map<String, Value>) {
    let (Line::Deny {
        path,
        hash,
        range,
        at_ms,
        ..
    }
    | Line::Observe {
        path,
        hash,
        range,
        at_ms,
        ..
    }
    | Line::Outline {
        path,
        hash,
        range,
        at_ms,
        ..
    }
    | Line::Hold {
        path,
        hash,
        range,
        at_ms,
        ..
    }
    | Line::Prevented {
        path,
        hash,
        range,
        at_ms,
        ..
    }) = line
    else {
        return;
    };
    insert_slice(object, path, *hash, *range);
    object.insert("at".to_owned(), json!(at_ms));
}

/// The file, the revision and the run: the three fields every line about a
/// slice of a file carries, spelled once so the five kinds that carry them
/// cannot drift apart.
fn insert_slice(object: &mut Map<String, Value>, path: &str, hash: Sha256Id, range: Interval) {
    object.insert("p".to_owned(), json!(path));
    object.insert("h".to_owned(), json!(hash.to_hex()));
    object.insert("lo".to_owned(), json!(range.lo));
    object.insert("hi".to_owned(), json!(range.hi));
}

/// Where a file changed: from which revision, where the journal knew it, to
/// which, and at which sites. An unknown old revision is left out of the row
/// rather than written as null, which is the shape the reader accepts both
/// ways.
fn insert_change(
    object: &mut Map<String, Value>,
    path: &str,
    hash_old: Option<Sha256Id>,
    hash_new: Sha256Id,
    sites: &[Interval],
) {
    object.insert("p".to_owned(), json!(path));
    if let Some(old) = hash_old {
        object.insert("h_old".to_owned(), json!(old.to_hex()));
    }
    object.insert("h_new".to_owned(), json!(hash_new.to_hex()));
    let rows: Vec<Value> = sites.iter().map(|s| json!([s.lo, s.hi])).collect();
    object.insert("sites".to_owned(), Value::Array(rows));
}

/// A string field, or `None` when it is absent or not a string.
pub(super) fn text_at<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key)?.as_str()
}

/// A line-number-sized field, or `None` when it is absent, not a number, or
/// larger than a line number can be.
pub(super) fn u32_at(object: &Map<String, Value>, key: &str) -> Option<u32> {
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
pub(super) fn hash_at(object: &Map<String, Value>, key: &str) -> Option<Sha256Id> {
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
pub(super) fn range_at(object: &Map<String, Value>) -> Option<Interval> {
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

#[cfg(test)]
mod tests {
    use super::super::fixtures::{hash, held_hold, one_of_each, run};
    use super::*;

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
                            | "c"
                            | "call"
                            | "prompt"
                            | "type"
                            | "by"
                            | "names"
                            | "agent"
                            | "route"
                            | "runs"
                            | "acted"
                            | "frame_bytes"
                            | "member"
                            | "basis"
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
}

//! What a change to a file leaves the window holding.
//!
//! An edit-shaped payload says what was replaced with what; the bytes on
//! disk say what is there now. Everything in this module is the arithmetic
//! between the two — where the file changed, by how many lines it grew, and
//! whether the window can still claim every line of it — and none of it
//! touches a file this plane owns.

use rift_agent::window_journal::{Interval, Journal, Line};

use super::MAX_EDIT_SITES;

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

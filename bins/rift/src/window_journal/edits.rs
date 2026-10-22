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

/// One replacement a call made, borrowed from whichever payload shape
/// carried it: a lone edit is one region, a multi-region call several.
struct Region<'a> {
    new_string: &'a str,
    old_string_lines: u32,
    replace_all: bool,
}

/// Where one occurrence of a region's text sits in the new bytes: the bytes,
/// so two regions can be checked for claiming the same ones, and the lines.
struct Placed {
    bytes: std::ops::Range<usize>,
    lines: Interval,
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
            let region = Region {
                new_string,
                old_string_lines: *old_string_lines,
                replace_all: *replace_all,
            };
            replacement_facts(new_text, &[region], measured_shift, held_before)
        }
        rift_agent::EditKind::Regions { regions } => {
            let regions: Vec<Region<'_>> = regions
                .iter()
                .map(|r| Region {
                    new_string: &r.new_string,
                    old_string_lines: r.old_string_lines,
                    replace_all: r.replace_all,
                })
                .collect();
            replacement_facts(new_text, &regions, measured_shift, held_before)
        }
        // A notebook cell, a patch whose chunks could not be read: the change
        // is real and worth recording, and where it landed does not follow
        // from the payload without the bytes that were there before.
        rift_agent::EditKind::Other => silent(measured_shift.unwrap_or_default()),
    }
}

/// What a call of one or more replacements left behind, held to one rule for
/// every region: its text is in the new bytes exactly where a replacement
/// would have put it, or nothing is claimed for the call at all.
///
/// Every region is located on its own in the bytes read after the call
/// landed. One that is absent, present more than once where the tool promised
/// one, or lying across another region's bytes is a place this payload cannot
/// name, and naming the others would describe a change only partly understood
/// — so the whole call stays silent, as a call nobody could place does today.
/// The file's measured growth must then be the sum of what the regions account
/// for, or something else changed it too. Only after all of that does a
/// window that held every line before hold every line after.
fn replacement_facts(
    new_text: &str,
    regions: &[Region<'_>],
    measured_shift: Option<i64>,
    held_before: bool,
) -> EditFacts {
    let silent = EditFacts {
        sites: Vec::new(),
        shift: measured_shift.unwrap_or_default(),
        full: false,
    };
    let haystack = normalised(new_text);
    let mut placed: Vec<Placed> = Vec::new();
    let mut grown = 0i64;
    for region in regions {
        // An empty stretch is nothing to look for, and `locate` finds nothing,
        // which lands on the underivable arm: a deletion names no place.
        let needle = normalised(region.new_string);
        let found = locate(&haystack, &needle);
        if found.is_empty() || (!region.replace_all && found.len() > 1) {
            return silent;
        }
        let per_site = i64::from(line_span(&needle)) - i64::from(region.old_string_lines);
        let count = i64::try_from(found.len()).unwrap_or(i64::MAX);
        grown = grown.saturating_add(count.saturating_mul(per_site));
        placed.extend(found);
        if placed.len() > MAX_EDIT_SITES {
            return silent;
        }
    }
    placed.sort_by_key(|p| p.bytes.start);
    if placed
        .windows(2)
        .any(|w| matches!(w, [a, b] if a.bytes.end > b.bytes.start))
    {
        // Two regions claim the same bytes: one of them was rewritten by the
        // other, or matched inside it, and which is which is a guess.
        return silent;
    }
    if measured_shift.is_some_and(|measured| measured != grown) {
        // The file grew by a different number of lines than these
        // replacements account for, so something else changed it too —
        // somewhere this window has not seen.
        return silent;
    }
    EditFacts {
        sites: merged(placed.iter().map(|p| p.lines)),
        shift: measured_shift.unwrap_or(grown),
        full: held_before,
    }
}

/// Runs of lines in ascending order, with any two that share a line taken as
/// one: two regions on one line changed that line, once.
fn merged(runs: impl Iterator<Item = Interval>) -> Vec<Interval> {
    let mut out: Vec<Interval> = Vec::new();
    for run in runs {
        match out.last_mut() {
            Some(last) if run.lo <= last.hi => {
                if let Some(joined) = Interval::new(last.lo, last.hi.max(run.hi)) {
                    *last = joined;
                }
            }
            _ => out.push(run),
        }
    }
    out
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
fn locate(haystack: &str, needle: &str) -> Vec<Placed> {
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
        let Some(lines) = Interval::new(line, line.saturating_add(span.saturating_sub(1))) else {
            break;
        };
        let end = start + needle.len();
        out.push(Placed {
            bytes: start..end,
            lines,
        });
        if out.len() > MAX_EDIT_SITES {
            return Vec::new();
        }
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

#[cfg(test)]
mod tests {
    use rift_agent::window_journal::{decide, DenyShape, EditEcho, Limits, Mode, ResolvedRead};
    use rift_agent::{EditKind, Replacement};

    use super::*;

    fn run(lo: u32, hi: u32) -> Interval {
        Interval::new(lo, hi).expect("a run")
    }

    fn hash(byte: u8) -> rift_core::Sha256Id {
        rift_core::Sha256Id::from_bytes([byte; 32])
    }

    /// A window that read every line of a file this many lines long.
    fn held(total_lines: u32) -> Option<PriorRevision> {
        Some(PriorRevision {
            hash: hash(1),
            total_lines: Some(total_lines),
            held_in_full: true,
        })
    }

    /// Replacements as the adapters build them: the old side counted the way
    /// [`line_span`] counts, so text ending in a line break spans one more.
    fn regions(pairs: &[(&str, u32)]) -> EditKind {
        EditKind::Regions {
            regions: pairs
                .iter()
                .map(|(new_string, old_string_lines)| Replacement {
                    new_string: (*new_string).to_owned(),
                    old_string_lines: *old_string_lines,
                    replace_all: false,
                })
                .collect(),
        }
    }

    /// The facts for `kind` against `text`, as bytes read after the call.
    fn facts(kind: &EditKind, text: &str, prior: Option<PriorRevision>) -> EditFacts {
        let lines = u32::try_from(text.lines().count()).expect("a small file");
        edit_facts(kind, text, lines, hash(2), None, prior)
    }

    #[test]
    fn every_region_that_landed_is_a_site_and_the_window_holds_the_result() {
        let after = "a\nB\nc\nd\nE\nE2\n";
        let got = facts(&regions(&[("B\n", 2), ("E\nE2\n", 2)]), after, held(5));
        assert_eq!(got.sites, [run(2, 2), run(5, 6)]);
        assert_eq!(got.shift, 1, "one line in, none out, and one replaced");
        assert!(
            got.full,
            "it held every line before and wrote every difference"
        );
    }

    #[test]
    fn a_region_whose_text_occurs_twice_leaves_the_call_unplaced() {
        let after = "keep\nkeep\nnew\n";
        let got = facts(&regions(&[("keep\n", 2), ("new\n", 2)]), after, held(3));
        assert!(got.sites.is_empty(), "two candidates are no site at all");
        assert!(!got.full);
    }

    #[test]
    fn a_region_that_is_not_in_the_bytes_leaves_the_call_unplaced() {
        // The second replacement did not land as described.
        let after = "a\nB\nc\n";
        let got = facts(&regions(&[("B\n", 2), ("C\n", 2)]), after, held(3));
        assert!(got.sites.is_empty());
        assert!(!got.full);
    }

    #[test]
    fn regions_that_do_not_account_for_the_growth_claim_nothing() {
        // Both texts are there, and the file is a line longer than either
        // replacement made it: something else changed it as well.
        let after = "a\nB\nc\nD\nextra\n";
        let got = facts(&regions(&[("B\n", 2), ("D\n", 2)]), after, held(4));
        assert!(got.sites.is_empty());
        assert_eq!(got.shift, 1, "the measured growth is still reported");
        assert!(!got.full);
    }

    #[test]
    fn regions_lying_across_the_same_bytes_claim_nothing() {
        let after = "a\nfoo bar\nc\n";
        let got = facts(&regions(&[("foo bar\n", 2), ("bar", 1)]), after, held(3));
        assert!(got.sites.is_empty(), "one region was found inside another");
        assert!(!got.full);
    }

    #[test]
    fn a_region_at_the_end_of_the_file_is_placed_there() {
        let after = "a\nb\nlast";
        let got = facts(&regions(&[("a\n", 2), ("last", 1)]), after, held(3));
        assert_eq!(got.sites, [run(1, 1), run(3, 3)]);
        assert!(got.full);
    }

    #[test]
    fn two_regions_on_one_line_name_that_line_once() {
        let after = "x = left + right\n";
        let got = facts(&regions(&[("left", 1), ("right", 1)]), after, held(1));
        assert_eq!(got.sites, [run(1, 1)]);
        assert!(got.full);
    }

    #[test]
    fn a_window_that_held_part_of_the_file_holds_part_of_it_after() {
        let after = "a\nB\nc\nD\n";
        let prior = Some(PriorRevision {
            held_in_full: false,
            ..held(4).expect("a prior")
        });
        let got = facts(&regions(&[("B\n", 2), ("D\n", 2)]), after, prior);
        assert_eq!(
            got.sites,
            [run(2, 2), run(4, 4)],
            "the places are still named"
        );
        assert!(
            !got.full,
            "the lines it never saw are still lines it never saw"
        );
    }

    /// The line a multi-region edit writes is the line a lone edit writes,
    /// in the row format journals already hold — so a journal written before
    /// this reads the same, and a re-read is decided the same way.
    #[test]
    fn a_journal_row_with_several_sites_reads_back_and_declines_a_re_read() {
        let row = format!(
            r#"{{"v":1,"k":"edit","t":3,"p":"old.rs","h_old":"{}","h_new":"{}","sites":[[2,2],[5,6]],"shift":1,"full":true,"at":5}}"#,
            hash(1).to_hex(),
            hash(2).to_hex()
        );
        let line = Line::from_json_line(&row).expect("an edit row of the journal format");
        let Line::Edit { sites, full, .. } = &line else {
            panic!("an edit line, got {line:?}");
        };
        assert_eq!(sites.as_slice(), [run(2, 2), run(5, 6)]);
        assert!(*full);

        let journal = Journal::from_lines(vec![line]);
        let read = |lo, hi| ResolvedRead {
            path: "old.rs".to_owned(),
            hash: hash(2),
            range: run(lo, hi),
            total_lines: 40,
            channel: rift_agent::ReadChannel::Tool,
        };
        let decision = |lo, hi| {
            decide(
                &read(lo, hi),
                &journal,
                Mode::Enforce,
                Some(rift_agent::respond::Channel::DenyWithReason {
                    reason_field: "permissionDecisionReason",
                    confirmed: true,
                }),
                EditEcho::Pinned,
                64,
                Limits::default(),
            )
        };
        assert_eq!(
            decision(1, 40),
            rift_agent::window_journal::Decision::Deny(DenyShape::Delta {
                sites: vec![run(2, 2), run(5, 6)],
                shift: 1,
                window: run(1, 9),
            }),
            "re-reading the file is answered with where it changed"
        );
        assert_eq!(
            decision(5, 6),
            rift_agent::window_journal::Decision::Allow,
            "reading exactly a changed region is the right thing to do"
        );
    }
}

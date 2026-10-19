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

mod decision;
mod delivery;
mod family;
mod first_contact;
mod holds;
mod inherit;
mod intervals;
mod line;
mod matched;
mod narrowing;
pub mod output_match;
mod substitute;

use rift_core::Sha256Id;

use self::holds::held_of;

pub use self::decision::{
    decide, record, Decision, DenyShape, EditEcho, Limits, Mode, NARROW_MIN_HELD_LINES, NARROW_MODE,
};
pub use self::delivery::{reconcile_delivery, Reconcile, ResolvedRead, StatedDelivery};
pub use self::family::{
    decide_family_held, family_coverage, family_follow_up, held_runs, is_family_candidate,
    record_family, FamilyRoute, MAX_FAMILY_RUNS,
};
pub use self::first_contact::{decide_first_contact, FirstContactFloor, FIRST_CONTACT_MODE};
pub use self::holds::{DenyKey, HeldFile, StandingHold};
pub use self::inherit::{
    inherited_from_read, is_ident, record_inheritance, InheritClass, InheritModes, FORK_MODE,
    MAX_SPAWN_NAMES, PASTE_MODE,
};
pub use self::intervals::{Interval, IntervalSet};
pub use self::line::{EvictReason, Line};
pub use self::matched::{matched_lines, MATCHED_MODE, MAX_MATCHED_RUNS, OUTPUT_MATCHED};
pub use self::narrowing::{narrowing, Narrowing};
pub use self::substitute::{held_output, Delivered, HeldOutputDecision, HeldRun, SUBSTITUTE_MODE};

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

    /// The turn this window was last handed any of `run` of `(path, hash)` in,
    /// from reads alone.
    ///
    /// The latest, where several reads make up the run: a narrowing frame
    /// names one turn, and the last one the window saw the lines in is the
    /// one its context is most likely to still show.
    #[must_use]
    pub fn read_turn(&self, path: &str, hash: Sha256Id, run: Interval) -> Option<u32> {
        self.live().iter().rev().find_map(|line| match line {
            Line::Read {
                turn,
                path: p,
                hash: h,
                range,
                ..
            } if p == path && *h == hash && range.intersects(run) => Some(*turn),
            _ => None,
        })
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
    /// context window last lost what it held — with a reason, or with the
    /// file's outline in its stead.
    #[must_use]
    pub fn denied(&self, key: &DenyKey) -> bool {
        self.live().iter().any(|line| {
            matches!(
                line,
                Line::Deny { path, hash, range, .. } | Line::Outline { path, hash, range, .. }
                    if key.matches(path, *hash, *range)
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
                | Line::Outline { path: p, .. }
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

// Declared here rather than beside the other `mod` lines because the CI
// gates cut a file's production half at the first column-zero
// `#[cfg(test)]` that opens a module: above the journal it would hide every
// query below it from the decision-path walk.
#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::fixtures::{hash, held_hold, one_of_each, read_line, run};
    use super::*;
    use crate::{HiKind, ReadChannel, ReadRequest, ReadSpan};

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
                via: None,
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
                via: None,
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
}

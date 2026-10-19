//! The first contact: a read of a large file this window has never been
//! handed at this revision, which the gate may answer with the file's bound
//! outline instead — once, with the identical repeat allowed.
//!
//! A module of its own because it is a different decision from every other
//! one here. The rest decline a read because the window already holds the
//! bytes; this one answers a read the window does not hold, with a frame
//! that names the file's shape and definitions and the cheaper read to make.
//! Whether the outline is bound is the caller's question — it needs a file —
//! so this half sees only the journal, the request and the floors.

use super::decision::act_on;
use super::{Decision, DenyShape, Journal, Mode, ResolvedRead};
use crate::respond::Channel;

/// The mode the first-contact lever ships at: the documented default, which
/// the repository's `first_contact` policy key stands in for.
///
/// Enforce: the observe period's pool passed the lever's flip gate (owner
/// ruling 2026-10-19), so a first read the lever would answer is answered
/// where the family's channel is confirmed. A repository that writes
/// `observe` keeps the record without the answer; this constant caps nothing, and the
/// mode [`decide_first_contact`] is handed is the mode it acts on, under the
/// family's channel alone.
pub const FIRST_CONTACT_MODE: Mode = Mode::Enforce;

/// The size at which a file is large enough for its first read to be worth
/// answering with an outline: either floor is enough.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirstContactFloor {
    /// The fewest lines a large file has.
    pub min_lines: u32,
    /// The fewest bytes a large file has.
    pub min_bytes: u64,
    /// The fewest lines a first page must ask for: below it the outline frame
    /// costs more than the page it would stand in for. The frame's own
    /// break-even, which the crate that renders the frame states.
    pub min_page_lines: u32,
}

/// What to do about a read that may be a first contact, given what this
/// window has been handed.
///
/// Every gate is a reason **not** to outline, the cheapest first:
///
/// * the lever is off;
/// * the read does not start at line one — a mid-file range names the lines
///   it wants, and no outline is cheaper than those lines;
/// * the file is under both floors;
/// * the read is a first page shorter than the page floor;
/// * the window holds some of this revision already, so this is no first
///   contact;
/// * this exact request was already answered once, so the repeat is honoured.
///
/// Only then do the mode — the lever as the policy resolved it, which ships
/// at [`FIRST_CONTACT_MODE`] — and the family's channel decide between acting
/// and recording. `file_bytes` is the whole file's size, which the caller has
/// in hand from hashing it.
#[must_use]
pub fn decide_first_contact(
    read: &ResolvedRead,
    journal: &Journal,
    mode: Mode,
    channel: Option<Channel>,
    floor: FirstContactFloor,
    file_bytes: u64,
) -> Decision {
    if mode == Mode::Off || read.range.lo != 1 {
        return Decision::Allow;
    }
    if read.total_lines < floor.min_lines && file_bytes < floor.min_bytes {
        return Decision::Allow;
    }
    let whole = read.range.hi >= read.total_lines;
    if !whole && read.range.lines() < floor.min_page_lines {
        return Decision::Allow;
    }
    if !journal.coverage(&read.path, read.hash).is_empty() || journal.denied(&read.deny_key()) {
        return Decision::Allow;
    }
    act_on(DenyShape::Outlined, mode, true, channel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window_journal::{record, Interval, Limits, Line};
    use crate::{HiKind, ReadChannel, ReadRequest, ReadSpan};

    const FLOOR: FirstContactFloor = FirstContactFloor {
        min_lines: 400,
        min_bytes: 16_384,
        min_page_lines: 90,
    };

    fn hash(n: u8) -> rift_core::Sha256Id {
        rift_core::Sha256Id::from_bytes([n; 32])
    }

    fn read(lo: u32, hi: HiKind, total: u32) -> ResolvedRead {
        let request = ReadRequest {
            path: "src/big.rs".to_owned(),
            span: ReadSpan::lines(lo, hi),
            channel: ReadChannel::Tool,
        };
        ResolvedRead::new(&request, hash(1), total).unwrap()
    }

    fn whole(total: u32) -> ResolvedRead {
        read(1, HiKind::DefaultWindow(HiKind::END_OF_FILE), total)
    }

    const CONFIRMED: Option<Channel> = Some(Channel::DenyWithReason {
        reason_field: "permissionDecisionReason",
        confirmed: true,
    });

    /// The same channel before anyone watched a model receive it.
    const UNCONFIRMED: Option<Channel> = Some(Channel::DenyWithReason {
        reason_field: "permissionDecisionReason",
        confirmed: false,
    });

    fn first(read: &ResolvedRead, journal: &Journal, mode: Mode, bytes: u64) -> Decision {
        decide_first_contact(read, journal, mode, CONFIRMED, FLOOR, bytes)
    }

    /// ★ Under the default lever a first whole read and a first page of a
    /// large file are answered with the outline on a confirmed channel.
    #[test]
    fn under_the_default_a_first_read_of_a_large_file_is_answered_with_its_outline() {
        assert_eq!(FIRST_CONTACT_MODE, Mode::Enforce, "the documented default");
        let empty = Journal::default();
        for (read, bytes) in [
            (whole(400), 9_000),
            (whole(120), 16_384),
            (read(1, HiKind::Explicit(2_000), 900), 40_000),
            (read(1, HiKind::Explicit(90), 900), 40_000),
        ] {
            assert_eq!(
                first(&read, &empty, FIRST_CONTACT_MODE, bytes),
                Decision::Deny(DenyShape::Outlined),
                "{read:?}"
            );
        }
    }

    /// ★ The written lever decides, and the family's channel is the only
    /// cap: `enforce` on a confirmed channel answers the first read with the
    /// outline; the same lever on an unconfirmed channel, or on none, only
    /// observes it.
    #[test]
    fn a_first_read_is_answered_only_where_enforce_is_written_and_the_channel_confirmed() {
        let empty = Journal::default();
        let big = whole(900);
        assert_eq!(
            decide_first_contact(&big, &empty, Mode::Enforce, CONFIRMED, FLOOR, 40_000),
            Decision::Deny(DenyShape::Outlined)
        );
        assert_eq!(
            decide_first_contact(&big, &empty, Mode::Enforce, UNCONFIRMED, FLOOR, 40_000),
            Decision::Observe(DenyShape::Outlined)
        );
        assert_eq!(
            decide_first_contact(&big, &empty, Mode::Enforce, None, FLOOR, 40_000),
            Decision::Observe(DenyShape::Outlined)
        );
    }

    /// A small file, a mid-file range, a short first page and a lever that
    /// is off are never outlined.
    #[test]
    fn only_a_first_page_or_whole_read_of_a_large_file_is_a_candidate() {
        let empty = Journal::default();
        for (read, bytes, mode) in [
            (whole(399), 16_383, Mode::Observe),
            (read(401, HiKind::Explicit(600), 900), 40_000, Mode::Observe),
            (read(1, HiKind::Explicit(89), 900), 40_000, Mode::Observe),
            (whole(900), 40_000, Mode::Off),
        ] {
            assert_eq!(
                first(&read, &empty, mode, bytes),
                Decision::Allow,
                "{read:?}"
            );
        }
    }

    /// A window that holds any of the revision is past its first contact, and
    /// the repeat of a request already answered once runs.
    #[test]
    fn a_held_revision_or_an_answered_request_is_no_first_contact() {
        let big = whole(900);
        let held = Journal::from_lines(vec![Line::Read {
            turn: 1,
            path: "src/big.rs".to_owned(),
            hash: hash(1),
            range: Interval { lo: 500, hi: 520 },
            total_lines: 900,
            at_ms: 0,
        }]);
        assert_eq!(first(&big, &held, Mode::Observe, 40_000), Decision::Allow);
        let answered = Journal::from_lines(vec![Line::Outline {
            turn: 1,
            path: "src/big.rs".to_owned(),
            hash: hash(1),
            range: big.range,
            at_ms: 0,
        }]);
        assert_eq!(
            first(&big, &answered, Mode::Observe, 40_000),
            Decision::Allow
        );
        let other_revision = Journal::from_lines(vec![Line::Read {
            turn: 1,
            path: "src/big.rs".to_owned(),
            hash: hash(2),
            range: Interval { lo: 1, hi: 900 },
            total_lines: 900,
            at_ms: 0,
        }]);
        assert_eq!(
            first(&big, &other_revision, Mode::Observe, 40_000),
            Decision::Observe(DenyShape::Outlined)
        );
    }

    /// An observed outline leaves the read the call ran as, so the window is
    /// credited with what it was handed; an answered one would leave an
    /// `outline` line, which is no coverage at all.
    #[test]
    fn an_observed_outline_records_the_read_and_an_answered_one_records_none() {
        let big = whole(900);
        let observed = record(
            &big,
            &Decision::Observe(DenyShape::Outlined),
            40_000,
            Limits::default(),
            3,
            7,
        );
        assert_eq!(observed.kind(), "read");
        let answered = record(
            &big,
            &Decision::Deny(DenyShape::Outlined),
            40_000,
            Limits::default(),
            3,
            7,
        );
        assert_eq!(answered.kind(), "outline");
        let journal = Journal::from_lines(vec![answered.clone()]);
        assert!(journal.coverage("src/big.rs", hash(1)).is_empty());
        assert_eq!(
            Line::from_json_line(&answered.to_json_line()),
            Some(answered)
        );
    }

    proptest::proptest! {
        /// ★ Below enforce — the default among them — no decision carries an
        /// answered outline, and neither does enforce on a channel no run has
        /// confirmed.
        #[test]
        fn no_outline_is_answered_below_enforce_or_on_an_unconfirmed_channel(
            lo in 1u32..50,
            span in 0u32..3_000,
            total in 1u32..5_000,
            bytes in 0u64..100_000,
            (mode, channel) in proptest::sample::select(vec![
                (Mode::Off, CONFIRMED),
                (Mode::Observe, CONFIRMED),
                (Mode::Enforce, UNCONFIRMED),
                (Mode::Enforce, None),
            ]),
            held in proptest::bool::ANY,
        ) {
            let lo = lo.min(total);
            let read = read(lo, HiKind::Explicit(lo.saturating_add(span)), total);
            let journal = if held {
                Journal::from_lines(vec![Line::Read {
                    turn: 1,
                    path: "src/big.rs".to_owned(),
                    hash: hash(1),
                    range: Interval { lo, hi: lo },
                    total_lines: total,
                    at_ms: 0,
                }])
            } else {
                Journal::default()
            };
            let decision = decide_first_contact(&read, &journal, mode, channel, FLOOR, bytes);
            proptest::prop_assert!(!matches!(decision, Decision::Deny(_)), "{decision:?}");
        }
    }
}

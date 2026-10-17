//! Where a request that is not wholly held stands against what is: nothing
//! of it held, one held run at one end, or held lines scattered through it.
//!
//! A separate module because the classification is asked twice — by the
//! decision, which narrows only the second case, and by the replay, which
//! publishes all three — and one answer to both is what keeps the published
//! split a description of what the gate does.

use super::{Interval, Journal, ResolvedRead};

/// How a request that is not wholly held relates to what is, at the file's
/// current revision and from the window's own reads alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Narrowing {
    /// Nothing of the request is held, or all of it is — neither is a narrowing.
    NotNarrowable,
    /// One held run at one end of the request, and one unheld run beside it.
    Narrow {
        /// The held run, inside the request.
        held: Interval,
        /// The rest of the request.
        read: Interval,
    },
    /// The held lines fall in two or more runs, or what is left of the request
    /// does: a frame would have to name two windows, and none is sent.
    Multi,
}

/// Which way a request that is not wholly held relates to the window's own
/// reads of the file's current revision.
///
/// Read coverage only, never [`Journal::coverage`]: a window that wrote the
/// file in full holds all of it, so it never reaches here, and an edit that
/// left only its sites says nothing about the lines between them. Coverage of
/// an older revision, another window's, or lines from before the window last
/// lost what it held are not in [`Journal::read_coverage`] at all.
#[must_use]
pub fn narrowing(read: &ResolvedRead, journal: &Journal) -> Narrowing {
    let coverage = journal.read_coverage(&read.path, read.hash);
    let held: Vec<Interval> = coverage
        .runs()
        .iter()
        .filter_map(|run| run.intersection(read.range))
        .collect();
    let [held] = held.as_slice() else {
        return if held.is_empty() {
            Narrowing::NotNarrowable
        } else {
            Narrowing::Multi
        };
    };
    let before = Interval::new(read.range.lo, held.lo.saturating_sub(1));
    let after = held
        .hi
        .checked_add(1)
        .and_then(|lo| Interval::new(lo, read.range.hi));
    match (before, after) {
        (Some(rest), None) | (None, Some(rest)) => Narrowing::Narrow {
            held: *held,
            read: rest,
        },
        (Some(_), Some(_)) => Narrowing::Multi,
        (None, None) => Narrowing::NotNarrowable,
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{hash, read_line, resolved, run};
    use super::super::{
        decide, record, Decision, DenyShape, EditEcho, EvictReason, Limits, Line, Mode,
        NARROW_MIN_HELD_LINES, NARROW_MODE,
    };
    use super::*;
    use crate::respond::Channel;

    /// The one channel a decline may travel on.
    const CONFIRMED: Channel = Channel::DenyWithReason {
        reason_field: "permissionDecisionReason",
        confirmed: true,
    };

    /// The same channel before anyone watched a model receive it.
    const UNCONFIRMED: Channel = Channel::DenyWithReason {
        reason_field: "permissionDecisionReason",
        confirmed: false,
    };

    /// The settings with the narrowing lever written `enforce`, which is what
    /// the shape table is about; the default has its own test.
    fn narrowing_enforced() -> Limits {
        Limits {
            narrow_mode: Mode::Enforce,
            ..Limits::default()
        }
    }

    /// An enforcing family on a confirmed channel, deciding `read`.
    fn decide_enforcing(read: &ResolvedRead, journal: &Journal, limits: Limits) -> Decision {
        decide(
            read,
            journal,
            Mode::Enforce,
            Some(CONFIRMED),
            EditEcho::Pinned,
            64,
            limits,
        )
    }

    /// One window that read lines `lo`-`hi` of a nine-hundred-line file.
    fn held(lo: u32, hi: u32) -> Journal {
        Journal::from_lines(vec![read_line(1, "src/lib.rs", 0x3f, lo, hi)])
    }

    /// ★ The shape table: a request wholly inside coverage is the plain
    /// decline it always was; one that runs past either end of one held run
    /// is narrowed to the rest; and every other relation to coverage is
    /// allowed — held lines in the middle, two held runs, coverage of another
    /// revision — with the replay's name for each.
    #[test]
    fn a_request_held_at_one_end_is_narrowed_and_every_other_relation_runs() {
        let limits = narrowing_enforced();
        let table = [
            (held(1, 200), (50, 150), Decision::Deny(DenyShape::Plain)),
            (
                held(1, 200),
                (101, 400),
                Decision::Deny(DenyShape::Narrow {
                    held: run(101, 200),
                    read: run(201, 400),
                }),
            ),
            (
                held(300, 600),
                (101, 500),
                Decision::Deny(DenyShape::Narrow {
                    held: run(300, 500),
                    read: run(101, 299),
                }),
            ),
            (held(300, 400), (101, 600), Decision::Allow),
        ];
        for (journal, (lo, hi), expected) in table {
            let read = resolved("src/lib.rs", 0x3f, lo, hi, 900);
            assert_eq!(
                decide_enforcing(&read, &journal, limits),
                expected,
                "{lo}-{hi}"
            );
        }
        assert_eq!(
            narrowing(
                &resolved("src/lib.rs", 0x3f, 101, 600, 900),
                &held(300, 400)
            ),
            Narrowing::Multi,
            "held lines in the middle leave two runs to read"
        );

        let two_runs = Journal::from_lines(vec![
            read_line(1, "src/lib.rs", 0x3f, 1, 200),
            read_line(2, "src/lib.rs", 0x3f, 300, 500),
        ]);
        let across = resolved("src/lib.rs", 0x3f, 150, 450, 900);
        assert_eq!(
            decide_enforcing(&across, &two_runs, limits),
            Decision::Allow
        );
        assert_eq!(narrowing(&across, &two_runs), Narrowing::Multi);

        let newer = resolved("src/lib.rs", 0x40, 101, 400, 900);
        assert_eq!(
            decide_enforcing(&newer, &held(1, 200), limits),
            Decision::Allow
        );
        assert_eq!(
            narrowing(&newer, &held(1, 200)),
            Narrowing::NotNarrowable,
            "lines of another revision are other bytes"
        );
    }

    /// ★ A narrowing rests on the window's own reads and nothing else: not on
    /// an edit, however much of the file it wrote, and not on lines read
    /// before the window last lost what it held.
    #[test]
    fn neither_an_edit_nor_a_lost_read_is_narrowed_on() {
        let limits = narrowing_enforced();
        let edited = Journal::from_lines(vec![
            read_line(1, "src/lib.rs", 0x3f, 1, 900),
            Line::Edit {
                turn: 2,
                path: "src/lib.rs".to_owned(),
                hash_old: Some(hash(0x3f)),
                hash_new: hash(0x40),
                sites: vec![run(37, 44)],
                shift: 0,
                full: false,
                at_ms: 2,
            },
        ]);
        let around_the_edit = resolved("src/lib.rs", 0x40, 30, 300, 900);
        assert_eq!(
            decide_enforcing(&around_the_edit, &edited, limits),
            Decision::Allow
        );
        assert_eq!(
            narrowing(&around_the_edit, &edited),
            Narrowing::NotNarrowable
        );

        let mut lost = held(1, 200);
        lost.push(Line::Evict {
            turn: 2,
            reason: EvictReason::Compact,
            at_ms: 2,
        });
        let after = resolved("src/lib.rs", 0x3f, 101, 400, 900);
        assert_eq!(decide_enforcing(&after, &lost, limits), Decision::Allow);
        assert_eq!(narrowing(&after, &lost), Narrowing::NotNarrowable);
    }

    /// ★ One narrowing per key: the identical repeat runs, and the line it
    /// leaves is a read of the whole range, so the window holds it all.
    #[test]
    fn the_repeat_of_a_narrowed_call_runs_and_is_recorded_as_read() {
        let limits = narrowing_enforced();
        let read = resolved("src/lib.rs", 0x3f, 101, 400, 900);
        let mut journal = held(1, 200);
        let first = decide_enforcing(&read, &journal, limits);
        assert!(
            matches!(first, Decision::Deny(DenyShape::Narrow { .. })),
            "{first:?}"
        );
        journal.push(record(&read, &first, 64, limits, 2, 2));

        let repeat = decide_enforcing(&read, &journal, limits);
        assert_eq!(repeat, Decision::Allow, "the repeat is honoured");
        let line = record(&read, &repeat, 64, limits, 3, 3);
        assert_eq!(line.kind(), "read");
        journal.push(line);
        assert!(journal
            .read_coverage("src/lib.rs", hash(0x3f))
            .contains(run(1, 400)));
    }

    /// ★ The floor is counted on the held lines, at the computed constant, and
    /// a narrowing under it is still decided and recorded — observed, never
    /// declined — exactly as a plain slice under its own floor is. The plain
    /// floor is another frame's price and does not move this one.
    #[test]
    fn a_narrowing_under_its_floor_is_observed_and_never_declined() {
        let limits = narrowing_enforced();
        let journal = held(1, 200);
        let holding = |lines: u32| resolved("src/lib.rs", 0x3f, 201 - lines, 400, 900);

        let under = holding(NARROW_MIN_HELD_LINES - 1);
        assert!(matches!(
            decide_enforcing(&under, &journal, limits),
            Decision::Observe(DenyShape::Narrow { .. })
        ));
        let at = holding(NARROW_MIN_HELD_LINES);
        assert!(matches!(
            decide_enforcing(&at, &journal, limits),
            Decision::Deny(DenyShape::Narrow { .. })
        ));
        let plain_floor_raised = Limits {
            min_slice_lines: NARROW_MIN_HELD_LINES + 100,
            ..limits
        };
        assert!(
            matches!(
                decide_enforcing(&at, &journal, plain_floor_raised),
                Decision::Deny(DenyShape::Narrow { .. })
            ),
            "the plain frame's floor does not bind the narrowing frame"
        );
    }

    /// ★ Under the default policy an enforcing family only observes a
    /// narrowing: the lever's default is observe, the default settings carry
    /// it, and the family's own mode does not lift it — the decision is
    /// reached, recorded, and the call runs.
    #[test]
    fn under_the_default_policy_an_enforcing_family_only_observes_a_narrowing() {
        assert_eq!(NARROW_MODE, Mode::Observe, "the documented default");
        assert_eq!(Limits::default().narrow_mode, NARROW_MODE);
        let read = resolved("src/lib.rs", 0x3f, 101, 400, 900);
        assert_eq!(
            decide_enforcing(&read, &held(1, 200), Limits::default()),
            Decision::Observe(DenyShape::Narrow {
                held: run(101, 200),
                read: run(201, 400),
            })
        );
        let off = Limits {
            narrow_mode: Mode::Off,
            ..Limits::default()
        };
        assert_eq!(
            decide_enforcing(&read, &held(1, 200), off),
            Decision::Allow,
            "with narrowing off the decision is the build without it"
        );
    }

    /// ★ The written lever decides, and the family's channel is the only
    /// cap: narrowing at `enforce` declines on a confirmed channel and only
    /// observes on an unconfirmed one, or on none.
    #[test]
    fn a_narrowing_is_declined_only_where_enforce_is_written_and_the_channel_confirmed() {
        let read = resolved("src/lib.rs", 0x3f, 101, 400, 900);
        let journal = held(1, 200);
        let shape = DenyShape::Narrow {
            held: run(101, 200),
            read: run(201, 400),
        };
        let under = |channel: Option<Channel>| {
            decide(
                &read,
                &journal,
                Mode::Enforce,
                channel,
                EditEcho::Pinned,
                64,
                narrowing_enforced(),
            )
        };
        assert_eq!(under(Some(CONFIRMED)), Decision::Deny(shape.clone()));
        assert_eq!(under(Some(UNCONFIRMED)), Decision::Observe(shape.clone()));
        assert_eq!(under(None), Decision::Observe(shape));
    }

    /// ★ **An observed narrowing leaves the window holding what it was handed
    /// — no less, and no more.** The call ran, so the lines it delivered are
    /// coverage like any allowed read's; and a slice past what a delivery can
    /// vouch for is credited with its first line, exactly as an allowed read
    /// is.
    #[test]
    fn an_observed_narrowing_records_what_the_call_delivered_as_coverage() {
        let limits = Limits::default();
        let read = resolved("src/lib.rs", 0x3f, 101, 400, 900);
        let mut journal = held(1, 200);
        let decision = decide_enforcing(&read, &journal, limits);
        assert!(matches!(
            decision,
            Decision::Observe(DenyShape::Narrow { .. })
        ));
        let line = record(&read, &decision, 64, limits, 2, 2);
        assert_eq!(line, record(&read, &Decision::Allow, 64, limits, 2, 2));
        journal.push(line);
        let coverage = journal.read_coverage("src/lib.rs", hash(0x3f));
        assert!(coverage.contains(run(1, 400)), "no less than it was handed");
        assert!(!coverage.contains(run(401, 401)), "and no more");

        let too_large = limits.max_delivered_bytes + 1;
        let mut journal = held(1, 200);
        journal.push(record(&read, &decision, too_large, limits, 2, 2));
        assert!(
            !journal
                .read_coverage("src/lib.rs", hash(0x3f))
                .contains(run(201, 201)),
            "a delivery nothing vouches for grants nothing past its first line"
        );
    }

    proptest::proptest! {
        /// ★ A narrowing's held run is always lines the window read at this
        /// revision, inside the request, and the held run and the run to read
        /// are exactly the request — disjoint, adjacent, and nothing beside.
        #[test]
        fn a_narrowing_holds_only_read_lines_and_splits_the_request_exactly(
            reads in proptest::collection::vec((1u32..=400, 0u32..=120), 1..5),
            lo in 1u32..=500,
            span in 0u32..=300,
        ) {
            let journal = Journal::from_lines(
                reads
                    .iter()
                    .map(|(at, len)| read_line(1, "src/lib.rs", 0x3f, *at, at + len))
                    .collect(),
            );
            let request = resolved("src/lib.rs", 0x3f, lo, lo + span, 900);
            let Narrowing::Narrow { held, read } = narrowing(&request, &journal) else {
                return Ok(());
            };
            let coverage = journal.read_coverage("src/lib.rs", hash(0x3f));
            proptest::prop_assert!(coverage.contains(held), "{held:?} is not held");
            proptest::prop_assert!(request.range.contains(held));
            proptest::prop_assert!(request.range.contains(read));
            proptest::prop_assert!(!held.intersects(read));
            proptest::prop_assert_eq!(held.lines() + read.lines(), request.range.lines());
            proptest::prop_assert!(
                held.hi + 1 == read.lo || read.hi + 1 == held.lo,
                "the two runs meet"
            );
            proptest::prop_assert!(
                !coverage.runs().iter().any(|run| run.intersects(read)),
                "nothing of the run to read is held"
            );
            let decided = decide_enforcing(&request, &journal, narrowing_enforced());
            if let Decision::Deny(shape) | Decision::Observe(shape) = decided {
                proptest::prop_assert_eq!(shape, DenyShape::Narrow { held, read });
            }
        }
    }
}

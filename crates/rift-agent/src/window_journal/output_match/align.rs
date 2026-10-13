//! Cutting a delivery into segments and giving each to the one member of the
//! command line that owns it.
//!
//! A chain's output is one stream, and coverage is booked per member: a
//! segment is a stretch of consecutive grounded lines of one run, and it is
//! booked to exactly one member, in order: the first from the last owner
//! onward that names its file, or the next such member when the first has
//! already shown those lines. A segment no member from the last owner onward
//! names, or only a `cd` names, books nothing, because a segment nobody can
//! own is output nobody can account for.

use std::collections::BTreeMap;

use super::super::{Interval, IntervalSet};
use super::place::Placement;

/// The lines of one file a member was shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Booking {
    /// The member, counted from zero.
    pub member: usize,
    /// The file, by index.
    pub file: usize,
    /// The runs of the file, sorted and disjoint.
    pub runs: Vec<Interval>,
}

/// What the alignment knows about each member and each file.
pub struct Owners<'a> {
    /// For each file, the members that name it, ascending.
    pub named_by: &'a [Vec<usize>],
    /// For each member, whether it changes directory.
    pub cd: &'a [bool],
}

/// The bookings a delivery's placements make, one per member and file, in
/// member order.
#[must_use]
pub fn align(owners: &Owners<'_>, placed: &[Option<Placement>]) -> Vec<Booking> {
    let mut cursor = 0usize;
    let mut last: BTreeMap<(usize, usize), u32> = BTreeMap::new();
    let mut booked: BTreeMap<(usize, usize), IntervalSet> = BTreeMap::new();
    for segment in segments(placed) {
        let Some(member) = owner(owners, &segment, cursor, &last) else {
            continue;
        };
        cursor = member;
        let set = booked.entry((member, segment.file)).or_default();
        for line in &segment.lines {
            if let Some(run) = Interval::new(*line, *line) {
                set.insert(run);
            }
        }
        let end = segment.lines.last().copied().unwrap_or_default();
        last.insert((member, segment.file), end);
    }
    booked
        .into_iter()
        .map(|((member, file), set)| Booking {
            member,
            file,
            runs: set.runs().to_vec(),
        })
        .collect()
}

/// A stretch of consecutive grounded lines placed in one run.
struct Segment {
    file: usize,
    lines: Vec<u32>,
}

/// The segments of a delivery, in output order.
fn segments(placed: &[Option<Placement>]) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut open: Option<usize> = None;
    for slot in placed {
        let Some(place) = slot.filter(|p| p.grounded) else {
            open = None;
            continue;
        };
        match (open, out.last_mut()) {
            (Some(run), Some(last)) if run == place.run => last.lines.push(place.line),
            _ => out.push(Segment {
                file: place.file,
                lines: vec![place.line],
            }),
        }
        open = Some(place.run);
    }
    out
}

/// The one member a segment aligns to: the first from `cursor` onward that
/// names its file and is not a `cd`, moving past it to the next when it has
/// already shown these lines — output runs forward through a member, and a
/// file printed again is the next member's. `None` when no member is left.
fn owner(
    owners: &Owners<'_>,
    segment: &Segment,
    cursor: usize,
    last: &BTreeMap<(usize, usize), u32>,
) -> Option<usize> {
    let first = segment.lines.first().copied().unwrap_or_default();
    let mut naming = owners
        .named_by
        .get(segment.file)?
        .iter()
        .copied()
        .filter(|member| *member >= cursor && !owners.cd.get(*member).copied().unwrap_or(true));
    let candidate = naming.next()?;
    let shown_again = last
        .get(&(candidate, segment.file))
        .is_some_and(|end| first <= *end);
    if shown_again {
        return naming.next();
    }
    Some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(file: usize, line: u32, run: usize) -> Option<Placement> {
        Some(Placement {
            file,
            line,
            run,
            grounded: true,
        })
    }

    /// ★ Two members' segments are booked to each in order; a segment no
    /// member names books nothing.
    #[test]
    fn each_segment_is_booked_to_the_member_that_names_its_file() {
        let named_by = [vec![1], vec![2], vec![]];
        let owners = Owners {
            named_by: &named_by,
            cd: &[true, false, false],
        };
        let placed = [at(0, 4, 0), at(0, 5, 0), None, at(1, 9, 1), at(2, 1, 2)];
        let booked = align(&owners, &placed);
        assert_eq!(
            booked,
            [
                Booking {
                    member: 1,
                    file: 0,
                    runs: vec![Interval::new(4, 5).unwrap()]
                },
                Booking {
                    member: 2,
                    file: 1,
                    runs: vec![Interval::new(9, 9).unwrap()]
                },
            ]
        );
    }

    /// ★ Zero coverage for an unaligned segment: a file only a `cd` names, a
    /// file no member names, and a segment out of member order.
    #[test]
    fn an_unaligned_segment_books_nothing() {
        let placed = [at(0, 1, 0), at(0, 2, 0)];
        for (named_by, cd) in [(vec![vec![0]], vec![true]), (vec![vec![]], vec![false])] {
            let owners = Owners {
                named_by: &named_by,
                cd: &cd,
            };
            assert!(align(&owners, &placed).is_empty(), "{named_by:?} {cd:?}");
        }
        // The second file is named only by a member before the first
        // segment's owner, so it is out of order and books nothing.
        let named_by = [vec![1], vec![0]];
        let owners = Owners {
            named_by: &named_by,
            cd: &[false, false],
        };
        let booked = align(&owners, &[at(0, 1, 0), at(1, 1, 1)]);
        assert_eq!(booked.len(), 1);
        assert_eq!(booked[0].member, 1);
    }

    /// ★ The same file read twice in one chain books each read to its own
    /// member, in order: the second printing goes to the next member.
    #[test]
    fn a_file_printed_twice_is_booked_to_each_member_in_turn() {
        let named_by = [vec![0, 2]];
        let owners = Owners {
            named_by: &named_by,
            cd: &[false, false, false],
        };
        let placed = [at(0, 1, 0), at(0, 2, 0), None, at(0, 1, 1), at(0, 2, 1)];
        let members: Vec<usize> = align(&owners, &placed).iter().map(|b| b.member).collect();
        assert_eq!(members, [0, 2]);
    }

    /// Under `2>&1` a foreign line splits a run into two segments, and
    /// each is booked on its own grounding.
    #[test]
    fn a_run_split_by_a_foreign_line_is_booked_piece_by_piece() {
        let named_by = [vec![0]];
        let owners = Owners {
            named_by: &named_by,
            cd: &[false],
        };
        let placed = [
            at(0, 1, 0),
            at(0, 2, 0),
            None,
            at(0, 3, 1),
            Some(Placement {
                file: 0,
                line: 4,
                run: 1,
                grounded: false,
            }),
        ];
        let booked = align(&owners, &placed);
        assert_eq!(booked[0].runs, vec![Interval::new(1, 3).unwrap()]);
    }
}

//! Narrowings stacked behind one read: each keeps a run of the run before it,
//! so the stack is one run of the file wherever the counts alone place it.

use crate::shell_read::{recognise, ShellCall, ShellSpan};
use crate::{HiKind, ReadSpan};

use super::{member, read, read_of};

/// **Stacked narrowings on one read compose into the run they keep.**
///
/// The first `20` lines and then the last `5` of those are lines 16 to 20,
/// and every pair of `head`, `tail` and `sed -n 'A,Bp'` whose ends the
/// command line states composes the same way, through any number of stages
/// and over any member whose own run is a number. A run that turns out to
/// lie past the end of a shorter file is not one of its intervals, and the
/// resolution step drops it, as it does for one narrowing.
#[test]
fn stacked_narrowings_on_one_read_keep_the_run_their_counts_name() {
    let file = "src/lib.rs".to_owned();
    let lines = |lo, hi| Some((file.clone(), ReadSpan::lines(lo, HiKind::Explicit(hi))));
    for (command, expected) in [
        ("cat src/lib.rs | head -n 20 | tail -n 5", lines(16, 20)),
        ("cat src/lib.rs | head -n 5 | tail -n 20", lines(1, 5)),
        ("cat src/lib.rs | head -n 50 | head -n 10", lines(1, 10)),
        ("cat src/lib.rs | sed -n '1,5p' | head -n 2", lines(1, 2)),
        (
            "cat src/lib.rs | sed -n '10,40p' | sed -n '5,8p'",
            lines(14, 17),
        ),
        (
            "cat src/lib.rs | sed -n '10,40p' | tail -n 3",
            lines(38, 40),
        ),
        (
            "cat src/lib.rs | head -n 30 | sed -n '25,90p'",
            lines(25, 30),
        ),
        (
            "sed -n '100,200p' src/lib.rs | head -n 50 | tail -n 10",
            lines(140, 149),
        ),
        (
            "head -n 80 src/lib.rs | tail -n 30 | head -n 10",
            lines(51, 60),
        ),
        (
            "cat src/lib.rs | head -n 100 | tail -n 50 | head -n 10",
            lines(51, 60),
        ),
        (
            "cd crates && cat src/lib.rs | head -n 20 | tail -n 5",
            Some((
                "crates/src/lib.rs".to_owned(),
                ReadSpan::lines(16, HiKind::Explicit(20)),
            )),
        ),
    ] {
        assert_eq!(read_of(command), expected, "{command}");
    }
    assert_eq!(
        read_of("cat src/lib.rs | tail -n 30 | tail -n 5"),
        Some((file.clone(), ReadSpan::LastLines { count: 5 }))
    );
}

/// **A stack whose counts do not place a run stays counted, never coverage.**
///
/// The first `5` of the last `30` start at a line only the file's length
/// names; a pick past the end of the run before it printed nothing; and a
/// stack behind several joined reads, or behind a filter, cuts lines no
/// count of one file names. Each read is still counted.
#[test]
fn a_stack_the_counts_cannot_place_leaves_each_read_filtered() {
    for command in [
        "cat src/lib.rs | tail -n 30 | head -n 5",
        "cat src/lib.rs | tail -n 30 | sed -n '2,4p'",
        "cat src/lib.rs | head -n 5 | sed -n '9,12p'",
        "cat src/lib.rs | sed -n '10,40p' | sed -n '35,50p'",
        "cat src/lib.rs | head -n 20 | grep needle | tail -n 5",
    ] {
        assert_eq!(
            recognise(command),
            Some(read("src/lib.rs", ShellSpan::Filtered)),
            "{command}"
        );
        assert_eq!(read_of(command), None, "{command}");
    }
    assert_eq!(
        recognise("cat a.rs b.rs | head -n 20 | tail -n 5"),
        Some(ShellCall::Read(vec![
            member("a.rs", ShellSpan::Filtered),
            member("b.rs", ShellSpan::Filtered),
        ]))
    );
}
